//! The quick-ask box: a key pressed anywhere opens a small window, a line typed
//! into it goes to dsh as a session's next turn, and the box goes away again.
//!
//! The window is this app's own page, `dist/ask.html`, and loads nothing of
//! dsh's. It is granted no IPC — `capabilities/` names only the main window —
//! and talks back the way every page here does, by navigating to
//! `dsh-window://`; its own navigation handler below is the only one that
//! hears it. What it sends is the text, the workspace to run it in and, if not
//! a new one, which of that workspace's sessions; what that turns into is a
//! call on the plugin in the main window, `__dshSignals.ask`, which makes the
//! session if it has to and sends the line — the same two steps dsh's own
//! "new session" and send button take.
//!
//! ## What it offers
//!
//! The recent workspaces, with the sessions of each that the plugin lists (see
//! `watchRecent` there), and any other folder through the system's own picker.
//! A workspace or session the page sends back is only taken if this app
//! offered it: the URL is the page's to write. A picked folder counts as
//! offered for the rest of the run, the picker being somewhere a script cannot
//! reach — the same reasoning as [`crate::plugins::pick_local`].
//!
//! ## The dropdown
//!
//! Each of the two choices opens a list that hangs below its chip, past the
//! box's own edge. That is a second window, `dist/ask-menu.html`: a page
//! cannot draw outside its window, and growing the box to make room for the
//! list was the box changing shape under the user. It cannot take the focus,
//! so the box keeps it and the keyboard stays there; the box says what to
//! list and where (`quick-menu`), which row the keys are on
//! (`quick-menu-active`), and a click on a row comes back here
//! (`menu-pick`) and goes on to the box as the choice it was.
//!
//! On Windows both windows are acrylic glass with rounded corners. Elsewhere
//! they are opaque: a see-through window on macOS takes Tauri's private-API
//! feature, and Linux has no blur behind a window to ask for.
//!
//! The box does not wait for the turn. It closes on Enter, and the turn's end
//! is announced the way any other is: by the notification [`crate::signal`]
//! raises.
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
//! with the line still in it, the same workspace and session chosen, and the
//! reason under it, so nothing typed is lost.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tauri::{
    AppHandle, LogicalSize, Manager, PhysicalPosition, Url, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};

/// The two windows' labels.
const LABEL: &str = "ask";
const MENU: &str = "ask-menu";

/// The box's size.
const WIDTH: f64 = 640.0;
const HEIGHT: f64 = 132.0;

/// The gap between a chip and the list hanging off it.
const GAP: f64 = 6.0;

/// How many sessions are kept for the box, over all the recent workspaces.
/// The plugin sends at most eight a workspace; see `SESSIONS` there.
pub const SESSIONS: usize = crate::recent::LIMIT * 8;

/// One session the box offers, under the workspace it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub path: String,
    pub id: String,
    pub title: String,
}

/// A line, the workspace it is for by path, and the session in it — empty for
/// a new one.
#[derive(Clone)]
struct Line {
    path: String,
    session: String,
    text: String,
}

/// The line waiting for dsh to be up, if any.
static PENDING: Mutex<Option<Line>> = Mutex::new(None);

/// The line last handed over, for the box to come back with if it fails.
static LAST: Mutex<Option<Line>> = Mutex::new(None);

/// What the box was last told to show, for a page that loads after it was
/// told: the first time the window is built, its page is not there yet when
/// [`show`] hands it the payload, so the page asks again (`quick-ready`). The
/// same for the dropdown's page (`menu-ready`).
static SHOWN: Mutex<String> = Mutex::new(String::new());
static MENU_SHOWN: Mutex<String> = Mutex::new(String::new());

/// The plugin's latest list of sessions; see [`set_sessions`].
static LISTED: Mutex<Vec<Session>> = Mutex::new(Vec::new());

/// Folders chosen in the box's own picker this run.
static PICKED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The picker is up. The box loses the focus to it, and must not be put away
/// for that; see [`build`].
static PICKING: AtomicBool = AtomicBool::new(false);

/// The plugin's latest sessions. Read the next time the box is shown.
pub fn set_sessions(list: Vec<Session>) {
    if let Ok(mut listed) = LISTED.lock() {
        *listed = list;
    }
}

/// The key was pressed: open the box, or close it if it is already open.
pub fn toggle(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(LABEL) {
        if window.is_visible().unwrap_or(false) {
            hide(app);
            return;
        }
    }
    show(app, "", "", None);
}

/// Put the box up, with `text` already in it, `error` under it, and `line`'s
/// workspace and session chosen, or else the latest workspace and a new
/// session.
fn show(app: &AppHandle, text: &str, error: &str, line: Option<&Line>) {
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

    hide_menu(app);
    let _ = window.set_theme(crate::theme::preference().window());
    let _ = window.center();
    let call = payload(text, error, line);
    let _ = window.eval(&call);
    if let Ok(mut shown) = SHOWN.lock() {
        *shown = call;
    }
    let _ = window.show();
    let _ = window.set_focus();
}

/// What the page is handed each time it is shown: the workspaces and their
/// sessions to choose from, which are chosen, what to put in the box, and every
/// word it shows, in the language of the moment.
fn payload(text: &str, error: &str, line: Option<&Line>) -> String {
    let sessions = LISTED.lock().map(|listed| listed.clone()).unwrap_or_default();
    let recent = crate::recent::list();
    let mut workspaces: Vec<_> = recent
        .iter()
        .map(|workspace| workspace_json(&workspace.title, &workspace.path, &sessions))
        .collect();
    // A picked folder the box is coming back to after a failure.
    if let Some(line) = line {
        if !recent.iter().any(|workspace| workspace.path == line.path) {
            workspaces.push(workspace_json(&folder_title(&line.path), &line.path, &sessions));
        }
    }
    let payload = serde_json::json!({
        "workspaces": workspaces,
        "path": line.map(|line| line.path.as_str()).unwrap_or(""),
        "session": line.map(|line| line.session.as_str()).unwrap_or(""),
        "text": text,
        "error": error,
        "glass": GLASS,
        "words": words(),
    });
    format!("window.__dshAsk && window.__dshAsk({payload})")
}

fn workspace_json(title: &str, path: &str, sessions: &[Session]) -> serde_json::Value {
    let sessions: Vec<_> = sessions
        .iter()
        .filter(|session| session.path == path)
        .map(|session| serde_json::json!({ "id": session.id, "title": session.title }))
        .collect();
    serde_json::json!({ "title": title, "path": path, "sessions": sessions })
}

fn words() -> serde_json::Value {
    serde_json::json!({
        "placeholder": t!("让 dsh 做点什么…", "Ask dsh to do something…"),
        "hint": t!("Enter 发送 · Shift+Enter 换行", "Enter to send · Shift+Enter for a new line"),
        "pick": t!("选择文件夹…", "Choose a folder…"),
        "other": t!("打开其他文件夹…", "Open another folder…"),
        "fresh": t!("新会话", "New session"),
        "send": t!("发送", "Send"),
        "workspace": t!("工作区", "Workspace"),
        "session": t!("会话", "Session"),
    })
}

/// What a picked folder is called in the box: its own name, or the whole path
/// for a drive root, which has none.
fn folder_title(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// Whether the two windows are glass; see the module docs.
const GLASS: bool = cfg!(windows);

/// The settings both windows share: frameless, on top, out of the taskbar,
/// and on Windows see-through behind acrylic.
fn builder<'a>(
    app: &'a AppHandle,
    label: &str,
    page: &str,
) -> WebviewWindowBuilder<'a, tauri::Wry, AppHandle> {
    let builder = WebviewWindowBuilder::new(app, label, WebviewUrl::App(page.into()))
        .title("dsh")
        .resizable(false)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        .theme(crate::theme::preference().window());
    #[cfg(windows)]
    let builder = builder.transparent(true).effects(
        tauri::window::EffectsBuilder::new()
            .effect(tauri::window::Effect::Acrylic)
            .build(),
    );
    builder
}

/// Round the corners the way Windows 11 rounds its own menus. A frameless
/// window is square unless it asks; Windows 10 has no such attribute, and the
/// call failing there leaves it square.
#[cfg(windows)]
fn round(window: &WebviewWindow) {
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    };
    let Ok(hwnd) = window.hwnd() else {
        return;
    };
    let preference = DWMWCP_ROUND;
    // SAFETY: a live handle, and a value of the size the attribute takes.
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &preference as *const _ as *const _,
            std::mem::size_of_val(&preference) as u32,
        )
    };
}

#[cfg(not(windows))]
fn round(_: &WebviewWindow) {}

fn build(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let handle = app.clone();
    let window = builder(app, LABEL, "ask.html")
        .inner_size(WIDTH, HEIGHT)
        .on_navigation(move |url| navigated(&handle, url))
        .build()?;
    round(&window);

    // Away once it is not in front, the way a launcher is: the box is for one
    // line, and one left floating over other work is in the way.
    //
    // Not on the first `Focused(false)`. The first time the window is shown,
    // WebView2 creates its child window and hands it the focus, and Tauri
    // reports that hand-over as the window losing focus and getting it back —
    // so hiding on the event itself hid the box the moment it first appeared.
    // A moment later the window is asked again, and only a window that is
    // still not focused is put away. Nor while the folder picker is up, nor
    // when it was its own dropdown that took the focus: the box is what both
    // of those answer to.
    let away = window.clone();
    let app = app.clone();
    window.on_window_event(move |event| {
        if let tauri::WindowEvent::Focused(false) = event {
            let away = away.clone();
            let app = app.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(150));
                let menu = app
                    .get_webview_window(MENU)
                    .and_then(|menu| menu.is_focused().ok())
                    .unwrap_or(false);
                if menu {
                    let _ = away.set_focus();
                    return;
                }
                if !away.is_focused().unwrap_or(false) && !PICKING.load(Ordering::SeqCst) {
                    hide(&app);
                }
            });
        }
    });
    Ok(window)
}

fn build_menu(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let handle = app.clone();
    let window = builder(app, MENU, "ask-menu.html")
        .inner_size(280.0, 120.0)
        .focusable(false)
        .on_navigation(move |url| menu_navigated(&handle, url))
        .build()?;
    round(&window);
    Ok(window)
}

/// The box's own navigation: its page loading, and its verbs. Nothing else is
/// somewhere it goes.
fn navigated(app: &AppHandle, url: &Url) -> bool {
    if url.scheme() == crate::controls::SCHEME {
        match url.host_str() {
            Some("quick-ask") => submitted(app, url),
            Some("quick-close") => hide(app),
            Some("quick-pick") => pick(app),
            Some("quick-menu") => open_menu(app, url),
            Some("quick-menu-active") => {
                let index: i64 = field(url, "i").parse().unwrap_or(-1);
                if let Some(menu) = app.get_webview_window(MENU) {
                    let _ = menu.eval(format!("window.__dshMenuActive && window.__dshMenuActive({index})"));
                }
            }
            Some("quick-menu-close") => hide_menu(app),
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
    is_own_page(url)
}

/// The dropdown's navigation: its page loading, and a row picked.
fn menu_navigated(app: &AppHandle, url: &Url) -> bool {
    if url.scheme() == crate::controls::SCHEME {
        match url.host_str() {
            Some("menu-pick") => {
                let index: i64 = field(url, "i").parse().unwrap_or(-1);
                hide_menu(app);
                if let Some(window) = app.get_webview_window(LABEL) {
                    let _ = window.eval(format!("window.__dshAskChose && window.__dshAskChose({index})"));
                    let _ = window.set_focus();
                }
            }
            Some("menu-ready") => {
                let call = MENU_SHOWN.lock().map(|shown| shown.clone()).unwrap_or_default();
                if let Some(menu) = app.get_webview_window(MENU) {
                    let _ = menu.eval(&call);
                }
            }
            _ => {}
        }
        return false;
    }
    is_own_page(url)
}

/// The page itself, under whichever spelling of the app's own origin this
/// platform serves it from.
fn is_own_page(url: &Url) -> bool {
    url.scheme() == "tauri" || url.host_str() == Some("tauri.localhost")
}

fn field(url: &Url, name: &str) -> String {
    url.query_pairs()
        .find_map(|(key, value)| (key == name).then(|| value.into_owned()))
        .unwrap_or_default()
}

/// Away, the box and its dropdown both.
fn hide(app: &AppHandle) {
    hide_menu(app);
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.hide();
    }
}

fn hide_menu(app: &AppHandle) {
    if let Some(menu) = app.get_webview_window(MENU) {
        let _ = menu.hide();
    }
}

/// Where the dropdown goes and how big it is, in the box's own CSS pixels:
/// the chip's left edge, its top and bottom, and the list's size.
#[derive(Debug, PartialEq)]
struct Anchor {
    x: f64,
    top: f64,
    bottom: f64,
    width: f64,
    height: f64,
}

impl Anchor {
    /// Read off the page's URL, and held to sizes a list can be whatever the
    /// URL says.
    fn read(url: &Url) -> Option<Anchor> {
        let number = |name: &str| field(url, name).parse::<f64>().ok().filter(|n| n.is_finite());
        Some(Anchor {
            x: number("x")?.clamp(0.0, WIDTH),
            top: number("top")?.clamp(0.0, HEIGHT),
            bottom: number("bottom")?.clamp(0.0, HEIGHT),
            width: number("w")?.clamp(160.0, 520.0),
            height: number("h")?.clamp(40.0, 440.0),
        })
    }

    /// Where the list's top-left corner goes on screen: under the chip, or
    /// above it when there is not room below. `origin` is the box's content
    /// on screen, `floor` the bottom of the work area, both physical.
    fn place(&self, origin: (i32, i32), scale: f64, floor: i32) -> (i32, i32) {
        let x = origin.0 + (self.x * scale).round() as i32;
        let below = origin.1 + ((self.bottom + GAP) * scale).round() as i32;
        if below + (self.height * scale).round() as i32 <= floor {
            return (x, below);
        }
        (x, origin.1 + ((self.top - GAP - self.height) * scale).round() as i32)
    }
}

/// The page opened one of its lists: put the dropdown under that chip.
fn open_menu(app: &AppHandle, url: &Url) {
    let Some(anchor) = Anchor::read(url) else {
        return;
    };
    // Re-read, so that what is evaluated in the dropdown is JSON this side
    // wrote, whatever the URL held.
    let Ok(items) = serde_json::from_str::<serde_json::Value>(&field(url, "items")) else {
        return;
    };
    if !items.is_array() {
        return;
    }
    let active: i64 = field(url, "active").parse().unwrap_or(0);
    let Some(window) = app.get_webview_window(LABEL) else {
        return;
    };
    let menu = match app.get_webview_window(MENU) {
        Some(menu) => menu,
        None => match build_menu(app) {
            Ok(menu) => menu,
            Err(error) => {
                eprintln!("dsh-desktop: could not open the quick-ask list: {error}");
                return;
            }
        },
    };

    let scale = window.scale_factor().unwrap_or(1.0);
    let origin = window
        .inner_position()
        .map(|position| (position.x, position.y))
        .unwrap_or_default();
    let floor = window
        .current_monitor()
        .ok()
        .flatten()
        .map(|monitor| {
            let area = monitor.work_area();
            area.position.y + area.size.height as i32
        })
        .unwrap_or(i32::MAX);
    let (x, y) = anchor.place(origin, scale, floor);

    let call = format!(
        "window.__dshMenu && window.__dshMenu({})",
        serde_json::json!({ "items": items, "active": active, "glass": GLASS })
    );
    let _ = menu.set_theme(crate::theme::preference().window());
    let _ = menu.set_size(LogicalSize::new(anchor.width, anchor.height));
    let _ = menu.set_position(PhysicalPosition::new(x, y));
    let _ = menu.eval(&call);
    if let Ok(mut shown) = MENU_SHOWN.lock() {
        *shown = call;
    }
    let _ = menu.show();
    // It cannot take the focus, but showing a window can still hand it over
    // on some systems; the box is where the keys go.
    let _ = window.set_focus();
}

/// "Open another folder…": the system's folder picker, over the box.
///
/// On the main thread, where every platform wants its dialogs, but queued
/// behind the navigation callback this came from rather than inside it. What
/// was picked is handed to the page, which adds it to the list and chooses it;
/// the text in the box stays where it was.
fn pick(app: &AppHandle) {
    hide_menu(app);
    PICKING.store(true, Ordering::SeqCst);
    let handle = app.clone();
    let queued = app.run_on_main_thread(move || {
        let mut dialog = rfd::FileDialog::new().set_title(t!("选择一个文件夹", "Choose a folder"));
        if let Some(window) = handle.get_webview_window(LABEL) {
            dialog = dialog.set_parent(&window);
        }
        let picked = dialog.pick_folder().filter(|path| path.is_dir());
        PICKING.store(false, Ordering::SeqCst);

        let Some(window) = handle.get_webview_window(LABEL) else {
            return;
        };
        let _ = window.show();
        let _ = window.set_focus();
        let Some(path) = picked else {
            return;
        };
        let path = path.to_string_lossy().into_owned();
        if let Ok(mut known) = PICKED.lock() {
            if !known.contains(&path) {
                known.push(path.clone());
            }
        }
        let _ = window.eval(picked_call(&path));
    });
    if queued.is_err() {
        PICKING.store(false, Ordering::SeqCst);
    }
}

/// The call [`pick`] makes.
fn picked_call(path: &str) -> String {
    let workspace = serde_json::json!({ "title": folder_title(path), "path": path, "sessions": [] });
    format!("window.__dshAskPicked && window.__dshAskPicked({workspace})")
}

/// Whether the box offered this workspace and, unless it is a new one, this
/// session in it.
fn offered(path: &str, session: &str) -> bool {
    let known = crate::recent::list().iter().any(|workspace| workspace.path == path)
        || PICKED.lock().map(|picked| picked.iter().any(|p| p == path)).unwrap_or(false);
    if !known {
        return false;
    }
    session.is_empty()
        || LISTED
            .lock()
            .map(|listed| listed.iter().any(|s| s.path == path && s.id == session))
            .unwrap_or(false)
}

/// A line was sent from the box.
fn submitted(app: &AppHandle, url: &Url) {
    let text = field(url, "text").trim().to_string();
    let path = field(url, "path");
    let session = field(url, "session");
    if text.is_empty() || !offered(&path, &session) {
        return;
    }

    hide(app);
    let line = Line { path, session, text };
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
    if line.session.is_empty() {
        eprintln!("dsh-desktop: a quick ask goes to {}", line.path);
    } else {
        eprintln!("dsh-desktop: a quick ask goes to {} in {}", line.session, line.path);
    }
    let _ = window.eval(call(&line.path, &line.session, &line.text));
    if let Ok(mut last) = LAST.lock() {
        *last = Some(line);
    }
}

/// The plugin sent the line; nothing to keep.
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
    show(app, &line.text, &error, Some(&line));
}

/// The call [`hand`] makes. Every string as JSON, the text being the user's
/// own; see `signal::drafted` for why not `{:?}`.
fn call(path: &str, session: &str, text: &str) -> String {
    let json = |text: &str| serde_json::to_string(text).expect("a string is always serializable");
    let (path, session, text) = (json(path), json(session), json(text));
    format!(
        "window.__dshSignals ? window.__dshSignals.ask({path}, {text}, {session}) \
         : (window.location.href = 'dsh-window://signal?event=ask-failed&reason=not%20ready&n=' + Date.now());"
    )
}

#[cfg(test)]
mod tests {
    use super::{call, folder_title, payload, picked_call, Anchor, Line};
    use tauri::Url;

    #[test]
    fn hands_the_line_over_as_javascript_strings() {
        let call = call(r"C:\work\a b", "", "it's \"quoted\"\nand two lines");
        assert!(call.starts_with(r#"window.__dshSignals ? window.__dshSignals.ask("C:\\work\\a b", "it's \"quoted\"\nand two lines", "")"#), "{call}");
        assert!(call.contains("event=ask-failed"), "the fallback when no plugin is there");

        let call = super::call("/w", "session-1", "x");
        assert!(call.contains(r#"ask("/w", "x", "session-1")"#), "{call}");
    }

    fn read(call: &str, prefix: &str) -> serde_json::Value {
        let json = call
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix(')'))
            .expect("one call");
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn the_payload_is_one_call_with_every_word_the_page_shows() {
        let value = read(&payload("draft", "", None), "window.__dshAsk && window.__dshAsk(");
        assert_eq!(value["text"], "draft");
        assert_eq!(value["path"], "", "the page picks the latest workspace");
        assert_eq!(value["glass"], cfg!(windows));
        for word in ["placeholder", "hint", "pick", "other", "fresh", "send", "workspace", "session"] {
            assert!(value["words"][word].as_str().is_some_and(|w| !w.is_empty()), "{word}");
        }
        assert!(value["workspaces"].is_array());
    }

    /// Coming back after a failure, to a folder that was picked rather than
    /// listed: it is in the list again, and chosen.
    #[test]
    fn a_failed_line_comes_back_with_its_workspace_and_session() {
        let line = Line {
            path: "/elsewhere/picked".into(),
            session: "session-1".into(),
            text: "again".into(),
        };
        let value = read(&payload("again", "no", Some(&line)), "window.__dshAsk && window.__dshAsk(");
        assert_eq!(value["path"], "/elsewhere/picked");
        assert_eq!(value["session"], "session-1");
        let last = value["workspaces"].as_array().unwrap().last().unwrap().clone();
        assert_eq!(last["title"], "picked");
        assert_eq!(last["path"], "/elsewhere/picked");
    }

    #[test]
    fn a_picked_folder_is_called_by_its_name() {
        assert_eq!(folder_title("/home/me/work"), "work");
        assert_eq!(folder_title("/"), "/", "a root has no name of its own");
        let value = read(&picked_call("/a/b c"), "window.__dshAskPicked && window.__dshAskPicked(");
        assert_eq!(value["title"], "b c");
        assert_eq!(value["path"], "/a/b c");
        assert_eq!(value["sessions"], serde_json::json!([]));
    }

    /// The dropdown hangs under its chip, flips above it at the bottom of the
    /// screen, and takes no size from the URL that a list could not be.
    #[test]
    fn the_dropdown_goes_under_its_chip_or_above_it() {
        let url = |query: &str| Url::parse(&format!("dsh-window://quick-menu?{query}")).unwrap();
        let anchor = Anchor::read(&url("x=16&top=90&bottom=120&w=300&h=200")).unwrap();
        // At 150%, from a box whose content starts at (100, 200).
        assert_eq!(anchor.place((100, 200), 1.5, 2000), (124, 389));
        // Not enough room below: above the chip instead.
        assert_eq!(anchor.place((100, 200), 1.5, 600), (124, 26));

        let wild = Anchor::read(&url("x=-5&top=0&bottom=9999&w=1&h=100000")).unwrap();
        assert_eq!((wild.x, wild.bottom, wild.width, wild.height), (0.0, 132.0, 160.0, 440.0));
        assert_eq!(Anchor::read(&url("x=1&top=2&bottom=3&w=NaN&h=4")), None);
        assert_eq!(Anchor::read(&url("top=2&bottom=3&w=4&h=4")), None);
    }
}
