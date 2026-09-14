//! Taking dsh's spent launch cookies out of the jar before the window is sent
//! back to it.
//!
//! `dsh web` mints a token per launch and hands the browser a cookie named for
//! it — `dsh-auth-<token id>`, `HttpOnly`, `path=/`; see [`crate::auth`] for the
//! exchange that stores it. The name carries the token, so every launch stores a
//! *different* cookie rather than replacing one, and cookies are keyed by host
//! rather than by port: every `dsh web` this app has ever started is one more
//! cookie on `127.0.0.1`, and nothing takes one away again.
//!
//! A browser profile absorbs that, because it is shared with everything else the
//! machine browses and people clear it. This app's does not: it is its own, it
//! is only ever pointed at dsh, and it lives as long as the install.
//!
//! ## What it costs, and why it does not look like a cookie
//!
//! dsh serves every plugin's browser half as one combined script whose URL names
//! all of them at once — 2.6 KB of request line on an ordinary profile; see
//! [`crate::transport`]. That request therefore carries a larger header block
//! than any other the window makes, and it is the first to cross the 16 KB
//! `maxHeaderSize` Node answers `431 Request Header Fields Too Large` to. The
//! index is a short URL and still fits, so the page loads, dsh's boot card comes
//! up, and what it says is "Failed to load plugins" over a list of forty-odd
//! packages the user never installed. Nothing on screen is about a cookie.
//!
//! The three things a user does about that — restart dsh, reopen the app, remove
//! the plugin it seems to blame — each start `dsh web` again, so each one mints
//! another cookie. Every attempt at the repair makes the next request larger.
//! Enough of them and the index stops being served too, and the window has no
//! page at all.
//!
//! ## When they come off
//!
//! Before the window is navigated to dsh, because the header this is about is on
//! that navigation itself: a purge on the far side of it would be one page load
//! too late every time.
//!
//! And all of them rather than all but one. At that moment none of them is
//! current — the cookie this launch authenticates with is the one dsh has not
//! minted yet, and it is minted by the `?token=` request this is clearing the
//! way for.

use tauri::{Url, WebviewWindow};

/// The prefix on every cookie `dsh web` mints, and the whole of what this is
/// allowed to delete.
///
/// The rest of the name is the token's id, which is what makes each launch's
/// cookie a new one rather than a replacement — and so what makes them pile up.
/// Everything else on dsh's origin belongs to dsh or to a plugin, and clearing
/// the jar wholesale would be taking those too.
const SPENT: &str = "dsh-auth-";

/// Whether a cookie in dsh's jar is one of the launch cookies.
fn spent(name: &str) -> bool {
    name.starts_with(SPENT)
}

/// Take every spent launch cookie off `url`'s origin.
///
/// Best effort and quiet: a jar that cannot be read, or a cookie that will not
/// delete, is a start that carries one more header than it needed to rather than
/// a start that does not happen. Said on the terminal either way, because the
/// failure this prevents is invisible until it is total.
///
/// Must not run on the main thread. `cookies_for_url` posts to the event loop
/// and blocks until it answers, so asking from the thread that would deliver the
/// answer is a deadlock — the one Tauri documents against wry#583. Every caller
/// is downstream of the spawn in [`crate::start_serving`].
pub fn purge(window: &WebviewWindow, url: &Url) {
    let jar = match window.cookies_for_url(url.clone()) {
        Ok(jar) => jar,
        Err(error) => {
            eprintln!("dsh-desktop: could not read dsh's cookies: {error}");
            return;
        }
    };

    let mut gone = 0usize;
    for cookie in jar {
        if !spent(cookie.name()) {
            continue;
        }

        match window.delete_cookie(cookie) {
            Ok(()) => gone += 1,
            Err(error) => {
                eprintln!("dsh-desktop: could not delete a spent dsh cookie: {error}");
            }
        }
    }

    // Only when it did something. A line per launch saying nothing was there is
    // noise on the one stream a packaged app's user can be asked to read.
    if gone > 0 {
        eprintln!("dsh-desktop: dropped {gone} spent dsh login cookie(s) before opening dsh");
    }
}

#[cfg(test)]
mod tests {
    use super::spent;

    /// The shape dsh actually mints, which is the reason the prefix is a prefix
    /// and not a name: the tail is the launch's token id, and it is different
    /// every time.
    #[test]
    fn a_launch_cookie_is_spent() {
        assert!(spent("dsh-auth-dBvBAPAr1o4MpSRDfpNL5vfNMVzrWGYQgc1TppaaKVo"));
        assert!(spent("dsh-auth-BTR3-CAbdSwF_Ouymm-0ZcE6XFPtsamnPjrmCDvAvcE"));
    }

    /// Everything else on that origin is dsh's or a plugin's, and this is not
    /// entitled to it. A prefix that widened to `dsh-` would take them.
    #[test]
    fn nothing_else_is() {
        for name in ["dsh-theme", "dsh-locale", "dshmarket-token", "session"] {
            assert!(!spent(name), "{name} is not this module's to delete");
        }
    }
}
