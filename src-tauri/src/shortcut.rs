//! Keys that work from anywhere: one brings the window up and puts it away
//! again, one opens the quick-ask box ([`crate::ask`]), and one puts the
//! clipboard into the composer of the session on screen ([`crate::paste`]).
//! Each is a [`Slot`] with its own key, its own row on the settings card and
//! its own entry in `desktop.json`.
//!
//! Global means registered with the operating system: it works whichever app
//! has the focus, and for as long as this app runs no other app gets that key.
//! That is why the default is Ctrl+Alt+D and not Ctrl+Shift+D, which Chrome
//! and VS Code both use, and why a key chosen on the settings card needs two
//! modifiers (see [`accept`]).
//!
//! Stored in `desktop.json` under each slot's key (`shortcut` for the window):
//! absent is the slot's default, `""` is off, anything else is what the card
//! recorded. The quick-ask box has a default and the clipboard key has none:
//! the obvious keys for pasting are taken by other apps' own paste commands —
//! Ctrl+Alt+V is Office's Paste Special — and a global key would take them. A key another program already
//! holds cannot be registered. At launch that is said in a notification, since
//! nobody is looking at the card then; either way the card's row says it.
//!
//! The plugin is used from Rust only. Its commands are not granted to the
//! webview, and the one way the page reaches this module is a slot's verb —
//! see [`crate::controls`].

use std::str::FromStr;
use std::sync::Mutex;

use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

/// One of the keys this app registers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// Show or hide the window.
    Window,
    /// Open the quick-ask box.
    Ask,
    /// Put the clipboard into the composer on screen.
    Paste,
}

impl Slot {
    pub const ALL: [Slot; 3] = [Slot::Window, Slot::Ask, Slot::Paste];

    /// The settings card's row for it, and the verb that row sends.
    pub fn verb(self) -> &'static str {
        match self {
            Slot::Window => "shortcut",
            Slot::Ask => "ask-shortcut",
            Slot::Paste => "paste-shortcut",
        }
    }

    pub fn from_verb(verb: &str) -> Option<Slot> {
        Slot::ALL.into_iter().find(|slot| slot.verb() == verb)
    }

    /// Where it is stored in `desktop.json`. The window's is the name it had
    /// when it was the only one.
    pub fn setting(self) -> &'static str {
        match self {
            Slot::Window => "shortcut",
            Slot::Ask => "askShortcut",
            Slot::Paste => "pasteShortcut",
        }
    }

    /// What a launch registers when nothing has been chosen, `""` for nothing.
    ///
    /// The quick-ask box's is Ctrl+Alt+Space, except on macOS, where
    /// Ctrl+Option+Space is the system's own "previous input source".
    fn default(self) -> &'static str {
        match self {
            Slot::Window => "Ctrl+Alt+D",
            Slot::Ask if cfg!(target_os = "macos") => "Ctrl+Alt+KeyQ",
            Slot::Ask => "Ctrl+Alt+Space",
            Slot::Paste => "",
        }
    }

    fn index(self) -> usize {
        self as usize
    }

    fn act(self, app: &AppHandle) {
        match self {
            Slot::Window => toggle(app),
            Slot::Ask => crate::ask::toggle(app),
            Slot::Paste => crate::paste::run(app),
        }
    }
}

/// Why the key asked for is not the one in effect.
enum Failure {
    /// Another program registered it first.
    Taken,
    /// Fewer than two modifiers; see [`accept`].
    TooFew,
    /// Anything else the OS said.
    Refused(String),
    /// Another of this app's own keys is already that one.
    Ours(Slot),
}

#[derive(Default)]
struct State {
    /// The key registered now, if any.
    active: Option<Shortcut>,
    /// The last key asked for that did not take. Cleared by one that does.
    failed: Option<(Shortcut, Failure)>,
}

static STATE: Mutex<[State; 3]> = Mutex::new([
    State {
        active: None,
        failed: None,
    },
    State {
        active: None,
        failed: None,
    },
    State {
        active: None,
        failed: None,
    },
]);

fn state() -> std::sync::MutexGuard<'static, [State; 3]> {
    STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The plugin, with the one handler every key it registers goes to, which
/// hands the press to whichever slot holds that key.
pub(crate) fn plugin() -> TauriPlugin<Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, shortcut, event| {
            if event.state() != ShortcutState::Pressed {
                return;
            }
            let slot = holder(&state(), *shortcut);
            if let Some(slot) = slot {
                slot.act(app);
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

/// Register each slot's stored key, once, at launch.
///
/// A file whose key does not parse is left as it is and registers nothing:
/// it was edited by hand, and the card shows the key as off until a new one
/// is recorded over it. The two-modifier rule is not applied here — it guards
/// the verb the page can send, not a file the user wrote.
pub(crate) fn init(app: &AppHandle) {
    for slot in Slot::ALL {
        init_slot(app, slot);
    }
}

fn init_slot(app: &AppHandle, slot: Slot) {
    let keys = crate::settings::shortcut(app, slot.setting())
        .unwrap_or_else(|| slot.default().to_string());
    if keys.is_empty() {
        return;
    }
    let Ok(shortcut) = Shortcut::from_str(&keys) else {
        eprintln!("dsh-desktop: ignoring the unreadable shortcut {keys:?}");
        return;
    };
    let other = holder(&state(), shortcut);
    if let Some(other) = other {
        state()[slot.index()].failed = Some((shortcut, Failure::Ours(other)));
        return;
    }

    match app.global_shortcut().register(shortcut) {
        Ok(()) => state()[slot.index()].active = Some(shortcut),
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
            state()[slot.index()].failed = Some((shortcut, failure));
        }
    }
}

/// Which slot holds `shortcut` now, if one does.
fn holder(state: &[State; 3], shortcut: Shortcut) -> Option<Slot> {
    Slot::ALL
        .into_iter()
        .find(|slot| state[slot.index()].active == Some(shortcut))
}

/// The key the settings card recorded for `slot`, or `""` for off.
///
/// The key now in effect stays in effect until its replacement has
/// registered, so a key that is taken costs nothing but the message saying so.
/// A key another of this app's slots holds is refused here rather than handed
/// to the OS, which would call it taken by "another program".
pub(crate) fn change(app: &AppHandle, slot: Slot, keys: &str) {
    let wanted = if keys.is_empty() {
        None
    } else {
        match accept(keys) {
            Ok(shortcut) => Some(shortcut),
            Err(Some(shortcut)) => {
                state()[slot.index()].failed = Some((shortcut, Failure::TooFew));
                return sync(app);
            }
            // Nothing the card records fails to parse; something else sent it.
            Err(None) => return,
        }
    };

    let shortcuts = app.global_shortcut();
    let mut state = state();
    if let Some(shortcut) = wanted {
        if let Some(other) = holder(&state, shortcut).filter(|other| *other != slot) {
            state[slot.index()].failed = Some((shortcut, Failure::Ours(other)));
            drop(state);
            return sync(app);
        }
    }
    let own = &mut state[slot.index()];
    if wanted != own.active {
        if let Some(shortcut) = wanted {
            if let Err(error) = shortcuts.register(shortcut) {
                own.failed = Some((shortcut, failure(&error)));
                drop(state);
                return sync(app);
            }
        }
        if let Some(old) = own.active.take() {
            let _ = shortcuts.unregister(old);
        }
        own.active = wanted;
    }
    own.failed = None;
    drop(state);

    crate::settings::set_shortcut(
        app,
        slot.setting(),
        &wanted.map(Shortcut::into_string).unwrap_or_default(),
    );
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
        Failure::Ours(other) => t!(
            "{} 已经是「{}」的快捷键了。",
            "{} is already the key for “{}”.",
            keys,
            name(*other)
        ),
    }
}

/// What the card calls a slot's row.
pub(crate) fn name(slot: Slot) -> &'static str {
    match slot {
        // Not "global shortcut": it sits under the settings card's
        // "Shortcuts" heading, beside two more that are just as global.
        Slot::Window => t!("显示窗口", "Show window"),
        Slot::Ask => t!("快速提问", "Quick ask"),
        Slot::Paste => t!("粘贴到 dsh", "Paste into dsh"),
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

/// A row's second line while nothing has gone wrong.
pub(crate) fn note(slot: Slot) -> &'static str {
    match slot {
        Slot::Window => t!(
            "在任何地方按下它都能显示或隐藏窗口。点右边的按键录一个新的，录的时候按 Backspace 就是关掉。",
            "Shows or hides the window from anywhere. Click the key on the right to record a new one; Backspace while recording turns it off."
        ),
        Slot::Ask => t!(
            "弹出一个小输入框，写一句话回车，就在最近的工作区里开一个新会话去跑。",
            "Opens a small box: type a line and press Enter to start a new session on it in your latest workspace."
        ),
        Slot::Paste => t!(
            "把剪贴板里的文字放进当前会话的输入框，窗口跟着出来。默认没有设，点右边录一个。",
            "Puts the text on the clipboard into the composer of the session on screen, and brings the window up. Off until you record a key."
        ),
    }
}

/// Tell each slot's row what its key is and whether it took.
///
/// Pushed on every page load, after every change, and after a language switch:
/// every string in it is written in the language of the moment.
pub(crate) fn sync(app: &AppHandle) {
    for slot in Slot::ALL {
        let state = state();
        let own = &state[slot.index()];
        let keys = own.active.as_ref().map(label).unwrap_or_default();
        let problem = own
            .failed
            .as_ref()
            .map(|(shortcut, failure)| explain(shortcut, failure));
        drop(state);

        let warn = problem.is_some();
        let note = problem.unwrap_or_else(|| note(slot).to_string());
        let words = serde_json::json!({
            "off": t!("未设置", "Off"),
            "press": t!("按下组合键…", "Press keys…"),
        });
        let verb = slot.verb();
        let keys = serde_json::to_string(&keys).expect("a string is always serializable");
        let note = serde_json::to_string(&note).expect("a string is always serializable");
        crate::controls::eval(
            app,
            &format!(
                "window.__dshShortcut && window.__dshShortcut('{verb}', {keys}, {note}, {warn}, {words})"
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the card calls Alt: macOS names the key Option.
    const ALT: &str = if cfg!(target_os = "macos") {
        "Option"
    } else {
        "Alt"
    };

    #[test]
    fn the_default_is_two_modifiers_and_d() {
        let shortcut = accept(Slot::Window.default())
            .ok()
            .expect("the default passes its own rule");
        assert_eq!(label(&shortcut), format!("Ctrl+{ALT}+D"));
    }

    /// Every slot's default passes the card's own rule, no two are the same
    /// key, and the clipboard's is none at all.
    #[test]
    fn the_defaults_are_keys_the_card_would_take_and_do_not_collide() {
        let defaults: Vec<_> = Slot::ALL
            .into_iter()
            .map(|slot| slot.default())
            .filter(|keys| !keys.is_empty())
            .map(|keys| accept(keys).ok().expect("a default passes the card's rule"))
            .collect();
        assert_eq!(defaults.len(), 2);
        assert_ne!(defaults[0], defaults[1]);
        assert_eq!(Slot::Paste.default(), "");
    }

    #[test]
    fn each_slot_has_its_own_verb_and_setting() {
        for slot in Slot::ALL {
            assert_eq!(Slot::from_verb(slot.verb()), Some(slot));
        }
        assert_eq!(Slot::Window.setting(), "shortcut", "the name it had alone");
        assert_eq!(Slot::from_verb("nonsense"), None);
    }

    #[test]
    fn what_the_card_sends_is_what_it_shows() {
        // The card sends `KeyboardEvent.code`.
        let shortcut = accept("Ctrl+Shift+Digit1").ok().unwrap();
        assert_eq!(label(&shortcut), "Ctrl+Shift+1");
        let shortcut = accept("Alt+Shift+F5").ok().unwrap();
        assert_eq!(label(&shortcut), format!("{ALT}+Shift+F5"));
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
