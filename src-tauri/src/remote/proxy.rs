//! The gateway itself: one listener on `0.0.0.0`, and every request on it
//! answered, refused or handed to dsh.
//!
//! ## Why a proxy rather than `dsh web --host 0.0.0.0`
//!
//! Because dsh says not to. `dsh web` has the flag, and its own documentation
//! lists binding a non-loopback address under known limitations: the server
//! carries no TLS, no authentication and no origin policy of its own, so the
//! flag publishes every route and every static asset to the whole network. What
//! it is missing is exactly what this module is — see
//! [`crate::remote::trust`] for the fence and [`crate::remote::session`] for
//! the handshake — so dsh keeps its loopback port and this stands in front.
//!
//! ## One upstream connection per request
//!
//! No pooling. dsh is on loopback, where a TCP connection costs a few tens of
//! microseconds and no packet leaves the machine, and a pool is a second piece
//! of state that has to be invalidated when dsh restarts — which is the failure
//! this whole module is most likely to get wrong (see
//! [`crate::remote::upstream`]). The simplicity is worth more than the
//! handshakes.
//!
//! ## WebSockets are copied, not parsed
//!
//! dsh's realtime channel is a WebSocket at `/api/remote.mux`, and everything
//! that makes this feature worth having — the thinking stream, the approval
//! prompts, the answer travelling back — is on it. Once the 101 has been
//! forwarded in both directions, the two sockets are joined with
//! [`tokio::io::copy_bidirectional`] and nothing here looks at a frame again.
//!
//! That is deliberate. A frame parser would be a second implementation of a
//! protocol dsh and its client already agree on, and it would have to be kept
//! in step with them forever. Copying bytes cannot be wrong about a frame it
//! never reads, and dsh's own heartbeat pings cross it without anything here
//! knowing what a ping is.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::header::{CONNECTION, COOKIE, HOST, LOCATION, ORIGIN, SET_COOKIE, UPGRADE, USER_AGENT};
use http::{HeaderValue, Method, Request, Response, StatusCode};
use http_body_util::{combinators::BoxBody, BodyExt, Empty, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::{TcpListener, TcpStream};

use super::session::COOKIE as DEVICE_COOKIE;
use super::{trust, upstream, Shared};

/// How long a phone waits at the pairing URL for somebody to answer the dialog
/// on the desktop.
///
/// Two minutes: long enough to walk to the computer, short enough that a phone
/// left on a table gives up rather than holding a socket open all afternoon. The
/// dialog outlives it — the desktop is free to answer a question the phone has
/// stopped waiting for, and the answer simply lands on a closed channel.
const PAIRING_WAIT: Duration = Duration::from_secs(120);

/// How long a device cookie is set to live in the phone's jar.
///
/// It has to agree with the store's own expiry, which is what actually decides:
/// this is the browser's copy of the same deadline, so that a phone that has
/// aged out stops sending a cookie rather than being told about it.
const COOKIE_MAX_AGE: u64 = 30 * 24 * 60 * 60;

/// What every response this module builds carries.
///
/// Boxed because the two kinds are different types — dsh's answer streams from
/// the upstream connection, ours is a string that is already complete — and a
/// service returns one type.
type Body = BoxBody<Bytes, Box<dyn std::error::Error + Send + Sync>>;

/// Accept until told to stop.
///
/// The shutdown channel is the app closing, and it matters that it is a channel
/// rather than a flag: the listener has to come out of `accept` to notice, and
/// `select!` is what makes it.
pub async fn serve(
    listener: TcpListener,
    shared: Arc<Shared>,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
) {
    loop {
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            _ = &mut shutdown => return,
        };

        let Ok((stream, peer)) = accepted else {
            // An accept that fails is one client's problem — a connection reset
            // between the SYN and here — not the listener's. Anything that
            // really takes the listener down ends the process, which takes this
            // task with it.
            continue;
        };

        // Counted before anything is read off it. What the card's silence watch
        // is asking is whether a packet ever arrived at all — a connection that
        // then fails the fence still answers that question, and a firewall
        // dropping the SYN is what makes the count stay at zero.
        shared
            .seen
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let shared = shared.clone();
        tokio::spawn(async move {
            let service = service_fn(move |request| handle(shared.clone(), peer, request));
            // `with_upgrades`, or the 101 this forwards is a status code and
            // nothing else: without it hyper never hands over the socket, and
            // the WebSocket dsh's client opens would connect and then go silent.
            let served = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .with_upgrades()
                .await;

            if let Err(error) = served {
                // Ordinary on this listener: a phone that locks its screen drops
                // the connection without closing it. Worth a line in the
                // terminal and nothing else.
                eprintln!("dsh-desktop: a remote connection ended: {error}");
            }
        });
    }
}

/// One request, from the fence to the answer.
///
/// `Infallible` because every failure below is a response. A service that
/// returns `Err` drops the connection with nothing on it, which from the phone
/// is indistinguishable from the app having been closed.
async fn handle(
    shared: Arc<Shared>,
    peer: SocketAddr,
    request: Request<Incoming>,
) -> Result<Response<Body>, Infallible> {
    // First and unconditionally. Everything after this point is allowed to
    // assume the request came from a browser talking to an address this machine
    // actually has.
    let authorities = shared.authorities();
    let refusal = trust::provenance(
        header(&request, HOST),
        header(&request, ORIGIN),
        header_named(&request, "sec-fetch-site"),
        &authorities,
    );
    if let Err(why) = refusal {
        eprintln!("dsh-desktop: refused a remote request from {peer}: {why}");
        return Ok(refused());
    }

    let device =
        cookie_value(&request, DEVICE_COOKIE).and_then(|value| shared.store.verify(&value));

    // A pairing URL from somebody who is already paired is a phone that scanned
    // the code twice. Sending it to `/` costs nobody a nonce and is what the
    // user meant by scanning.
    if let Some(token) = query_value(&request, "pair_token") {
        if device.is_some() {
            return Ok(redirect("/", None));
        }
        return Ok(pair(shared, peer, request, token).await);
    }

    if device.is_none() {
        return Ok(unauthenticated());
    }

    Ok(forward(shared, request).await)
}

/// The handshake: spend the nonce, ask the desktop, and answer with what the
/// human said.
///
/// The nonce is spent before the dialog goes up, not after it is answered. A
/// nonce that survived a refusal would let whoever was refused try again as
/// often as they liked, and the point of the desktop dialog is that it is asked
/// once.
async fn pair(
    shared: Arc<Shared>,
    peer: SocketAddr,
    request: Request<Incoming>,
    token: String,
) -> Response<Body> {
    if !shared.store.redeem_pair(&token) {
        return page(
            StatusCode::FORBIDDEN,
            t!("这个二维码已经失效", "This code is no longer valid"),
            t!(
                "配对码只有五分钟有效，而且只能用一次。回到电脑上重新打开「手机连接」再扫一次。",
                "A pairing code lasts five minutes and can be used once. \
                 Open Connect a phone on the computer again and scan the new one."
            ),
        );
    }

    let label = trust::label(header_named(&request, USER_AGENT.as_str()));
    let address = peer.ip().to_string();

    let answer = shared.approve.ask(&label, &address);
    let allowed = matches!(
        tokio::time::timeout(PAIRING_WAIT, answer).await,
        Ok(Ok(true))
    );

    if !allowed {
        return page(
            StatusCode::FORBIDDEN,
            t!("这台电脑拒绝了连接", "The computer turned this device away"),
            t!(
                "电脑上没有同意这次连接。需要的话，回到电脑重新打开「手机连接」再试一次。",
                "Nobody at the computer allowed this connection. \
                 Open Connect a phone there again to try once more."
            ),
        );
    }

    let (_device, cookie) = shared.store.authorize(label, address);
    super::paired(&shared);

    // 303 rather than serving the index here: the phone lands on a clean `/`
    // with the pairing nonce out of its address bar, so a reload or a bookmark
    // does not carry a spent token around forever. Exactly the shape dsh's own
    // token exchange uses, for exactly the same reason.
    redirect("/", Some(&cookie))
}

/// Hand the request to dsh and the answer back.
async fn forward(shared: Arc<Shared>, mut request: Request<Incoming>) -> Response<Body> {
    let Some(authority) = shared.upstream.authority() else {
        return page(
            StatusCode::SERVICE_UNAVAILABLE,
            t!("dsh 没有在运行", "dsh is not running"),
            t!(
                "电脑上的 dsh 停了。它一般会自己起来，起来之后刷新这个页面就行。",
                "dsh stopped on the computer. It usually starts itself again; \
                 reload this page once it has."
            ),
        );
    };

    let cookie = ensure_cookie(&shared).await;

    // Taken before the headers are rewritten, because the rewrite drops
    // `Connection` and `Upgrade` along with the other hop-by-hop headers — and
    // these two are the request.
    let upgrading = is_upgrade(
        request.headers().get(CONNECTION),
        request.headers().get(UPGRADE),
    );
    let upgrade_header = request.headers().get(UPGRADE).cloned();

    upstream::rewrite_request(request.headers_mut(), &authority, cookie.as_deref());
    if upgrading {
        request
            .headers_mut()
            .insert(CONNECTION, HeaderValue::from_static("Upgrade"));
        if let Some(value) = upgrade_header {
            request.headers_mut().insert(UPGRADE, value);
        }
    }

    // Reserved before the request is sent. `hyper::upgrade::on` has to be asked
    // for the socket while the request is still hyper's; once it has been sent
    // there is nothing left to ask.
    let downstream = upgrading.then(|| hyper::upgrade::on(&mut request));

    let sent = send(&authority, request).await;
    let Ok(mut response) = sent else {
        return page(
            StatusCode::BAD_GATEWAY,
            t!("连不上 dsh", "Could not reach dsh"),
            t!(
                "网关连不上电脑上的 dsh。过一会儿刷新试试。",
                "The gateway could not reach dsh on the computer. Try reloading in a moment."
            ),
        );
    };

    let switching = response.status() == StatusCode::SWITCHING_PROTOCOLS;
    if switching {
        if let Some(downstream) = downstream {
            join(downstream, hyper::upgrade::on(&mut response));
        }
    }

    upstream::rewrite_response(response.headers_mut(), &authority, switching);
    response.map(|body| body.map_err(box_error).boxed())
}

/// Once the 101 is through, the two sockets are one pipe.
///
/// Spawned rather than awaited: the 101 has to be returned to the phone first,
/// or there is no socket on this side to take over. Both futures resolve once
/// hyper has finished with their connections, which is after this function's
/// caller has sent the response.
fn join(downstream: hyper::upgrade::OnUpgrade, upstream_side: hyper::upgrade::OnUpgrade) {
    tokio::spawn(async move {
        let (Ok(phone), Ok(dsh)) = tokio::join!(downstream, upstream_side) else {
            return;
        };

        let mut phone = TokioIo::new(phone);
        let mut dsh = TokioIo::new(dsh);
        // Ends when either side closes, which is the phone navigating away or
        // dsh going down. Nothing to do about either but stop copying.
        let _ = tokio::io::copy_bidirectional(&mut phone, &mut dsh).await;
    });
}

/// The cookie to talk to dsh with, exchanging the launch token for one if there
/// is none.
///
/// Serialised on [`Shared::exchange`], and the held cookie is looked at again
/// *inside* the lock. A page load is thirty requests arriving at once; without
/// the second look, all thirty would arrive at an empty slot and redeem the
/// launch token thirty times over.
async fn ensure_cookie(shared: &Arc<Shared>) -> Option<String> {
    if let Some(held) = shared.upstream.cookie() {
        return Some(held);
    }

    let _exchanging = shared.exchange.lock().await;
    if let Some(held) = shared.upstream.cookie() {
        return Some(held);
    }

    let url = shared.upstream.launch_url()?;
    let authority = shared.upstream.authority()?;
    let target = match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_string(),
    };

    let request = Request::builder()
        .method(Method::GET)
        .uri(&target)
        .header(HOST, &authority)
        .body(Empty::<Bytes>::new())
        .ok()?;

    let response = send(&authority, request).await.ok()?;
    let cookie = upstream::cookie_from(response.headers())?;
    shared.upstream.hold(cookie.clone());
    Some(cookie)
}

/// One request to dsh, over a connection opened for it.
async fn send<B>(authority: &str, request: Request<B>) -> Result<Response<Incoming>, BoxedError>
where
    B: hyper::body::Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<BoxedError>,
{
    let stream = TcpStream::connect(authority).await?;
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;

    // The same `with_upgrades` as on the server side, and for the same reason:
    // without it the connection task finishes at the 101 and takes the socket
    // dsh is about to speak WebSocket on with it.
    tokio::spawn(async move {
        let _ = connection.with_upgrades().await;
    });

    Ok(sender.send_request(request).await?)
}

type BoxedError = Box<dyn std::error::Error + Send + Sync>;

fn box_error<E: std::error::Error + Send + Sync + 'static>(error: E) -> BoxedError {
    Box::new(error)
}

/// Whether the request is asking to stop being HTTP.
///
/// Both halves are required, which is what the specification says and also what
/// keeps a stray `Upgrade` header from sending the code down the WebSocket path
/// on an ordinary request.
fn is_upgrade(connection: Option<&HeaderValue>, upgrade: Option<&HeaderValue>) -> bool {
    let nominated = connection
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
        });

    nominated && upgrade.is_some()
}

fn header<B>(request: &Request<B>, name: http::HeaderName) -> Option<&str> {
    request.headers().get(name)?.to_str().ok()
}

fn header_named<'a, B>(request: &'a Request<B>, name: &str) -> Option<&'a str> {
    request.headers().get(name)?.to_str().ok()
}

/// One cookie out of the phone's `Cookie` header.
fn cookie_value<B>(request: &Request<B>, name: &str) -> Option<String> {
    request
        .headers()
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value.to_string())
}

/// One query parameter, decoded.
fn query_value<B>(request: &Request<B>, name: &str) -> Option<String> {
    let query = request.uri().query()?;
    form_urlencoded::parse(query.as_bytes())
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

/// 303 to somewhere on this gateway, optionally handing over the device cookie.
///
/// No `Secure`, because Phase 1 is plain HTTP on the local network and a
/// `Secure` cookie would simply never be sent. That is the accepted risk written
/// down in the specification, and the attribute goes on the day there is a
/// tunnel with TLS on it.
fn redirect(target: &str, cookie: Option<&str>) -> Response<Body> {
    let mut response = Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(LOCATION, target);

    if let Some(cookie) = cookie {
        response = response.header(
            SET_COOKIE,
            format!(
                "{DEVICE_COOKIE}={cookie}; Path=/; HttpOnly; SameSite=Strict; Max-Age={COOKIE_MAX_AGE}"
            ),
        );
    }

    response
        .body(empty())
        .expect("a redirect with a fixed shape is always buildable")
}

/// What a request that failed the fence gets.
///
/// No explanation and no page. Whatever sent it is not a phone the user
/// scanned a code with — it is a port scan, or a page in somebody's browser
/// trying its luck — and telling it what it got wrong is telling it what to fix.
fn refused() -> Response<Body> {
    Response::builder()
        .status(StatusCode::FORBIDDEN)
        .body(empty())
        .expect("a bare 403 is always buildable")
}

/// What a browser with no session gets: 401, and dsh's own words for it.
///
/// The same status dsh answers an unauthenticated index request with, because
/// the phone is talking to something that stands in for dsh and should not be
/// distinguishable from it by status code.
fn unauthenticated() -> Response<Body> {
    page(
        StatusCode::UNAUTHORIZED,
        t!("这台设备还没有配对", "This device is not paired"),
        t!(
            "回到电脑上打开「手机连接」，扫一下那个二维码。",
            "Open Connect a phone on the computer and scan the code it shows."
        ),
    )
}

/// One of this gateway's own pages.
///
/// Self-contained and tiny: it is served to a phone that may have no session,
/// on a gateway that will not proxy anything for it, so there is nowhere to
/// fetch a stylesheet from. The dark half is a media query rather than dsh's
/// theme — this page never gets to ask dsh anything.
fn page(status: StatusCode, title: &str, body: &str) -> Response<Body> {
    let html = format!(
        "<!doctype html><html lang=\"{lang}\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>{title}</title><style>\
         :root{{color-scheme:light dark}}\
         body{{margin:0;min-height:100vh;display:flex;align-items:center;justify-content:center;\
         padding:24px;box-sizing:border-box;background:#fff;color:#1c1c1e;\
         font:16px/1.6 -apple-system,BlinkMacSystemFont,\"Segoe UI\",\"Microsoft YaHei\",system-ui,sans-serif}}\
         main{{max-width:22em;text-align:center}}\
         h1{{margin:0 0 .6em;font-size:1.15rem;font-weight:600}}\
         p{{margin:0;opacity:.7}}\
         @media (prefers-color-scheme:dark){{body{{background:#1c1c1e;color:#f2f2f7}}}}\
         </style></head><body><main><h1>{title}</h1><p>{body}</p></main></body></html>",
        lang = crate::i18n::tag(),
        title = escape(title),
        body = escape(body),
    );

    Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(
            Full::new(Bytes::from(html))
                .map_err(|never| match never {})
                .boxed(),
        )
        .expect("a page with a fixed shape is always buildable")
}

/// The four characters that would otherwise end an element early. Every string
/// these pages interpolate is one of this app's own, but a page builder that is
/// only safe because of where its arguments come from is one bad call away from
/// not being.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn empty() -> Body {
    Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(headers: &[(&str, &str)], uri: &str) -> Request<()> {
        let mut builder = Request::builder().uri(uri);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(()).unwrap()
    }

    #[test]
    fn a_websocket_handshake_is_recognised() {
        let request = request(
            &[
                ("connection", "keep-alive, Upgrade"),
                ("upgrade", "websocket"),
            ],
            "/api/remote.mux",
        );
        assert!(is_upgrade(
            request.headers().get(CONNECTION),
            request.headers().get(UPGRADE)
        ));
    }

    /// Both headers, or it is an ordinary request. A page can send either one on
    /// its own, and neither alone is an upgrade.
    #[test]
    fn half_an_upgrade_is_not_one() {
        for headers in [
            vec![("connection", "Upgrade")],
            vec![("upgrade", "websocket")],
            vec![("connection", "keep-alive"), ("upgrade", "websocket")],
            vec![],
        ] {
            let request = request(&headers, "/");
            assert!(
                !is_upgrade(
                    request.headers().get(CONNECTION),
                    request.headers().get(UPGRADE)
                ),
                "{headers:?} is not an upgrade"
            );
        }
    }

    #[test]
    fn the_device_cookie_is_found_among_the_others() {
        let request = request(
            &[(
                "cookie",
                "theme=dark; dsh_mobile_session=v1.d1.0.mac; other=1",
            )],
            "/",
        );
        assert_eq!(
            cookie_value(&request, DEVICE_COOKIE).as_deref(),
            Some("v1.d1.0.mac")
        );
    }

    /// A cookie whose name merely ends with ours is not ours. The split is on
    /// the whole name for that reason.
    #[test]
    fn a_lookalike_cookie_is_not_it() {
        let request = request(&[("cookie", "not_dsh_mobile_session=forged")], "/");
        assert_eq!(cookie_value(&request, DEVICE_COOKIE), None);
    }

    #[test]
    fn no_cookie_header_is_no_session() {
        assert_eq!(cookie_value(&request(&[], "/"), DEVICE_COOKIE), None);
    }

    #[test]
    fn the_pairing_nonce_is_read_off_the_url() {
        let scanned = request(&[], "/?pair_token=abc%2Ddef&x=1");
        assert_eq!(
            query_value(&scanned, "pair_token").as_deref(),
            Some("abc-def")
        );

        assert_eq!(query_value(&request(&[], "/"), "pair_token"), None);
        assert_eq!(query_value(&request(&[], "/?other=1"), "pair_token"), None);
    }

    /// The cookie the phone is handed. Every attribute on it is load-bearing —
    /// `HttpOnly` keeps it out of any script dsh loads, `SameSite=Strict` is the
    /// second half of the cross-site defence, and the absence of `Secure` is
    /// what makes it work at all over plain HTTP.
    #[test]
    fn the_device_cookie_goes_out_with_the_attributes_it_needs() {
        let response = redirect("/", Some("v1.d1.0.mac"));
        let cookie = response.headers()[SET_COOKIE].to_str().unwrap();

        assert!(cookie.starts_with("dsh_mobile_session=v1.d1.0.mac;"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Path=/"));
        assert!(!cookie.contains("Secure"), "Phase 1 is plain HTTP");
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()[LOCATION], "/");
    }

    #[test]
    fn a_redirect_that_is_not_a_handshake_sets_nothing() {
        assert!(!redirect("/", None).headers().contains_key(SET_COOKIE));
    }

    /// A refusal says nothing at all, on purpose.
    #[test]
    fn the_fences_refusal_carries_no_explanation() {
        let response = refused();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(!response.headers().contains_key(http::header::CONTENT_TYPE));
    }

    #[test]
    fn the_pages_close_their_own_tags() {
        let response = page(StatusCode::UNAUTHORIZED, "<b>t</b>", "a & b \"c\"");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(escape("<b>t</b>"), "&lt;b&gt;t&lt;/b&gt;");
        assert_eq!(escape("a & b \"c\""), "a &amp; b &quot;c&quot;");
    }
}
