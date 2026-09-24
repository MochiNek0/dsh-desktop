//! The mobile stylesheet patch: whether it is on, written where the plugin can
//! read it.
//!
//! dsh's web UI is most of the way to working on a phone — the chat screen, the
//! trajectory and the model list all carry their own narrow-width rules — but
//! the settings dialog does not. It is a two-column shell with a fixed 188px
//! nav, so on a 390px phone the pane beside it gets about 150px and every label
//! in it wraps to one character per line. The patch is a handful of rules that
//! stack that one dialog; `plugin/lib/index.js` holds the CSS and says what each
//! rule is for.
//!
//! ## Why a file, and why this one
//!
//! The CSS is injected by the plugin, which runs inside dsh. The decision
//! belongs to the user, who is at the desktop. Something has to carry one
//! boolean between two processes.
//!
//! A file rather than an environment variable, because a variable is read once
//! when dsh starts and this has to be answerable while dsh is running: the
//! point of the switch is that it can be turned off the day dsh ships its own
//! rules, and if that meant restarting dsh it would be a worse switch than
//! editing a config by hand. dsh re-collects the injection table on every index
//! request — "fresh per call, so subscribers read live state" is the
//! webserver's own description — so a file the plugin stats per page load is as
//! live as the mechanism allows.
//!
//! Under `$DSH_HOME/.dsh-desktop/` because that is already this app's corner of
//! dsh's home, the staged plugin copy being its sibling, and because `DSH_HOME`
//! is the one path both halves arrive at without being told: dsh reads it, and
//! this app passes it through untouched. See [`crate::plugins::dsh_home`].
//!
//! Presence is the whole signal. The bytes inside are a sentence for whoever
//! finds the file and wonders what it is; nothing reads them.

use std::path::{Path, PathBuf};

/// What goes in the file, for a human who finds it rather than for a parser.
const NOTE: &str = "dsh-desktop: while this file exists, the phone gets a small stylesheet patch\n\
                    for dsh's settings dialog. Delete it, or use the switch on the\n\
                    phone-connection card, to turn it off.\n\
                    \n\
                    To change the patch itself, put a stylesheet of your own next to this\n\
                    file as mobile.css; it is used instead of the built-in one from the next\n\
                    page load, so a fix for a newer dsh needs no new build of the app.\n";

/// The stylesheet the plugin uses in place of its built-in one, when it is
/// there.
///
/// The other half of the same two-process agreement as [`flag`], and the same
/// warning applies: the plugin spells this name in `override()` in
/// `plugin/lib/index.js`, and a disagreement is silent — the app would write a
/// stylesheet nobody reads. `the_names_are_the_ones_the_plugin_looks_for` holds
/// the pair together.
///
/// Only the test needs the path now: this file is the user's, and the app
/// never writes it. See [`downloaded`].
#[cfg(test)]
pub(super) fn sheet() -> PathBuf {
    crate::plugins::dsh_home()
        .join(".dsh-desktop")
        .join("mobile.css")
}

/// Where [`super::patch`] keeps the stylesheet it downloads.
///
/// Not [`sheet`]: that file is the user's, and a download written over it would
/// throw away whatever they put there. The plugin reads this one only when the
/// user has none — `downloaded()` in `plugin/lib/index.js` spells the same name.
pub(super) fn downloaded() -> PathBuf {
    crate::plugins::dsh_home()
        .join(".dsh-desktop")
        .join("mobile-patch.css")
}

/// The flag's path.
fn flag() -> PathBuf {
    crate::plugins::dsh_home()
        .join(".dsh-desktop")
        .join("mobile-css")
}

/// Whether the patch is on.
///
/// A missing file is off, and so is a home this process cannot read: the card
/// then draws an unticked box, the user ticks it, and [`write`] reports the real
/// error. Guessing "on" would instead show a tick for something that is not
/// happening.
pub fn enabled() -> bool {
    read(&flag())
}

/// Turn it on or off.
pub fn set(on: bool) -> Result<(), String> {
    write(&flag(), on)
}

/// The two above, against a path a test can choose.
fn read(path: &Path) -> bool {
    path.exists()
}

/// Turning off an already-off patch succeeds rather than reporting a missing
/// file: the card sends the state it wants, not a diff, and a fresh profile has
/// no file to remove.
fn write(path: &Path, on: bool) -> Result<(), String> {
    if !on {
        return match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(t!(
                "没能关掉样式补丁：{}",
                "could not turn the stylesheet patch off: {}",
                error
            )),
        };
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            t!(
                "没能建起 {}：{}",
                "could not create {}: {}",
                parent.display(),
                error
            )
        })?;
    }
    std::fs::write(path, NOTE).map_err(|error| {
        t!(
            "没能打开样式补丁：{}",
            "could not turn the stylesheet patch on: {}",
            error
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of this test's own. The real flag lives under `$DSH_HOME`,
    /// and a test that wrote there would turn the patch on for whoever ran
    /// `cargo test`.
    fn elsewhere(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dsh-desktop-style-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join(".dsh-desktop").join("mobile-css")
    }

    /// The path the plugin looks for, spelled the same way on this side. If
    /// this moves, the switch silently stops doing anything — the plugin finds
    /// no file and injects nothing, with no error anywhere. The same two names
    /// are pinned in `plugin/test/index.test.mjs`.
    #[test]
    fn the_flag_is_where_the_plugin_looks_for_it() {
        let path = flag();
        assert!(path.ends_with("mobile-css"), "{}", path.display());
        assert!(
            path.parent().is_some_and(|dir| dir.ends_with(".dsh-desktop")),
            "{} is not under .dsh-desktop",
            path.display()
        );
    }

    #[test]
    fn on_then_off_reads_back_both_ways() {
        let path = elsewhere("roundtrip");
        assert!(!read(&path), "a fresh profile has the patch off");

        write(&path, true).unwrap();
        assert!(read(&path), "and on once it is asked for");

        write(&path, false).unwrap();
        assert!(!read(&path), "and off again");

        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    /// The parent does not exist on a first run: turning the patch on has to
    /// make it rather than fail.
    #[test]
    fn the_first_time_makes_its_own_directory() {
        let path = elsewhere("mkdir");
        assert!(!path.parent().unwrap().exists());
        write(&path, true).unwrap();
        assert!(read(&path));
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn turning_off_what_was_never_on_is_not_an_error() {
        let path = elsewhere("idempotent");
        write(&path, false).unwrap();
        write(&path, false).unwrap();
        assert!(!read(&path));
    }

    /// Whatever is in the file, presence is the signal — so a file someone
    /// emptied still counts as on, the same as it would to the plugin's
    /// `existsSync`.
    #[test]
    fn an_empty_file_is_still_on() {
        let path = elsewhere("empty");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"").unwrap();
        assert!(read(&path));
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }
}
