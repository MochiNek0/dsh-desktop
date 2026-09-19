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

        // The one refusal that deserves words. A phone holding a device cookie
        // for this gateway, knocking on an address this machine really has and
        // the active tunnel no longer publishes, is not an attack: it is a
        // phone that was paired before the channel moved under it, and a blank
        // 403 tells it nothing it can act on.
        //
        // The cookie is the cheap half and is tested first. A port scanner has
        // none, and the thing this path must never do is walk the machine's
        // network adapters once per probe — see [`trust::Refusal`].
        let stranded = cookie_value(&request, DEVICE_COOKIE).is_some()
            && this_machine(header(&request, HOST));
        return Ok(if stranded { moved() } else { refused() });
    }

    // Before the cookie is looked at, and that is the point of them: see
    // [`homescreen`].
    if let Some(response) = homescreen(&shared, request.uri().path()) {
        return Ok(response);
    }

    let device =
        cookie_value(&request, DEVICE_COOKIE).and_then(|value| shared.store.verify(&value));

    // A pairing URL from somebody who is already paired is a phone that scanned
    // the code twice, or typed one it did not need. Sending it to `/` costs
    // nobody a nonce and is what the user meant either way.
    let offer = query_value(&request, "pair_token")
        .map(Offer::Token)
        .or_else(|| query_value(&request, "pair_code").map(Offer::Code));

    if let Some(offer) = offer {
        if device.is_some() {
            return Ok(redirect("/", None, false));
        }
        return Ok(pair(shared, peer, request, offer).await);
    }

    if device.is_none() {
        return Ok(unauthenticated());
    }

    Ok(forward(shared, request).await)
}

/// How the phone offered the nonce: off the QR code, or typed by hand.
///
/// The two are the same nonce and reach the same dialog. They are kept apart
/// only so far as the answers differ — what to say when it is wrong, and
/// whether a wrong one is worth counting.
enum Offer {
    Token(String),
    Code(String),
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
    offer: Offer,
) -> Response<Body> {
    // Whoever the active tunnel says is asking: the socket's peer on a tunnel
    // the phone dialled itself, and a header the tunnel vouches for once there
    // is a reverse proxy in front of this listener. Both of the things below
    // are about *which device* is talking — the rate limit and the address a
    // human is shown — and both are wrong the moment every request arrives
    // from `127.0.0.1`. See [`RemoteTunnel::client_ip`].
    let who = shared.client_ip(request.headers(), peer);

    let redeemed = match &offer {
        Offer::Token(token) => shared.store.redeem_pair(token),
        Offer::Code(typed) => {
            if shared.guesses.blocked(who) {
                return repair(
                    StatusCode::TOO_MANY_REQUESTS,
                    Some(t!(
                        "错的次数太多了。等五分钟再试，或者回到电脑上重新扫码。",
                        "Too many wrong codes. Wait five minutes and try again, \
                         or go back to the computer and scan instead."
                    )),
                );
            }

            let redeemed = shared.store.redeem_code(typed);
            if redeemed {
                shared.guesses.right(who);
            } else {
                shared.guesses.wrong(who);
            }
            redeemed
        }
    };

    if !redeemed {
        return match offer {
            Offer::Token(_) => page(
                StatusCode::FORBIDDEN,
                t!("这个二维码已经失效", "This code is no longer valid"),
                t!(
                    "配对码只有五分钟有效，而且只能用一次。回到电脑上重新打开「手机连接」再扫一次。",
                    "A pairing code lasts five minutes and can be used once. \
                     Open Connect a phone on the computer again and scan the new one."
                ),
            ),
            // Back to the same page with the box still on it. A dead end here
            // would send the user to the computer for a code they are one
            // typo away from — which is the trip this whole entrance exists to
            // save them.
            Offer::Code(_) => repair(
                StatusCode::UNAUTHORIZED,
                Some(t!(
                    "这六个字符不对，或者已经过期了。看一眼电脑上的卡片再输一次。",
                    "Those six characters are wrong, or they have expired. \
                     Check the card on the computer and try again."
                )),
            ),
        };
    }

    // Before the dialog, not after it: the nonce is gone either way, and the
    // card would otherwise print a spent one for as long as the question stood
    // open — a minute during which anyone reading it off the screen is reading
    // something dead.
    super::spent(&shared);

    let label = trust::label(header_named(&request, USER_AGENT.as_str()));
    let address = who.to_string();

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
    let secure = shared.secure();

    // 303 rather than serving the index here: the phone lands on a clean `/`
    // with the pairing nonce out of its address bar, so a reload or a bookmark
    // does not carry a spent token around forever. Exactly the shape dsh's own
    // token exchange uses, for exactly the same reason.
    redirect("/", Some(&cookie), secure)
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
/// `Secure` comes from the active tunnel's [`scheme`] and from nowhere else —
/// never from a header, and never from anything this process can observe about
/// the connection, which terminates no TLS and would report plaintext under a
/// proxy that did. Both of today's tunnels are plain HTTP, so the attribute
/// stays off and the LAN's sniffing risk is the accepted one written down in
/// the specification.
///
/// [`scheme`]: crate::remote::tunnel::RemoteTunnel::scheme
fn redirect(target: &str, cookie: Option<&str>, secure: bool) -> Response<Body> {
    let mut response = Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(LOCATION, target);

    if let Some(cookie) = cookie {
        response = response.header(
            SET_COOKIE,
            format!(
                "{DEVICE_COOKIE}={cookie}; Path=/; HttpOnly; SameSite=Strict;                  Max-Age={COOKIE_MAX_AGE}{}",
                if secure { "; Secure" } else { "" }
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

/// Whether a `Host` the fence turned away names an address this machine
/// actually has.
///
/// The port is not compared: a request that arrived at this listener arrived
/// here whatever port it thinks it asked for, and what is being decided is only
/// whether to spend words on the sender.
///
/// Loopback is not on the list [`tunnel::addresses`] returns, deliberately, and
/// that carries through to here: a request whose `Host` is `127.0.0.1` is a page
/// in the *desktop's* browser, which is the case the fence exists for. It keeps
/// the bare refusal.
///
/// [`tunnel::addresses`]: crate::remote::tunnel::addresses
fn this_machine(host: Option<&str>) -> bool {
    let Some(host) = host else {
        return false;
    };
    let Some(address) = host.trim().split(':').next() else {
        return false;
    };

    super::tunnel::addresses()
        .iter()
        .any(|local| local.to_string() == address)
}

/// What a phone stranded on the old channel is told.
///
/// It cannot be helped back in from here — the six-character entrance would
/// hand it a cookie scoped to an address the fence is going to refuse on the
/// very next request — so this page is honest about being a dead end and points
/// at the only thing that does work, which is the card on the desktop.
fn moved() -> Response<Body> {
    page(
        StatusCode::FORBIDDEN,
        t!(
            "这台电脑换了连接通道",
            "This computer moved to another channel"
        ),
        t!(
            "手机用的还是原来的地址，而电脑现在从另一个通道对外，配对是跟着地址走的。             回到电脑上打开「手机连接」，扫一下新的二维码——换回原来的通道，这个地址就又能用了。",
            "The phone is still on the old address, and this computer now publishes another              one; a pairing follows the address. Open Connect a phone on the computer and scan              the new code — or switch the channel back, and this address works again."
        ),
    )
}

/// The two files that turn the phone's tab into an icon on its home screen, or
/// `None` for every other path.
///
/// Served here rather than by dsh because they are nothing to do with dsh: the
/// names are outside its route table, so nothing collides, and the tags that
/// point at them are put on its index by `plugin/lib/index.js` — which is the
/// only place they can go, since dsh gzips the index before the gateway sees a
/// byte of it.
///
/// ## Why these two answer without a cookie
///
/// Every other path on this listener needs one. These two do not, for two
/// reasons that point the same way.
///
/// The first is that a manifest is fetched with credentials omitted unless the
/// `<link>` carries `crossorigin="use-credentials"` — so behind the cookie
/// check, the browser would be handed the 401 page where it expected JSON, and
/// the failure would be a home-screen icon that silently does not install. The
/// attribute exists and would work; what it would not do is help the icon,
/// which the operating system may re-fetch long after a session has aged out,
/// and a broken icon is a thing the OS caches.
///
/// The second is that there is nothing here to protect. An app icon and a name
/// are not secrets, and anything that can reach this port already learns more
/// than they carry from the pairing page it gets for asking — see
/// [`repair`]. The trust fence still applies to both: a cross-site request for
/// either one is refused before this function is reached.
fn homescreen(shared: &Shared, path: &str) -> Option<Response<Body>> {
    match path {
        "/dsh-mobile-manifest.json" => Some(manifest()),
        "/dsh-mobile-icon.png" => Some(icon(shared)),
        _ => None,
    }
}

/// The manifest, which is what Android reads. iOS has never read one for this —
/// the `apple-` meta tags in the plugin are its half.
///
/// `start_url` deliberately carries no `pair_token`. A nonce is good for five
/// minutes and for one use, and writing one into the thing a home-screen icon
/// opens for the next thirty days would bake in a dead token.
///
/// One icon, at 512. Chrome's installability floor is 144, so this clears it,
/// and every consumer downscales. A second file at 192 would be sharper on a
/// low-density phone and is the only thing missing here; it is left out because
/// it would mean carrying an image resampler in the build for one image.
fn manifest() -> Response<Body> {
    let json = serde_json::json!({
        "id": "/",
        "name": "DeepSeek Harness",
        "short_name": "DSH",
        "start_url": "/",
        "scope": "/",
        "display": "standalone",
        "background_color": "#ffffff",
        "theme_color": "#ffffff",
        "icons": [{
            "src": "/dsh-mobile-icon.png",
            "sizes": "512x512",
            "type": "image/png",
            "purpose": "any",
        }],
    })
    .to_string();

    Response::builder()
        .status(StatusCode::OK)
        .header(http::header::CONTENT_TYPE, "application/manifest+json")
        .header(http::header::CACHE_CONTROL, "no-cache")
        .body(
            Full::new(Bytes::from(json))
                .map_err(|never| match never {})
                .boxed(),
        )
        .expect("a manifest with a fixed shape is always buildable")
}

/// The home-screen icon: the app's whale, flattened onto white.
///
/// Read off the disk on each request rather than compiled in. It is staged into
/// the resource directory by `scripts/make-mobile-icon.mjs`, which runs from
/// `beforeBuildCommand` — and a build that has not run that script yet is a
/// build `include_bytes!` would refuse to compile at all, which would make a
/// bare `cargo test` in a fresh clone fail over an icon.
///
/// So a missing file is a 404 and nothing worse. The phone loses the icon and
/// keeps everything else.
fn icon(shared: &Shared) -> Response<Body> {
    let bytes = shared
        .approve
        .app()
        .and_then(crate::dsh::resources)
        .map(|resources| resources.join("mobile-icon.png"))
        .and_then(|path| std::fs::read(path).ok());

    let Some(bytes) = bytes else {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(empty())
            .expect("a bare 404 is always buildable");
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(http::header::CONTENT_TYPE, "image/png")
        // A day. The icon changes when the app is updated and not otherwise, and
        // the alternative — revalidating on every home-screen launch — is a
        // round trip on the path where the user is waiting for dsh to appear.
        .header(http::header::CACHE_CONTROL, "public, max-age=86400")
        .body(
            Full::new(Bytes::from(bytes))
                .map_err(|never| match never {})
                .boxed(),
        )
        .expect("a png response with a fixed shape is always buildable")
}

/// What a browser with no session gets: 401, dsh's own status for it, and the
/// way back in.
///
/// The status is dsh's because the phone is talking to something that stands in
/// for dsh and should not be distinguishable from it by status code.
fn unauthenticated() -> Response<Body> {
    repair(StatusCode::UNAUTHORIZED, None)
}

/// The page with the box on it: type the six characters from the card.
///
/// This is the one page in the gateway that is a *way out* of a failure rather
/// than a report of one, so it is worth saying what it is not. It cannot help a
/// phone whose problem is that this address stopped answering — a moved port,
/// a new DHCP lease, a tunnel switched underneath it. In that case nothing
/// here runs at all; the browser shows its own connection-refused page and this
/// gateway never hears about it. What it is for is the far commoner failure
/// where the gateway is fine and only the cookie is gone.
///
/// A plain `GET` form, with no script anywhere on the page. Submitting it is an
/// ordinary same-origin navigation to `/?pair_code=…`, which is what keeps an
/// iOS home-screen window from spilling the user back into Safari — and what
/// makes this work at all on a gateway serving plain HTTP, where a scanner
/// could not: `getUserMedia` needs a secure context and a form does not.
fn repair(status: StatusCode, problem: Option<&str>) -> Response<Body> {
    let form = format!(
        "<form method=\"get\" action=\"/\">\
         <input name=\"pair_code\" maxlength=\"16\" autocomplete=\"off\" autocorrect=\"off\" \
         autocapitalize=\"characters\" spellcheck=\"false\" aria-label=\"{label}\" \
         placeholder=\"{placeholder}\">\
         <button type=\"submit\">{submit}</button>\
         </form>{problem}",
        label = escape(t!("配对码", "Pairing code")),
        placeholder = escape(t!("六个字符", "six characters")),
        submit = escape(t!("连接", "Connect")),
        problem = problem
            .map(|why| format!("<p class=\"bad\">{}</p>", escape(why)))
            .unwrap_or_default(),
    );

    shell(
        status,
        t!("这台设备还没有配对", "This device is not paired"),
        &format!(
            "<p>{}</p>{form}",
            escape(t!(
                "电脑上的「手机连接」卡片，二维码旁边有六个字符。输进去就行——不用相机，也不用离开这一页。",
                "The card on the computer shows six characters beside the QR code. \
                 Type them in — no camera needed, and no leaving this page."
            ))
        ),
    )
}

/// One of this gateway's own pages, from a title and a line of text.
fn page(status: StatusCode, title: &str, body: &str) -> Response<Body> {
    shell(status, title, &format!("<p>{}</p>", escape(body)))
}

/// The shell both of those go in.
///
/// Self-contained and tiny: it is served to a phone that may have no session,
/// on a gateway that will not proxy anything for it, so there is nowhere to
/// fetch a stylesheet from. The dark half is a media query rather than dsh's
/// theme — this page never gets to ask dsh anything.
///
/// `body` is markup, not text, and it is the one argument here that is not
/// escaped on the way in. Every caller builds it out of [`escape`] and literals
/// in this file; nothing reaches it from a request.
fn shell(status: StatusCode, title: &str, body: &str) -> Response<Body> {
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
         form{{display:flex;gap:8px;margin:1.4em 0 0}}\
         input{{flex:1;min-width:0;padding:.55em .6em;box-sizing:border-box;\
         font:inherit;font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;\
         letter-spacing:.16em;text-align:center;text-transform:uppercase;\
         color:inherit;background:transparent;border:1px solid;border-color:currentColor;\
         border-radius:9px;opacity:.85}}\
         button{{padding:.55em 1.1em;font:inherit;color:#fff;background:#1c1c1e;\
         border:0;border-radius:9px;cursor:pointer}}\
         p.bad{{margin:1em 0 0;font-size:.9rem;opacity:1;color:#c0392b}}\
         @media (prefers-color-scheme:dark){{body{{background:#1c1c1e;color:#f2f2f7}}\
         button{{color:#1c1c1e;background:#f2f2f7}}p.bad{{color:#ff7a6b}}}}\
         </style></head><body><main><h1>{title}</h1>{body}</main></body></html>",
        lang = crate::i18n::tag(),
        title = escape(title),
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
        let response = redirect("/", Some("v1.d1.0.mac"), false);
        let cookie = response.headers()[SET_COOKIE].to_str().unwrap();

        assert!(cookie.starts_with("dsh_mobile_session=v1.d1.0.mac;"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Path=/"));
        assert!(!cookie.contains("Secure"), "both of M2's tunnels are plain HTTP");
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()[LOCATION], "/");
    }

    /// And the same cookie on a tunnel that terminates TLS. Nothing answers
    /// that way yet — see `tunnel::Scheme` — so this pins the attribute to the
    /// one thing allowed to decide it, which is the argument and never a
    /// header.
    #[test]
    fn a_secure_scheme_is_the_only_thing_that_adds_secure() {
        let cookie = redirect("/", Some("v1.d1.0.mac"), true);
        let cookie = cookie.headers()[SET_COOKIE].to_str().unwrap();

        assert!(cookie.ends_with("; Secure"));
        assert!(cookie.contains("SameSite=Strict"));
    }

    #[test]
    fn a_redirect_that_is_not_a_handshake_sets_nothing() {
        assert!(!redirect("/", None, false).headers().contains_key(SET_COOKIE));
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
