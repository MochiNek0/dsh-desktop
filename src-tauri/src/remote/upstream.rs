//! The gateway's own session with dsh, and the headers that keep the two sides
//! apart.
//!
//! dsh is not being asked to trust the phone. It is being talked to exactly as
//! the desktop webview talks to it — from loopback, with a cookie dsh minted for
//! loopback — and the phone never sees any of that. So there are two sessions
//! stacked here: the device's, which [`crate::remote::session`] signs, and this
//! one, which dsh signs and which lives and dies in this process.
//!
//! ## Getting it
//!
//! `dsh web` prints one URL per launch with a token on it, and `GET /?token=…`
//! answers 303 with the cookie everything after that is authenticated by. The
//! token is the *process's* rather than one use of it — see
//! [`crate::auth`], which spends it a second time from inside the webview — so
//! this is free to spend it a third.
//!
//! ## Why it must not leak
//!
//! dsh binds that cookie to the authority it was minted for, in the signed
//! payload and in the cookie's own name. Handed to the phone it would be a
//! cookie for `127.0.0.1:<dsh port>` sitting in a jar keyed by a LAN address:
//! never sent back, never usable, and — because the name carries dsh's token —
//! one more entry in a jar this app already has a module about pruning (see
//! [`crate::cookies`]). So every `Set-Cookie` dsh sends is dropped here, without
//! exception, and the only cookie the phone ever receives is the one this
//! gateway signed itself.
//!
//! ## Why it has to be got again
//!
//! The token belongs to the dsh process. `crate::resume` restarts `dsh web`
//! after a crash, often on the same port, and the new process mints a new token
//! and will not accept the old cookie. Nothing about that is visible from the
//! socket — the port answers either way — so a gateway that cached the cookie
//! forever would work perfectly until the first time dsh died, and then serve
//! dsh's 401 page to a phone with no way to reopen anything.
//!
//! [`Upstream::ready`] is what closes that: it is called with every URL dsh
//! prints, and it drops the held cookie on the spot. The next request through
//! the proxy notices there is none and exchanges again.

use std::sync::Mutex;

use http::header::{
    HeaderMap, HeaderValue, CONNECTION, COOKIE, HOST, LOCATION, ORIGIN, SET_COOKIE,
};
use tauri::Url;

/// Headers that describe one hop of a connection rather than the message on it,
/// and so must not be copied to the next hop. RFC 9110 §7.6.1, plus `Upgrade`
/// and the two `Sec-WebSocket-*` negotiation headers, which are handled by the
/// upgrade path in [`crate::remote::proxy`] rather than by the ordinary one.
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Where dsh is and how to be it.
#[derive(Default)]
pub struct Upstream {
    /// The URL dsh last printed, token and all. `None` while no dsh is serving,
    /// which is the whole of "the gateway has nothing to forward to".
    launch: Mutex<Option<Url>>,
    /// The `Cookie` header value to send upstream, once one has been exchanged
    /// for. `None` means the exchange has to be done — either because it never
    /// has been, or because [`Upstream::ready`] threw the last one away.
    cookie: Mutex<Option<String>>,
}

impl Upstream {
    /// dsh is serving at this URL. Called for every URL dsh prints, first
    /// launch and every restart after it.
    ///
    /// The cookie goes whether or not the URL changed. A dsh that came back on
    /// the same port is the case this is really for: nothing about the address
    /// moved, and the token behind it is nonetheless a different one.
    pub fn ready(&self, url: &Url) {
        *self.launch.lock().unwrap() = Some(url.clone());
        *self.cookie.lock().unwrap() = None;
    }

    /// No dsh is serving. Forwarding stops until there is one again.
    pub fn gone(&self) {
        *self.launch.lock().unwrap() = None;
        *self.cookie.lock().unwrap() = None;
    }

    /// `127.0.0.1:<port>`, or `None` when no dsh is serving.
    pub fn authority(&self) -> Option<String> {
        let launch = self.launch.lock().unwrap();
        let url = launch.as_ref()?;
        Some(format!("{}:{}", url.host_str()?, url.port()?))
    }

    /// The URL to redo the token exchange against.
    pub fn launch_url(&self) -> Option<Url> {
        self.launch.lock().unwrap().clone()
    }

    /// The cookie to send upstream, if one has been exchanged for.
    pub fn cookie(&self) -> Option<String> {
        self.cookie.lock().unwrap().clone()
    }

    /// Remember what the exchange came back with.
    pub fn hold(&self, cookie: String) {
        *self.cookie.lock().unwrap() = Some(cookie);
    }
}

/// Turn the phone's request into one from this machine's own browser.
///
/// Three things happen, and all three are the difference between a proxy and a
/// hole in a firewall:
///
/// - `Host` and `Origin` become dsh's own authority, which is what makes dsh
///   read the request as loopback. This is the rewrite that disarms dsh's fence,
///   and it happens only after [`crate::remote::trust::provenance`] has already
///   answered for the request that is being rewritten.
/// - The phone's `Cookie` header is replaced outright rather than added to. The
///   phone's jar holds this gateway's `dsh_mobile_session`, which dsh has no use
///   for and no business seeing.
/// - Hop-by-hop headers are dropped, because this is a new hop.
///
/// `sec-fetch-site` is deliberately left as the browser wrote it. dsh checks it
/// too, and passing it through means dsh's answer agrees with the one already
/// given here rather than being computed from a header this process invented.
pub fn rewrite_request(headers: &mut HeaderMap, authority: &str, cookie: Option<&str>) {
    drop_hop_by_hop(headers);

    if let Ok(value) = HeaderValue::from_str(authority) {
        headers.insert(HOST, value);
    }
    if headers.contains_key(ORIGIN) {
        if let Ok(value) = HeaderValue::from_str(&format!("http://{authority}")) {
            headers.insert(ORIGIN, value);
        }
    }

    headers.remove(COOKIE);
    if let Some(cookie) = cookie {
        if let Ok(value) = HeaderValue::from_str(cookie) {
            headers.insert(COOKIE, value);
        }
    }
}

/// Turn dsh's answer into one the phone can hold on to.
///
/// `Set-Cookie` goes, every one of them — see the module docs. A `Location` that
/// names dsh's own authority is made relative, since the phone cannot reach
/// `127.0.0.1:<dsh port>` and would follow it into nothing; dsh's own 303 after
/// the token exchange is already relative, so this is for whatever else a
/// future dsh redirects.
///
/// `upgraded` is a 101, where `Connection` and `Upgrade` are not headers to be
/// dropped as hop-by-hop but the entire content of the answer. Everything else
/// about the response is treated the same either way — a 101 that somehow
/// carried a `Set-Cookie` would still lose it.
pub fn rewrite_response(headers: &mut HeaderMap, authority: &str, upgraded: bool) {
    headers.remove(SET_COOKIE);
    if !upgraded {
        drop_hop_by_hop(headers);
    }

    let Some(location) = headers.get(LOCATION).and_then(|value| value.to_str().ok()) else {
        return;
    };
    let Some(rest) = location
        .strip_prefix(&format!("http://{authority}"))
        .or_else(|| location.strip_prefix(&format!("https://{authority}")))
    else {
        return;
    };

    let relative = if rest.is_empty() { "/" } else { rest };
    if let Ok(value) = HeaderValue::from_str(relative) {
        headers.insert(LOCATION, value);
    }
}

/// Drop the headers that belong to the connection rather than to the message,
/// including whatever the `Connection` header itself nominated.
fn drop_hop_by_hop(headers: &mut HeaderMap) {
    let nominated: Vec<String> = headers
        .get_all(CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|name| name.trim().to_ascii_lowercase())
        .filter(|name| !name.is_empty())
        .collect();

    for name in HOP_BY_HOP
        .iter()
        .map(|name| name.to_string())
        .chain(nominated)
    {
        headers.remove(&name);
    }
}

/// The cookie to send upstream, out of whatever the 303 set.
///
/// dsh's cookie is named for its own launch token, so there is nothing constant
/// to look for; what is taken is the name and value of every `Set-Cookie` on the
/// response, joined the way a browser would send them back. The attributes are
/// dropped, which is what a `Cookie` header is: a jar sends names and values and
/// nothing else.
pub fn cookie_from(headers: &HeaderMap) -> Option<String> {
    let jar: Vec<&str> = headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|value| value.split(';').next())
        .map(str::trim)
        .filter(|pair| pair.contains('='))
        .collect();

    (!jar.is_empty()).then(|| jar.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::header::{HeaderName, REFERER, USER_AGENT};

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        headers
    }

    #[test]
    fn the_request_arrives_at_dsh_looking_like_loopback() {
        let mut request = headers(&[
            ("host", "192.168.1.100:59000"),
            ("origin", "http://192.168.1.100:59000"),
        ]);
        rewrite_request(&mut request, "127.0.0.1:3080", Some("dsh-auth-ab=cd"));

        assert_eq!(request[HOST], "127.0.0.1:3080");
        assert_eq!(request[ORIGIN], "http://127.0.0.1:3080");
        assert_eq!(request[COOKIE], "dsh-auth-ab=cd");
    }

    /// A request that had no `Origin` is not given one. The header's absence is
    /// information — it is what a typed address looks like — and inventing one
    /// would be answering a question dsh did not ask.
    #[test]
    fn an_absent_origin_stays_absent() {
        let mut request = headers(&[("host", "192.168.1.100:59000")]);
        rewrite_request(&mut request, "127.0.0.1:3080", None);

        assert!(!request.contains_key(ORIGIN));
    }

    /// The phone's jar holds this gateway's own session cookie. dsh must not
    /// see it, and must not see it even when there is nothing to put in its
    /// place.
    #[test]
    fn the_phones_cookies_never_reach_dsh() {
        let mut request = headers(&[
            ("host", "192.168.1.100:59000"),
            ("cookie", "dsh_mobile_session=v1.d1.0.mac; other=1"),
        ]);
        rewrite_request(&mut request, "127.0.0.1:3080", None);

        assert!(!request.contains_key(COOKIE));
    }

    /// What the browser said about the request's provenance is dsh's to read
    /// too. It is the one header here that is not rewritten.
    #[test]
    fn what_the_browser_said_is_passed_through() {
        let mut request = headers(&[
            ("host", "192.168.1.100:59000"),
            ("sec-fetch-site", "same-origin"),
            ("user-agent", "iPhone"),
        ]);
        rewrite_request(&mut request, "127.0.0.1:3080", None);

        assert_eq!(request["sec-fetch-site"], "same-origin");
        assert_eq!(request[USER_AGENT], "iPhone");
    }

    #[test]
    fn hop_by_hop_headers_do_not_cross_the_gateway() {
        let mut request = headers(&[
            ("host", "192.168.1.100:59000"),
            ("connection", "keep-alive, x-private"),
            ("keep-alive", "timeout=5"),
            ("x-private", "secret"),
            ("transfer-encoding", "chunked"),
        ]);
        rewrite_request(&mut request, "127.0.0.1:3080", None);

        for gone in ["connection", "keep-alive", "x-private", "transfer-encoding"] {
            assert!(!request.contains_key(gone), "{gone} is hop-by-hop");
        }
    }

    /// The single most important line in the module: dsh's cookie is bound to
    /// dsh's authority, and a phone that stored one would be storing rubbish
    /// that dsh's own name-per-token scheme never cleans up.
    #[test]
    fn dshs_cookies_never_reach_the_phone() {
        let mut response = headers(&[
            ("set-cookie", "dsh-auth-abcd=signed; Path=/; HttpOnly"),
            ("set-cookie", "another=1"),
            ("content-type", "text/html"),
        ]);
        rewrite_response(&mut response, "127.0.0.1:3080", false);

        assert!(!response.contains_key(SET_COOKIE));
        assert_eq!(response["content-type"], "text/html");
    }

    #[test]
    fn a_redirect_to_dsh_itself_is_made_relative() {
        let mut response = headers(&[("location", "http://127.0.0.1:3080/sessions/1")]);
        rewrite_response(&mut response, "127.0.0.1:3080", false);
        assert_eq!(response[LOCATION], "/sessions/1");

        let mut response = headers(&[("location", "http://127.0.0.1:3080")]);
        rewrite_response(&mut response, "127.0.0.1:3080", false);
        assert_eq!(response[LOCATION], "/");
    }

    /// Everything else is left exactly as dsh wrote it — a relative redirect,
    /// which is what dsh's own 303 sends, and a link to somewhere that is not
    /// dsh.
    #[test]
    fn other_redirects_are_left_alone() {
        for location in ["/", "https://deepseek.com/docs", "http://127.0.0.1:9999/x"] {
            let mut response = headers(&[("location", location)]);
            rewrite_response(&mut response, "127.0.0.1:3080", false);
            assert_eq!(response[LOCATION], location);
        }
    }

    /// dsh names its cookie after the launch token, so there is no fixed name
    /// to look for. What goes back upstream is every pair, without attributes —
    /// which is what a browser would have sent.
    #[test]
    fn the_exchanged_cookie_is_what_a_browser_would_send_back() {
        let response = headers(&[
            (
                "set-cookie",
                "dsh-auth-Ab_9=signed-value; Path=/; HttpOnly; SameSite=Strict",
            ),
            ("set-cookie", "extra=two; Path=/"),
        ]);

        assert_eq!(
            cookie_from(&response).as_deref(),
            Some("dsh-auth-Ab_9=signed-value; extra=two")
        );
    }

    #[test]
    fn a_response_that_set_nothing_yields_nothing() {
        assert_eq!(
            cookie_from(&headers(&[("content-type", "text/html")])),
            None
        );
        assert_eq!(cookie_from(&headers(&[("set-cookie", "deleted")])), None);
    }

    /// A restart is the path that only breaks once dsh has died, so it is the
    /// one worth a test: whatever was held is gone the moment a dsh says it is
    /// serving, same port or not.
    #[test]
    fn a_dsh_that_came_back_invalidates_the_held_cookie() {
        let upstream = Upstream::default();
        upstream.ready(&"http://127.0.0.1:3080/?token=first".parse().unwrap());
        upstream.hold("dsh-auth-first=x".into());
        assert!(upstream.cookie().is_some());

        upstream.ready(&"http://127.0.0.1:3080/?token=second".parse().unwrap());
        assert_eq!(upstream.cookie(), None, "the token behind it changed");
        assert_eq!(upstream.authority().as_deref(), Some("127.0.0.1:3080"));
    }

    #[test]
    fn nothing_is_forwarded_when_no_dsh_is_serving() {
        let upstream = Upstream::default();
        assert_eq!(upstream.authority(), None);

        upstream.ready(&"http://127.0.0.1:3080/?token=t".parse().unwrap());
        upstream.gone();
        assert_eq!(upstream.authority(), None);
        assert_eq!(upstream.launch_url(), None);
    }

    /// `Referer` is not rewritten, and that is a decision rather than an
    /// omission: dsh's fence does not read it, so rewriting it would be this
    /// process editing a header nothing downstream consults. Written down as a
    /// test so that the next person to notice it finds the answer rather than
    /// the question.
    #[test]
    fn the_referer_is_left_alone() {
        let mut request = headers(&[
            ("host", "192.168.1.100:59000"),
            ("referer", "http://192.168.1.100:59000/sessions"),
        ]);
        rewrite_request(&mut request, "127.0.0.1:3080", None);

        assert_eq!(request[REFERER], "http://192.168.1.100:59000/sessions");
    }
}
