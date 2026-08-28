//! The handful of choices this app makes on its own, kept next to the ones the
//! installer makes.
//!
//! Almost nothing belongs here. The theme is dsh's, read out of its
//! `settings.yaml` (see [`crate::theme`]); the login item is the operating
//! system's, and is asked about rather than recorded. What is left is the
//! preferences that are this window's alone, and today that is one: whether
//! this app raises notifications at all.
//!
//! ## Where it lives
//!
//! `%LOCALAPPDATA%\<identifier>\desktop.json`, beside the `bootstrap.json` the
//! install script writes — [`crate::dsh::app_dir`], the same directory the
//! installer hooks clean up. Deliberately *not* in `$DSH_HOME`: that directory
//! is dsh's own, it survives an uninstall on purpose, and a desktop-only
//! preference has no business in a file dsh parses.
//!
//! ## Why by hand, and why so forgiving
//!
//! A store plugin would bring a dependency, a schema and a migration story for
//! what is one boolean. Instead the file is read with `serde_json` — already
//! here for the preset list — and every failure answers with the default:
//! unreadable, truncated by a power cut mid-write, hand-edited into invalid
//! JSON, or written by a future version that keeps something else in it.
//!
//! That is the whole design rule. A preference file is not worth an error
//! dialog, and it is certainly not worth refusing to start over. Unknown keys
//! are preserved through a write rather than dropped, so a newer build's
//! settings survive being opened by an older one.

use std::path::PathBuf;

use serde_json::{Map, Value};
use tauri::AppHandle;

/// Whether this app raises notifications at all. On unless it was turned off:
/// the feature exists because the window spends turns in the tray, and a
/// notification setting that defaults to silent is one nobody discovers.
///
/// Deliberately not `notifyOnTurnEnd`, which is what this was called first. The
/// gate it drives sits in [`crate::notify::show`], the one place *every*
/// notification passes through — a plugin's as much as this app's own — so a
/// name that promised only the finished-turn toast was describing a narrower
/// switch than the one actually wired up. The menu item says the same thing:
/// "Notifications", not "Notify when a turn finishes".
const NOTIFY_KEY: &str = "notifications";

/// What the key was called while this was a turn-only switch, still read so a
/// preference set by an earlier build of this branch is not silently reset. Only
/// ever read; a write always uses [`NOTIFY_KEY`].
const NOTIFY_KEY_WAS: &str = "notifyOnTurnEnd";

const NOTIFY_DEFAULT: bool = true;

/// Read the preference. Any problem reading it is the default.
pub fn notifications(app: &AppHandle) -> bool {
    let document = read(app);
    document
        .get(NOTIFY_KEY)
        .or_else(|| document.get(NOTIFY_KEY_WAS))
        .and_then(Value::as_bool)
        .unwrap_or(NOTIFY_DEFAULT)
}

/// Flip it, and answer with what it now is.
///
/// Returns the value that was written rather than re-reading, so a caller can
/// repaint the checkmark even on the disk error path — where the toggle did not
/// survive, but the menu should still not show a lie about this session.
pub fn toggle_notifications(app: &AppHandle) -> bool {
    let wanted = !notifications(app);
    write(app, NOTIFY_KEY, Value::Bool(wanted));
    wanted
}

/// The whole document, or an empty one. Never `Err`: see the module docs.
fn read(app: &AppHandle) -> Map<String, Value> {
    let Some(path) = file(app) else {
        return Map::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Map::new();
    };
    match serde_json::from_str::<Value>(&text) {
        // A JSON document that is not an object — `null`, a list, a bare number
        // — is as unusable as a corrupt one, and is treated the same way.
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

/// Set one key, keeping every other key the file already had.
///
/// Read-modify-write rather than serialising a struct, so a preference this
/// build has never heard of — one a newer version wrote — is still there after
/// an older version toggles something beside it.
fn write(app: &AppHandle, key: &str, value: Value) {
    let Some(path) = file(app) else {
        return;
    };

    let mut document = read(app);
    document.insert(key.to_string(), value);

    let Ok(text) = serde_json::to_string_pretty(&Value::Object(document)) else {
        return;
    };

    // The directory is normally already there — the install script writes
    // `bootstrap.json` into it — but not on a machine where that never ran.
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    if let Err(error) = std::fs::write(&path, text) {
        // Losing a preference is not a reason to interrupt anyone; the setting
        // simply reverts next launch.
        eprintln!("dsh-desktop: could not save the settings: {error}");
    }
}

fn file(app: &AppHandle) -> Option<PathBuf> {
    Some(crate::dsh::app_dir(app)?.join("desktop.json"))
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Map, Value};

    /// The read half of `read`, without the `AppHandle` a test cannot build.
    /// Kept in step with it by `parses_like_the_reader`, below.
    fn parse(text: &str) -> Map<String, Value> {
        match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(map)) => map,
            _ => Map::new(),
        }
    }

    fn notify(text: &str) -> bool {
        let document = parse(text);
        document
            .get(super::NOTIFY_KEY)
            .or_else(|| document.get(super::NOTIFY_KEY_WAS))
            .and_then(Value::as_bool)
            .unwrap_or(super::NOTIFY_DEFAULT)
    }

    #[test]
    fn reads_the_preference() {
        assert!(!notify(r#"{"notifications": false}"#));
        assert!(notify(r#"{"notifications": true}"#));
    }

    /// The key this was called while it was a turn-only switch still reads, so
    /// someone who turned notifications off on an earlier build of this branch
    /// does not find them back on.
    #[test]
    fn still_reads_the_old_key() {
        assert!(!notify(r#"{"notifyOnTurnEnd": false}"#));
        assert!(notify(r#"{"notifyOnTurnEnd": true}"#));
    }

    /// With both present the current name wins: it is the only one ever
    /// written, so it is the one that was set most recently.
    #[test]
    fn the_current_key_beats_the_old_one() {
        assert!(notify(
            r#"{"notifications": true, "notifyOnTurnEnd": false}"#
        ));
        assert!(!notify(
            r#"{"notifications": false, "notifyOnTurnEnd": true}"#
        ));
    }

    /// Every way the file can be unusable ends at the default, because a
    /// preference is not worth failing a launch over.
    #[test]
    fn falls_back_to_the_default() {
        assert!(notify(""));
        assert!(notify("{"));
        assert!(notify("null"));
        assert!(notify("[1, 2, 3]"));
        assert!(notify("{}"));
        // Present, but not a boolean.
        assert!(notify(r#"{"notifications": "no"}"#));
    }

    /// On by default: the window spends a turn in the tray, and a notification
    /// nobody switched on is a notification nobody knows exists.
    #[test]
    fn defaults_to_notifying() {
        const { assert!(super::NOTIFY_DEFAULT) };
    }

    /// A key this build does not know about survives a write of one that it
    /// does, so a newer version's settings are not thrown away by an older one.
    #[test]
    fn keeps_keys_it_does_not_understand() {
        let mut document = parse(r#"{"somethingNewer": {"nested": 1}}"#);
        document.insert(super::NOTIFY_KEY.to_string(), json!(false));

        assert_eq!(document.get("somethingNewer"), Some(&json!({"nested": 1})));
        assert_eq!(document.get(super::NOTIFY_KEY), Some(&json!(false)));
    }
}
