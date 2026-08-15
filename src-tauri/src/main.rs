// The release build is a GUI app: no console window behind it.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

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
use tauri_plugin_window_state::StateFlags;

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
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![AUTOSTART_FLAG]),
        ))
        // Remember where the user put the window. Visibility is deliberately not
        // remembered: the window can be hidden to the tray, and restoring that
        // would start the app with nothing on screen.
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(
                    StateFlags::SIZE
                        | StateFlags::POSITION
                        | StateFlags::MAXIMIZED
                        | StateFlags::FULLSCREEN,
                )
                .build(),
        )
        .setup(move |app| {
            // Before the first spawn, while nothing holds the directory open.
            dsh::promote(app.handle());

            // Ahead of the spawn below, so that the files dsh is about to read
            // are being read from a dozen threads while it reads them from one.
            warm::start(app.handle());

            // Started before the window rather than after it. dsh takes seconds
            // to boot and WebView2 takes its own; the two have nothing to say to
            // each other until the server is listening, so they may as well take
            // them at the same time.
            let started = server::start(app.handle());

            let preference = theme::preference();
            let splash = Splash::default();
            let visible = !std::env::args().any(|argument| argument == AUTOSTART_FLAG);

            let built: tauri::Result<WebviewWindow> = (|| {
                let window = build_window(
                    app.handle(),
                    setup_origin.clone(),
                    splash.clone(),
                    preference,
                    visible,
                )?;
                build_tray(app.handle())?;
                Ok(window)
            })();

            // The server is started before the window, so if there turns out to
            // be no window to serve it is this code's job to stop it: a setup
            // that returns an error never reaches the exit handler below.
            let window = match built {
                Ok(window) => window,
                Err(error) => {
                    if let Ok((mut child, _)) = started {
                        child.stop();
                    }
                    return Err(error.into());
                }
            };

            theme::paint(&window, preference);
            theme::follow(window.clone(), preference);

            match started {
                Ok((child, events)) => {
                    *setup_server.lock().unwrap() = Some(child);
                    watch(app.handle().clone(), window, setup_origin.clone(), splash, events);
                }
                Err(error) => {
                    splash.fail(
                        &window,
                        "启动 dsh 失败",
                        &format!(
                            "无法执行 dsh：{error}\n\n\
                             安装包内置的运行时不可用，PATH 中也没有找到 dsh\
                             （终端里执行 `dsh --version` 验证）。\
                             可用 DSH_BIN 环境变量指向 dsh 可执行文件的完整路径。"
                        ),
                    );
                    // Nothing is booting for them to get in the way of, and an
                    // update is one of the things that could fix this.
                    check_for_updates(app.handle());
                }
            }

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
        .center()
        .visible(visible)
        // The frame is themed at creation rather than repainted right after it,
        // and the loading page is told which theme it is being opened in.
        .theme(preference.window())
        .initialization_script(theme::script(preference))
        .on_page_load(move |webview, payload| {
            if payload.event() == PageLoadEvent::Finished {
                splash.flush(&webview);
            }
        })
        .on_navigation(move |url| {
            if is_ours(url, &origin) {
                return true;
            }
            // A link out of the app belongs in the user's browser, not in place
            // of the session they are working in.
            let _ = opener.opener().open_url(url.to_string(), None::<&str>);
            false
        })
        .build()?;

    // Closing the window parks the app in the tray instead of tearing the agent
    // down mid-task. Quitting for real goes through the tray menu.
    window.on_window_event(move |event| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            if let Some(window) = closer.get_webview_window("main") {
                let _ = window.hide();
            }
        }
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

/// Wait for the server, hand the window over to it, and only then go looking
/// for updates.
fn watch(
    app: tauri::AppHandle,
    window: WebviewWindow,
    origin: Origin,
    splash: Splash,
    events: Receiver<server::Event>,
) {
    std::thread::spawn(move || {
        serve(&window, &origin, &splash, &events);
        check_for_updates(&app);
    });
}

/// A check the boot finished with while the window was still hidden in the
/// tray. Picked up by [`reveal`].
static PENDING_CHECK: AtomicBool = AtomicBool::new(false);

/// Look for a newer app and a newer dsh, at most once per run.
///
/// Held back until the boot has settled — or, when it never does, until the
/// wait has gone on long enough to call slow. Both checks spawn processes and
/// reach the network, and on a first launch — where the whole of dsh is being
/// read off disk for the first time — that is contention for the one thing the
/// user is actually waiting on.
fn check_for_updates(app: &tauri::AppHandle) {
    // Both checks can end in a dialog, and a login-item launch is sitting in
    // the tray: a modal over whatever the user is doing, from an app they never
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

        dsh::check(app);
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
