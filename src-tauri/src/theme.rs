//! The light/dark preference, taken from dsh rather than invented here.
//!
//! The browser UI keeps its theme in `$DSH_HOME/settings.yaml` under
//! `ui-theme.preference` — `light`, `dark`, or `system`. Two things out here
//! are set from it: the colour behind the webview, which is what shows in the
//! gap between two documents, and the window's own theme.
//!
//! The window's theme is not the colour of a frame — there is no frame. It is
//! what `prefers-color-scheme` answers inside the webview, and everything in
//! the window resolves `system` against it: this app's loading page, the cards
//! [`crate::controls`] draws over dsh, and dsh's own client. The last of those
//! is why it is set from the preference rather than left with the desktop.
//! dsh's page picks a theme twice on the way up — once from a bootstrap script
//! its server writes into the document with the saved preference already in
//! it, and once when the client theme plugin activates, which starts at its
//! own default of `system` and adopts the saved value a moment later. With a
//! dark dsh on a light desktop that second pass arrives light and is corrected
//! on the next frame: the whole window flashes white just as "Loading
//! plugins…" finishes. Answering the query with dsh's own preference lands the
//! transient default on the saved theme, and there is nothing left to flash.
//!
//! What that costs is that the answer becomes this app's to keep true. Set at
//! the launch and never touched again — which is what it was — a user who
//! switched dsh to `system` was told back dsh's last explicit theme, and dsh
//! stayed dark on a light desktop until a restart. So the file is watched for
//! as long as the app runs; see [`watch`].

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use tauri::window::Color;
use tauri::{Theme, WebviewWindow};

/// The loading page's background in each theme, matching the `--bg` it paints
/// itself with (see `dist/index.html`). This is what the window shows in the
/// moment between opening and the webview's first frame.
const LIGHT_BG: Color = Color(0xff, 0xff, 0xff, 0xff);
const DARK_BG: Color = Color(0x10, 0x10, 0x14, 0xff);

/// The dark one again, as CSS.
///
/// [`crate::controls`] takes a strip off the top of whatever page the window is
/// showing, and on dsh's page that strip is the page's own canvas — which is
/// the UA's white for as long as dsh's shell takes to paint itself. It covers
/// it with this until the page has a background of its own, so the band is the
/// same colour the window is rather than a white one; after that the cover
/// comes off and the band is dsh's own colour again.
pub(crate) fn dark_css() -> String {
    format!("#{:02x}{:02x}{:02x}", DARK_BG.0, DARK_BG.1, DARK_BG.2)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Preference {
    Light,
    Dark,
    /// What dsh falls back to when the field is absent.
    #[default]
    System,
}

impl Preference {
    /// The window's theme: `None` hands it to the desktop, which is the whole
    /// of what `system` means here.
    pub(crate) fn window(self) -> Option<Theme> {
        match self {
            Self::Light => Some(Theme::Light),
            Self::Dark => Some(Theme::Dark),
            Self::System => None,
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

/// Put the background behind the webview, and WebView2's own default one, in
/// the given theme — the two colours that show wherever a document has not
/// painted yet, which is every gap between two documents.
pub fn background(window: &WebviewWindow, preference: Preference) {
    let resolved = match preference {
        Preference::Light => Theme::Light,
        Preference::Dark => Theme::Dark,
        // Which is where the window's own theme has been left for this one.
        Preference::System => window.theme().unwrap_or(Theme::Light),
    };
    let background = if resolved == Theme::Dark {
        DARK_BG
    } else {
        LIGHT_BG
    };
    let _ = window.set_background_color(Some(background));
}

/// How often the settings file is looked at, with the window on screen and
/// without.
///
/// The gap between the two is what makes the fast one affordable. This poll is
/// the whole of the delay a user sees after picking `system` in dsh — dsh has
/// no answer of its own for that one, it asks the media query, and the query
/// does not change until the pass below has been round — so it has to be short
/// enough to read as part of the same click. Against that, an app that lives in
/// the tray for days should not be waking ten times a second to `stat` a file
/// nobody is touching; a window nobody can see is a window nobody is switching
/// the theme in.
const WATCHING: Duration = Duration::from_millis(100);
const HIDDEN: Duration = Duration::from_secs(2);

/// Follow the preference for as long as the app runs, so that what the webview
/// answers stays what dsh would answer.
///
/// Nothing tells this side when the theme is switched inside dsh: dsh writes
/// the file and repaints its own page, and the shell around it is not asked.
/// Watching the page instead of the file would miss the one switch this most
/// needs to see — dark to `system`, where the window is still answering `dark`,
/// so dsh resolves `system` straight back to dark and nothing changes on screen
/// to be noticed. So the file: a `metadata` call, and a read of it only once it
/// has moved. dsh writes it atomically — a temporary file and a rename — so a
/// read that catches one is either the old document or the new one, never half
/// of either.
pub fn watch(window: WebviewWindow, launched_with: Preference) {
    std::thread::spawn(move || {
        let mut seen = stamp();
        let mut preference = launched_with;

        loop {
            // Asked every time rather than remembered: the window is hidden and
            // shown from the tray, from the close button and from a second
            // launch, and none of those come past here.
            std::thread::sleep(if window.is_visible().unwrap_or(true) {
                WATCHING
            } else {
                HIDDEN
            });

            let now = stamp();
            if now == seen {
                continue;
            }
            seen = now;

            // The file moves for every setting dsh keeps in it, and the theme
            // is one field of one section; most of these writes are somebody
            // else's.
            let next = self::preference();
            if next == preference {
                continue;
            }
            preference = next;

            // The call that reaches the webview: tao resolves `None` against
            // the desktop, and Tauri passes the resolved change on to WebView2's
            // `SetPreferredColorScheme`. It travels through the event loop, so
            // the theme `background` reads back can still be the old one for a
            // moment — the `ThemeChanged` that follows repaints it; see
            // `build_window`.
            let _ = window.set_theme(preference.window());
            background(&window, preference);
        }
    });
}

/// What the settings file looks like from the outside. `None` while there is no
/// file to look at — a dsh that has never run has not written one — which
/// compares equal to itself, so the file appearing later is a move like any
/// other.
fn stamp() -> Option<(SystemTime, u64)> {
    let data = std::fs::metadata(settings_file()?).ok()?;
    Some((data.modified().ok()?, data.len()))
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
