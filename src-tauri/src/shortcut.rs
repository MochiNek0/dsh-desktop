//! A key that brings the window up from anywhere, and puts it away again.
//!
//! Global means registered with the operating system: it works whichever app
//! has the focus, and for as long as this app runs no other app gets that key.
//! That is why the default is Ctrl+Alt+D and not Ctrl+Shift+D, which Chrome
//! and VS Code both use, and why a key chosen on the settings card needs two
//! modifiers (see [`accept`]).
//!
//! Stored as `shortcut` in `desktop.json`: absent is [`DEFAULT`], `""` is off,
//! anything else is what the card recorded. A key another program already
//! holds cannot be registered. At launch that is said in a notification, since
//! nobody is looking at the card then; either way the card's row says it.
//!
//! The plugin is used from Rust only. Its commands are not granted to the
//! webview, and the one way the page reaches this module is the `shortcut`
//! verb — see [`crate::controls`].

use std::str::FromStr;
use std::sync::Mutex;

use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

/// What a launch registers when nothing has been chosen.
const DEFAULT: &str = "Ctrl+Alt+D";

/// Why the key asked for is not the one in effect.
enum Failure {
    /// Another program registered it first.
    Taken,
    /// Fewer than two modifiers; see [`accept`].
    TooFew,
    /// Anything else the OS said.
    Refused(String),
}

#[derive(Default)]
struct State {
    /// The key registered now, if any.
    active: Option<Shortcut>,
    /// The last key asked for that did not take. Cleared by one that does.
    failed: Option<(Shortcut, Failure)>,
}

static STATE: Mutex<State> = Mutex::new(State {
    active: None,
    failed: None,
});

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The plugin, with the one handler every key it registers goes to.
pub(crate) fn plugin() -> TauriPlugin<Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, _shortcut, event| {
            if event.state() == ShortcutState::Pressed {
                toggle(app);
            }
        })
        .build()
}

/// Up and in front if it is anything less; away to the tray if it already is.
fn toggle(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let in_front = window.is_visible().unwrap_or(false)
        && !window.is_minimized().unwrap_or(false)
        && window.is_focused().unwrap_or(false);
    if in_front {
        crate::park(&window);
    } else {
        crate::reveal(app);
    }
}

/// Register the stored key, once, at launch.
///
/// A file whose key does not parse is left as it is and registers nothing:
/// it was edited by hand, and the card shows the key as off until a new one
/// is recorded over it. The two-modifier rule is not applied here — it guards
/// the verb the page can send, not a file the user wrote.
pub(crate) fn init(app: &AppHandle) {
    let keys = crate::settings::shortcut(app).unwrap_or_else(|| DEFAULT.to_string());
    if keys.is_empty() {
        return;
    }
    let Ok(shortcut) = Shortcut::from_str(&keys) else {
        eprintln!("dsh-desktop: ignoring the unreadable shortcut {keys:?}");
        return;
    };

    match app.global_shortcut().register(shortcut) {
        Ok(()) => state().active = Some(shortcut),
        Err(error) => {
            let failure = failure(&error);
            // The same two gates every notification passes; see
            // `notify::show`. Not the third — whether the window has the
            // focus — because at launch it usually does, and then this would
            // never be seen.
            if crate::plugins::signalling(app) && crate::settings::notifications(app) {
                crate::toast::raise(
                    app,
                    t!("全局快捷键没有生效", "The global shortcut is not set").to_string(),
                    format!(
                        "{}{}{}",
                        explain(&shortcut, &failure),
                        if crate::i18n::chinese() { "" } else { " " },
                        t!("到设置里换一个组合。", "Pick another one in Settings.")
                    ),
                    None,
                );
            }
            state().failed = Some((shortcut, failure));
        }
    }
}

/// The key the settings card recorded, or `""` for off.
///
/// The key now in effect stays in effect until its replacement has
/// registered, so a key that is taken costs nothing but the message saying so.
pub(crate) fn change(app: &AppHandle, keys: &str) {
    let wanted = if keys.is_empty() {
        None
    } else {
        match accept(keys) {
            Ok(shortcut) => Some(shortcut),
            Err(Some(shortcut)) => {
                state().failed = Some((shortcut, Failure::TooFew));
                return sync(app);
            }
            // Nothing the card records fails to parse; something else sent it.
            Err(None) => return,
        }
    };

    let shortcuts = app.global_shortcut();
    let mut state = state();
    if wanted != state.active {
        if let Some(shortcut) = wanted {
            if let Err(error) = shortcuts.register(shortcut) {
                state.failed = Some((shortcut, failure(&error)));
                drop(state);
                return sync(app);
            }
        }
        if let Some(old) = state.active.take() {
            let _ = shortcuts.unregister(old);
        }
        state.active = wanted;
    }
    state.failed = None;
    drop(state);

    crate::settings::set_shortcut(app, &wanted.map(Shortcut::into_string).unwrap_or_default());
    sync(app);
}

/// A key the card may set: one that parses, with at least two of Ctrl, Alt,
/// Shift and the Windows key. One modifier is where every app keeps its own
/// shortcuts — Ctrl+K, Alt+F — and a global key takes its combination away
/// from all of them, so the card does not offer that. `Err(Some)` carries the
/// key that was refused, for the message.
fn accept(keys: &str) -> Result<Shortcut, Option<Shortcut>> {
    let shortcut = Shortcut::from_str(keys).map_err(|_| None)?;
    let held = [
        Modifiers::CONTROL,
        Modifiers::ALT,
        Modifiers::SHIFT,
        Modifiers::SUPER,
    ]
    .into_iter()
    .filter(|modifier| shortcut.mods.contains(*modifier))
    .count();
    if held < 2 {
        return Err(Some(shortcut));
    }
    Ok(shortcut)
}

/// The plugin hands its errors back as text, so the one worth telling apart
/// is told apart by its text: global-hotkey's `AlreadyRegistered`, which is
/// what Windows answers for a key another program holds.
fn failure(error: &tauri_plugin_global_shortcut::Error) -> Failure {
    let text = error.to_string();
    if text.contains("already registered") {
        Failure::Taken
    } else {
        Failure::Refused(text)
    }
}

fn explain(shortcut: &Shortcut, failure: &Failure) -> String {
    let keys = label(shortcut);
    match failure {
        Failure::Taken => t!(
            "{} 已被其他程序占用。",
            "{} is already taken by another program.",
            keys
        ),
        Failure::TooFew => t!(
            "{} 不行：要同时按住 Ctrl、Alt、Shift、Win 中的至少两个，免得抢走其他软件的快捷键。",
            "{} will not do: hold at least two of Ctrl, Alt, Shift and Win, so it does not take a shortcut another app uses.",
            keys
        ),
        Failure::Refused(error) => t!(
            "{} 注册不上：{}",
            "{} could not be registered: {}",
            keys,
            error
        ),
    }
}

/// How a key is written on the card: `Ctrl+Alt+D`.
fn label(shortcut: &Shortcut) -> String {
    let mut parts = Vec::new();
    if shortcut.mods.contains(Modifiers::CONTROL) {
        parts.push("Ctrl");
    }
    if shortcut.mods.contains(Modifiers::ALT) {
        parts.push(if cfg!(target_os = "macos") {
            "Option"
        } else {
            "Alt"
        });
    }
    if shortcut.mods.contains(Modifiers::SHIFT) {
        parts.push("Shift");
    }
    if shortcut.mods.contains(Modifiers::SUPER) {
        parts.push(if cfg!(target_os = "macos") {
            "Cmd"
        } else if cfg!(windows) {
            "Win"
        } else {
            "Super"
        });
    }
    let code = shortcut.key.to_string();
    let key = code
        .strip_prefix("Key")
        .or_else(|| code.strip_prefix("Digit"))
        .unwrap_or(&code);
    parts.push(key);
    parts.join("+")
}

/// The row's second line while nothing has gone wrong.
pub(crate) fn note() -> &'static str {
    t!(
        "在任何地方按下它都能显示或隐藏窗口。点右边的按键录一个新的，录的时候按 Backspace 就是关掉。",
        "Shows or hides the window from anywhere. Click the key on the right to record a new one; Backspace while recording turns it off."
    )
}

/// Tell the card's row what the key is and whether it took.
///
/// Pushed on every page load, after every change, and after a language switch:
/// every string in it is written in the language of the moment.
pub(crate) fn sync(app: &AppHandle) {
    let state = state();
    let keys = state.active.as_ref().map(label).unwrap_or_default();
    let problem = state
        .failed
        .as_ref()
        .map(|(shortcut, failure)| explain(shortcut, failure));
    drop(state);

    let warn = problem.is_some();
    let note = problem.unwrap_or_else(|| note().to_string());
    let words = serde_json::json!({
        "off": t!("未设置", "Off"),
        "press": t!("按下组合键…", "Press keys…"),
    });
    let keys = serde_json::to_string(&keys).expect("a string is always serializable");
    let note = serde_json::to_string(&note).expect("a string is always serializable");
    crate::controls::eval(
        app,
        &format!("window.__dshShortcut && window.__dshShortcut({keys}, {note}, {warn}, {words})"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_two_modifiers_and_d() {
        let shortcut = accept(DEFAULT)
            .ok()
            .expect("the default passes its own rule");
        assert_eq!(label(&shortcut), "Ctrl+Alt+D");
    }

    #[test]
    fn what_the_card_sends_is_what_it_shows() {
        // The card sends `KeyboardEvent.code`.
        let shortcut = accept("Ctrl+Shift+Digit1").ok().unwrap();
        assert_eq!(label(&shortcut), "Ctrl+Shift+1");
        let shortcut = accept("Alt+Shift+F5").ok().unwrap();
        assert_eq!(label(&shortcut), "Alt+Shift+F5");
    }

    #[test]
    fn one_modifier_is_refused() {
        assert!(matches!(accept("Ctrl+KeyK"), Err(Some(_))));
        assert!(matches!(accept("KeyK"), Err(Some(_))));
        assert!(matches!(accept("Ctrl+Nonsense"), Err(None)));
    }

    #[test]
    fn what_is_stored_reads_back() {
        let shortcut = accept("Ctrl+Alt+KeyJ").ok().unwrap();
        let stored = shortcut.into_string();
        assert_eq!(Shortcut::from_str(&stored).ok(), Some(shortcut));
    }
}
