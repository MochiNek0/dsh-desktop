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
//!
//! ## What the user sees while that happens
//!
//! [`Retry::recover`] cannot run any earlier than it does. The token is the
//! app's to remember, initialization scripts are fixed when the window is built
//! — long before any dsh has printed one — and the 401 is served on the `/` the
//! 303 redirected to, so the token is not in its address either. By the time
//! there is a page to run the exchange from, that page has been painted, and on
//! Linux every launch showed dsh's refusal for as long as a fetch and a
//! navigation take.
//!
//! [`shield`] is the half that can run at document start, and it is the half
//! with no token in it: it paints that one document the colour the window
//! already is and takes its text off the screen, so the gap between the loading
//! page and dsh's index reads as one background rather than as a white page
//! with an error on it. It comes off again the moment there is nothing left to
//! try — a fetch that failed, or a page load with no exchange armed — because
//! from there dsh's own message is the best thing on the screen.

use std::sync::{Arc, Mutex};

use tauri::{Url, WebviewWindow};

/// The global [`shield`] leaves behind for [`Retry::recover`] to call, and the
/// only thing the two have to agree about. Defined on dsh's refusal and nowhere
/// else, since that is the only document the shield covers.
const UNCOVER: &str = "window.__DSH_AUTH_SHIELD__";

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
    ///
    /// Runs on every page load rather than only on an armed one: an unarmed
    /// load is a page nothing is going to replace, and [`shield`] has to be
    /// told so before the user is left looking at a document with its text
    /// hidden.
    pub fn recover(&self, window: &WebviewWindow) {
        let armed = self.0.lock().unwrap().take();
        let _ = window.eval(script(armed.as_deref()));
    }
}

/// The document-start script, for the window's `initialization_script`.
///
/// Runs on every page this window loads and does nothing on almost all of them:
/// dsh's index and this app's own loading page are both `text/html`, and the
/// cover only ever goes over what is not.
///
/// The colour is picked by the media query rather than passed in, so it is the
/// one the window is actually answering at the moment the document loads —
/// which is dsh's own preference, kept true for the life of the app by
/// [`crate::theme::watch`]. Both sides of it come from the constants the window
/// background is set from, so the covered page is the colour of the gap it sits
/// in rather than a near miss.
pub fn shield() -> String {
    format!(
        r#"(function () {{
  // dsh's index, or this app's loading page. Neither is covered.
  if (document.contentType === 'text/html') return;

  var style = document.createElement('style');
  style.textContent =
    ':root{{background:' +
    (window.matchMedia('(prefers-color-scheme: dark)').matches ? '{dark}' : '{light}') +
    ' !important}}body{{visibility:hidden !important}}';

  // At document start the root element is there for every document this window
  // loads. The listener is the fallback for one where it is not, and costs a
  // late cover rather than no cover.
  function cover() {{
    if (style.parentNode) return true;
    if (!document.documentElement) return false;
    document.documentElement.appendChild(style);
    return true;
  }}
  if (!cover()) document.addEventListener('readystatechange', cover, true);

  // What `recover` calls once there is nothing left to try. dsh's own message
  // is what the page says, and the user should be able to read it.
  {uncover} = function () {{
    document.removeEventListener('readystatechange', cover, true);
    if (style.parentNode) style.parentNode.removeChild(style);
  }};
}})();"#,
        dark = crate::theme::dark_css(),
        light = crate::theme::light_css(),
        uncover = UNCOVER,
    )
}

/// What [`Retry::recover`] evaluates: the exchange when one is armed, and the
/// uncover on its own when none is.
fn script(url: Option<&str>) -> String {
    let Some(url) = url else {
        return format!(
            r#"(function () {{
  // Nothing armed, so nothing is going to replace this page.
  if ({uncover}) {uncover}();
}})();"#,
            uncover = UNCOVER,
        );
    };

    let url = serde_json::to_string(url).expect("a string is always serializable");
    format!(
        r#"(function () {{
  // dsh's own index, which is what an exchange that went through looks like.
  // The cover comes off it rather than being left on: a shield that read the
  // type differently at document start than this reads it here would otherwise
  // leave a window with nothing in it at all.
  if (document.contentType === 'text/html') {{ if ({uncover}) {uncover}(); return; }}

  // Same-origin, so the cookie the 303 carries is stored the way any other
  // first-party cookie is. Where the redirect it follows ends up does not
  // matter — the jar is what this is for.
  fetch({url}, {{ credentials: 'same-origin', cache: 'no-store' }})
    .then(function () {{ location.replace('/'); }})
    // Nothing left to try. The page dsh served says what happened, so the
    // cover comes off it.
    .catch(function () {{ if ({uncover}) {uncover}(); }});
}})();"#,
        url = url,
        uncover = UNCOVER,
    )
}

#[cfg(test)]
mod tests {
    use super::{script, shield, Retry, UNCOVER};

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
        let script = script(Some("http://127.0.0.1:3080/?token=ab_cd\"');alert(1);//"));
        assert!(
            script.contains(r#"fetch("http://127.0.0.1:3080/?token=ab_cd\"');alert(1);//""#),
            "the URL must be a JSON string: {script}"
        );
    }

    /// dsh's index is `text/html`, and a window already showing it is a window
    /// with nothing to recover — and, since nothing covered it, nothing the
    /// uncover has to do either.
    #[test]
    fn html_is_left_alone() {
        let armed = script(Some("http://127.0.0.1:3080/"));
        assert!(
            armed.contains(&format!("=== 'text/html') {{ if ({UNCOVER}) {UNCOVER}(); return; }}")),
            "an html page is uncovered and left: {armed}"
        );
        assert!(shield().contains("=== 'text/html') return"));
    }

    /// A page load with nothing armed is the end of the road for whatever the
    /// window is showing, so it uncovers rather than tries.
    #[test]
    fn an_unarmed_load_only_uncovers() {
        let script = script(None);
        assert!(!script.contains("fetch("), "nothing to fetch: {script}");
        assert!(script.contains(&format!("if ({UNCOVER}) {UNCOVER}();")));
    }

    /// The two scripts are evaluated separately into one document and share
    /// exactly one name. Spelled differently in either of them the cover would
    /// go on and never come off, and the failure is a blank window rather than
    /// anything that says what happened.
    #[test]
    fn the_cover_comes_off_by_the_name_it_went_on_with() {
        assert!(shield().contains(&format!("{UNCOVER} = function ()")));
        assert!(script(None).contains(UNCOVER));
        assert!(script(Some("http://127.0.0.1:3080/")).contains(UNCOVER));
    }

    /// The colours are written into the script by `format!` rather than read
    /// from it, and a placeholder that stopped being substituted would paint
    /// the one document nobody is meant to read in whatever the UA's default
    /// is — which is the white page this exists to stop showing.
    #[test]
    fn both_themes_reach_the_script() {
        let shield = shield();
        assert!(
            shield.contains(&format!("? '{}'", crate::theme::dark_css())),
            "the dark background must be substituted: {shield}"
        );
        assert!(
            shield.contains(&format!(": '{}'", crate::theme::light_css())),
            "the light background must be substituted: {shield}"
        );
    }

    /// Every brace in a script body is doubled for `format!`, and one that is
    /// not is a syntax error this app would ship without noticing: the scripts
    /// are evaluated by the webview, not by the compiler. Balance is the
    /// cheapest check that catches an unescaped `{` having eaten the rest of a
    /// line.
    #[test]
    fn the_scripts_are_balanced() {
        for script in [shield(), script(None), script(Some("http://127.0.0.1:3080/"))] {
            let mut depth = 0i32;
            for character in script.chars() {
                match character {
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    _ => {}
                }
                assert!(depth >= 0, "a closing brace with nothing open: {script}");
            }
            assert_eq!(depth, 0, "unbalanced braces: {script}");
        }
    }
}
