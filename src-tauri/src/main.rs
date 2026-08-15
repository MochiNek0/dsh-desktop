// The release build is a GUI app: no console window behind it.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod dsh;
mod server;
mod update;

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::webview::PageLoadEvent;
use tauri::{Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_window_state::StateFlags;

/// The origin `dsh web` bound to, once it has. Navigation inside it stays in the
/// window; anything else is a link to the outside world.
type Origin = Arc<RwLock<Option<String>>>;

/// How long to sit on the boot message before telling the user it is slow.
const SLOW_BOOT: Duration = Duration::from_secs(20);

fn main() {
    let origin: Origin = Arc::new(RwLock::new(None));
    let server: Arc<Mutex<Option<server::Server>>> = Arc::new(Mutex::new(None));

    let setup_origin = origin.clone();
    let setup_server = server.clone();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
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
            let splash = Splash::default();
            let window = build_window(app.handle(), setup_origin.clone(), splash.clone())?;
            build_tray(app.handle())?;

            // A dev build's version never matches a release, so it would prompt
            // on every run.
            #[cfg(not(debug_assertions))]
            update::check_quietly(app.handle());

            // Before the first spawn, while nothing holds the directory open.
            dsh::promote(app.handle());
            dsh::check(app.handle());

            match server::start(app.handle()) {
                Ok((child, events)) => {
                    *setup_server.lock().unwrap() = Some(child);
                    watch(window, setup_origin.clone(), splash, events);
                }
                Err(error) => splash.fail(
                    &window,
                    "启动 dsh 失败",
                    &format!(
                        "无法执行 dsh：{error}\n\n\
                         安装包内置的运行时不可用，PATH 中也没有找到 dsh\
                         （终端里执行 `dsh --version` 验证）。\
                         可用 DSH_BIN 环境变量指向 dsh 可执行文件的完整路径。"
                    ),
                ),
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
/// dsh UI once the server is up.
fn build_window(
    app: &tauri::AppHandle,
    origin: Origin,
    splash: Splash,
) -> tauri::Result<WebviewWindow> {
    let opener = app.clone();
    let closer = app.clone();

    let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("dsh desktop")
        .inner_size(1360.0, 900.0)
        .min_inner_size(720.0, 520.0)
        .center()
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
    let check = MenuItem::with_id(app, "check", "检查更新…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出 dsh", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &check, &quit])?;

    let mut tray = TrayIconBuilder::new()
        .tooltip("dsh desktop")
        .menu(&menu)
        // Left click reveals the window; the menu belongs on the right button.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => reveal(app),
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

/// Bring the window back to the front, whatever it was hidden behind.
fn reveal(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
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

/// Wait for the server, then hand the window over to it.
fn watch(window: WebviewWindow, origin: Origin, splash: Splash, events: Receiver<server::Event>) {
    std::thread::spawn(move || loop {
        match events.recv_timeout(SLOW_BOOT) {
            Ok(server::Event::Ready(url)) => {
                let Ok(url) = Url::parse(&url) else {
                    splash.fail(
                        &window,
                        "启动 dsh 失败",
                        &format!("无法解析 dsh 输出的地址：{url}"),
                    );
                    return;
                };

                *origin.write().unwrap() = Some(url.origin().ascii_serialization());
                splash.status(&window, "正在打开界面…");

                let handle = window.app_handle().clone();
                let _ = handle.run_on_main_thread(move || {
                    if let Err(error) = window.navigate(url) {
                        splash.fail(&window, "打开界面失败", &error.to_string());
                    }
                });
                return;
            }
            Ok(server::Event::Failed(output)) => {
                splash.fail(
                    &window,
                    "dsh 已退出",
                    &if output.is_empty() {
                        "dsh 在开始服务前就退出了，且没有任何输出。".to_string()
                    } else {
                        format!("dsh 在开始服务前就退出了。它的输出：\n\n{output}")
                    },
                );
                return;
            }
            Err(RecvTimeoutError::Timeout) => splash.status(&window, "dsh 启动较慢，仍在等待…"),
            Err(RecvTimeoutError::Disconnected) => return,
        }
    });
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
