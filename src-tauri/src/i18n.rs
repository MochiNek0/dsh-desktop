//! Two languages, taken from the language dsh is in.
//!
//! Every string the user reads is written twice, inline, at the place it is
//! used — see [`t`]. The alternative is a key table somewhere else, and a key
//! table is a second thing to keep in step: a string in it has to be looked up
//! again to be read, a call site cannot be understood on its own, and a key that
//! stops being used stays there forever, translated. Both halves side by side
//! means neither can drift from the other unnoticed.
//!
//! There is no language setting here, and no menu item to add one. There is one
//! already, in dsh's own Settings → General, and dsh writes what was picked into
//! the same `settings.yaml` the theme is read out of — see [`crate::theme`].
//! This app is a frame around that page, so the frame asks the page's own
//! setting rather than asking the question a second time in a second place. The
//! system locale is what is left when dsh has not been asked, which is also
//! where dsh itself starts.
//!
//! The file answers the question at startup. After that the page does:
//! switching the language inside dsh swaps its copy live without reloading the
//! document, and [`crate::controls`] watches `<html lang>` for exactly that and
//! calls [`switch`]. Which is why this is not settled once — see the note there
//! about what a change has to repaint.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

/// Whether the Chinese half of each pair is the one shown.
///
/// Every `t!` in the app goes through here, so it is an atomic load and the
/// file is read once, behind the [`OnceLock`], on the first call.
pub fn chinese() -> bool {
    cell().load(Ordering::Relaxed)
}

/// Take the language from the page, which has just switched to it. `true` when
/// that was a change.
///
/// A change is the caller's cue to repaint, and the caller has to, because
/// making this move is only half of it. Strings that go through `t!` when they
/// are needed — every dialog, every notification, every status line — come out
/// in the new language on their own. Strings already drawn do not: the menu
/// labels were baked into the injected script when the document loaded, and the
/// tray menu's were built when the app started. See [`crate::controls::relabel`]
/// and the tray in `main`.
pub fn switch(tag: &str) -> bool {
    let chinese = is_chinese(tag);
    cell().swap(chinese, Ordering::Relaxed) != chinese
}

/// What the injected scripts and the loading page switch on; see
/// `dist/index.html`. The pages carry their own strings — pushing every label
/// across from Rust would mean a `window.eval` per word.
pub fn tag() -> &'static str {
    if chinese() {
        "zh"
    } else {
        "en"
    }
}

fn cell() -> &'static AtomicBool {
    static CHINESE: OnceLock<AtomicBool> = OnceLock::new();
    CHINESE.get_or_init(|| AtomicBool::new(settled()))
}

/// The language to open in, before any page has had a chance to say otherwise.
///
/// dsh ships `zh` and `en`, and lets plugins register further languages whose
/// fallback chain has to terminate at `en`. There are two halves here, so
/// anything that is not Chinese gets the English one — which is where dsh's own
/// chain would have ended up too.
fn settled() -> bool {
    match dsh_locale() {
        Some(locale) => is_chinese(&locale),
        // A locale nothing can be made of answers Chinese, which is what every
        // string in this app was before there were two of them.
        None => sys_locale::get_locale().is_none_or(|locale| is_chinese(&locale)),
    }
}

/// `zh`, `zh-CN`, `zh-Hans-CN`: the language subtag is the whole question.
fn is_chinese(locale: &str) -> bool {
    locale.to_ascii_lowercase().starts_with("zh")
}

/// The language dsh would show right now, or `None` when its settings say
/// nothing about it.
///
/// Nothing is one of two things, and this deliberately does not tell them
/// apart, because the answer is the same either way. The key is absent until
/// someone picks a language — until then dsh takes the browser's, which in this
/// window is the system's, so falling back to the system locale is agreeing
/// with dsh rather than guessing past it. And a file that cannot be read at all
/// is dsh's to complain about, not a reason for the frame around it to refuse
/// to draw.
fn dsh_locale() -> Option<String> {
    let text = std::fs::read_to_string(crate::theme::settings_file()?).ok()?;
    parse(&text)
}

/// Read `locale.preference` out of the settings document.
///
/// The same scan [`crate::theme`] reads `ui-theme.preference` with, for the
/// same reason: every namespace is a top-level key with its section indented
/// under it, and one string out of a file whose other sections belong to
/// plugins is not worth pulling in a YAML parser for. Anything the scan does
/// not recognise leaves the answer `None`, which is the system locale — what
/// this did before there was a preference to read.
fn parse(text: &str) -> Option<String> {
    let mut section = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if !line.starts_with([' ', '\t']) {
            section = trimmed == "locale:";
            continue;
        }
        if !section {
            continue;
        }

        if let Some(value) = trimmed.strip_prefix("preference:") {
            let value = value.trim().trim_matches(['"', '\'']);
            return (!value.is_empty()).then(|| value.to_string());
        }
    }

    None
}

/// One string in both languages, Chinese first.
///
/// With no further arguments it is a `&'static str`, so it can go anywhere a
/// literal could. With them both halves are format strings and the result is a
/// `String` — the arguments are evaluated once, by whichever half runs.
///
/// ```ignore
/// note(app, t!("找不到 dsh", "dsh not found"), &detail);
/// report(&t!("正在安装 {}…", "Installing {}…", name), -1.0);
/// ```
macro_rules! t {
    ($zh:literal, $en:literal) => {
        if $crate::i18n::chinese() {
            $zh
        } else {
            $en
        }
    };
    ($zh:literal, $en:literal, $($arg:tt)+) => {
        if $crate::i18n::chinese() {
            format!($zh, $($arg)+)
        } else {
            format!($en, $($arg)+)
        }
    };
}

#[cfg(test)]
mod tests {
    use super::{is_chinese, parse};

    /// Both arms compile and pick the same half, whichever half that is.
    #[test]
    fn both_arms_agree() {
        let bare = t!("中文", "English");
        let formatted = t!("中文 {}", "English {}", 1);

        assert_eq!(formatted, format!("{bare} 1"));
    }

    #[test]
    fn the_tag_names_the_half_that_won() {
        let expected = if super::chinese() { "zh" } else { "en" };

        assert_eq!(super::tag(), expected);
    }

    #[test]
    fn reads_the_language_dsh_was_set_to() {
        assert_eq!(parse("locale:\n  preference: en\n").as_deref(), Some("en"));
    }

    #[test]
    fn reads_it_between_other_sections() {
        let settings = "\
ui-theme:
  preference: system
locale:
  preference: zh
web-search-free:
  provider: tavily
";
        assert_eq!(parse(settings).as_deref(), Some("zh"));
    }

    /// `ui-theme` has a `preference` of its own, and in a real file it is the
    /// section directly above this one.
    #[test]
    fn ignores_the_field_in_another_section() {
        let settings = "\
ui-theme:
  preference: dark
agent-default-model:
  provider: tokenrhythm
";
        assert_eq!(parse(settings), None);
    }

    #[test]
    fn has_nothing_to_say_before_a_language_was_picked() {
        assert_eq!(parse("ui-onboarding:\n  welcomeNoticeVersion: 1\n"), None);
    }

    #[test]
    fn takes_a_quoted_value_and_declines_an_empty_one() {
        assert_eq!(
            parse("locale:\n  preference: \"en\"\n").as_deref(),
            Some("en")
        );
        assert_eq!(parse("locale:\n  preference:\n"), None);
    }

    /// A language dsh registered from a plugin is not one of the two halves
    /// here, and dsh's own fallback chain has to end at `en` as well.
    #[test]
    fn only_chinese_is_the_chinese_half() {
        for locale in ["zh", "zh-CN", "zh-Hans-CN", "ZH"] {
            assert!(is_chinese(locale), "{locale} is Chinese");
        }
        for locale in ["en", "en-GB", "ja", "de", ""] {
            assert!(!is_chinese(locale), "{locale} is not Chinese");
        }
    }
}
