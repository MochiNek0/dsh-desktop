//! The light/dark preference, taken from dsh rather than invented here.
//!
//! The browser UI keeps its theme as the `preference` of its `ui-theme` plugin
//! — `light`, `dark`, or `system` — which dsh 0.2 writes into the profile's
//! `cordis.patch.yml` and dsh 0.1 kept in `$DSH_HOME/settings.yaml`; see
//! [`setting`]. Two things out here
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
    css(DARK_BG)
}

/// The light one, as CSS.
///
/// [`crate::auth`] covers dsh's refusal page with whichever of the two the
/// window's own theme resolves to, so the one document the user is not meant
/// to read is the colour of the gap it sits in rather than the UA's white.
pub(crate) fn light_css() -> String {
    css(LIGHT_BG)
}

fn css(colour: Color) -> String {
    format!("#{:02x}{:02x}{:02x}", colour.0, colour.1, colour.2)
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
    setting("ui-theme", "preference")
        .as_deref()
        .and_then(named)
        .unwrap_or_default()
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
/// without (in front and not, on Windows; see [`watch`]).
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

        // On Windows: is the window the one in front, asked of the OS. Not
        // `is_visible`, which a minimised or buried window also answers yes to
        // and which wakes the event loop to answer at all; and not the window's
        // `Focused` event, which reads false while the webview holds the focus.
        // Switching the theme in dsh means clicking in this window, so in front
        // is the only time the fast poll is earning anything.
        #[cfg(windows)]
        let looked_at = {
            let hwnd = window.hwnd().ok().map(|hwnd| hwnd.0 as isize);
            move || match hwnd {
                Some(raw) => unsafe {
                    windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow().0 as isize == raw
                },
                None => true,
            }
        };
        #[cfg(not(windows))]
        let looked_at = || window.is_visible().unwrap_or(true);

        loop {
            // Asked every time rather than remembered: the window is hidden and
            // shown from the tray, from the close button and from a second
            // launch, and none of those come past here.
            std::thread::sleep(if looked_at() { WATCHING } else { HIDDEN });

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

/// What the two files a setting can come from look like from the outside —
/// both, because which of them answers changes when dsh 0.2 imports the old
/// one; see [`setting`]. `None` while there is no file to look at — a dsh that
/// has never run has not written one — which compares equal to itself, so the
/// file appearing later is a move like any other.
fn stamp() -> [Option<(SystemTime, u64)>; 2] {
    [legacy_file(), patch_file()].map(|path| {
        let data = std::fs::metadata(path?).ok()?;
        Some((data.modified().ok()?, data.len()))
    })
}

/// One field of one of dsh's settings, as dsh would read it right now. `None`
/// when nothing says — a field present with nothing after the colon included —
/// or when there is nothing to read.
///
/// dsh 0.1 kept every setting in `$DSH_HOME/settings.yaml`, one top-level
/// section per plugin. dsh 0.2 keeps them in the profile, as the `config` of
/// each plugin's entry in `cordis.patch.yml`; on its first boot it moves the
/// old file's sections there and renames the file to `settings.yaml.imported`.
/// So while `settings.yaml` exists it is the answer — on 0.1 it is the only
/// one, and on 0.2 it is what the first boot is about to import — and once it
/// is gone, the profile is.
///
/// Public to the crate because the theme is not the only thing this app takes
/// from dsh rather than deciding for itself: [`crate::i18n`] reads the language
/// the user picked the same way, and the rule for finding it belongs in one
/// place.
pub(crate) fn setting(entry: &str, name: &str) -> Option<String> {
    let read = |path: Option<PathBuf>| std::fs::read_to_string(path?).ok();
    match read(legacy_file()) {
        Some(text) => lookup(&text, true, entry, name),
        None => lookup(&read(patch_file())?, false, entry, name),
    }
}

/// [`setting`] without the files: `legacy` is which of the two documents
/// `text` is.
pub(crate) fn lookup(text: &str, legacy: bool, entry: &str, name: &str) -> Option<String> {
    let value = if legacy {
        field(text, entry, name)
    } else {
        configured(text, entry, name)
    }?;
    (!value.is_empty()).then(|| value.to_string())
}

/// `$DSH_HOME`, wherever that resolves to.
fn home() -> Option<PathBuf> {
    #[allow(deprecated)]
    let home = match std::env::var_os("DSH_HOME") {
        Some(home) if !home.is_empty() => PathBuf::from(home),
        // The same default dsh resolves: `~/.dsh`.
        _ => std::env::home_dir()?.join(".dsh"),
    };

    Some(home)
}

/// `$DSH_HOME/settings.yaml`, dsh 0.1's settings document.
fn legacy_file() -> Option<PathBuf> {
    Some(home()?.join("settings.yaml"))
}

/// The patch layer of the profile this app boots dsh with, which is where dsh
/// 0.2 writes what its Settings page changes.
fn patch_file() -> Option<PathBuf> {
    Some(
        home()?
            .join("profiles")
            .join(crate::plugins::PROFILE)
            .join("cordis.patch.yml"),
    )
}

/// One indented field out of one top-level section of dsh 0.1's settings
/// document, unquoted and trimmed. `None` when the section is absent or holds
/// no such field; `Some("")` when it holds the field with nothing after the
/// colon, which [`lookup`] counts as nothing said.
///
/// Every namespace in the file is a top-level key with its section indented
/// under it, so the field wanted is the first `<name>:` inside the block that
/// starts at column zero with `<section>:`. That is little enough of YAML to
/// read directly rather than pull in a parser for one string — and the rest of
/// the file belongs to dsh's plugins, which this has no business parsing.
fn field<'a>(text: &'a str, section: &str, name: &str) -> Option<&'a str> {
    let heading = format!("{section}:");
    let key = format!("{name}:");
    let mut inside = false;

    // A BOM is what PowerShell and older Notepad leave at the front, and it
    // would otherwise be read as part of the first section's name.
    for line in text.trim_start_matches('\u{feff}').lines() {
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

/// One field out of one entry's `config:` in a profile patch, unquoted and
/// trimmed, with the same `None` and `Some("")` as [`field`].
///
/// The patch is a top-level YAML list with one item per plugin entry, which dsh
/// writes as
///
/// ```yaml
/// - id: ui-theme
///   name: "@deepseek-ai/dsh-client-ui-theme"
///   config:
///     preference: dark
/// ```
///
/// so an item runs from a `-` at column zero to the next one, its `id` is one
/// of its own keys (on the `-` line or not), and the field wanted is a direct
/// child of its `config:` — not anything deeper, where some other setting of
/// the same plugin could have a `preference` of its own. An id patched twice
/// ends up with both items merged in order, so a later value wins.
fn configured<'a>(text: &'a str, entry: &str, name: &str) -> Option<&'a str> {
    let key = format!("{name}:");
    let unquote = |value: &'a str| value.trim().trim_matches(['"', '\'']);

    let mut found = None;
    // The item being read: whether it is the entry wanted, what its config
    // says, the indent of its own keys, and that of its config's.
    let mut wanted = false;
    let mut value = None;
    let mut keys = None;
    let mut config = None;

    for line in text.trim_start_matches('\u{feff}').lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let (indent, body) = match line.strip_prefix('-') {
            Some(rest) => {
                if wanted {
                    found = value.or(found);
                }
                wanted = false;
                value = None;
                config = None;
                keys = None;

                let body = rest.trim_start();
                if body.is_empty() {
                    continue;
                }
                (line.len() - body.len(), body.trim_end())
            }
            None => (line.len() - line.trim_start().len(), trimmed),
        };

        let own = *keys.get_or_insert(indent);
        if indent == own {
            config = None;
            if let Some(id) = body.strip_prefix("id:") {
                wanted = unquote(id) == entry;
            } else if body == "config:" {
                // Its children's indent, once the first of them says.
                config = Some(None);
            }
            continue;
        }

        let Some(children) = config.as_mut() else {
            continue;
        };
        if indent == *children.get_or_insert(indent) {
            if let Some(found) = body.strip_prefix(&key) {
                value = Some(unquote(found));
            }
        }
    }

    if wanted {
        found = value.or(found);
    }
    found
}

/// The preference a value names. Anything that is not one of the three known
/// values is `None`, the same as saying nothing.
fn named(value: &str) -> Option<Preference> {
    match value {
        "light" => Some(Preference::Light),
        "dark" => Some(Preference::Dark),
        "system" => Some(Preference::System),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{lookup, named, Preference};

    fn parse(text: &str) -> Option<Preference> {
        named(&lookup(text, true, "ui-theme", "preference")?)
    }

    fn patched(text: &str) -> Option<Preference> {
        named(&lookup(text, false, "ui-theme", "preference")?)
    }

    /// The shape dsh 0.2 writes, between two other entries.
    #[test]
    fn reads_the_profile_patch() {
        let patch = "\
# Your patch layer for this dsh profile
- id: agent-default-model
  name: \"@deepseek-ai/dsh-agent-default-model\"
  config:
    provider: agy
- id: ui-theme
  name: \"@deepseek-ai/dsh-client-ui-theme\"
  config:
    preference: dark
- id: next-prompt
  name: dsh-ghost-prompt
  config:
    preference: light
";
        assert_eq!(patched(patch), Some(Preference::Dark));
    }

    #[test]
    fn reads_the_patch_with_the_id_not_first() {
        let patch = "\
- name: '@deepseek-ai/dsh-client-ui-theme'
  config:
    fontSize: 15
    preference: light
  id: ui-theme
";
        assert_eq!(patched(patch), Some(Preference::Light));
    }

    #[test]
    fn ignores_a_preference_deeper_than_the_config() {
        let patch = "\
- id: ui-theme
  config:
    fonts:
      preference: dark
    fontSize: 15
";
        assert_eq!(patched(patch), None);
    }

    #[test]
    fn takes_the_later_of_two_patches_to_one_entry() {
        let patch = "\
- id: ui-theme
  config:
    preference: dark
- id: other
- id: ui-theme
  config:
    preference: light
- id: ui-theme
  disabled: false
";
        assert_eq!(patched(patch), Some(Preference::Light));
    }

    #[test]
    fn has_nothing_to_say_without_the_entry() {
        assert_eq!(patched("- id: next-prompt\n  config:\n    preference: dark\n"), None);
        assert_eq!(patched("- id: ui-theme\n  name: x\n"), None);
    }

    #[test]
    fn reads_the_field() {
        assert_eq!(
            parse("ui-theme:\n  preference: dark\n"),
            Some(Preference::Dark)
        );
    }

    #[test]
    fn reads_the_field_behind_a_bom() {
        assert_eq!(
            parse("\u{feff}ui-theme:\n  preference: dark\n"),
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
