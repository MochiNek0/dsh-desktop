//! The quick-ask box: a key pressed anywhere opens a small window, a line typed
//! into it starts a new session on that line, and the box goes away again.
//!
//! The window is this app's own page, `dist/ask.html`, and loads nothing of
//! dsh's. It is granted no IPC — `capabilities/` names only the main window —
//! and talks back the way every page here does, by navigating to
//! `dsh-window://`; its own navigation handler below is the only one that
//! hears it. What it sends is the text and which recent workspace to run it
//! in, and what that turns into is a call on the plugin in the main window,
//! `__dshSignals.ask`, which makes the session and sends the line — the same
//! two steps dsh's own "new session" and send button take.
//!
//! The box does not wait for the turn. It closes on Enter, and the turn's end
//! is announced the way any other is: by the notification [`crate::signal`]
//! raises, which can be replied to.
//!
//! ## Before dsh is up
//!
//! A line typed while dsh is still starting — or restarting under the window —
//! has nothing to go to yet. It waits here until the plugin says it can open a
//! folder (`folder-ready`, the two services this needs too) and goes then. One
//! at a time: a second line typed before the first went is the newer request.
//!
//! ## When it fails
//!
//! The plugin answers `ask-failed` with dsh's reason, and the box comes back
//! with the line still in it and the reason under it, so nothing typed is lost.

use std::sync::Mutex;

use tauri::{AppHandle, Manager, Url, WebviewUrl, WebviewWindowBuilder};

/// The window's label.
const LABEL: &str = "ask";

/// A line and the workspace it is for, by path.
#[derive(Clone)]
struct Line {
    path: String,
    text: String,
}

/// The line waiting for dsh to be up, if any.
static PENDING: Mutex<Option<Line>> = Mutex::new(None);

/// The line last handed over, for the box to come back with if it fails.
static LAST: Mutex<Option<Line>> = Mutex::new(None);

/// What the box was last told to show, for a page that loads after it was
/// told: the first time the window is built, its page is not there yet when
/// [`show`] hands it the payload, so the page asks again (`quick-ready`).
static SHOWN: Mutex<String> = Mutex::new(String::new());

/// The key was pressed: open the box, or close it if it is already open.
pub fn toggle(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(LABEL) {
        if window.is_visible().unwrap_or(false) {
            let _ = window.hide();
            return;
        }
    }
    show(app, "", "");
}

/// Put the box up, with `text` already in it and `error` under it.
fn show(app: &AppHandle, text: &str, error: &str) {
    let window = match app.get_webview_window(LABEL) {
        Some(window) => window,
        None => match build(app) {
            Ok(window) => window,
            Err(error) => {
                eprintln!("dsh-desktop: could not open the quick-ask box: {error}");
                return;
            }
        },
    };

    let _ = window.set_theme(crate::theme::preference().window());
    let _ = window.center();
    let call = payload(text, error);
    let _ = window.eval(&call);
    if let Ok(mut shown) = SHOWN.lock() {
        *shown = call;
    }
    let _ = window.show();
    let _ = window.set_focus();
}

/// What the page is handed each time it is shown: the recent workspaces to
/// choose from, what to put in the box, and every word it shows, in the
/// language of the moment.
fn payload(text: &str, error: &str) -> String {
    let workspaces: Vec<_> = crate::recent::list()
        .into_iter()
        .map(|workspace| serde_json::json!({ "title": workspace.title, "path": workspace.path }))
        .collect();
    let payload = serde_json::json!({
        "workspaces": workspaces,
        "text": text,
        "error": error,
        "words": {
            "placeholder": t!("让 dsh 做点什么…", "Ask dsh to do something…"),
            "hint": t!("Enter 发送 · Shift+Enter 换行 · Esc 关闭", "Enter to send · Shift+Enter for a new line · Esc to close"),
            "none": t!(
                "还没有工作区。先在 dsh 里打开一个文件夹，这里才有地方跑。",
                "No workspace yet. Open a folder in dsh first, so there is somewhere to run this."
            ),
            "in": t!("在", "In"),
        },
    });
    format!("window.__dshAsk && window.__dshAsk({payload})")
}

fn build(app: &AppHandle) -> tauri::Result<tauri::WebviewWindow> {
    let handle = app.clone();
    let window = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("ask.html".into()))
        .title("dsh")
        .inner_size(640.0, 132.0)
        .resizable(false)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        .theme(crate::theme::preference().window())
        .on_navigation(move |url| navigated(&handle, url))
        .build()?;

    // Away once it is not in front, the way a launcher is: the box is for one
    // line, and one left floating over other work is in the way.
    //
    // Not on the first `Focused(false)`. The first time the window is shown,
    // WebView2 creates its child window and hands it the focus, and Tauri
    // reports that hand-over as the window losing focus and getting it back —
    // so hiding on the event itself hid the box the moment it first appeared.
    // A moment later the window is asked again, and only a window that is
    // still not focused is put away.
    let away = window.clone();
    window.on_window_event(move |event| {
        if let tauri::WindowEvent::Focused(false) = event {
            let away = away.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(150));
                if !away.is_focused().unwrap_or(false) {
                    let _ = away.hide();
                }
            });
        }
    });
    Ok(window)
}

/// The box's own navigation: its page loading, and its two verbs. Nothing else
/// is somewhere it goes.
fn navigated(app: &AppHandle, url: &Url) -> bool {
    if url.scheme() == crate::controls::SCHEME {
        match url.host_str() {
            Some("quick-ask") => submitted(app, url),
            Some("quick-close") => hide(app),
            Some("quick-ready") => {
                let call = SHOWN.lock().map(|shown| shown.clone()).unwrap_or_default();
                if let Some(window) = app.get_webview_window(LABEL) {
                    let _ = window.eval(&call);
                }
            }
            _ => {}
        }
        return false;
    }
    // The page itself, under whichever spelling of the app's own origin this
    // platform serves it from.
    url.scheme() == "tauri" || url.host_str() == Some("tauri.localhost")
}

fn hide(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.hide();
    }
}

/// A line was sent from the box.
///
/// The workspace has to be one the box offered, which is one of the recent
/// workspaces: the URL is the page's to write, and a path from it is only taken
/// if this app already listed it.
fn submitted(app: &AppHandle, url: &Url) {
    let field = |name: &str| {
        url.query_pairs()
            .find_map(|(key, value)| (key == name).then(|| value.into_owned()))
            .unwrap_or_default()
    };
    let text = field("text").trim().to_string();
    let path = field("path");
    if text.is_empty() {
        return;
    }
    if !crate::recent::list().iter().any(|workspace| workspace.path == path) {
        return;
    }

    hide(app);
    let line = Line { path, text };
    if crate::folder::is_ready() {
        hand(app, line);
    } else if let Ok(mut pending) = PENDING.lock() {
        eprintln!("dsh-desktop: a quick ask waits for dsh");
        *pending = Some(line);
    }
}

/// The plugin can take a line now; see `folder-ready`.
pub fn ready(app: &AppHandle) {
    let line = PENDING.lock().ok().and_then(|mut pending| pending.take());
    if let Some(line) = line {
        hand(app, line);
    }
}

fn hand(app: &AppHandle, line: Line) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    eprintln!("dsh-desktop: a quick ask goes to {}", line.path);
    let _ = window.eval(call(&line.path, &line.text));
    if let Ok(mut last) = LAST.lock() {
        *last = Some(line);
    }
}

/// The plugin started the session; nothing to keep.
pub fn asked() {
    if let Ok(mut last) = LAST.lock() {
        *last = None;
    }
}

/// The plugin could not. The box comes back with the line in it.
pub fn failed(app: &AppHandle, reason: &str) {
    let Some(line) = LAST.lock().ok().and_then(|mut last| last.take()) else {
        return;
    };
    eprintln!("dsh-desktop: a quick ask failed: {reason}");
    let error = t!("没能开始：{}", "Could not start it: {}", reason);
    show(app, &line.text, &error);
}

/// The call [`hand`] makes. Both strings as JSON, the text being the user's
/// own; see `signal::sent` for why not `{:?}`.
fn call(path: &str, text: &str) -> String {
    let path = serde_json::to_string(path).expect("a string is always serializable");
    let text = serde_json::to_string(text).expect("a string is always serializable");
    format!(
        "window.__dshSignals ? window.__dshSignals.ask({path}, {text}) \
         : (window.location.href = 'dsh-window://signal?event=ask-failed&reason=not%20ready&n=' + Date.now());"
    )
}

#[cfg(test)]
mod tests {
    use super::{call, payload};

    #[test]
    fn hands_the_line_over_as_javascript_strings() {
        let call = call(r"C:\work\a b", "it's \"quoted\"\nand two lines");
        assert!(call.starts_with(r#"window.__dshSignals ? window.__dshSignals.ask("C:\\work\\a b", "it's \"quoted\"\nand two lines")"#), "{call}");
        assert!(call.contains("event=ask-failed"), "the fallback when no plugin is there");
    }

    #[test]
    fn the_payload_is_one_call_with_every_word_the_page_shows() {
        let payload = payload("draft", "");
        let json = payload
            .strip_prefix("window.__dshAsk && window.__dshAsk(")
            .and_then(|rest| rest.strip_suffix(')'))
            .expect("one call");
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(value["text"], "draft");
        for word in ["placeholder", "hint", "none", "in"] {
            assert!(value["words"][word].as_str().is_some_and(|w| !w.is_empty()), "{word}");
        }
        assert!(value["workspaces"].is_array());
    }
}
