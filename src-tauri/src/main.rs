// The release build is a GUI app: no console window behind it.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// First, and out of alphabetical order: it defines `t!`, and a macro is only
// in scope for the modules declared after it.
#[macro_use]
mod i18n;

mod auth;
mod controls;
mod dialog;
mod dsh;
mod notify;
mod panel;
mod plugins;
mod server;
mod settings;
mod setup;
mod theme;
mod turn;
mod update;
mod waiting;

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Once, RwLock};
use std::time::{Duration, Instant};

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::webview::{NewWindowResponse, PageLoadEvent};
use tauri::{Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tauri_plugin_autostart::{ManagerExt, MacosLauncher};
use tauri_plugin_opener::OpenerExt;

/// The origin `dsh web` bound to, once it has. Navigation inside it stays in the
/// window; anything else is a link to the outside world.
type Origin = Arc<RwLock<Option<String>>>;

/// Where the bundled loading page is, learned from the first page load rather
/// than asked for when the window is built.
///
/// There is no other way to name it — it is `http://tauri.localhost` in a release
/// build and the dev server under `tauri dev` — and asking the webview is not one:
/// `WebviewWindow::url()` reads what the webview has navigated to, and inside
/// `setup` it has not navigated yet, so the answer is `about:blank`. Sending the
/// window *there* for an update's progress is a blank window, which is what this
/// exists to have got wrong once.
type Home = Arc<RwLock<Option<Url>>>;

/// How long to sit on the boot message before telling the user it is slow.
const SLOW_BOOT: Duration = Duration::from_secs(20);

/// How many times in a row a `dsh web` that died young is started again before
/// the app stops trying and says so; see [`watch`].
///
/// "In a row" is the load-bearing half. A server that had been up for [`STEADY`]
/// before it went is a fresh incident and gets the whole budget again — without
/// that, a machine where dsh dies once an hour would eventually spend a lifetime
/// counter and stop being restarted for no reason the user could see. What is
/// given up on is only a dsh that cannot stay up.
const RESTARTS: usize = 3;

/// How long a server has to have been serving for its exit to count as an
/// incident of its own rather than another turn of a crash loop.
const STEADY: Duration = Duration::from_secs(60);

/// How long a restarted server gets to print its URL before the attempt is
/// called a failure. [`serve`] waits for as long as it takes and says so on the
/// loading page; this one runs behind a page the user is still working in, where
/// the only thing it has to say it with is a line of status text.
const RESUME_TIMEOUT: Duration = Duration::from_secs(60);

/// Passed by the login item the tray menu creates. The app is starting because
/// the machine did, not because anyone asked to see it, so it waits in the tray
/// with dsh already running behind it.
const AUTOSTART_FLAG: &str = "--autostart";

/// What the boot and a dsh update both work on: the window's loading page, the
/// origin navigation is judged against, and the server that is running or about
/// to be.
///
/// Cloned rather than borrowed — every field is already shared, and both users
/// are threads that outlive the call that spawned them. A clone also lives in
/// Tauri's state, which is how a click in the window's own menu reaches it: the
/// navigation handler that receives one has an `AppHandle` and nothing else.
#[derive(Clone)]
struct Session {
    origin: Origin,
    splash: Splash,
    server: Arc<Mutex<Option<server::Server>>>,
    /// Somewhere to put the window back to when dsh has to come down for an
    /// update; see [`Home`].
    home: Home,
    /// Which server the watcher in [`watch`] is watching.
    ///
    /// A `dsh web` that exits is worth interrupting the user over — unless it
    /// exited because this app stopped it, which is how an update and a plugin
    /// install both begin. The two are indistinguishable from the child's side:
    /// the pipes close either way. So every deliberate stop moves this on, and
    /// a watcher whose number is no longer current knows the exit was ours and
    /// says nothing.
    epoch: Arc<AtomicU64>,
    /// The second try at `dsh web`'s token exchange, for the webviews that need
    /// one; see [`auth`].
    auth: auth::Retry,
}

fn main() {
    // Before anything else, and from the thread that is about to become the
    // event loop: `dialog::confirm` blocks on a message only this thread can
    // deliver, and this is what lets it assert it is not being called here.
    dialog::remember_main_thread();

    let server: Arc<Mutex<Option<server::Server>>> = Arc::new(Mutex::new(None));
    let setup_server = server.clone();

    let app = tauri::Builder::default()
        // First, before anything this process would otherwise start: a second
        // launch has to be turned away before it spawns a dsh of its own. What
        // the user meant by launching again is "show me the app", so the copy
        // already running answers for it.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            reveal(app);
        }))
        .plugin(tauri_plugin_opener::init())
        // Raised from Rust only; nothing in the webview is granted it. See
        // `notify`.
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![AUTOSTART_FLAG]),
        ))
        .setup(move |app| {
            let preference = theme::preference();
            let origin: Origin = Arc::new(RwLock::new(None));
            let home: Home = Arc::new(RwLock::new(None));
            let splash = Splash::default();
            let auth = auth::Retry::default();
            let visible = !std::env::args().any(|argument| argument == AUTOSTART_FLAG);

            // The window comes up first now: the update check runs behind it and
            // can put a question and a progress bar on the loading page, neither
            // of which has anywhere to go without a window.
            let window = build_window(
                app.handle(),
                origin.clone(),
                home.clone(),
                splash.clone(),
                auth.clone(),
                preference,
                visible,
            )?;

            let session = Session {
                origin,
                splash,
                server: setup_server.clone(),
                home,
                epoch: Arc::new(AtomicU64::new(0)),
                auth,
            };
            app.manage(session.clone());

            build_tray(app.handle())?;

            theme::paint(&window, preference);

            boot(app.handle().clone(), window, session);

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build the dsh desktop app");

    app.run(move |_handle, event| {
        if let tauri::RunEvent::Exit = event {
            dsh::stop();
            plugins::stop();
            if let Some(child) = server.lock().unwrap().as_mut() {
                child.stop();
            }
        }
    });
}

/// The one window: it opens on the local loading page and is navigated to the
/// dsh UI once the server is up. `visible` is false when the app was started by
/// the login item, where it belongs in the tray until it is asked for.
fn build_window(
    app: &tauri::AppHandle,
    origin: Origin,
    home: Home,
    splash: Splash,
    auth: auth::Retry,
    preference: theme::Preference,
    visible: bool,
) -> tauri::Result<WebviewWindow> {
    let opener = app.clone();
    let closer = app.clone();
    let new_window_opener = app.clone();
    let new_window_origin = origin.clone();

    let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("dsh desktop")
        .inner_size(1360.0, 900.0)
        .min_inner_size(720.0, 520.0)
        // The same place every launch. Restoring the last geometry meant the
        // window was built here and moved afterwards, which is one jump across
        // the screen in front of the user — and the only thing it bought was
        // not having to move a window that opens where it can be seen anyway.
        .center()
        .visible(visible)
        // No frame: minimise, maximise and close are drawn into the page by
        // `controls`, which carries its own colours and so needs nothing out
        // here to repaint it when dsh changes theme.
        .decorations(false)
        // Still set, even without a frame to paint: it is what the webview
        // resolves `prefers-color-scheme` against, so the loading page opens in
        // the theme dsh is about to show.
        .theme(preference.window())
        // Tauri replaces WebView2's drag-drop handler by default and routes the
        // events to its own `DragDropEvent` — which has the side effect of
        // swallowing the page's HTML5 drag events (the `dragstart`/`dragover`/
        // `drop` a `draggable` row fires) before they reach the DOM. dsh's
        // plugin cards sort by dragging, so the desktop shell would lose that
        // while it still works in a plain browser. This app has no use for
        // OS-level file drops — nothing listens for them — so turn the
        // interception off and let the HTML5 events through.
        .disable_drag_drop_handler()
        .initialization_script(theme::script(preference))
        .initialization_script(controls::script())
        // Turns the page's own `Notification` calls into real ones.
        .initialization_script(notify::script())
        // Raises one of those calls itself when a turn ends, so a finished run
        // is announced without a plugin having to be installed for it. After
        // `notify`, because it calls the shim that one installs.
        .initialization_script(turn::script())
        // And one when dsh stops to ask the user something, which the turn
        // watcher cannot see: the run has not ended, it is blocked. Same
        // shim, so both land in the same suppression check.
        .initialization_script(waiting::script())
        // The plugin panel, drawn over whatever page is showing when it is
        // asked for — dsh's included, which is the point of it being here.
        .initialization_script(panel::script())
        // The runtime chooser, shown on the loading page when a launch finds no
        // dsh; see `setup`.
        .initialization_script(setup::script())
        // The app's own dialogs, in place of the window manager's; see `dialog`.
        .initialization_script(dialog::script())
        // Which language the pages pick their own strings out of; see `i18n`.
        .initialization_script(format!(
            "window.__DSH_LANG__ = {:?};",
            crate::i18n::tag()
        ))
        // The build's own version, for the line under the wordmark on the
        // loading page. Read from the crate at compile time, which is the same
        // number `sync-version` keeps `tauri.conf.json` and `package.json` on.
        .initialization_script(format!(
            "window.__DSH_VERSION__ = {:?};",
            env!("CARGO_PKG_VERSION")
        ))
        .on_page_load(move |webview, payload| {
            // The first page this window ever loads is the bundled loading page,
            // and this is the one place its address is stated by something that
            // knows it. See [`Home`].
            let mut first = home.write().unwrap();
            if first.is_none() {
                *first = Some(payload.url().clone());
            }
            drop(first);

            if payload.event() == PageLoadEvent::Finished {
                splash.flush(&webview);
                // The chrome was just drawn by a fresh document that has no way
                // of knowing whether the window is maximised, the login item is
                // on, or notifications are — nothing resized to tell it, and
                // nothing was toggled. Every navigation lands here, so every
                // navigation gets all three.
                controls::sync(&webview);
                controls::sync_autostart(webview.app_handle());
                controls::sync_notify(webview.app_handle());
                // Last, and only where the page that just loaded is dsh
                // refusing to serve one: it replaces the page.
                auth.recover(&webview);
            }
        })
        .on_navigation(move |url| {
            // A window button, before anything treats it as somewhere to go.
            if let Some(action) = controls::action(url) {
                controls::perform(&opener, action);
                return false;
            }
            if is_ours(url, &origin) {
                return true;
            }
            // A link out of the app belongs in the user's browser, not in place
            // of the session they are working in.
            let _ = opener.opener().open_url(url.to_string(), None::<&str>);
            false
        })
        .on_new_window(move |url, _features| {
            if !is_ours(&url, &new_window_origin) {
                let _ = new_window_opener.opener().open_url(url.to_string(), None::<&str>);
            }
            NewWindowResponse::Deny
        })
        .build()?;

    window.on_window_event(move |event| match event {
        // Closing the window parks the app in the tray instead of tearing the
        // agent down mid-task. Quitting for real goes through the tray menu.
        tauri::WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            if let Some(window) = closer.get_webview_window("main") {
                let _ = window.hide();
            }
        }
        // The maximise button's glyph. A snap or a Win+Up never reaches the
        // page, so the page is told rather than left to work it out.
        tauri::WindowEvent::Resized(_) => {
            if let Some(window) = closer.get_webview_window("main") {
                controls::sync(&window);
            }
        }
        _ => {}
    });

    Ok(window)
}

/// The tray icon's id, so [`switch_language`] can find it again.
const TRAY: &str = "main";

/// The two items in it, in the language of the moment.
///
/// Built rather than kept, because a menu cannot be relabelled in place: the
/// items are handed to the OS when the tray is built, and the way to change
/// what they say is to hand it another menu. The ids are what
/// `on_menu_event` matches, and they are not translated.
fn tray_menu(app: &tauri::AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let show = MenuItem::with_id(
        app,
        "show",
        t!("显示窗口", "Show window"),
        true,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, "quit", t!("退出 dsh", "Quit dsh"), true, None::<&str>)?;

    Menu::with_items(app, &[&show, &quit])
}

/// Follow dsh into the language it has just been switched to.
///
/// Reached from the page, which is the only thing that sees the switch happen:
/// dsh writes it through to `<html lang>` without loading the document again,
/// and [`controls`] watches for that. Everything drawn from here on reads the
/// new language on its own — what needs saying out loud is the two menus that
/// were drawn before it.
fn switch_language(app: &tauri::AppHandle, tag: &str) {
    if !i18n::switch(tag) {
        return;
    }

    controls::relabel(app);

    let Some(tray) = app.tray_by_id(TRAY) else {
        return;
    };
    match tray_menu(app) {
        Ok(menu) => {
            let _ = tray.set_menu(Some(menu));
        }
        Err(error) => eprintln!("dsh-desktop: could not relabel the tray menu: {error}"),
    }
}

/// The tray icon: how the window comes back once it has been closed, and the
/// only way to actually quit while it is away.
///
/// Two items, and deliberately: everything else the app can be asked to do is in
/// the window's own menu (see [`controls`]), where it can be drawn to look like
/// the app rather than like a system context menu. What is left here is what is
/// only ever wanted when there is no window to look at.
fn build_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    let menu = tray_menu(app)?;

    let mut tray = TrayIconBuilder::with_id(TRAY)
        .tooltip("dsh desktop")
        .menu(&menu)
        // Left click reveals the window; the menu belongs on the right button.
        //
        // Windows and macOS only, and not by choice: `tray-icon` documents
        // `TrayIconEvent` as unsupported on Linux — the StatusNotifierItem the
        // AppIndicator backend registers has no click to deliver, so
        // `on_tray_icon_event` below is simply never called there. This is why
        // "Show window" is a menu item rather than a comment saying "just click
        // the icon": on Linux the menu is the only way back to a hidden window,
        // and the two platforms that do get the click get it as a shortcut.
        //
        // Nothing to fix here. If a later tray-icon gains Linux click events
        // this comment is what to delete.
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "show" => reveal(app),
            // Exits the run loop, which stops the dsh server on the way out.
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                reveal(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }

    tray.build(app)?;
    Ok(())
}

/// Add or remove the login item, and leave the checkmark showing what the
/// system actually ended up with rather than what was asked for.
fn toggle_autostart(app: &tauri::AppHandle) {
    let manager = app.autolaunch();
    let was = manager.is_enabled().unwrap_or(false);

    let changed = if was { manager.disable() } else { manager.enable() };
    if let Err(error) = changed {
        eprintln!("dsh-desktop: could not change the login item: {error}");
    }

    controls::sync_autostart(app);
}

/// Turn this app's notifications on or off, and repaint the checkmark.
///
/// Every notification, not only the finished-turn one: the gate is in
/// `notify::show`, which they all pass through. See [`settings`].
///
/// Unlike the login item there is nobody to ask afterwards what actually
/// happened — the answer is whatever was just written — so the checkmark is
/// pushed from the value [`settings`] returns rather than by reading the file
/// back. A write that failed leaves the setting on for this session, which the
/// checkmark then honestly shows.
fn toggle_notify_turns(app: &tauri::AppHandle) {
    settings::toggle_notifications(app);
    controls::sync_notify(app);
}

/// Quit, from the window's own menu rather than the tray.
///
/// On a thread, unlike the tray's: the click arrives inside the webview's
/// navigation callback, and the way out from here stops dsh and waits on its
/// process tree. That is not work to do while a webview is blocked waiting for
/// an answer about where it is allowed to navigate.
fn quit(app: &tauri::AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || app.exit(0));
}

/// Bring the window back to the front, whatever it was hidden behind.
pub(crate) fn reveal(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }

    // The app has been asked for, so a check held back for want of a window to
    // ask in now has one. Only ever a check the boot already finished with.
    if PENDING_CHECK.swap(false, Ordering::Relaxed) {
        check_for_updates(app);
    }
}

/// Hand the setup panel its payload, queueing through the splash the way a
/// dialog is — a chooser raised before the loading page has loaded is otherwise
/// lost. See `setup`.
pub(crate) fn deliver_setup(app: &tauri::AppHandle, payload: &str) {
    if let Some(session) = app.try_state::<Session>() {
        if let Some(window) = app.get_webview_window("main") {
            let quoted = serde_json::to_string(payload)
                .unwrap_or_else(|_| "\"\"".to_string());
            session
                .splash
                .send(&window, format!("window.__dshSetup({quoted})"));
        }
    }
}

/// A line of progress into the runtime panel itself.
///
/// The boot's chooser reports onto the loading page underneath it, which is the
/// only page of ours with a status line. The panel opened from the menu is drawn
/// over a running dsh, which has no such line and is not ours to write on — so
/// that one carries its own, and this is what feeds it. Without it an install
/// started from the menu is a frozen card for several minutes.
pub(crate) fn setup_status(app: &tauri::AppHandle, text: &str, percent: f64) {
    if let Some(session) = app.try_state::<Session>() {
        if let Some(window) = app.get_webview_window("main") {
            session
                .splash
                .call(&window, "__dshSetupStatus", &[text, &format!("{percent:.1}")]);
        }
    }
}

/// Open the runtime panel from the menu; see `setup::manage`.
///
/// Off the navigation callback that delivered the click, the way `open_plugins`
/// is: the loop this starts blocks for as long as the panel is up, and the
/// dialogs it raises block on answers the main thread has to deliver.
fn open_runtime(app: &tauri::AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || setup::manage(&app));
}

/// Take the setup panel down; see `setup`.
///
/// Queued through the splash like the delivery above, and for the same reason
/// rather than a different one: the two have to arrive in the order they were
/// made. A hide evaluated straight into the document while the show it undoes is
/// still sitting in the queue would be a panel that comes back up after it was
/// answered.
pub(crate) fn hide_setup(app: &tauri::AppHandle) {
    if let Some(session) = app.try_state::<Session>() {
        if let Some(window) = app.get_webview_window("main") {
            session.splash.send(
                &window,
                "window.__dshSetupHide && window.__dshSetupHide()".to_string(),
            );
        }
    }
}

/// Whether a navigation target is part of this app: the bundled loading page, or
/// the dsh server we started.
fn is_ours(url: &Url, origin: &Origin) -> bool {
    match url.scheme() {
        "http" | "https" => {}
        // tauri:// (the bundled page), about:, blob:, data: — never external.
        _ => return true,
    }

    if url.host_str() == Some("tauri.localhost") {
        return true;
    }

    origin
        .read()
        .unwrap()
        .as_deref()
        .is_some_and(|ours| url.origin().ascii_serialization() == ours)
}

/// Whether a boot or a dsh update owns the window and the server right now.
///
/// The two do the same things in the same order — settle which dsh runs, then
/// start it and hand the window over — and both take minutes. Running them at
/// once would be two npms writing one tree and two `dsh web` children racing for
/// the one slot in [`Session`], so the second one is turned away rather than
/// joining in.
static BUSY: AtomicBool = AtomicBool::new(false);

/// Clears [`BUSY`] however the thread holding it ends, including the early
/// returns for an app that is quitting.
struct Busy;

impl Drop for Busy {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::SeqCst);
    }
}

/// Settle which dsh this launch runs, start it, and hand the window over to it.
///
/// One background thread for the whole sequence, because the whole sequence is
/// blocking and ordered: the update check waits on npm, the question it can
/// raise waits on the user, the download that may follow runs for minutes, and
/// only once all of that is behind us is there a server to wait for. None of it
/// belongs on the main thread.
fn boot(app: tauri::AppHandle, window: WebviewWindow, session: Session) {
    // Nothing else can have it yet: this runs from `setup`, before the event
    // loop that would deliver a click on the tray's update item.
    BUSY.store(true, Ordering::SeqCst);

    std::thread::spawn(move || {
        let _busy = Busy;
        // The update check is skipped entirely on a login-item launch, which is
        // sitting in the tray with nobody looking at it: a modal asking about a
        // 185 MB download, from an app the user never opened, belongs to no
        // window on screen. The next launch someone actually asks for does the
        // check.
        //
        // The chooser is not skippable the same way. A login-item launch with
        // nothing runnable cannot sit in the tray — there is nothing to run
        // behind the icon — so the window comes up and the question is asked.
        // The visible-launch path reaches the same place through `gate`, which
        // runs the update check first and hands off to `setup::present` when
        // there is no dsh, or one behind a Node too old to run it; the autostart
        // path asks `dsh::needs_setup` the same question and goes straight
        // there.
        //
        // False means the app is quitting and took the install running under
        // this call down with it. Starting a server now would be starting one
        // for a process that is already on its way out.
        let report = reporter(&session.splash, &window);
        if window_is_visible(&app) {
            if !dsh::gate(&app, &report) {
                return;
            }
        } else if dsh::needs_setup(&app) && !setup::present(&app, &report) {
            return;
        }

        // Once, on the launch that first has a dsh to add plugins to — and once
        // more for an install that predates the panel existing. It is shown
        // before the server starts rather than after, because installing a
        // plugin means stopping the server again, and the user has just watched
        // it start.
        //
        // Marked as shown before it is shown: a panel that crashes the launch it
        // appears on should not appear on the next one too. What happens next is
        // the panel's — see [`leave_plugins`].
        if window_is_visible(&app) && !plugins::guided(&app) {
            plugins::mark_guided(&app);
            show_plugins(&app, &session, true);
            return;
        }

        start_serving(&app, &window, &session);
        check_for_updates(&app);
    });
}

/// Update dsh because the user asked for it, with the server down for the
/// duration: npm is about to replace the tree `dsh web` is running out of, and a
/// half-swapped one underneath a live server is worse than a wait.
///
/// One thread for the whole sequence, for the reasons [`boot`] runs on one — the
/// check waits on npm, the question waits on the user, and the install runs for
/// minutes — and the window goes back to the loading page for it, because that
/// is the only page of ours with anywhere to put the progress.
fn update_dsh(app: &tauri::AppHandle) {
    // Held from before the first dialog: two of these would ask twice and then
    // stop, update and restart the server twice over each other.
    if BUSY.swap(true, Ordering::SeqCst) {
        dsh::note(
            app,
            t!("请稍等", "One moment"),
            t!(
                "dsh 正在启动或更新中，等它忙完再试。",
                "dsh is starting or updating; try again once it has finished."
            ),
        );
        return;
    }

    let session = app.state::<Session>().inner().clone();
    let app = app.clone();

    std::thread::spawn(move || {
        let _busy = Busy;

        // Before the first thing that reaches the network. Until the answer is
        // in there is nothing to show but the page the user was already on, and
        // fifteen seconds of that is a menu item that did nothing.
        let saying = |text: &str| controls::busy(&app, text);
        let Some((prefix, installed)) = dsh::requested(&app, &saying) else {
            return;
        };
        let Some(window) = app.get_webview_window("main") else {
            return;
        };

        stop_server(&session);
        // Back to queueing until the loading page below has loaded; the reports
        // that follow would otherwise be evaluated into the outgoing document.
        session.splash.rearm();

        let handle = app.clone();
        let back = window.clone();
        // Recorded by the first page load, which is long over: the click that got
        // here came from a menu drawn by the page that replaced it.
        let home = session.home.read().unwrap().clone();
        let _ = app.run_on_main_thread(move || {
            // The window may well be hidden in the tray, which is no place for
            // an update the user is waiting on.
            reveal(&handle);
            match home {
                Some(home) => {
                    if let Err(error) = back.navigate(home) {
                        eprintln!("dsh-desktop: could not return to the loading page: {error}");
                    }
                }
                None => eprintln!(
                    "dsh-desktop: the loading page's address is not known yet; \
                     the update has nowhere to report progress"
                ),
            }
        });

        // False means the app is quitting and took npm down with it.
        let report = reporter(&session.splash, &window);
        if !dsh::update(&app, &prefix, &installed, &report) {
            return;
        }

        start_serving(&app, &window, &session);
    });
}

/// Start `dsh web` and hand the window over to it. Blocks until it is serving or
/// has given up, reporting either onto the loading page.
fn start_serving(app: &tauri::AppHandle, window: &WebviewWindow, session: &Session) {
    session.splash.status(window, t!("正在启动 dsh…", "Starting dsh…"));
    session.splash.progress(window, -1.0);

    match server::start(app, None) {
        Ok((child, events)) => {
            *session.server.lock().unwrap() = Some(child);
            if serve(window, &session.origin, &session.splash, &session.auth, &events) {
                watch(window, session, events);
            }
        }
        // Most often this is a machine where fetching dsh failed: neither the
        // installer nor the boot above carries one, they download it, and an app
        // that got this far without one has nothing to run. By now both have
        // tried, so what is left to suggest is the network and doing it by hand.
        Err(error) => session.splash.fail(
            window,
            t!("启动 dsh 失败", "Could not start dsh"),
            &t!(
                "无法执行 dsh：{}\n\n\
                 dsh 没有安装成功，通常是网络或代理的问题。\
                 换一个网络或代理后重启应用，它会再试一次。\n\n\
                 也可以自己在终端里执行 `npm install -g @deepseek-ai/dsh` 安装，\
                 或用 DSH_BIN 环境变量指向 dsh 可执行文件的完整路径。",
                "dsh could not be executed: {}\n\n\
                 It did not install, which is usually the network or a proxy. \
                 Restart the app on a different connection and it will try again.\n\n\
                 You can also install it yourself with \
                 `npm install -g @deepseek-ai/dsh`, or point the DSH_BIN \
                 environment variable at the dsh executable.",
                error
            ),
        ),
    }
}

/// What [`dsh::gate`] and [`dsh::update`] write their progress through.
fn reporter<'a>(splash: &'a Splash, window: &'a WebviewWindow) -> impl Fn(&str, f64) + 'a {
    move |text: &str, percent: f64| {
        if !text.is_empty() {
            splash.status(window, text);
        }
        splash.progress(window, percent);
    }
}

/// Take the running server down on purpose, and say so.
///
/// The saying is [`Session::epoch`]: the watcher started for this server is
/// about to see it exit, and this is what tells it the exit was asked for. Moved
/// before the child is killed, so there is no window in which the watcher could
/// read the old number.
fn stop_server(session: &Session) {
    session.epoch.fetch_add(1, Ordering::SeqCst);

    if let Some(mut running) = session.server.lock().unwrap().take() {
        running.stop();
    }
    // The dsh page went with it, so nothing may be treated as ours until a new
    // server says otherwise.
    *session.origin.write().unwrap() = None;
}

/// Wait for the running server to exit, start it again where that is worth
/// doing, and put the window somewhere the user can see it when it is not.
///
/// Without this the failure is silent in the worst way: `dsh web` dies, the
/// window goes on showing the page it served, and every click on it fails in
/// whatever way that page fails when its backend is gone. The process exiting is
/// the signal — not a port probe on a timer, which is a second thing that can be
/// wrong about a question the pipe already answers exactly.
///
/// What it does about it is [`resume`], which in the ordinary case the user
/// never sees: dsh comes back on the port it was on and the page reconnects
/// itself. Only a dsh that will not come back, or one that comes back and dies
/// again [`RESTARTS`] times over, reaches [`give_up`] and the loading page.
fn watch(window: &WebviewWindow, session: &Session, events: Receiver<server::Event>) {
    let epoch = session.epoch.load(Ordering::SeqCst);
    let session = session.clone();
    let window = window.clone();

    std::thread::spawn(move || {
        let mut events = events;
        // Quick deaths in a row. Reset rather than incremented by one that took
        // its time; see [`RESTARTS`].
        let mut flaps = 0usize;

        let last = loop {
            let started = Instant::now();

            // Every other event is behind us — this loop is entered after a
            // `Ready`, from `serve` the first time and from `resume` after that.
            let Some(output) = events.iter().find_map(|event| match event {
                server::Event::Exited(output) => Some(output),
                _ => None,
            }) else {
                // The channel closed without an exit, which is the app shutting
                // down. Nothing to report and nowhere left to report it.
                return;
            };

            if session.epoch.load(Ordering::SeqCst) != epoch {
                // We stopped it: an update or a plugin install, either of which is
                // already showing the user what it is doing.
                return;
            }

            // The parent is gone but the slot still holds it, and on Unix an
            // unreaped child is a zombie until something waits on it. `stop` waits,
            // and takes down any of the tree that outlived its parent while it is
            // there.
            if let Some(mut dead) = session.server.lock().unwrap().take() {
                dead.stop();
            }

            flaps = if started.elapsed() < STEADY { flaps + 1 } else { 1 };
            if flaps > RESTARTS {
                break output;
            }

            // Nothing else may be starting a dsh while this starts one. A flag
            // already held is a boot, an update, a plugin install or a restart,
            // and every one of those starts dsh itself when it is finished — so
            // this steps aside, and silently, because whatever holds the flag is
            // already saying on screen what it is doing.
            //
            // The block is the whole of the borrow: the wait above must not hold
            // the flag, or a user who wanted to update dsh would be told to wait
            // for a server that is running perfectly well.
            let resumed = {
                if BUSY.swap(true, Ordering::SeqCst) {
                    return;
                }
                let _busy = Busy;
                resume(&window, &session)
            };

            match resumed {
                Ok(next) => events = next,
                Err(failed) => break failed,
            }
        };

        give_up(&window, &session, &last);
    });
}

/// Start `dsh web` again after it exited on its own, without taking the window
/// off the page dsh was serving.
///
/// The port it was on is asked for again, and that is the whole point of this
/// function. Since 0.1.2 dsh authenticates a browser with a cookie bound to the
/// authority it was minted for, signed with a secret that lives in dsh's
/// credential store rather than in the process — so a server that comes back on
/// the same port is one the loaded page is *still authenticated against*, and
/// dsh's own client reconnects to it unprompted, backing off from half a second
/// to ten and never giving up. Nothing here navigates, so nothing is lost: not
/// the draft in the composer, not the scroll position, not the session being
/// read. What the user sees is dsh's own connection indicator go and come back.
///
/// Which is also why the reporting goes through [`crate::controls::busy`] rather
/// than the splash: the page underneath is dsh's, and it is staying.
///
/// `Err` carries the output of the attempt that failed.
fn resume(window: &WebviewWindow, session: &Session) -> Result<Receiver<server::Event>, String> {
    let app = window.app_handle();
    controls::busy(
        app,
        t!("dsh 已断开，正在重新启动…", "dsh disconnected; restarting it…"),
    );

    let was = served_port(&session.origin);
    let mut outcome = attempt(window, session, was);

    // A port is a request, not a reservation, and dsh was not holding this one
    // for the moment it took to notice. Losing it costs the page its own
    // reconnect — the cookie is bound to the authority — so the second try takes
    // whatever it is given and navigates, which is a reload rather than a
    // failure.
    if outcome.is_err() && was.is_some() {
        outcome = attempt(window, session, None);
    }

    // Down either way: dsh draws its own connection status now, so a line of
    // ours saying the same thing over the top of it is one too many — and on the
    // failing path `give_up` is about to put the whole loading page up.
    controls::busy(app, "");
    outcome
}

/// One start, waited out. The child goes into the session's slot before the wait
/// rather than after it, so that a quit landing midway takes it down with
/// everything else instead of leaving a `dsh web` with no owner.
fn attempt(
    window: &WebviewWindow,
    session: &Session,
    port: Option<u16>,
) -> Result<Receiver<server::Event>, String> {
    let app = window.app_handle();

    let (child, events) = server::start(app, port).map_err(|error| error.to_string())?;
    *session.server.lock().unwrap() = Some(child);

    match events.recv_timeout(RESUME_TIMEOUT) {
        Ok(server::Event::Ready(url)) => {
            let Ok(url) = Url::parse(&url) else {
                return stillborn(
                    session,
                    t!(
                        "无法解析 dsh 输出的地址：{}",
                        "dsh printed an address that cannot be parsed: {}",
                        url
                    ),
                );
            };

            let origin = url.origin().ascii_serialization();
            let same = session.origin.read().unwrap().as_deref() == Some(origin.as_str());
            *session.origin.write().unwrap() = Some(origin);

            // Only when the port moved. On the same one the page is already
            // pointed at a server that is back, and reloading it would throw
            // away the very thing staying put is for.
            if !same {
                session.auth.arm(&url);

                let window = window.clone();
                let _ = app.run_on_main_thread(move || {
                    if let Err(error) = window.navigate(url) {
                        eprintln!("dsh-desktop: could not follow dsh to its new port: {error}");
                    }
                });
            }
            Ok(events)
        }
        // `Failed` is the port already taken, and every other way a start dies
        // before it serves. `Exited` cannot come first — the pump only sends one
        // once a URL has gone past — but it would be the same news.
        Ok(server::Event::Failed(output) | server::Event::Exited(output)) => {
            stillborn(session, output)
        }
        Err(_) => stillborn(
            session,
            t!(
                "dsh 启动后一直没有开始服务。",
                "dsh started but never began serving."
            )
            .to_string(),
        ),
    }
}

/// Take back a child that was started and never served, and answer with why.
fn stillborn(session: &Session, why: String) -> Result<Receiver<server::Event>, String> {
    if let Some(mut dead) = session.server.lock().unwrap().take() {
        dead.stop();
    }
    Err(why)
}

/// The port the running server bound, read back out of the origin [`serve`]
/// recorded. Kept nowhere else on purpose: a second copy of the same fact is a
/// second thing that can be out of date.
fn served_port(origin: &Origin) -> Option<u16> {
    let origin = origin.read().unwrap().clone()?;
    Url::parse(&origin).ok()?.port()
}

/// Put the window back on the loading page with the failure on it, once starting
/// dsh again has stopped being worth trying.
fn give_up(window: &WebviewWindow, session: &Session, output: &str) {
    *session.origin.write().unwrap() = None;
    session.splash.rearm();

    let handle = window.app_handle().clone();
    let target = handle.clone();
    let back = window.clone();
    let home = session.home.read().unwrap().clone();
    let _ = handle.run_on_main_thread(move || {
        reveal(&target);
        if let Some(home) = home {
            if let Err(error) = back.navigate(home) {
                eprintln!("dsh-desktop: could not return to the loading page: {error}");
            }
        }
    });

    // Queued by `rearm` until the loading page above has loaded.
    session.splash.fail_retry(
        window,
        t!("dsh 已退出", "dsh exited"),
        &if output.is_empty() {
            t!(
                "dsh 意外退出了，重新启动也没能让它回来，且没有留下任何输出。",
                "dsh exited unexpectedly and did not come back when it was restarted, \
                 without printing anything."
            )
            .to_string()
        } else {
            t!(
                "dsh 意外退出了，重新启动也没能让它回来。它最后的输出：\n\n{}",
                "dsh exited unexpectedly and did not come back when it was restarted. \
                 Its last output:\n\n{}",
                output
            )
        },
    );
}

/// Start `dsh web` again. Two callers reach this: the menu's "Restart dsh"
/// item, where the server is still up and serving the page the user is looking
/// at; and the loading page's retry button, where it has already exited.
///
/// The second is why this used to be a bare `start_serving`. The first needs
/// more: the running server has to come down before a fresh one can take its
/// place, and the page it was serving has to go back to the loading page — the
/// only one of ours with a status line a start reports onto. Both are harmless
/// from the retry button: `stop_server` on nothing is a no-op, and the loading
/// page is already where it is.
fn restart_dsh(app: &tauri::AppHandle) {
    if BUSY.swap(true, Ordering::SeqCst) {
        dsh::note(
            app,
            t!("请稍等", "One moment"),
            t!(
                "dsh 正在启动或更新中，等它忙完再试。",
                "dsh is starting or updating; try again once it has finished."
            ),
        );
        return;
    }

    let session = app.state::<Session>().inner().clone();
    let app = app.clone();

    std::thread::spawn(move || {
        let _busy = Busy;
        let Some(window) = app.get_webview_window("main") else {
            return;
        };

        // Down first: a server still serving has to stop before a fresh one
        // starts, and from the retry button there is nothing to stop.
        stop_server(&session);

        // The dsh page died with the server above; only the loading page can
        // show a start's progress, so back there the way an update goes back.
        // Skipped from the retry button, which is already on it.
        let home = session.home.read().unwrap().clone();
        let arrived = home
            .as_ref()
            .is_some_and(|home| window.url().is_ok_and(|showing| &showing == home));
        if !arrived {
            session.splash.rearm();

            let back = window.clone();
            let target = home.clone();
            let _ = app.run_on_main_thread(move || {
                if let Some(home) = target {
                    if let Err(error) = back.navigate(home) {
                        eprintln!("dsh-desktop: could not return to the loading page: {error}");
                    }
                }
            });
        }

        start_serving(&app, &window, &session);
    });
}

/// Open the plugin panel, from the menu.
fn open_plugins(app: &tauri::AppHandle) {
    let session = app.state::<Session>().inner().clone();
    let app = app.clone();

    // Off the navigation callback that delivered the click: this navigates the
    // window, and the webview is currently blocked waiting for an answer about
    // where it is allowed to go.
    std::thread::spawn(move || show_plugins(&app, &session, false));
}

/// Put the panel up over whatever the window is showing.
///
/// It is drawn into that page rather than being a page — or a window — of its
/// own; `panel` says why, and the short of it is that looking at a list should
/// not cost the harness underneath a reload. dsh keeps running behind it.
/// Nothing is installed until the user asks, and only then does the server have
/// to come down.
///
/// `first` is the one-time guide on a first launch rather than the menu. The
/// panel is the same either way; what differs is the way out of it — the guide
/// is a step to skip, and the menu is somewhere to come back from.
fn show_plugins(app: &tauri::AppHandle, session: &Session, first: bool) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };

    session.splash.plugins(&window, &plugins::listing(app), first);
}

/// Install what the panel asked for, with `dsh web` down for the duration.
///
/// It has to come down: pnpm is about to rewrite the profile directory the
/// running server read its plugins out of. It also has to come back up
/// afterwards, which is what leaving the panel does — the new plugins are only
/// in the window once dsh has been started again to load them.
fn install_plugins(app: &tauri::AppHandle, ids: Vec<String>, spec: Option<String>) {
    if BUSY.swap(true, Ordering::SeqCst) {
        dsh::note(
            app,
            t!("请稍等", "One moment"),
            t!(
                "dsh 正在启动或更新中，等它忙完再装插件。",
                "dsh is starting or updating; wait for that to finish before installing plugins."
            ),
        );
        return;
    }

    let session = app.state::<Session>().inner().clone();
    let app = app.clone();

    std::thread::spawn(move || {
        let _busy = Busy;
        let Some(window) = app.get_webview_window("main") else {
            return;
        };

        stop_server(&session);

        let log = |line: &str| session.splash.plugin_log(&window, line);
        match plugins::install(&app, &ids, spec.as_deref(), &log) {
            Ok(()) => {
                session.splash.plugin_lists(&window, &plugins::listing(&app));
                session.splash.plugin_done(
                    &window,
                    true,
                    t!(
                        "装好了。回到 dsh 时会重新启动它，插件在那之后生效。",
                        "Done. dsh restarts on the way back, and the plugins take effect then."
                    ),
                );
            }
            Err(error) => session.splash.plugin_done(&window, false, &error),
        }
    });
}

/// Take the ticked plugins out again, with `dsh web` down for the duration and
/// for the same reason the install takes it down: pnpm is about to rewrite the
/// directory the running server read them out of.
fn remove_plugins(app: &tauri::AppHandle, names: Vec<String>) {
    if BUSY.swap(true, Ordering::SeqCst) {
        dsh::note(
            app,
            t!("请稍等", "One moment"),
            t!(
                "dsh 正在启动或更新中，等它忙完再动插件。",
                "dsh is starting or updating; wait for that to finish before changing plugins."
            ),
        );
        return;
    }

    let session = app.state::<Session>().inner().clone();
    let app = app.clone();

    std::thread::spawn(move || {
        let _busy = Busy;
        let Some(window) = app.get_webview_window("main") else {
            return;
        };

        stop_server(&session);

        let log = |line: &str| session.splash.plugin_log(&window, line);
        match plugins::remove(&app, &names, &log) {
            Ok(()) => {
                session.splash.plugin_lists(&window, &plugins::listing(&app));
                session.splash.plugin_done(
                    &window,
                    true,
                    t!(
                        "卸载完成。回到 dsh 时会重新启动它。",
                        "Removed. dsh restarts on the way back."
                    ),
                );
            }
            Err(error) => session.splash.plugin_done(&window, false, &error),
        }
    });
}

/// Leave the panel: back to dsh, starting it if it is not running.
///
/// Both cases happen. The panel opened from the menu left the server up, and
/// the page it was drawn over is still underneath it — taking it away is the
/// whole of going back. The panel that opened on a first launch, or that has
/// just installed something, has no server to go back to, and that is the one
/// case that costs a page load.
fn leave_plugins(app: &tauri::AppHandle) {
    let session = app.state::<Session>().inner().clone();
    let app = app.clone();

    std::thread::spawn(move || {
        let Some(window) = app.get_webview_window("main") else {
            return;
        };

        // Down first, and without asking anyone. Taking [`BUSY`] before this
        // meant a panel opened while the boot was still running could not be
        // closed at all: the boot holds the flag from its first line until dsh
        // is serving, so the click raised a "one moment" note instead — and that
        // note is a dialog, drawn *under* this panel, so what the user saw was a
        // button that did nothing. Closing the panel conflicts with nothing; it
        // is only the dsh underneath that one thread at a time may drive.
        session.splash.plugin_hide(&window);

        // Which leaves the question this was really guarding: is there a dsh to
        // bring back, and is it ours to bring back? A flag already held is the
        // boot or an update, and both start dsh themselves when they are done.
        if BUSY.swap(true, Ordering::SeqCst) {
            return;
        }
        let _busy = Busy;

        let serving = session
            .origin
            .read()
            .unwrap()
            .clone()
            .filter(|_| session.server.lock().unwrap().is_some());

        if serving.is_some() {
            return;
        }

        // No server: the window is either on the loading page it started on, or
        // on the dead page of the dsh an install just stopped. The second has no
        // status line for a start to be reported on, so it goes back to ours.
        let home = session.home.read().unwrap().clone();
        let arrived = home
            .as_ref()
            .is_some_and(|home| window.url().is_ok_and(|showing| &showing == home));

        if !arrived {
            session.splash.rearm();

            let back = window.clone();
            let target = home.clone();
            let _ = app.run_on_main_thread(move || {
                if let Some(home) = target {
                    if let Err(error) = back.navigate(home) {
                        eprintln!("dsh-desktop: could not return to the loading page: {error}");
                    }
                }
            });
        }

        start_serving(&app, &window, &session);
        check_for_updates(&app);
    });
}

/// A check the boot finished with while the window was still hidden in the
/// tray. Picked up by [`reveal`].
static PENDING_CHECK: AtomicBool = AtomicBool::new(false);

/// Look for a newer app, at most once per run. dsh is not checked here — that
/// happens in [`boot`], before there is a dsh running to interrupt.
///
/// Held back until the boot has settled — or, when it never does, until the
/// wait has gone on long enough to call slow. The check reaches the network,
/// and on a first launch — where the whole of dsh is being read off disk for
/// the first time — that is contention for the one thing the user is actually
/// waiting on.
fn check_for_updates(app: &tauri::AppHandle) {
    // The check can end in a dialog, and a login-item launch is sitting in the
    // tray: a modal over whatever the user is doing, from an app they never
    // opened, belongs to no window on screen. It waits until one is asked for.
    if !window_is_visible(app) {
        PENDING_CHECK.store(true, Ordering::Relaxed);
        return;
    }

    static ONCE: Once = Once::new();

    ONCE.call_once(|| {
        // A dev build's version never matches a release, so it would prompt on
        // every run.
        #[cfg(not(debug_assertions))]
        update::check_quietly(app);
    });
}

/// Whether there is a window on screen to hang a dialog off. A window that
/// cannot be asked is treated as visible: the checks are the point, and the
/// only launch that starts hidden is the one that passes [`AUTOSTART_FLAG`].
fn window_is_visible(app: &tauri::AppHandle) -> bool {
    app.get_webview_window("main")
        .and_then(|window| window.is_visible().ok())
        .unwrap_or(true)
}

/// Block until the server is serving or has given up, reporting either into the
/// loading page. `true` once the window has been handed over to it, which is
/// also when there is something left to watch; see [`watch`].
fn serve(
    window: &WebviewWindow,
    origin: &Origin,
    splash: &Splash,
    auth: &auth::Retry,
    events: &Receiver<server::Event>,
) -> bool {
    loop {
        match events.recv_timeout(SLOW_BOOT) {
            Ok(server::Event::Ready(url)) => {
                let Ok(url) = Url::parse(&url) else {
                    splash.fail(
                        window,
                        t!("启动 dsh 失败", "Could not start dsh"),
                        &t!(
                            "无法解析 dsh 输出的地址：{}",
                            "dsh printed an address that cannot be parsed: {}",
                            url
                        ),
                    );
                    return false;
                };

                *origin.write().unwrap() = Some(url.origin().ascii_serialization());
                splash.status(window, t!("正在打开界面…", "Opening the interface…"));

                auth.arm(&url);

                let handle = window.app_handle().clone();
                let window = window.clone();
                let splash = splash.clone();
                let _ = handle.run_on_main_thread(move || {
                    if let Err(error) = window.navigate(url) {
                        splash.fail(
                            &window,
                            t!("打开界面失败", "Could not open the interface"),
                            &error.to_string(),
                        );
                    }
                });
                return true;
            }
            Ok(server::Event::Failed(output)) => {
                splash.fail(
                    window,
                    t!("dsh 已退出", "dsh exited"),
                    &if output.is_empty() {
                        t!(
                            "dsh 在开始服务前就退出了，且没有任何输出。",
                            "dsh exited before it began serving, without printing anything."
                        )
                        .to_string()
                    } else {
                        t!(
                            "dsh 在开始服务前就退出了。它的输出：\n\n{}",
                            "dsh exited before it began serving. Its output:\n\n{}",
                            output
                        )
                    },
                );
                return false;
            }
            // Only the deliberate stops reach here after a `Ready`, and those
            // are the watcher's business rather than this one's; see [`watch`].
            Ok(server::Event::Exited(_)) => return false,
            Err(RecvTimeoutError::Timeout) => {
                splash.status(
                    window,
                    t!("dsh 启动较慢，仍在等待…", "dsh is slow to start; still waiting…"),
                );
                // A dsh that neither serves nor exits would otherwise keep the
                // checks out of reach for as long as it hangs — and an update
                // is one of the things that fixes that. They run once, so the
                // timeouts after this one cost nothing.
                check_for_updates(window.app_handle());
            }
            Err(RecvTimeoutError::Disconnected) => return false,
        }
    }
}

/// The loading page's two hooks (see `dist/index.html`). The server can fail
/// before the page has finished loading, and a call evaluated into an empty
/// document is simply lost — so calls made that early wait for the load.
#[derive(Clone, Default)]
struct Splash {
    state: Arc<Mutex<SplashState>>,
}

#[derive(Default)]
struct SplashState {
    loaded: bool,
    pending: Vec<String>,
}

impl Splash {
    /// Update the status line.
    fn status(&self, window: &WebviewWindow, text: &str) {
        self.call(window, "dshStatus", &[text]);
    }

    /// Move the download bar. A negative percentage puts it away.
    fn progress(&self, window: &WebviewWindow, percent: f64) {
        self.call(window, "dshProgress", &[&format!("{percent:.1}")]);
    }

    /// Show the plugin panel, with the presets and what is already installed.
    /// The first argument is the JSON [`plugins::listing`] built; the second
    /// tells it whether this is the first-launch guide or a visit from the
    /// menu, which is the difference between skipping it and leaving it.
    fn plugins(&self, window: &WebviewWindow, listing: &str, first: bool) {
        self.call(
            window,
            "__dshPlugins",
            &[listing, if first { "first" } else { "" }],
        );
    }

    /// Redraw the two lists — what can go in, and what is in — leaving the log
    /// and the message above them where they are. An install or a removal makes
    /// both lists wrong the moment it succeeds.
    fn plugin_lists(&self, window: &WebviewWindow, listing: &str) {
        self.call(window, "__dshPluginLists", &[listing]);
    }

    /// Take it away again. What it was drawn over was never navigated away
    /// from, so this is the whole of putting the user back where they were.
    fn plugin_hide(&self, window: &WebviewWindow) {
        self.call(window, "__dshPluginHide", &[]);
    }

    /// One line of an install's output, verbatim. There is a lot of it — this is
    /// pnpm's own log — and all of it goes on screen: when this fails, what it
    /// printed is the whole of what the user has to go on.
    fn plugin_log(&self, window: &WebviewWindow, line: &str) {
        self.call(window, "__dshPluginLog", &[line]);
    }

    /// How the install ended, and what to say about it.
    fn plugin_done(&self, window: &WebviewWindow, ok: bool, text: &str) {
        self.call(
            window,
            "__dshPluginDone",
            &[if ok { "ok" } else { "failed" }, text],
        );
    }

    /// Replace the spinner with an error the user can read and copy.
    fn fail(&self, window: &WebviewWindow, title: &str, detail: &str) {
        self.failed(window, title, detail, false);
    }

    /// The same, for the one failure the user can do something about from here:
    /// a dsh that was serving and stopped. The page draws a button that starts
    /// it again.
    fn fail_retry(&self, window: &WebviewWindow, title: &str, detail: &str) {
        self.failed(window, title, detail, true);
    }

    fn failed(&self, window: &WebviewWindow, title: &str, detail: &str, retry: bool) {
        eprintln!("dsh-desktop: {title}: {detail}");
        self.call(
            window,
            "dshError",
            &[title, detail, if retry { "retry" } else { "" }],
        );

        // A login-item launch leaves the window hidden in the tray, where an
        // error report is written to a page the user has no reason to open. The
        // boot is over either way, so whatever lands here asks for the window.
        let handle = window.app_handle().clone();
        let target = handle.clone();
        let _ = handle.run_on_main_thread(move || reveal(&target));
    }

    /// Back to queueing, for a window on its way to a fresh loading page: a call
    /// evaluated into the document being navigated away from is lost the same way
    /// one made before the first load is, and the next load flushes both.
    fn rearm(&self) {
        self.state.lock().unwrap().loaded = false;
    }

    /// Run the calls that were made before the page could receive them.
    fn flush(&self, window: &WebviewWindow) {
        let mut state = self.state.lock().unwrap();
        state.loaded = true;
        for js in state.pending.drain(..) {
            let _ = window.eval(&js);
        }
    }

    fn call(&self, window: &WebviewWindow, function: &str, args: &[&str]) {
        let args: Vec<String> = args
            .iter()
            .map(|arg| serde_json::to_string(arg).expect("a string is always serializable"))
            .collect();
        self.send(
            window,
            format!(
                "window.{function} && window.{function}({})",
                args.join(", ")
            ),
        );
    }

    /// Evaluate a whole script, or hold it until a document can receive it.
    ///
    /// The queueing half of [`Self::call`], reachable on its own for
    /// [`crate::dialog`]: that module builds its own call rather than a
    /// `window.fn(args)` — one JSON payload, and deliberately unguarded — but it
    /// needs exactly this. A dialog evaluated into a document on its way out is
    /// a dialog nobody sees, and for `dialog::confirm` that is a worker thread
    /// waiting on a click that cannot arrive: the boot asks its question before
    /// the first `PageLoadEvent::Finished` has landed.
    fn send(&self, window: &WebviewWindow, js: String) {
        let mut state = self.state.lock().unwrap();
        if state.loaded {
            let _ = window.eval(&js);
        } else {
            state.pending.push(js);
        }
    }
}
