// The release build is a GUI app: no console window behind it.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// First, and out of alphabetical order: it defines `t!`, and a macro is only
// in scope for the modules declared after it.
#[macro_use]
mod i18n;

mod auth;
mod controls;
mod cookies;
mod dialog;
mod dsh;
mod memory;
mod notify;
mod panel;
mod plugins;
mod remote;
mod server;
mod settings;
mod setup;
mod signal;
mod theme;
mod toast;
mod update;

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
            // Nothing is bound and no port is open for a machine that has never
            // paired a phone; this is the state the button on the titlebar
            // reaches. One that has goes straight back up — the pairing outlives
            // the process now, and a pairing with no socket under it is a phone
            // looking at a refused connection. See [`remote::resume`].
            app.manage(remote::Remote::new(app.handle().clone()));
            remote::resume(app.handle());

            // Once a launch, and only for someone who has the patch switched
            // on, so that a fix for a newer dsh reaches them without waiting
            // for a release of this app. Quiet: they did not ask for it, and
            // the stylesheet already on disk is what the phone uses meanwhile.
            // See [`remote::refresh_patch`].
            remote::refresh_patch(app.handle(), false);

            build_tray(app.handle())?;

            boot(app.handle().clone(), window, session);

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build the dsh desktop app");

    app.run(move |handle, event| match event {
        // Asked to quit while this machine is answering the internet. The
        // tunnel would come down here anyway — that is what `Exit` below does —
        // but coming down silently is what makes it the same experience as
        // having remembered to switch it off, which is how a user learns that
        // leaving it on costs nothing. See `remote::confirm_exit`.
        tauri::RunEvent::ExitRequested { api, .. } => {
            if remote::confirm_exit(handle) {
                api.prevent_exit();
            }
        }
        tauri::RunEvent::Exit => {
            dsh::stop();
            plugins::stop();
            // Before the server, because what this takes down is a socket bound
            // to every interface on the machine — and, on a public channel, a
            // `cloudflared` holding a connection to Cloudflare's edge. See
            // `remote::shutdown`.
            remote::shutdown(handle);
            if let Some(child) = server.lock().unwrap().as_mut() {
                child.stop();
            }
        }
        _ => {}
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
        // Never at build time, even when this launch wants a window. WebView2
        // takes its default background from the options the controller is
        // created with, and Tauri only reaches that through the builder — which
        // cannot be told the answer for `system`, since resolving that needs a
        // window to ask. So the window is built hidden, painted below once it
        // exists and can be asked, and shown after. Built visible, it opened on
        // the controller's own white and turned dark a frame or two later,
        // which is the flash this avoids.
        .visible(false)
        // No frame: minimise, maximise and close are drawn into the page by
        // `controls`, which carries its own colours and so needs nothing out
        // here to repaint it when dsh changes theme.
        .decorations(false)
        // Not a frame colour — there is no frame — but the answer every media
        // query in the webview gets, dsh's own included: tauri-runtime-wry
        // creates the webview with `with_theme(window.theme())`, so this is the
        // earliest hook there is, and every later change goes through
        // `set_theme`. `None` is `system`, which leaves it with the desktop.
        // See the module docs in [`theme`] for why it is dsh's preference and
        // not the desktop that answers, and `theme::watch` for what keeps it
        // true after the launch.
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
        .initialization_script(controls::script())
        // Turns the page's own `Notification` calls into real ones.
        .initialization_script(notify::script())
        // The plugin panel, drawn over whatever page is showing when it is
        // asked for — dsh's included, which is the point of it being here.
        .initialization_script(panel::script())
        // The runtime chooser, shown on the loading page when a launch finds no
        // dsh; see `setup`.
        .initialization_script(setup::script())
        // The app's own dialogs, in place of the window manager's; see `dialog`.
        .initialization_script(dialog::script())
        // The pairing card: a QR code and who is on it. Drawn over dsh's page
        // like the plugin panel, and for the same reason — that is where the
        // user is looking. See `remote`.
        .initialization_script(remote::script())
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
        // Watches dsh's own boot for the one failure this app cannot see any
        // other way: the server serving a page whose plugins did not load. See
        // `plugins::stall_watch`.
        .initialization_script(plugins::stall_watch())
        // Over dsh's refusal, for the moment between it loading and `auth`
        // getting the window past it. Document start is as early as there is,
        // and the reason the exchange itself cannot run here is that the token
        // is not in the refusal's address; see [`auth`].
        .initialization_script(auth::shield())
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
                // on, notifications are, or this launch is running on no
                // plugins — nothing resized to tell it, and nothing was
                // toggled. Every navigation lands here, so every navigation
                // gets all four.
                controls::sync(&webview);
                controls::sync_autostart(webview.app_handle());
                controls::sync_notify(webview.app_handle());
                controls::sync_safe(webview.app_handle());
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
            // `tauri dev` serves the bundled page off a localhost port instead
            // of `tauri.localhost`, which is the only spelling of it `is_ours`
            // knows — so under `npm run dev` the loading page was turned away
            // and the window sat on `about:blank`.
            //
            // That went unnoticed because a launch normally does not *stay* on
            // that page: dsh starts, and the navigation to its origin is one
            // `is_ours` does allow, so the blank window lasts as long as the
            // boot does. It is every launch that has something to say there
            // that was lost — a dsh that will not start, the runtime chooser,
            // the first-launch plugin guide.
            //
            // Debug builds only, and a release build has no dev URL to match
            // even if this were compiled into one.
            #[cfg(debug_assertions)]
            if is_dev_server(&opener, url) {
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

    // Before it is on screen: this is the call that reaches WebView2's default
    // background, and the window that would show the wrong one is not up yet.
    theme::background(&window, preference);
    if visible {
        let _ = window.show();
    }

    // From here the preference is the file's to change and this app's to
    // follow; see [`theme::watch`].
    theme::watch(window.clone(), preference);

    let repaint = window.clone();
    window.on_window_event(move |event| match event {
        // Closing the window parks the app in the tray instead of tearing the
        // agent down mid-task. Quitting for real goes through the tray menu.
        tauri::WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            if let Some(window) = closer.get_webview_window("main") {
                let _ = window.hide();
                memory::trim(&window);
            }
        }
        // The maximise button's glyph. A snap or a Win+Up never reaches the
        // page, so the page is told rather than left to work it out.
        tauri::WindowEvent::Resized(_) => {
            if let Some(window) = closer.get_webview_window("main") {
                controls::sync(&window);
            }
        }
        // The window's theme has settled somewhere new: the desktop moved under
        // a `system` preference, or `theme::watch` has just changed it. Either
        // way the colour behind the webview is the one thing that does not
        // follow on its own — the pages in the window are already repainting off
        // the media query this event is the tail of.
        tauri::WindowEvent::ThemeChanged(_) => {
            theme::background(&repaint, theme::preference());
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

    // The standing indication the specification asks for, and the third and
    // fourth items this menu has ever had. They are here rather than only on
    // the card because the card is a thing you have to go and open: a tunnel
    // that is still up at midnight is exactly the tunnel nobody is looking at
    // a card about.
    //
    // Two rows, not one: the hostname is what makes the warning concrete, and
    // a row that says what is happening and a row that ends it are different
    // clicks. The first is disabled — it is a label, and a label you can press
    // is a button that does nothing.
    let Some(host) = remote::exposed(app) else {
        return Menu::with_items(app, &[&show, &quit]);
    };

    let warning = MenuItem::with_id(
        app,
        "public-host",
        t!("⚠ 公网可访问：{}", "⚠ On the internet at {}", host),
        false,
        None::<&str>,
    )?;
    let close = MenuItem::with_id(
        app,
        "public-off",
        t!("关闭公网通道", "Switch the public tunnel off"),
        true,
        None::<&str>,
    )?;

    Menu::with_items(app, &[&warning, &close, &show, &quit])
}

/// Draw the tray again, because what it says may have changed.
///
/// Called from [`remote`] whenever a tunnel arrives somewhere or leaves it. A
/// menu cannot be relabelled in place — see [`tray_menu`] — so this is the same
/// rebuild [`switch_language`] does, for the same reason.
pub(crate) fn refresh_tray(app: &tauri::AppHandle) {
    let Some(tray) = app.tray_by_id(TRAY) else {
        return;
    };

    match tray_menu(app) {
        Ok(menu) => {
            let _ = tray.set_menu(Some(menu));
        }
        Err(error) => eprintln!("dsh-desktop: could not redraw the tray menu: {error}"),
    }

    // The tooltip is the half that shows without a click, which on Windows is
    // the half a user actually meets.
    let _ = tray.set_tooltip(Some(match remote::exposed(app) {
        Some(host) => t!(
            "dsh desktop — 公网可访问：{}",
            "dsh desktop — on the internet at {}",
            host
        ),
        None => "dsh desktop".to_string(),
    }));
}

/// Follow dsh into the language it has just been switched to.
///
/// Reached from the page, which is the only thing that sees the switch happen:
/// dsh writes it through to `<html lang>` without loading the document again,
/// and [`controls`] watches for that. Everything drawn from here on reads the
/// new language on its own — what needs saying out loud is the two menus that
/// were drawn before it, and the two injected cards that carry their labels
/// inside the script rather than asking for them when they draw.
fn switch_language(app: &tauri::AppHandle, tag: &str) {
    if !i18n::switch(tag) {
        return;
    }

    controls::relabel(app);
    // `relabel` carries the menu's own labels and nothing else, and safe mode
    // puts two strings on screen that are not labels: the titlebar's standing
    // one and the hint on the row. Both would keep the language the window was
    // built in — which, on a page that never loads again, is the rest of the
    // run.
    controls::sync_safe(app);
    // Not only what is on screen. An initialization script is composed once,
    // when the window is built, so without these two the plugin panel and the
    // runtime chooser would stay in the language the app started in for the
    // rest of the run — a reload included. See [`panel::relabel`].
    panel::relabel(app);
    setup::relabel(app);

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
            "public-off" => remote::stop_public(app),
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
/// Flip the notification preference, unless there is nothing to notify about.
///
/// The menu draws the row inert without the signal plugin, so an arriving verb
/// normally means the row was usable. Checked again anyway: the verb is a
/// navigation, and every verb on that channel is reachable from any script the
/// window loads (see [`controls`]). Refusing here keeps the stored preference
/// from being flipped behind a switch the user cannot see the state of.
fn toggle_notify_turns(app: &tauri::AppHandle) {
    if !plugins::signalling(app) {
        return;
    }
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
        memory::restore(&window);
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

/// Put the registry question again, from the menu.
///
/// The same dialog the first install raised, with whatever was answered then
/// already on the machine — so this is how a user who kept their own registry
/// tries ours, and how one who let us choose goes back.
///
/// On its own thread: `dialog::confirm` blocks until the click comes back, and
/// the click comes back on the main one.
fn open_registry(app: &tauri::AppHandle) {
    let app = app.clone();

    std::thread::spawn(move || {
        // Asking npm takes a moment, and the menu has just closed over a window
        // with nothing to show for the click yet.
        let saying = |text: &str| controls::busy(&app, text);
        saying(t!("正在读取 npm 配置…", "Reading npm's configuration…"));
        let configured = dsh::configured_registry(&app);
        saying("");

        // Nothing of their own to choose against. Said rather than silently
        // doing nothing, because a menu item that leads nowhere looks broken —
        // and this is the answer to "why was I never asked".
        let Some(configured) = configured else {
            dsh::note(
                &app,
                t!("没有可选的源", "Nothing to choose between"),
                t!(
                    "你的 npm 没有配置自己的源，dsh desktop 会自己测速选一个最快的。\n\n\
                     如果以后用 npm config set registry 配置了自己的源，这里就可以选了。",
                    "Your npm has no registry of its own, so dsh measures its own \
                     sources and takes the fastest.\n\nPoint npm somewhere with npm \
                     config set registry and this becomes a choice."
                ),
            );
            return;
        };

        // Nothing written down when nothing was answered; see `choose_registry`.
        if let Some(source) = dsh::choose_registry(&app, &configured) {
            settings::set_registry(&app, source);
        }
    });
}

/// Move dsh between the release candidates and the alpha line.
///
/// The one row on the settings card that can end in an install, so it holds the
/// same [`Busy`] guard [`update_dsh`] does and ends in the same
/// [`reinstall_dsh`] — the difference is only what was agreed to. See
/// [`settings::Channel`] for what the two lines are, and why the app does not
/// try to keep their data apart.
///
/// On its own thread: the lookup waits on npm, the question waits on the user,
/// and the install runs for minutes.
fn open_channel(app: &tauri::AppHandle) {
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

        // A registry away, and the menu has just closed over a window with
        // nothing to show for the click yet.
        let saying = |text: &str| controls::busy(&app, text);
        saying(t!("正在查询 dsh 的版本…", "Looking up dsh's versions…"));
        let found = dsh::tags(&app);
        let installable = dsh::installable(&app);
        saying("");

        let Some(tags) = found else {
            dsh::note(
                &app,
                t!("查不到版本", "The versions could not be looked up"),
                t!(
                    "无法查询 dsh 的版本，通常是网络或代理的问题。",
                    "dsh's versions could not be looked up, which is usually the \
                     network or a proxy."
                ),
            );
            return;
        };

        // Asked before the question is put rather than after it is answered: an
        // agreement this cannot act on is worse than saying so up front.
        let Some((prefix, installed)) = installable else {
            dsh::note(
                &app,
                t!("不能从这里切换", "This cannot be switched from here"),
                &t!(
                    "这台机器上的 dsh 不在本应用能写的目录里，或者还没装好。\n\n\
                     要换一条线，请自己在终端里执行：\n\nnpm install -g {}@alpha",
                    "The dsh on this machine is not in a directory this app may \
                     write to, or is not installed yet.\n\nTo change lines, run it \
                     yourself in a terminal:\n\nnpm install -g {}@alpha",
                    dsh::PACKAGE
                ),
            );
            return;
        };

        let current = settings::dsh_channel(&app);
        let wanted = match current {
            settings::Channel::Rc => settings::Channel::Alpha,
            settings::Channel::Alpha => settings::Channel::Rc,
        };

        // The one state with nothing to ask: on rc, with an alpha that is not
        // ahead. Told rather than drawn as a row greyed out on the card — the
        // card would have to know the answer before it could grey anything out,
        // and that answer is a registry away.
        if current == settings::Channel::Rc && !tags.alpha_is_ahead() {
            dsh::note(
                &app,
                t!("现在不能切到 alpha", "Alpha is not ahead right now"),
                &t!(
                    "rc 是 {}，alpha 是 {}。\n\n\
                     alpha 没有跑在 rc 前面，切过去等于装一个更旧的 dsh，\
                     所以现在不让切。等 alpha 发出比 rc 新的版本再来。",
                    "The rc line is at {}, the alpha line at {}.\n\n\
                     Alpha is not ahead of rc, so switching would install an older \
                     dsh than the one you have. Come back once alpha has published \
                     something newer than rc.",
                    tags.rc,
                    alpha_or_none(&tags)
                ),
            );
            return;
        }

        if !agreed(&app, &tags, wanted) {
            return;
        }

        settings::set_dsh_channel(&app, wanted);
        reinstall_dsh(&app, &session, &prefix, &installed);
    });
}

/// `alpha` as the dialogs say it: the version, or that there is no such release
/// at all. A package with no `alpha` tag is not an error, and should not be
/// printed as a blank.
fn alpha_or_none(tags: &dsh::Tags) -> String {
    match &tags.alpha {
        Some(alpha) => alpha.to_string(),
        None => t!("还没有", "not published yet").to_string(),
    }
}

/// Put the question and wait for it.
///
/// Blocking, like every other dialog that decides what happens next.
fn agreed(app: &tauri::AppHandle, tags: &dsh::Tags, wanted: settings::Channel) -> bool {
    let (title, body, go) = channel_question(tags, wanted, &plugins::dsh_home());

    dialog::confirm(
        app,
        dialog::Ask {
            title,
            body,
            choices: vec![
                dialog::Choice::new("cancel", t!("取消", "Cancel")),
                dialog::Choice::primary("switch", go),
            ],
            // Replaced by `confirm`; it is the channel send that answers.
            answered: Box::new(|_, _| {}),
        },
        "switch",
    )
}

/// The words of the question, which are the whole of what the user has to
/// decide on — so they say what changes, what it costs, and what is left alone.
///
/// A function of its own, taking the home rather than resolving it, so that
/// `the_switch_says_what_it_costs` can read the finished text without a machine
/// to read it off.
///
/// The one thing this text must not do is promise isolation. Both lines run out
/// of the same `$DSH_HOME` and the app installs one dsh globally, so the
/// honest thing to say is what the risk is and where the directory to copy is —
/// not that going back is free. See [`settings::Channel`].
fn channel_question(
    tags: &dsh::Tags,
    wanted: settings::Channel,
    home: &std::path::Path,
) -> (String, String, String) {
    if wanted == settings::Channel::Alpha {
        (
            t!("切到 alpha 版？", "Switch to the alpha line?").to_string(),
            t!(
                "alpha 现在是 {}，rc 是 {}。\n\n\
                 alpha 会改还没定下来的东西，其中包括 dsh 存会话的格式。\
                 alpha 打开过的会话，切回 rc 之后不保证还能用。\n\n\
                 两条线用的是同一个数据目录：\n{}\n\n\
                 切之前请自己把这个目录备份一份。切回 rc 随时可以，\
                 但已经被 alpha 改过的数据，只有你的备份能还原——\
                 本应用不会替你留副本。\n\n\
                 还有一点：全局只装得下一个 dsh，切过去之后，\
                 你自己在终端里敲的 dsh 也是 alpha。",
                "Alpha is at {}, and rc at {}.\n\n\
                 Alpha changes things that are not settled yet, and one of them is \
                 the format dsh writes its sessions in. A session alpha has opened \
                 is not one rc promises to be able to read afterwards.\n\n\
                 Both lines use the same home:\n{}\n\n\
                 Back that directory up yourself before switching. You can return \
                 to rc whenever you like, but anything alpha has already rewritten \
                 is restorable only from your own copy — this app does not keep \
                 one.\n\n\
                 One more thing: only one dsh can be installed globally, so after \
                 this the dsh you type in your own terminal is the alpha one too.",
                alpha_or_none(tags),
                tags.rc,
                home.display()
            ),
            t!("已备份，切到 alpha", "I have a backup; switch").to_string(),
        )
    } else {
        (
            t!("切回 rc 版？", "Switch back to rc?").to_string(),
            t!(
                "现在在 alpha {}，rc 是 {}。\n\n\
                 切回去会把 dsh 换成 rc，数据目录不变：\n{}\n\n\
                 你在 alpha 期间写下的东西都还在那里，但 rc 不保证每一条都还能打开。\
                 真打不开，就用你切去 alpha 之前的备份还原。",
                "You are on alpha {}, and rc is at {}.\n\n\
                 Switching back replaces dsh with rc. The home does not change:\n{}\n\n\
                 Everything you wrote while on alpha is still in it, but rc does not \
                 promise to be able to open all of it. If something will not open, \
                 restore the backup you took before switching to alpha.",
                alpha_or_none(tags),
                tags.rc,
                home.display()
            ),
            t!("切回 rc", "Switch back to rc").to_string(),
        )
    }
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

/// Whether `url` is the dev server `tauri dev` serves the bundled page from.
///
/// The page's address is the CLI's to choose — it picks a port — so it is read
/// off the config the CLI wrote rather than written down here. Compiled only
/// into a debug build, where that address exists.
#[cfg(debug_assertions)]
fn is_dev_server(app: &tauri::AppHandle, url: &Url) -> bool {
    app.config()
        .build
        .dev_url
        .as_ref()
        .is_some_and(|dev| dev.origin() == url.origin())
}

/// Whether a boot or a dsh update owns the window and the server right now.
///
/// The two do the same things in the same order — settle which dsh runs, then
/// start it and hand the window over — and both take minutes. Running them at
/// once would be two npms writing one tree and two `dsh web` children racing for
/// the one slot in [`Session`], so the second one is turned away rather than
/// joining in.
static BUSY: AtomicBool = AtomicBool::new(false);

/// Whether the plugin panel is on screen.
///
/// The two verbs it sends are the only ones in [`controls::Action`] that stop
/// dsh and run a package manager, and they were the only ones a panel had never
/// had to be open for. Their neighbours all have this door: `setup::answered`
/// does nothing unless the chooser has a thread waiting on it, and
/// `dialog::answered` checks the token it handed the dialog it drew. So any
/// script on dsh's page — a plugin's included — could take the server down and
/// put a package into the profile with the panel never having been opened, and
/// all the user would see is dsh disappearing.
///
/// Not a token, which `controls` explains cannot work here: the scripts that
/// draw the panel share a JavaScript context with dsh's own code, so anything
/// handed to them is readable by the page. Whether the panel is up is the one
/// part of the question this side knows on its own, and it is the part that
/// narrows the verbs from "any script, any time" to "while the user is looking
/// at the panel".
static PANEL: AtomicBool = AtomicBool::new(false);

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

        // The plugin every notification starts at, put in on the first launch
        // that has a dsh to put it into. See [`plugins::adopt`], which does it
        // once and then leaves the decision to the user.
        //
        // Here rather than a line later because the panel below reads what is
        // installed to draw itself, and a first launch should find the plugin
        // already in rather than offered. Here rather than anywhere after,
        // because everything after this point has a server running and an
        // install is a reason to stop one.
        //
        // Skipped on a login-item launch, for the reason the update check
        // above is: nobody is looking, and this can reach the network — an
        // absent pnpm is an `npm install -g` away. The next launch someone
        // actually asks for does it.
        let adoption_failed = window_is_visible(&app) && plugins::adopt(&app, &report);

        // Once, on the launch that first has a dsh to add plugins to — and once
        // more for an install that predates the panel existing. It is shown
        // before the server starts rather than after, because installing a
        // plugin means stopping the server again, and the user has just watched
        // it start.
        //
        // And once more again for a launch whose own attempt at the signal
        // plugin failed. That attempt is not repeated — see [`plugins::adopt`]
        // — so this is where it gets said: the panel lists the plugin, installs
        // it on a click, and this time has somewhere to print the reason if it
        // fails again. The alternative is notifications that never work and a
        // grey menu item to find out from.
        //
        // Marked as shown before it is shown: a panel that crashes the launch it
        // appears on should not appear on the next one too. What happens next is
        // the panel's — see [`leave_plugins`].
        if window_is_visible(&app) && (adoption_failed || !plugins::guided(&app)) {
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

        reinstall_dsh(&app, &session, &prefix, &installed);
    });
}

/// Take dsh down, put another one in its place, and start serving again.
///
/// The half of [`update_dsh`] that runs once something has been agreed to, and
/// the reason it is a function of its own: [`open_channel`] agrees to a
/// different thing — a release line rather than a version — and then needs
/// exactly this. Which version npm fetches is not decided here at all; that is
/// `-Channel` and the tag behind it, which [`dsh::run`] puts on the command.
///
/// The caller owns the [`Busy`] guard and is already off the main thread, which
/// is what makes it safe to block here for as long as npm takes.
fn reinstall_dsh(
    app: &tauri::AppHandle,
    session: &Session,
    prefix: &std::path::Path,
    installed: &semver::Version,
) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };

    stop_server(app, session);
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
    if !dsh::update(app, prefix, installed, &report) {
        return;
    }

    start_serving(app, &window, session);
}

/// Start `dsh web` and hand the window over to it. Blocks until it is serving or
/// has given up, reporting either onto the loading page.
fn start_serving(app: &tauri::AppHandle, window: &WebviewWindow, session: &Session) {
    // Before the spawn, because what it repairs is a `dsh web` that exits on
    // the way up rather than one that misbehaves once it is serving: a name on
    // the profile's layer stack that no longer resolves is a `throw` before the
    // port is bound. See [`plugins::audit`] for how the profile gets into that
    // state and why nothing in dsh gets it out again.
    //
    // Said out loud when it does something. This rewrites a file the user owns,
    // and the alternative to saying so is a launch that inexplicably worked
    // after one that inexplicably did not. Folded into the line the start was
    // going to draw anyway rather than shown first and overwritten a moment
    // later, which is a message nobody can read.
    let cleared = plugins::audit(app);

    // And again on every start for as long as safe mode lasts, rather than only
    // on the way into it: dsh reconciles the layer stack after any plugin change
    // pnpm completes, which puts back every plugin the user did not remove. See
    // [`plugins::engage`].
    //
    // The recorded set and not every plugin: a launch that widened a repair
    // aimed at one plugin into an outage across all of them would be undoing
    // the user's answer on their behalf.
    let safe = plugins::safe(app);
    if safe {
        if let Err(why) = plugins::engage_recorded(app) {
            eprintln!("dsh-desktop: could not keep the plugins off the layer stack: {why}");
        }
    }

    session.splash.status(
        window,
        &if safe {
            // Ahead of the repair line below, and not joined to it: a launch
            // with plugins set aside is the larger fact about this start, and
            // residue cleared out of a stack is not news beside it.
            //
            // Which sentence depends on whether anything is still loading. The
            // total one has to stay available -- the rescue button and the
            // stall dialog's fallback both still take every plugin off -- and
            // on that launch "these are not loading" listing all of them reads
            // worse than saying so plainly.
            let aside = plugins::set_aside(app);
            if plugins::partial(app) {
                t!(
                    "正在启动 dsh：这些插件不加载（{}）。",
                    "Starting dsh; these plugins are set aside ({}).",
                    aside.join(" ")
                )
            } else {
                t!(
                    "正在启动 dsh：插件都不加载。",
                    "Starting dsh with no plugins loaded."
                )
                .to_string()
            }
        } else if cleared.is_empty() {
            t!("正在启动 dsh…", "Starting dsh…").to_string()
        } else {
            t!(
                "清掉了上次装卸插件没收尾留下的残留（{}），正在启动 dsh…",
                "Cleared what an unfinished plugin change left behind ({}); starting dsh…",
                cleared.join(" ")
            )
        },
    );
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
fn stop_server(app: &tauri::AppHandle, session: &Session) {
    session.epoch.fetch_add(1, Ordering::SeqCst);

    if let Some(mut running) = session.server.lock().unwrap().take() {
        running.stop();
    }
    // The dsh page went with it, so nothing may be treated as ours until a new
    // server says otherwise.
    *session.origin.write().unwrap() = None;
    // And a phone on the gateway is told dsh stopped, rather than being handed
    // whatever a connection to a closed port looks like. See `remote`.
    remote::dsh_gone(app);
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

    // A dsh that exited on its own may have exited because the profile's layer
    // stack names something it can no longer resolve — an install or removal
    // that pnpm did not finish is exactly the kind of thing that stops a server
    // mid-run. Cheap, and the alternative is restarting into the same refusal
    // [`RESTARTS`] times over. Its own line goes to the terminal; there is
    // nothing on screen here but dsh's own connection indicator.
    plugins::audit(app);

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

            // The restart path, and the one the gateway would otherwise get
            // wrong: same port, new process, new token, and the cookie the
            // gateway is holding is now worth nothing. See `remote::upstream`.
            remote::dsh_ready(app, &url);

            // Only when the port moved. On the same one the page is already
            // pointed at a server that is back, and reloading it would throw
            // away the very thing staying put is for.
            if !same {
                // Before the navigation rather than after it: the header this
                // clears is on that request. See [`cookies::purge`].
                cookies::purge(window, &url);
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
    remote::dsh_gone(window.app_handle());
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
    session.splash.failed(
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
        true,
        // A dsh that served and then kept dying is a weaker case against the
        // plugins than one that never served at all — but it is still a case,
        // and a plugin is one of the few things here the user can act on.
        plugins::rescuable(window.app_handle()),
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
///
/// `into_plugins` opens the panel on the far side of the start; see
/// [`safe_start`], which is the one caller that wants it.
fn restart_dsh(app: &tauri::AppHandle, into_plugins: bool) {
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
        stop_server(&app, &session);

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

        // Only once dsh is actually serving. A safe start that failed anyway is
        // a start whose plugins were never the problem, and the error on the
        // loading page is the thing the user needs to read — drawing the panel
        // over it would hide the one answer this start produced.
        if into_plugins && session.origin.read().unwrap().is_some() {
            show_plugins(&app, &session, false);
        }
    });
}

/// Start dsh again with every installed plugin off the profile's layer stack,
/// and open the panel that can take one out.
///
/// The loading page's other button, and the way out of the failure a plugin
/// causes: `dsh web` composes that stack before it binds a port, so a bundle
/// that throws on the way up is an app with no page to open the plugin list
/// from. See [`plugins::engage_safe`] for what is moved and how it comes back.
/// dsh's page says its plugin boot stopped; offer the rescue.
///
/// See [`plugins::stall_watch`] for what was detected and why nothing else here
/// could detect it. This is the only path to safe mode that does not start from
/// a dsh that failed to serve — the server is up and fine, and the window is
/// showing dsh's "Failed to load plugins" card.
///
/// A question rather than a repair. Taking every plugin off the stack is a
/// visible change to the user's profile, and the one thing this app is sure of
/// is what the page looks like, not which plugin did it — [`safe_start`] is a
/// way to get to the panel and find out, not a diagnosis.
///
/// Silent when there is nothing to offer: a profile whose only plugins are
/// dsh's own is one safe mode cannot repair, and the card on screen is then
/// about something this app has no answer for.
///
/// On its own thread, like every other dialog here: [`dialog::confirm`] blocks
/// until the click comes back on the main thread, and this is reached from the
/// navigation handler, which runs on it.
fn plugins_stalled(app: &tauri::AppHandle, said: &str) {
    if !plugins::rescuable(app) {
        return;
    }

    let blamed = plugins::blamed(app, said);
    let app = app.clone();

    std::thread::spawn(move || {
        // Naming a plugin and setting every plugin aside are the same answer
        // asked two different ways, so the button says which one this is.
        let go = if blamed.is_empty() {
            t!("停用所有插件并重启", "Restart with no plugins")
        } else {
            t!("停用它并重启", "Set it aside and restart")
        };

        let agreed = dialog::confirm(
            &app,
            dialog::Ask {
                title: t!("插件没能加载", "The plugins did not load").to_string(),
                body: stall_question(&blamed),
                choices: vec![
                    dialog::Choice::new("stay", t!("先不管", "Leave it")),
                    dialog::Choice::primary("safe", go),
                ],
                // Replaced by `confirm`; it is the channel send that answers.
                answered: Box::new(|_, _| {}),
            },
            "safe",
        );

        if !agreed {
            return;
        }

        if blamed.is_empty() {
            safe_start(&app);
        } else {
            safe_start_only(&app, &blamed);
        }
    });
}

/// What the stall dialog says, which depends on whether the card named anything
/// this app recognises.
///
/// Naming the culprit is most of the value: the user is looking at a window
/// that says nothing but "Failed to load plugins", and the difference between
/// that and a plugin's name is the difference between a mystery and one line in
/// the panel to undo. It is also what makes the repair worth offering at all —
/// with a name, only that plugin goes and the rest keep working; without one
/// there is nothing to aim at and every plugin has to come off.
fn stall_question(blamed: &[String]) -> String {
    if blamed.is_empty() {
        return t!(
            "dsh 起来了，但它的插件里有没能激活的，于是整个页面停在了那张卡片上。

             卡片上没有报出是哪一个，所以只能先把插件全部停用再启动一次，             进去之后在插件面板里排查。其余的插件会记下来，退出安全模式时装回去。",
            "dsh started, but a plugin never activated, and that leaves the whole              page on that card.

The card did not name one this app recognises, so              the only repair left is to start again with every plugin set aside and              work it out from the plugin panel. They are written down and go back on              when you leave safe mode."
        )
        .to_string();
    }

    t!(
        "dsh 起来了，但这个插件没能激活，于是整个页面停在了那张卡片上：

{}

         这不是 dsh 本身的问题，是这个插件要的东西这个版本没有。

         可以只把它停用再启动一次，别的插件照常加载。它本来就没在工作，         所以停掉它不会少什么。之后在插件面板里更新或卸载它；         想把它装回来，用菜单里的「重新加载插件」。",
        "dsh started, but this plugin never activated, and that leaves the whole          page on that card:

{}

This is not dsh itself — it is that plugin          asking for something this version does not have.

The app can start          again with just that one set aside, and everything else loading as          usual. It was not working anyway, so nothing is lost by it. You can then          update or remove it from the plugin panel; “Load plugins again” in the          menu puts it back.",
        blamed.join("
")
    )
}

/// Set aside only the plugins that were named, and start again.
///
/// [`safe_start`] aimed at something: the same two steps, over a list rather
/// than over everything. The failure is worded the same way because it is the
/// same failure — a profile whose plugin list will not change.
fn safe_start_only(app: &tauri::AppHandle, names: &[String]) {
    match plugins::engage_only(app, names) {
        Ok(_) => restart_dsh(app, true),
        Err(why) => dsh::note(
            app,
            t!("没能停用插件", "Could not set the plugins aside"),
            &t!(
                "profile 的插件列表改不动，所以没有启动：{}",
                "The profile's plugin list could not be changed, so nothing was started: {}",
                why
            ),
        ),
    }
}

fn safe_start(app: &tauri::AppHandle) {
    match plugins::engage_safe(app) {
        Ok(_) => restart_dsh(app, true),
        Err(why) => dsh::note(
            app,
            t!("没能停用插件", "Could not set the plugins aside"),
            &t!(
                "profile 的插件列表改不动，所以没有启动：{}",
                "The profile's plugin list could not be changed, so nothing was started: {}",
                why
            ),
        ),
    }
}

/// Put the plugins back on the stack and restart into them. The menu item that
/// only exists while [`safe_start`] is in effect.
fn safe_off(app: &tauri::AppHandle) {
    match plugins::release_safe(app) {
        Ok(_) => restart_dsh(app, false),
        // Said rather than swallowed, and safe mode stays on: the record of
        // what to put back is only deleted once the stack has been written, so
        // the next launch — and the next click on this item — still has it.
        Err(why) => dsh::note(
            app,
            t!("没能装回插件", "Could not load the plugins again"),
            &t!(
                "profile 的插件列表改不动，插件仍然没有加载：{}",
                "The profile's plugin list could not be changed, so the plugins are still not \
                 loaded: {}",
                why
            ),
        ),
    }
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

    PANEL.store(true, Ordering::SeqCst);
    // Drawn with no update marks on it. Finding out which plugins are behind
    // means asking a registry, and the list is what the user came for — so it
    // goes up now and the marks arrive when they arrive; see `look_for_updates`.
    session.splash.plugins(&window, &plugins::listing(app, &[]), first);
    look_for_updates(app, session);
}

/// Ask pnpm which installed plugins are behind, and redraw the lists with what
/// it says. On a thread, because it reaches the network.
///
/// Nothing waits on this and nothing goes wrong when it never answers: the
/// panel simply keeps the lists it already has, which is what it had before
/// there were update buttons at all.
///
/// Two things are checked before the redraw, both of them about the panel
/// having moved on while the network was being waited for. A panel the user has
/// already left is not one to draw into — `plugin_lists` would evaluate into
/// whatever page replaced it — and a plugin job that has started since owns the
/// lists until it finishes, having stopped dsh to do it.
fn look_for_updates(app: &tauri::AppHandle, session: &Session) {
    let app = app.clone();
    let session = session.clone();

    std::thread::spawn(move || {
        let behind = plugins::outdated(&app);
        if behind.is_empty() {
            return;
        }

        if !PANEL.load(Ordering::SeqCst) || BUSY.load(Ordering::SeqCst) {
            return;
        }

        if let Some(window) = app.get_webview_window("main") {
            session
                .splash
                .plugin_lists(&window, &plugins::listing(&app, &behind));
        }
    });
}

/// Run one pnpm job against the profile with `dsh web` down for the duration,
/// reporting onto the panel.
///
/// The server has to come down: pnpm is about to rewrite the profile directory
/// the running server read its plugins out of. It also has to come back up
/// afterwards, which is what leaving the panel does — a change to the profile
/// is only in the window once dsh has been started again to load it.
///
/// Installing and removing were two copies of this, differing in three strings
/// and the one line that does the work. The copies are the reason it is worth
/// naming what they shared: the [`BUSY`] claim has to be taken on this thread
/// and released by the [`Busy`] guard on the spawned one, and a second job
/// slipping between those two would be a pnpm rewriting the directory the
/// first one is reading.
fn change_plugins<F>(app: &tauri::AppHandle, busy: &str, done: &'static str, work: F)
where
    F: FnOnce(&tauri::AppHandle, &plugins::Log<'_>) -> Result<(), String> + Send + 'static,
{
    // No panel, nobody asked. Silently, the way `setup::answered` drops an
    // answer nothing is waiting for: a note here would be a dialog any script
    // could raise at will. See [`PANEL`].
    if !PANEL.load(Ordering::SeqCst) {
        eprintln!("dsh-desktop: a plugin change arrived with no panel open; ignored");
        return;
    }

    if BUSY.swap(true, Ordering::SeqCst) {
        dsh::note(app, t!("请稍等", "One moment"), busy);
        return;
    }

    let session = app.state::<Session>().inner().clone();
    let app = app.clone();

    std::thread::spawn(move || {
        let _busy = Busy;
        let Some(window) = app.get_webview_window("main") else {
            return;
        };

        stop_server(&app, &session);

        let log = |line: &str| session.splash.plugin_log(&window, line);
        match work(&app, &log) {
            Ok(()) => {
                session
                    .splash
                    .plugin_lists(&window, &plugins::listing(&app, &[]));
                // What was behind a moment ago may not be any more — an update
                // is exactly the job that changes this — so it is asked again
                // rather than carried over.
                look_for_updates(&app, &session);
                session.splash.plugin_done(&window, true, done);
            }
            Err(error) => session.splash.plugin_done(&window, false, &error),
        }
    });
}

/// Install what the panel asked for: the ticked presets, and whatever was
/// typed into its box.
fn install_plugins(app: &tauri::AppHandle, ids: Vec<String>, spec: Option<String>) {
    change_plugins(
        app,
        t!(
            "dsh 正在启动或更新中，等它忙完再装插件。",
            "dsh is starting or updating; wait for that to finish before installing plugins."
        ),
        t!(
            "装好了。回到 dsh 时会重新启动它，插件在那之后生效。",
            "Done. dsh restarts on the way back, and the plugins take effect then."
        ),
        move |app, log| plugins::install(app, &ids, spec.as_deref(), log),
    );
}

/// Bring one plugin up to its newest release.
///
/// The same machinery an install runs through — dsh comes down, pnpm rewrites
/// the profile, dsh goes back up on the way out of the panel — because that is
/// what an update is: `dsh plugin add <name>` with no version, resolved again.
fn update_plugins(app: &tauri::AppHandle, names: Vec<String>) {
    change_plugins(
        app,
        t!(
            "dsh 正在启动或更新中，等它忙完再动插件。",
            "dsh is starting or updating; wait for that to finish before changing plugins."
        ),
        t!(
            "更新完成。回到 dsh 时会重新启动它。",
            "Updated. dsh restarts on the way back."
        ),
        move |app, log| plugins::update(app, &names, log),
    );
}

/// Take the ticked plugins out again.
fn remove_plugins(app: &tauri::AppHandle, names: Vec<String>) {
    change_plugins(
        app,
        t!(
            "dsh 正在启动或更新中，等它忙完再动插件。",
            "dsh is starting or updating; wait for that to finish before changing plugins."
        ),
        t!(
            "卸载完成。回到 dsh 时会重新启动它。",
            "Removed. dsh restarts on the way back."
        ),
        move |app, log| plugins::remove(app, &names, log),
    );
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
        PANEL.store(false, Ordering::SeqCst);
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
                // The token on this URL is what the phone gateway exchanges for
                // a dsh session of its own, and it belongs to this dsh process
                // rather than to this launch — so every URL dsh prints has to
                // reach `remote`, not just the first. See `remote::upstream`.
                remote::dsh_ready(window.app_handle(), &url);
                splash.status(window, t!("正在打开界面…", "Opening the interface…"));

                // Before the navigation rather than after it: the header this
                // clears is on that request. See [`cookies::purge`].
                cookies::purge(window, &url);
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
                // The failure a plugin most often causes: dsh composes the
                // profile's layer stack before it binds a port, so a bundle
                // that throws is an exit with no URL ever printed — this branch
                // exactly.
                splash.failed(
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
                    false,
                    plugins::rescuable(window.app_handle()),
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
        self.failed(window, title, detail, false, false);
    }

    /// The same, with the buttons the user can do something with from here.
    ///
    /// `retry` draws the button that starts dsh again; `rescue` the one that
    /// starts it with the installed plugins off the layer stack. The second is
    /// only offered where a plugin is a candidate for the cause — a dsh that
    /// would not come up — and only where there is a plugin to set aside; see
    /// [`plugins::rescuable`].
    fn failed(
        &self,
        window: &WebviewWindow,
        title: &str,
        detail: &str,
        retry: bool,
        rescue: bool,
    ) {
        eprintln!("dsh-desktop: {title}: {detail}");
        self.call(
            window,
            "dshError",
            &[
                title,
                detail,
                if retry { "retry" } else { "" },
                if rescue { "rescue" } else { "" },
            ],
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

#[cfg(test)]
mod tests {
    use crate::settings::Channel;
    use semver::Version;

    fn tags(rc: &str, alpha: &str) -> crate::dsh::Tags {
        crate::dsh::Tags {
            rc: Version::parse(rc).unwrap(),
            alpha: Some(Version::parse(alpha).unwrap()),
        }
    }

    /// The switch to alpha is not reversible by anything this app does — one
    /// home, one globally installed dsh — so the dialog is the whole of the
    /// mitigation, and every part of it has to be there. A version of this text
    /// that quietly loses the backup line is a version that has gone back to
    /// promising something it cannot deliver.
    #[test]
    fn the_switch_says_what_it_costs() {
        let (_, body, go) = super::channel_question(
            &tags("0.1.5-rc.2", "0.1.7-alpha.1"),
            Channel::Alpha,
            std::path::Path::new("/home/someone/.dsh"),
        );

        // Both versions, so the user knows what they are trading.
        assert!(body.contains("0.1.7-alpha.1") && body.contains("0.1.5-rc.2"));
        // The directory to copy, spelled out. Telling someone to back up
        // without saying what is the same as not telling them.
        assert!(body.contains("/home/someone/.dsh"));
        // That it is on them, and that the app keeps nothing.
        assert!(body.contains("备份") || body.to_lowercase().contains("back"));
        // That their own terminal changes under them too, which is the thing
        // nobody expects.
        assert!(body.contains("终端") || body.contains("terminal"));
        // And that the button is not an innocent one.
        assert!(go.contains("备份") || go.to_lowercase().contains("backup"));
    }

    /// Going back says where the data is and does not pretend rc can read all
    /// of it. It must not claim an untouched copy is waiting, because there is
    /// none.
    #[test]
    fn going_back_does_not_promise_an_untouched_home() {
        let (_, body, _) = super::channel_question(
            &tags("0.1.5-rc.2", "0.1.7-alpha.1"),
            Channel::Rc,
            std::path::Path::new("/home/someone/.dsh"),
        );

        assert!(body.contains("/home/someone/.dsh"));
        assert!(body.contains("不保证") || body.contains("does not"));
    }
}
