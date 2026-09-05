//! Getting the window past the authentication `dsh web` puts in front of its
//! index.
//!
//! Since dsh 0.1.2 the URL it prints carries this dsh process's launch token.
//! `GET /?token=…` is answered 303 to a clean `/` along with the browser cookie
//! everything after that is authenticated by — signed, `HttpOnly`, and
//! `SameSite=Strict` — and an index request arriving without that cookie gets a
//! 401 whose entire body is one line of text telling the reader to reopen the
//! URL dsh printed.
//!
//! WebView2 walks that redirect with the cookie it was just handed, so on
//! Windows the exchange is invisible. WebKitGTK lands on the 401 instead, which
//! is what every Linux report of "dsh web authentication required; reopen the
//! URL printed by dsh web" is looking at. The navigation this app makes starts
//! on its own bundled loading page — another origin — and a `Strict` cookie is
//! withheld from any request the engine reads as cross-site: the hop the 303
//! sends the window on is credited to the page it came from rather than to
//! where it is going, and the cookie dsh has just minted sits out the one
//! request it was minted for.
//!
//! There is nothing to reopen here, but the exchange can be asked for again
//! from somewhere that counts: the 401 page is dsh's own, on dsh's own origin,
//! so a `fetch` from inside it is a plain same-origin request and the cookie it
//! comes back with is stored like any other. Once it is in the jar, `/` is one
//! ordinary navigation away — no redirect for anything to be confused by. The
//! token is the dsh process's rather than one use of it, so asking twice is
//! allowed.

use std::sync::{Arc, Mutex};

use tauri::{Url, WebviewWindow};

/// The URL to redo the exchange with, armed for one navigation.
///
/// The arm is spent by the next page load whether or not it needed it, so the
/// recovery runs at most once per navigation this app makes. A window still on
/// the 401 after that is looking at a token dsh will not take, or a cookie jar
/// that keeps nothing — neither of which a page that replaces itself forever
/// would fix, and both of which its own message describes better than a blank
/// window would.
#[derive(Clone, Default)]
pub struct Retry(Arc<Mutex<Option<String>>>);

impl Retry {
    /// Remember the URL a navigation about to be made may have to make again.
    /// Called before the navigation, since the page load that answers for it is
    /// what spends it.
    pub fn arm(&self, url: &Url) {
        *self.0.lock().unwrap() = Some(url.to_string());
    }

    /// Redo the exchange, if what the window ended up showing is not dsh.
    ///
    /// dsh serves its index as `text/html` and its refusal as `text/plain`,
    /// which is the whole of the test: it needs no agreement with dsh about the
    /// wording of a message, and a page that is neither of the two — an error
    /// page, a 404 — is one this window has no more use for than the 401.
    pub fn recover(&self, window: &WebviewWindow) {
        let Some(url) = self.0.lock().unwrap().take() else {
            return;
        };
        let _ = window.eval(script(&url));
    }
}

fn script(url: &str) -> String {
    let url = serde_json::to_string(url).expect("a string is always serializable");
    format!(
        r#"(function () {{
  // dsh's own index, which is what an exchange that went through looks like.
  if (document.contentType === 'text/html') return;

  // Same-origin, so the cookie the 303 carries is stored the way any other
  // first-party cookie is. Where the redirect it follows ends up does not
  // matter — the jar is what this is for.
  fetch({url}, {{ credentials: 'same-origin', cache: 'no-store' }})
    .then(function () {{ location.replace('/'); }})
    // Nothing left to try. The page dsh served says what happened.
    .catch(function () {{}});
}})();"#
    )
}

#[cfg(test)]
mod tests {
    use super::{script, Retry};

    /// The arm is one navigation's, and the page load that follows it takes it
    /// whether or not the script it builds does anything.
    #[test]
    fn one_arm_is_one_recovery() {
        let retry = Retry::default();
        retry.arm(&"http://127.0.0.1:3080/?token=ab_cd".parse().unwrap());

        assert!(retry.0.lock().unwrap().take().is_some());
        assert!(retry.0.lock().unwrap().take().is_none(), "the arm is spent");
    }

    /// The URL is the one thing in the script that is not a literal, and it
    /// carries a token: a quote or a backslash in it would end the string it is
    /// in, so it goes in as JSON rather than as text.
    #[test]
    fn the_url_is_quoted() {
        let script = script("http://127.0.0.1:3080/?token=ab_cd\"');alert(1);//");
        assert!(
            script.contains(r#"fetch("http://127.0.0.1:3080/?token=ab_cd\"');alert(1);//""#),
            "the URL must be a JSON string: {script}"
        );
    }

    /// dsh's index is `text/html`, and a window already showing it is a window
    /// with nothing to recover.
    #[test]
    fn html_is_left_alone() {
        assert!(script("http://127.0.0.1:3080/").contains("=== 'text/html') return"));
    }
}
