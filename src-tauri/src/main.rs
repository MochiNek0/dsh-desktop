// The release build is a GUI app: no console window behind it.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod controls;
mod dsh;
mod server;
mod theme;
mod update;
mod warm;

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once, RwLock};
use std::time::Duration;

use tauri::menu::{CheckMenuItem, Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::webview::PageLoadEvent;
use tauri::{Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tauri_plugin_autostart::{ManagerExt, MacosLauncher};
use tauri_plugin_opener::OpenerExt;

/// The origin `dsh web` bound to, once it has. Navigation inside it stays in the
/// window; anything else is a link to the outside world.
type Origin = Arc<RwLock<Option<String>>>;

/// How long to sit on the boot message before telling the user it is slow.
const SLOW_BOOT: Duration = Duration::from_secs(20);

/// Passed by the login item the tray menu creates. The app is starting because
/// the machine did, not because anyone asked to see it, so it waits in the tray
/// with dsh already running behind it.
const AUTOSTART_FLAG: &str = "--autostart";

fn main() {
    let origin: Origin = Arc::new(RwLock::new(None));
    let server: Arc<Mutex<Option<server::Server>>> = Arc::new(Mutex::new(None));

    let setup_origin = origin.clone();
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
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![AUTOSTART_FLAG]),
        ))
        .setup(move |app| {
            // Before the first spawn, while nothing holds the directory open.
            dsh::promote(app.handle());

            // Ahead of the boot below, so that the files dsh is about to read
            // are being read from a dozen threads while it reads them from one.
            // The update check it now overlaps with is the usual case — there is
            // most often nothing newer to fetch — so this is still time the user
            // would otherwise spend waiting.
            warm::start(app.handle());

            let preference = theme::preference();
            let splash = Splash::default();
            let visible = !std::env::args().any(|argument| argument == AUTOSTART_FLAG);

            // The window comes up first now: the update check runs behind it and
            // can put a question and a progress bar on the loading page, neither
            // of which has anywhere to go without a window.
            let window = build_window(
                app.handle(),
                setup_origin.clone(),
                splash.clone(),
                preference,
                visible,
            )?;
            build_tray(app.handle())?;

            theme::paint(&window, preference);

            boot(
                app.handle().clone(),
                window,
                setup_origin.clone(),
                splash,
                setup_server.clone(),
            );

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build the dsh desktop app");

    app.run(move |_handle, event| {
        if let tauri::RunEvent::Exit = event {
            dsh::stop();
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
    splash: Splash,
    preference: theme::Preference,
    visible: bool,
) -> tauri::Result<WebviewWindow> {
    let opener = app.clone();
    let closer = app.clone();

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
        .initialization_script(theme::script(preference))
        .initialization_script(controls::script())
        .on_page_load(move |webview, payload| {
            if payload.event() == PageLoadEvent::Finished {
                splash.flush(&webview);
                // The buttons were just drawn by a fresh document that has no
                // way of knowing the window is maximised — nothing resized to
                // tell it. Every navigation lands here, so every navigation
                // gets the answer.
                controls::sync(&webview);
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

/// The tray icon: how the window comes back once it has been closed, and the
/// only way to actually quit.
fn build_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "显示窗口", true, None::<&str>)?;
    let autostart = CheckMenuItem::with_id(
        app,
        "autostart",
        "开机自启动",
        true,
        app.autolaunch().is_enabled().unwrap_or(false),
        None::<&str>,
    )?;
    let check = MenuItem::with_id(app, "check", "检查更新…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出 dsh", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &autostart, &check, &quit])?;

    let mut tray = TrayIconBuilder::new()
        .tooltip("dsh desktop")
        .menu(&menu)
        // Left click reveals the window; the menu belongs on the right button.
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "show" => reveal(app),
            "autostart" => toggle_autostart(app, &autostart),
            "check" => update::check_now(app),
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
fn toggle_autostart(app: &tauri::AppHandle, item: &CheckMenuItem<tauri::Wry>) {
    let manager = app.autolaunch();
    let was = manager.is_enabled().unwrap_or(false);

    let changed = if was { manager.disable() } else { manager.enable() };
    if let Err(error) = changed {
        eprintln!("dsh-desktop: 无法设置开机自启动：{error}");
    }

    let _ = item.set_checked(manager.is_enabled().unwrap_or(was));
}

/// Bring the window back to the front, whatever it was hidden behind.
fn reveal(app: &tauri::AppHandle) {
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

/// Settle which dsh this launch runs, start it, and hand the window over to it.
///
/// One background thread for the whole sequence, because the whole sequence is
/// blocking and ordered: the update check waits on npm, the question it can
/// raise waits on the user, the download that may follow runs for minutes, and
/// only once all of that is behind us is there a server to wait for. None of it
/// belongs on the main thread.
fn boot(
    app: tauri::AppHandle,
    window: WebviewWindow,
    origin: Origin,
    splash: Splash,
    server: Arc<Mutex<Option<server::Server>>>,
) {
    std::thread::spawn(move || {
        let report = |text: &str, percent: f64| {
            if !text.is_empty() {
                splash.status(&window, text);
            }
            splash.progress(&window, percent);
        };

        // Skipped entirely on a login-item launch, which is sitting in the tray
        // with nobody looking at it: a modal asking about a 185 MB download —
        // from an app the user never opened — belongs to no window on screen,
        // and the restart it leads to would be one nobody was expecting. The
        // next launch someone actually asks for does the check.
        //
        // False means the user took the update and the app is on its way to
        // restarting into it. Starting a server now would be starting one for a
        // process that is about to go away.
        if window_is_visible(&app) && !dsh::gate(&app, &report) {
            return;
        }

        report("正在启动 dsh…", -1.0);

        match server::start(&app) {
            Ok((child, events)) => {
                *server.lock().unwrap() = Some(child);
                serve(&window, &origin, &splash, &events);
            }
            // Most often this is an install whose dsh download failed: the
            // installer fetches dsh rather than carrying it, and an app that
            // got this far without one has nothing to run. Running the
            // installer again is the fix, so it is what this leads with.
            Err(error) => splash.fail(
                &window,
                "启动 dsh 失败",
                &format!(
                    "无法执行 dsh：{error}\n\n\
                     dsh 可能没有安装成功。请换一个网络或代理后重新运行安装程序。\n\n\
                     如果你自己装过 dsh，也可以在终端里执行 `dsh --version` 确认，\
                     或用 DSH_BIN 环境变量指向 dsh 可执行文件的完整路径。"
                ),
            ),
        }

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
/// loading page.
fn serve(
    window: &WebviewWindow,
    origin: &Origin,
    splash: &Splash,
    events: &Receiver<server::Event>,
) {
    loop {
        match events.recv_timeout(SLOW_BOOT) {
            Ok(server::Event::Ready(url)) => {
                let Ok(url) = Url::parse(&url) else {
                    splash.fail(
                        window,
                        "启动 dsh 失败",
                        &format!("无法解析 dsh 输出的地址：{url}"),
                    );
                    return;
                };

                *origin.write().unwrap() = Some(url.origin().ascii_serialization());
                splash.status(window, "正在打开界面…");

                let handle = window.app_handle().clone();
                let window = window.clone();
                let splash = splash.clone();
                let _ = handle.run_on_main_thread(move || {
                    if let Err(error) = window.navigate(url) {
                        splash.fail(&window, "打开界面失败", &error.to_string());
                    }
                });
                return;
            }
            Ok(server::Event::Failed(output)) => {
                splash.fail(
                    window,
                    "dsh 已退出",
                    &if output.is_empty() {
                        "dsh 在开始服务前就退出了，且没有任何输出。".to_string()
                    } else {
                        format!("dsh 在开始服务前就退出了。它的输出：\n\n{output}")
                    },
                );
                return;
            }
            Err(RecvTimeoutError::Timeout) => {
                splash.status(window, "dsh 启动较慢，仍在等待…");
                // A dsh that neither serves nor exits would otherwise keep the
                // checks out of reach for as long as it hangs — and an update
                // is one of the things that fixes that. They run once, so the
                // timeouts after this one cost nothing.
                check_for_updates(window.app_handle());
            }
            Err(RecvTimeoutError::Disconnected) => return,
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

    /// Replace the spinner with an error the user can read and copy.
    fn fail(&self, window: &WebviewWindow, title: &str, detail: &str) {
        eprintln!("dsh-desktop: {title}: {detail}");
        self.call(window, "dshError", &[title, detail]);

        // A login-item launch leaves the window hidden in the tray, where an
        // error report is written to a page the user has no reason to open. The
        // boot is over either way, so whatever lands here asks for the window.
        let handle = window.app_handle().clone();
        let target = handle.clone();
        let _ = handle.run_on_main_thread(move || reveal(&target));
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
        let js = format!(
            "window.{function} && window.{function}({})",
            args.join(", ")
        );

        let mut state = self.state.lock().unwrap();
        if state.loaded {
            let _ = window.eval(&js);
        } else {
            state.pending.push(js);
        }
    }
}
