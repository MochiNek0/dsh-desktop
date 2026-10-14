//! A folder handed to the app from outside: `dsh-desktop <path>`, the "用 dsh
//! 打开" entry Explorer and Linux file managers show on a folder, and on macOS
//! a folder dropped on the Dock icon or opened from Finder's "Open With" (those
//! arrive as `RunEvent::Opened`, not as arguments).
//!
//! What the user means by it is "open this folder in dsh", which in dsh's own
//! terms is a workspace for that directory and a session in it. Neither can be
//! made from out here — both are dsh client services, reachable only from a
//! client plugin's `ctx` — so the work is done by `openFolder` in the plugin
//! (`plugin/lib/client.js`), and this module's part is getting the path to it
//! at a moment it can take one.
//!
//! ## Waiting for the plugin
//!
//! A path can arrive before there is anything to hand it to: with the launch,
//! while the loading page is still up, or while dsh restarts under the window.
//! So it is held here until the plugin says `folder-ready`, which it does once
//! the two services it needs are up on the page now loaded. Every navigation
//! the window starts takes that back, because the door it named goes with the
//! document. One path is held at a time — a second one is a newer request, and
//! opening the older one after it would put the user somewhere they no longer
//! asked to be.
//!
//! ## Without the plugin
//!
//! Then nothing can open it, and saying so is the whole of the job: a click
//! that brings the window up and does nothing else reads as broken. The way
//! back is the one the notify row already points at — "load plugins again" for
//! a launch that set it aside, the plugin panel for one that never had it —
//! and the path is kept, so the dsh that comes back up with the plugin opens
//! it. A dismissed question drops it. Asked once per path, not once per page
//! that path waits through.
//!
//! That check waits for dsh's own page. On the loading page the plugin can be
//! absent only because this launch has not put it in yet — see
//! [`crate::plugins::adopt`] — and asking then would be asking about nothing.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tauri::AppHandle;

struct State {
    /// The plugin said `folder-ready` on the page now loaded.
    ready: bool,
    /// The page now loaded is dsh's, not the loading page.
    on_dsh: bool,
    pending: Option<PathBuf>,
    /// The user has already been told about the missing plugin for this path.
    /// Once per request: the way back runs through restarts and the plugin
    /// panel, and each of those loads a page that would otherwise ask again.
    asked: bool,
}

static STATE: Mutex<State> = Mutex::new(State {
    ready: false,
    on_dsh: false,
    pending: None,
    asked: false,
});

/// A path is with the plugin and has not been answered yet.
///
/// Gates the failure note: `folder-failed` comes down the same channel anything
/// in the window can navigate to, and a note raised for one nobody asked for
/// would be a dialog any script could put up with text of its choosing.
static IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// The folder a command line names, or `None` when it names none.
///
/// The first argument after the program that is not a flag — `--autostart` is
/// this app's own, and macOS has been known to pass a `-psn_…` — resolved
/// against the directory the command was run from. A file stands for the
/// folder it is in, which is what dropping one onto the app means. Something
/// that does not exist is not a request to create it.
pub fn requested(args: &[String], cwd: &Path) -> Option<PathBuf> {
    let arg = args.iter().skip(1).find(|arg| !arg.starts_with('-'))?;
    folder_of(&cwd.join(arg))
}

/// The folder `path` stands for: itself, or the one a file is in.
pub fn folder_of(path: &Path) -> Option<PathBuf> {
    let path = std::path::absolute(path).ok()?;
    if path.is_dir() {
        Some(path)
    } else if path.is_file() {
        path.parent().map(Path::to_path_buf)
    } else {
        None
    }
}

/// Open `dir` in dsh, now or as soon as it can be.
pub fn open(app: &AppHandle, dir: PathBuf) {
    eprintln!("dsh-desktop: asked to open {}", dir.display());
    let Ok(mut state) = STATE.lock() else { return };
    if state.ready {
        drop(state);
        hand(app, &dir);
        return;
    }
    state.pending = Some(dir);
    let ask = state.on_dsh && !crate::plugins::signalling(app);
    state.asked = ask;
    drop(state);
    if ask {
        missing(app);
    }
}

/// A navigation has started: whatever the plugin said was about the document
/// being left.
pub fn page_started() {
    if let Ok(mut state) = STATE.lock() {
        state.ready = false;
        state.on_dsh = false;
    }
}

/// A page finished loading. `on_dsh` is whether it is dsh's own.
pub fn page_finished(app: &AppHandle, on_dsh: bool) {
    let Ok(mut state) = STATE.lock() else { return };
    state.on_dsh = on_dsh;
    let ask = on_dsh
        && !state.ready
        && !state.asked
        && state.pending.is_some()
        && !crate::plugins::signalling(app);
    if ask {
        state.asked = true;
    }
    drop(state);
    if ask {
        missing(app);
    }
}

/// The plugin can take a folder now; see `folder-ready` in its client.js.
pub fn ready(app: &AppHandle) {
    let Ok(mut state) = STATE.lock() else { return };
    state.ready = true;
    let pending = state.pending.take();
    drop(state);
    if let Some(dir) = pending {
        hand(app, &dir);
    }
}

/// dsh took the folder. Whatever happens from here, dsh says itself.
pub fn opened() {
    IN_FLIGHT.store(false, Ordering::SeqCst);
}

/// dsh would not take the folder: it is not a directory dsh can adopt, or the
/// call never got that far.
pub fn failed(app: &AppHandle, reason: &str) {
    if !IN_FLIGHT.swap(false, Ordering::SeqCst) {
        eprintln!("dsh-desktop: ignoring a folder failure nobody asked for");
        return;
    }
    crate::dsh::note(
        app,
        t!("没能打开文件夹", "Could not open the folder"),
        &t!(
            "dsh 没有接受这个文件夹：{}",
            "dsh did not take the folder: {}",
            reason
        ),
    );
}

fn hand(app: &AppHandle, dir: &Path) {
    IN_FLIGHT.store(true, Ordering::SeqCst);
    crate::controls::eval(app, &call(dir));
}

/// The call [`hand`] makes, as a string so it can be read in a test.
///
/// Through `serde_json` rather than `{:?}`: a path is whatever the file system
/// allowed, and Rust's debug escapes (`\u{…}`) are not JavaScript's.
fn call(dir: &Path) -> String {
    let path =
        serde_json::to_string(&dir.to_string_lossy()).expect("a string is always serializable");
    format!("window.__dshSignals && window.__dshSignals.openFolder({path});")
}

/// Say that the plugin is not there, and offer the way back.
fn missing(app: &AppHandle) {
    let set_aside = crate::plugins::signal_set_aside(app);
    let (body, label) = if set_aside {
        (
            t!(
                "打开文件夹要用到「会话信号」插件，它这次被搁置了。重新加载插件后会接着打开。",
                "Opening a folder needs the Session signals plugin, which is set aside this launch. Load the plugins again and the folder opens after."
            ),
            t!("重新加载插件", "Load plugins again"),
        )
    } else {
        (
            t!(
                "打开文件夹要用到「会话信号」插件，现在没有安装。在插件面板里装上后会接着打开。",
                "Opening a folder needs the Session signals plugin, which is not installed. Install it from the plugin panel and the folder opens after."
            ),
            t!("打开插件面板", "Open the plugin panel"),
        )
    };

    crate::dialog::ask(
        app,
        crate::dialog::Ask {
            title: t!("没能打开文件夹", "Could not open the folder").to_string(),
            body: body.to_string(),
            choices: vec![
                crate::dialog::Choice::new("cancel", t!("取消", "Cancel")),
                crate::dialog::Choice::primary("fix", label),
            ],
            answered: Box::new(move |app, id| {
                if id != "fix" {
                    if let Ok(mut state) = STATE.lock() {
                        state.pending = None;
                    }
                } else if set_aside {
                    crate::safe_off(app);
                } else {
                    crate::open_plugins(app);
                }
            }),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn a_folder_on_the_command_line_is_resolved_against_the_cwd() {
        let root = std::env::temp_dir().join("dsh-desktop-folder-test-a");
        std::fs::create_dir_all(root.join("project")).unwrap();
        assert_eq!(
            requested(&args(&["dsh-desktop", "project"]), &root),
            Some(std::path::absolute(root.join("project")).unwrap())
        );
        assert_eq!(
            requested(&args(&["dsh-desktop", "."]), &root),
            Some(std::path::absolute(root.join(".")).unwrap())
        );
    }

    #[test]
    fn a_file_stands_for_its_folder() {
        let root = std::env::temp_dir().join("dsh-desktop-folder-test-b");
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("notes.txt");
        std::fs::write(&file, "").unwrap();
        assert_eq!(
            requested(
                &args(&["dsh-desktop", file.to_str().unwrap()]),
                Path::new("/")
            ),
            Some(std::path::absolute(&root).unwrap())
        );
    }

    /// The form Explorer's "用 dsh 打开" passes; see `DshFolderMenu` in
    /// `installer-hooks.nsh`.
    #[cfg(windows)]
    #[test]
    fn the_folder_menu_argument_names_the_folder_itself() {
        let root = std::env::temp_dir().join("dsh-desktop-folder-test-c");
        std::fs::create_dir_all(&root).unwrap();
        let arg = format!(r"{}\.", root.display());
        assert_eq!(
            requested(&args(&["dsh-desktop", &arg]), Path::new("/")),
            Some(std::path::absolute(&root).unwrap())
        );
        assert_eq!(
            requested(&args(&["dsh-desktop", r"C:\\."]), Path::new("/")),
            Some(PathBuf::from(r"C:\"))
        );
    }

    #[test]
    fn flags_and_missing_paths_open_nothing() {
        let cwd = std::env::temp_dir();
        assert_eq!(requested(&args(&["dsh-desktop"]), &cwd), None);
        assert_eq!(
            requested(&args(&["dsh-desktop", "--autostart"]), &cwd),
            None
        );
        assert_eq!(
            requested(&args(&["dsh-desktop", "no-such-folder-dsh-desktop"]), &cwd),
            None
        );
    }

    #[test]
    fn the_path_goes_into_the_call_as_a_javascript_string() {
        assert_eq!(
            call(Path::new(r#"C:\work\"quoted"\项目"#)),
            r#"window.__dshSignals && window.__dshSignals.openFolder("C:\\work\\\"quoted\"\\项目");"#
        );
    }
}
