// The release build is a GUI app: no console window behind it.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod server;

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use tauri::webview::PageLoadEvent;
use tauri::{Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;

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
        .setup(move |app| {
            let splash = Splash::default();
            let window = build_window(app.handle(), setup_origin.clone(), splash.clone())?;

            match server::start() {
                Ok((child, events)) => {
                    *setup_server.lock().unwrap() = Some(child);
                    watch(window, setup_origin.clone(), splash, events);
                }
                Err(error) => splash.fail(
                    &window,
                    "启动 dsh 失败",
                    &format!(
                        "无法执行 dsh：{error}\n\n\
                         请确认 dsh 已安装并在 PATH 中（终端里执行 `dsh --version` 验证），\
                         或用 DSH_BIN 环境变量指向 dsh 可执行文件的完整路径。"
                    ),
                ),
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build the dsh desktop app");

    app.run(move |_handle, event| {
        if let tauri::RunEvent::Exit = event {
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

    WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
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
        .build()
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
