//! The light/dark preference, taken from dsh rather than invented here.
//!
//! The browser UI keeps its theme in `$DSH_HOME/settings.yaml` under
//! `ui-theme.preference` — `light`, `dark`, or `system`. It is read once, at
//! startup, so that the window opens in the colour of the page it is about to
//! show instead of flashing the other one.
//!
//! Only once: there is no frame left to repaint. The window controls live
//! inside the page now (see [`crate::controls`]) and take their colours from
//! it, so a theme switch inside dsh reaches them as part of the same repaint
//! that changes everything else — nothing out here has to watch for it.
//!
//! `system` is not resolved here. The window follows the OS on its own once it
//! is told to, and the loading page has a media query.

use std::path::PathBuf;

use tauri::window::Color;
use tauri::{Theme, WebviewWindow};

/// The loading page's background in each theme, matching the `--bg` it paints
/// itself with (see `dist/index.html`). This is what the window shows in the
/// moment between opening and the webview's first frame.
const LIGHT_BG: Color = Color(0xff, 0xff, 0xff, 0xff);
const DARK_BG: Color = Color(0x10, 0x10, 0x14, 0xff);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Preference {
    Light,
    Dark,
    /// What dsh falls back to when the field is absent.
    #[default]
    System,
}

impl Preference {
    /// The window's theme: `None` hands it to the OS.
    pub fn window(self) -> Option<Theme> {
        match self {
            Self::Light => Some(Theme::Light),
            Self::Dark => Some(Theme::Dark),
            Self::System => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::System => "system",
        }
    }
}

/// The preference dsh would use right now, or the default if it has never been
/// set — or if anything at all is wrong with the file, which is dsh's problem
/// to report, not a reason to fail to open a window.
pub fn preference() -> Preference {
    read().unwrap_or_default()
}

/// `None` when the file could not be read at all, which is a different thing
/// from a file that is readable and says nothing about the theme — that one
/// answers `Some(default)`. [`preference`] collapses the two, since the default
/// is the answer either way; the distinction is kept here because this is where
/// it is still knowable.
fn read() -> Option<Preference> {
    let text = std::fs::read_to_string(settings_file()?).ok()?;
    Some(parse(&text).unwrap_or_default())
}

/// Told to the loading page before it parses, so it never paints the wrong
/// colour first. See the bootstrap script in `dist/index.html`.
pub fn script(preference: Preference) -> String {
    format!("window.__DSH_THEME__ = {:?}", preference.name())
}

/// Put the frame and the background behind the webview in the given theme.
pub fn paint(window: &WebviewWindow, preference: Preference) {
    let _ = window.set_theme(preference.window());

    // `set_theme(None)` gives the window back to the OS, so ask it what that
    // came out as rather than guessing at a background colour.
    let resolved = match preference {
        Preference::Light => Theme::Light,
        Preference::Dark => Theme::Dark,
        Preference::System => window.theme().unwrap_or(Theme::Light),
    };
    let background = if resolved == Theme::Dark {
        DARK_BG
    } else {
        LIGHT_BG
    };
    let _ = window.set_background_color(Some(background));
}

/// `$DSH_HOME/settings.yaml`, wherever that resolves to.
///
/// Public because the theme is not the only thing this app takes from dsh
/// rather than deciding for itself: [`crate::i18n`] reads the language the
/// user picked out of the same file, and the rule for finding it belongs in
/// one place.
pub fn settings_file() -> Option<PathBuf> {
    #[allow(deprecated)]
    let home = match std::env::var_os("DSH_HOME") {
        Some(home) if !home.is_empty() => PathBuf::from(home),
        // The same default dsh resolves: `~/.dsh`.
        _ => std::env::home_dir()?.join(".dsh"),
    };

    Some(home.join("settings.yaml"))
}

/// One indented field out of one top-level section of the settings document,
/// unquoted and trimmed. `None` when the section is absent or holds no such
/// field; `Some("")` when it holds the field with nothing after the colon,
/// which is a distinction both callers care about.
///
/// Every namespace in the file is a top-level key with its section indented
/// under it, so the field wanted is the first `<name>:` inside the block that
/// starts at column zero with `<section>:`. That is little enough of YAML to
/// read directly rather than pull in a parser for one string — and the rest of
/// the file belongs to dsh's plugins, which this has no business parsing.
///
/// Public to the crate because two things are read this way: the theme below
/// and the language in [`crate::i18n`]. It was written out once per caller,
/// which put the one tricky part — that a `preference:` only counts inside the
/// right section, and `ui-theme` and `locale` both have one — in two places at
/// once.
pub(crate) fn field<'a>(text: &'a str, section: &str, name: &str) -> Option<&'a str> {
    let heading = format!("{section}:");
    let key = format!("{name}:");
    let mut inside = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if !line.starts_with([' ', '\t']) {
            inside = trimmed == heading;
            continue;
        }
        if !inside {
            continue;
        }

        if let Some(value) = trimmed.strip_prefix(&key) {
            return Some(value.trim().trim_matches(['"', '\'']));
        }
    }

    None
}

/// Read `ui-theme.preference` out of the settings document. Anything the file
/// says that is not one of the three known values is `None`, the same as
/// saying nothing.
fn parse(text: &str) -> Option<Preference> {
    match field(text, "ui-theme", "preference")? {
        "light" => Some(Preference::Light),
        "dark" => Some(Preference::Dark),
        "system" => Some(Preference::System),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{parse, Preference};

    #[test]
    fn reads_the_field() {
        assert_eq!(
            parse("ui-theme:\n  preference: dark\n"),
            Some(Preference::Dark)
        );
    }

    #[test]
    fn reads_it_between_other_sections() {
        let settings = "\
ui-onboarding:
  welcomeNoticeVersion: 2026-08-13.1
ui-theme:
  preference: light
agent-default-model:
  provider: sensenova
";
        assert_eq!(parse(settings), Some(Preference::Light));
    }

    #[test]
    fn ignores_the_field_in_another_section() {
        let settings = "\
other:
  preference: dark
ui-theme:
  preference: system
";
        assert_eq!(parse(settings), Some(Preference::System));
    }

    #[test]
    fn has_nothing_to_say_without_the_section() {
        assert_eq!(parse("agent-default-model:\n  provider: sensenova\n"), None);
    }

    #[test]
    fn has_nothing_to_say_about_an_unknown_value() {
        assert_eq!(parse("ui-theme:\n  preference: solarized\n"), None);
    }
}
