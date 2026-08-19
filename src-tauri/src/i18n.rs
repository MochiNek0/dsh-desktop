//! Two languages, settled once from the system locale.
//!
//! Every string the user reads is written twice, inline, at the place it is
//! used — see [`t`]. The alternative is a key table somewhere else, and a key
//! table is a second thing to keep in step: a string in it has to be looked up
//! again to be read, a call site cannot be understood on its own, and a key that
//! stops being used stays there forever, translated. Both halves side by side
//! means neither can drift from the other unnoticed.
//!
//! There is no language setting, and no menu item to add one. The system
//! already carries the answer, and an app whose entire visible surface is
//! somebody else's web page — one that does its own language selection, in its
//! own settings — is not the place to ask the question a second time.

use std::sync::OnceLock;

/// Whether the Chinese half of each pair is the one shown.
///
/// Asked of the OS once. The locale cannot move under a running process in any
/// way worth repainting a window for, and this sits on paths — every dialog,
/// every status line — where asking again would be pure cost.
///
/// A locale nothing can be made of answers Chinese, which is what every string
/// in this app was before there were two of them.
pub fn chinese() -> bool {
    static CHINESE: OnceLock<bool> = OnceLock::new();
    *CHINESE.get_or_init(|| {
        sys_locale::get_locale()
            .is_none_or(|locale| locale.to_ascii_lowercase().starts_with("zh"))
    })
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
}
