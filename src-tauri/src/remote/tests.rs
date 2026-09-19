//! The gateway, driven end to end against a stub that behaves like `dsh web`.
//!
//! Everything else in this module tree is tested as a function: does the fence
//! refuse this header, does that cookie verify, does the rewrite drop what it
//! should. None of those catch the failures that actually happen here, which
//! are about wiring — a `with_upgrades` left off, a body that never streams, a
//! cookie exchanged against the wrong authority, a `Set-Cookie` that survives.
//! Each of those produces code that compiles, passes every unit test, and
//! serves a phone a blank page.
//!
//! So this raises the real listener, in front of a real HTTP server that
//! insists on being treated exactly the way dsh insists on being treated, and
//! talks to it over a real socket with hand-written requests. Hand-written
//! because the point is the bytes: a client library would normalise away the
//! very headers the fence is about.
//!
//! ## The stub is strict on purpose
//!
//! It refuses anything whose `Host` is not its own loopback authority, refuses
//! anything without the cookie it minted, and rotates its launch token whenever
//! the test says the dsh process restarted. If the gateway stops rewriting, or
//! starts forwarding the phone's own cookie, or holds a cookie past a restart,
//! the stub answers 401 and the test fails — rather than the gateway quietly
//! working for the wrong reason.
//!
//! ## Loopback, not `0.0.0.0`
//!
//! The listener under test is bound to `127.0.0.1`. Binding every interface is
//! what the app does and what [`super::Remote::start`] is for, but doing it in
//! a test would raise the Windows firewall dialog on whoever runs `cargo test`.
//! The fence is exercised regardless: the requests carry a LAN `Host` the
//! tunnel published, which is the header the fence actually reads.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::session::SessionStore;
use super::tunnel::{LanTunnel, RemoteTunnel};
use super::upstream::Upstream;
use super::{Approve, Shared};

/// A gateway, its stub dsh, and the address a phone would reach it at.
struct Harness {
    /// Where to open a socket: loopback, whatever port was free.
    at: SocketAddr,
    /// The `Host` a request has to carry, which is not the same thing.
    authority: String,
    shared: Arc<Shared>,
    dsh: Arc<StubState>,
    /// Dropped at the end of the test, which ends the accept loop.
    _stop: tokio::sync::oneshot::Sender<()>,
}

/// What the stub dsh knows about itself.
struct StubState {
    /// The launch token this "process" will accept. Changed by
    /// [`StubState::restart`], which is a dsh that died and came back.
    token: Mutex<String>,
    /// How many token exchanges have been done against it. The single-flight in
    /// `ensure_cookie` is the reason this is counted.
    exchanges: AtomicU64,
}

impl StubState {
    fn cookie(&self) -> String {
        format!("dsh-auth-{}=signed", self.token.lock().unwrap())
    }

    /// A new dsh process on the same port: new token, and every cookie the old
    /// one minted is now worthless.
    fn restart(&self) {
        *self.token.lock().unwrap() = "second".to_string();
    }
}

impl Harness {
    fn raise(answer: bool) -> Option<Self> {
        // A machine with no network has no authority to publish, and the fence
        // would refuse everything. Nothing to test there.
        super::tunnel::best_address()?;

        let runtime = runtime();
        let _guard = runtime.enter();

        let dsh = Arc::new(StubState {
            token: Mutex::new("first".to_string()),
            exchanges: AtomicU64::new(0),
        });
        let dsh_at = runtime.block_on(stub(dsh.clone()));

        let listener = runtime
            .block_on(tokio::net::TcpListener::bind(("127.0.0.1", 0)))
            .expect("a free loopback port");
        let at = listener.local_addr().expect("a bound listener");

        let mut tunnel = LanTunnel::default();
        let base = tunnel.start(at.port()).expect("an address to publish");
        let authority = base
            .strip_prefix("http://")
            .expect("the lan tunnel publishes http")
            .to_string();

        let upstream = Upstream::default();
        upstream.ready(
            &format!("http://{dsh_at}/?token={}", dsh.token.lock().unwrap())
                .parse()
                .unwrap(),
        );

        let shared = Arc::new(Shared {
            approve: Approve::Fixed(answer),
            store: SessionStore::default(),
            upstream,
            tunnel: Mutex::new(tunnel),
            guesses: super::trust::Guesses::default(),
            exchange: tokio::sync::Mutex::new(()),
            seen: AtomicU64::new(0),
        });

        let (stop, stopped) = tokio::sync::oneshot::channel();
        runtime.spawn(super::proxy::serve(listener, shared.clone(), stopped));

        Some(Self {
            at,
            authority,
            shared,
            dsh,
            _stop: stop,
        })
    }

    /// A pairing nonce, as the card would have put on a QR code.
    fn nonce(&self) -> String {
        self.shared.store.mint_pair().token
    }

    /// The same nonce as the card prints it, for the phone that types instead
    /// of scanning.
    fn code(&self) -> String {
        self.shared.store.mint_pair().code
    }

    /// One request, written out by hand. `extra` is whatever headers the case
    /// is about; `Host` is always the published authority unless a case
    /// overrides it.
    fn get(&self, target: &str, extra: &[(&str, &str)]) -> Answer {
        let mut request = format!("GET {target} HTTP/1.1\r\n");
        if !extra.iter().any(|(name, _)| *name == "Host") {
            request.push_str(&format!("Host: {}\r\n", self.authority));
        }
        for (name, value) in extra {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        request.push_str("Connection: close\r\n\r\n");

        Answer::read(self.at, &request)
    }
}

/// A response, parsed far enough to assert about.
struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Answer {
    fn read(at: SocketAddr, request: &str) -> Self {
        let mut socket = TcpStream::connect(at).expect("the gateway is listening");
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        socket
            .write_all(request.as_bytes())
            .expect("a written request");

        let mut reader = BufReader::new(socket);
        let mut line = String::new();
        reader.read_line(&mut line).expect("a status line");
        let status = line
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status in {line:?}"));

        let mut headers = Vec::new();
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).expect("a header line");
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':') {
                headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
            }
        }

        // `Connection: close` on every request, so the body is whatever arrives
        // before the far end hangs up. Chunked framing is left as it came: no
        // assertion here reads past the text it is looking for.
        let mut body = Vec::new();
        let _ = reader.read_to_end(&mut body);

        Self {
            status,
            headers,
            body: String::from_utf8_lossy(&body).into_owned(),
        }
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The device cookie's value out of a `Set-Cookie`, for the requests that
    /// follow the handshake.
    fn device_cookie(&self) -> Option<String> {
        let set = self.header("set-cookie")?;
        let pair = set.split(';').next()?;
        let (name, value) = pair.split_once('=')?;
        (name == super::session::COOKIE).then(|| value.to_string())
    }
}

/// One multi-threaded runtime per harness. Multi-threaded because the gateway
/// opens a connection to the stub from inside a task the same runtime is
/// driving, and the stub's own accept loop has to be able to run while it does.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("a runtime")
    })
}

/// A stand-in for `dsh web`, as strict as the real thing about who is talking
/// to it. Answers with its own address.
async fn stub(state: Arc<StubState>) -> SocketAddr {
    use bytes::Bytes;
    use http::{header, Request, Response, StatusCode};
    use http_body_util::Full;
    use hyper::body::Incoming;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("a free loopback port");
    let at = listener.local_addr().expect("a bound listener");

    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            let state = state.clone();
            tokio::spawn(async move {
                let service = service_fn(move |request: Request<Incoming>| {
                    let state = state.clone();
                    async move {
                        // Every ordinary answer carries a `Set-Cookie`, which
                        // the real dsh does on the exchange and may do again on
                        // any response. It is here on all of them so that the
                        // gateway's stripping is exercised by every assertion
                        // rather than only by the handshake.
                        let state_for_reply = state.clone();
                        let reply = move |status: StatusCode, body: &str| {
                            Response::builder()
                                .status(status)
                                .header(header::CONTENT_TYPE, "text/html")
                                .header(
                                    header::SET_COOKIE,
                                    format!("{}; Path=/; HttpOnly", state_for_reply.cookie()),
                                )
                                .body(Full::new(Bytes::from(body.to_string())))
                                .unwrap()
                        };

                        let host = request
                            .headers()
                            .get(header::HOST)
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or_default()
                            .to_string();

                        // dsh's own fence: loopback or nothing. The gateway's
                        // rewrite is what gets past this, so a rewrite that
                        // stopped happening fails here rather than silently.
                        if !host.starts_with("127.0.0.1:") {
                            return Ok::<_, std::convert::Infallible>(reply(
                                StatusCode::FORBIDDEN,
                                "not loopback",
                            ));
                        }

                        let token = request
                            .uri()
                            .query()
                            .and_then(|query| query.strip_prefix("token="))
                            .map(str::to_string);

                        if let Some(token) = token {
                            if token != *state.token.lock().unwrap() {
                                return Ok(reply(StatusCode::UNAUTHORIZED, "stale token"));
                            }
                            state.exchanges.fetch_add(1, Ordering::SeqCst);
                            return Ok(Response::builder()
                                .status(StatusCode::SEE_OTHER)
                                .header(header::LOCATION, "/")
                                .header(
                                    header::SET_COOKIE,
                                    format!("{}; Path=/; HttpOnly", state.cookie()),
                                )
                                .body(Full::new(Bytes::new()))
                                .unwrap());
                        }

                        let cookie = request
                            .headers()
                            .get(header::COOKIE)
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or_default();
                        if !cookie.contains(&state.cookie()) {
                            return Ok(reply(StatusCode::UNAUTHORIZED, "dsh: reopen the url"));
                        }

                        // The realtime channel. Answers 101 and then echoes
                        // whatever arrives, which is all the byte copy has to
                        // carry.
                        if request.uri().path() == "/api/remote.mux" {
                            let mut upgraded = Response::builder()
                                .status(StatusCode::SWITCHING_PROTOCOLS)
                                .header(header::CONNECTION, "Upgrade")
                                .header(header::UPGRADE, "websocket")
                                .body(Full::new(Bytes::new()))
                                .unwrap();

                            let taken = hyper::upgrade::on(request);
                            tokio::spawn(async move {
                                let Ok(socket) = taken.await else { return };
                                let mut socket = TokioIo::new(socket);
                                let mut buffer = [0u8; 64];
                                loop {
                                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                                    let Ok(read) = socket.read(&mut buffer).await else {
                                        return;
                                    };
                                    if read == 0 {
                                        return;
                                    }
                                    if socket.write_all(&buffer[..read]).await.is_err() {
                                        return;
                                    }
                                }
                            });

                            upgraded
                                .headers_mut()
                                .insert(header::SET_COOKIE, "dsh-auth-x=y".parse().unwrap());
                            return Ok(upgraded);
                        }

                        Ok(reply(StatusCode::OK, "<html>dsh index</html>"))
                    }
                });

                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(socket), service)
                    .with_upgrades()
                    .await;
            });
        }
    });

    let _ = at;
    at
}

/// The whole handshake, and what it costs the phone: one scan, one dialog, and
/// from then on a session that reaches dsh.
#[test]
fn a_phone_that_was_allowed_in_reaches_dsh() {
    let Some(harness) = Harness::raise(true) else {
        return;
    };

    let nonce = harness.nonce();
    let handshake = harness.get(&format!("/?pair_token={nonce}"), &[]);

    assert_eq!(
        handshake.status, 303,
        "the handshake redirects to a clean /"
    );
    assert_eq!(handshake.header("location"), Some("/"));

    let cookie = handshake
        .device_cookie()
        .expect("the handshake hands over a device cookie");

    let index = harness.get(
        "/",
        &[("Cookie", &format!("{}={cookie}", super::session::COOKIE))],
    );
    assert_eq!(index.status, 200, "and the session reaches dsh");
    assert!(index.body.contains("dsh index"), "{}", index.body);
}

/// The one thing that must never travel outward. dsh binds its cookie to its
/// own authority, so a phone that stored one would be storing rubbish — and
/// rubbish named after dsh's launch token, which is what `cookies.rs` exists to
/// clear up after.
#[test]
fn dshs_own_cookie_never_reaches_the_phone() {
    let Some(harness) = Harness::raise(true) else {
        return;
    };

    let nonce = harness.nonce();
    let handshake = harness.get(&format!("/?pair_token={nonce}"), &[]);
    let cookie = handshake.device_cookie().expect("a device cookie");

    for answer in [
        handshake,
        harness.get(
            "/",
            &[("Cookie", &format!("{}={cookie}", super::session::COOKIE))],
        ),
    ] {
        for (name, value) in &answer.headers {
            if name == "set-cookie" {
                assert!(
                    !value.contains("dsh-auth-"),
                    "dsh's cookie escaped: {value}"
                );
            }
        }
    }
}

/// No cookie is 401, the same answer dsh itself gives — so a phone whose
/// session expired sees what it would have seen talking to dsh directly.
#[test]
fn an_unpaired_device_is_told_to_pair() {
    let Some(harness) = Harness::raise(true) else {
        return;
    };

    assert_eq!(harness.get("/", &[]).status, 401);
    assert_eq!(
        harness
            .get("/", &[("Cookie", "dsh_mobile_session=v1.d1.0.forged")])
            .status,
        401,
        "and a forged one is no cookie at all"
    );
}

/// The fence, over a real socket. Every one of these reaches the listener and
/// none of them reaches dsh.
#[test]
fn the_fence_turns_away_what_it_should() {
    let Some(harness) = Harness::raise(true) else {
        return;
    };

    let nonce = harness.nonce();
    let authority = harness.authority.clone();

    for (why, headers) in [
        // DNS rebinding: the name resolves here, the request arrives here, and
        // the Host says otherwise.
        ("a rebound name", vec![("Host", "evil.example")]),
        // Loopback is not a phone. A page in the desktop's own browser must not
        // be able to reach this.
        ("loopback", vec![("Host", "127.0.0.1:1")]),
        (
            "a cross-site fetch",
            vec![
                ("Host", authority.as_str()),
                ("sec-fetch-site", "cross-site"),
            ],
        ),
        (
            "a disagreeing origin",
            vec![
                ("Host", authority.as_str()),
                ("Origin", "http://evil.example"),
            ],
        ),
    ] {
        let refused = harness.get(&format!("/?pair_token={nonce}"), &headers);
        assert_eq!(refused.status, 403, "{why} must be refused");
        assert!(refused.body.is_empty(), "{why} is told nothing");
    }

    // And the nonce none of them spent is still good.
    let handshake = harness.get(&format!("/?pair_token={nonce}"), &[]);
    assert_eq!(handshake.status, 303);
}

/// A nonce is worth one redemption, whatever the desktop answers. Without that,
/// a refusal would be an invitation to try again.
#[test]
fn a_refused_device_cannot_try_the_same_code_twice() {
    let Some(harness) = Harness::raise(false) else {
        return;
    };

    let nonce = harness.nonce();
    let refused = harness.get(&format!("/?pair_token={nonce}"), &[]);
    assert_eq!(refused.status, 403, "the desktop said no");
    assert_eq!(refused.device_cookie(), None, "and handed over nothing");

    let again = harness.get(&format!("/?pair_token={nonce}"), &[]);
    assert_eq!(again.status, 403, "the nonce was spent by the first try");
}

/// The whole point of the typed entrance, end to end: a phone that has lost its
/// cookie types six characters into the page it already has open and is back in
/// — no camera, and no walking to the computer.
#[test]
fn a_phone_that_types_the_six_characters_gets_back_in() {
    let Some(harness) = Harness::raise(true) else {
        return;
    };

    let unpaired = harness.get("/", &[]);
    assert_eq!(unpaired.status, 401);
    assert!(
        unpaired.body.contains("name=\"pair_code\""),
        "the 401 carries the way back in, not just the bad news: {}",
        unpaired.body
    );

    let code = harness.code();
    let handshake = harness.get(&format!("/?pair_code={code}"), &[]);

    assert_eq!(handshake.status, 303);
    assert_eq!(handshake.header("location"), Some("/"));

    let cookie = handshake
        .device_cookie()
        .expect("typing the code hands over a device cookie, exactly as scanning does");

    let index = harness.get(
        "/",
        &[("Cookie", &format!("{}={cookie}", super::session::COOKIE))],
    );
    assert_eq!(index.status, 200);
    assert!(index.body.contains("dsh index"), "{}", index.body);
}

/// As it comes off a keyboard rather than out of the generator: lower case, and
/// with the space someone put in to keep their place — which a `GET` form sends
/// as `+`, so this is also the check that the query is read the way a form
/// writes one.
#[test]
fn the_code_is_read_the_way_a_person_types_it() {
    let Some(harness) = Harness::raise(true) else {
        return;
    };

    let code = harness.code();
    let typed = format!("{}+{}", code[..3].to_lowercase(), code[3..].to_lowercase());

    let handshake = harness.get(&format!("/?pair_code={typed}"), &[]);
    assert_eq!(handshake.status, 303, "typed as {typed:?}");
}

/// Spent by the first taker, the same as the token is — they are one nonce.
#[test]
fn a_code_is_spent_once_and_the_qr_goes_with_it() {
    let Some(harness) = Harness::raise(true) else {
        return;
    };

    let minted = harness.shared.store.mint_pair();

    assert_eq!(
        harness
            .get(&format!("/?pair_code={}", minted.code), &[])
            .status,
        303
    );
    assert_eq!(
        harness
            .get(&format!("/?pair_token={}", minted.token), &[])
            .status,
        403,
        "redeeming the code spent the QR's token too"
    );
}

/// A wrong code lands back on the page with the box on it, not on a dead end:
/// the user is one typo from being in, and sending them to the computer for a
/// camera is the trip this entrance exists to save.
#[test]
fn a_wrong_code_comes_back_to_the_same_box() {
    let Some(harness) = Harness::raise(true) else {
        return;
    };

    harness.code();
    let wrong = harness.get("/?pair_code=00000000", &[]);

    assert_eq!(wrong.status, 401);
    assert!(wrong.body.contains("name=\"pair_code\""), "{}", wrong.body);
    assert_eq!(wrong.device_cookie(), None);
}

/// And a run of them costs the address five minutes. Ten is the limit, so the
/// eleventh is refused without the store being asked at all — which is what
/// keeps a billion codes from being searchable in a five-minute window.
#[test]
fn guessing_codes_stops_being_free() {
    let Some(harness) = Harness::raise(true) else {
        return;
    };

    let minted = harness.shared.store.mint_pair();
    for _ in 0..10 {
        assert_eq!(harness.get("/?pair_code=00000000", &[]).status, 401);
    }

    assert_eq!(
        harness.get("/?pair_code=00000000", &[]).status,
        429,
        "the eleventh wrong code is made to wait"
    );

    // And the wait is real: the right code, offered from the same address
    // during the cooldown, is not redeemed either.
    let blocked = harness.get(&format!("/?pair_code={}", minted.code), &[]);
    assert_eq!(blocked.status, 429);
    assert_eq!(blocked.device_cookie(), None);

    // The nonce was never spent, so it still works once the wait is over —
    // which the token half, never rate limited, can show without waiting.
    assert_eq!(
        harness
            .get(&format!("/?pair_token={}", minted.token), &[])
            .status,
        303
    );
}

/// The manifest answers without a cookie, which is the whole reason it is
/// served here rather than behind the session check: a browser fetches a
/// manifest with credentials omitted, so behind the check it would be handed
/// the pairing page where it expected JSON and the icon would silently never
/// install.
#[test]
fn the_manifest_is_served_to_a_phone_that_has_no_session() {
    let Some(harness) = Harness::raise(true) else {
        return;
    };

    let manifest = harness.get("/dsh-mobile-manifest.json", &[]);

    assert_eq!(manifest.status, 200, "not the 401 every other path gets");
    assert_eq!(
        manifest.header("content-type"),
        Some("application/manifest+json")
    );

    let parsed: serde_json::Value = serde_json::from_str(&manifest.body).expect("valid JSON");
    assert_eq!(parsed["display"], "standalone", "or it opens in a tab");
    assert_eq!(parsed["start_url"], "/");
    assert_eq!(
        parsed["icons"][0]["src"], "/dsh-mobile-icon.png",
        "the path the plugin's apple-touch-icon link also names"
    );
}

/// A nonce is good for five minutes and for one use. Writing one into the thing
/// a home-screen icon opens for the next thirty days would bake in a dead token
/// — and the failure would be an icon that opens on a refusal page.
#[test]
fn the_manifest_start_url_carries_no_nonce() {
    let Some(harness) = Harness::raise(true) else {
        return;
    };

    let body = harness.get("/dsh-mobile-manifest.json", &[]).body;
    assert!(!body.contains("pair_token"), "{body}");
    assert!(!body.contains("pair_code"), "{body}");
}

/// The fence comes first, even for the paths that need no cookie. A page on
/// another origin must not be able to read these any more than it can read
/// anything else here.
#[test]
fn the_public_paths_are_still_behind_the_fence() {
    let Some(harness) = Harness::raise(true) else {
        return;
    };

    for path in ["/dsh-mobile-manifest.json", "/dsh-mobile-icon.png"] {
        let refused = harness.get(path, &[("Sec-Fetch-Site", "cross-site")]);
        assert_eq!(refused.status, 403, "{path}");
    }
}

/// The path that only breaks after dsh has died once: same port, new process,
/// new token, and the cookie the gateway is holding is now worthless. The
/// gateway has to notice and exchange again, without the phone doing anything.
#[test]
fn a_dsh_that_restarted_is_reauthenticated_without_the_phone_noticing() {
    let Some(harness) = Harness::raise(true) else {
        return;
    };

    let nonce = harness.nonce();
    let cookie = harness
        .get(&format!("/?pair_token={nonce}"), &[])
        .device_cookie()
        .expect("a device cookie");
    let jar = format!("{}={cookie}", super::session::COOKIE);

    assert_eq!(harness.get("/", &[("Cookie", &jar)]).status, 200);
    assert_eq!(harness.dsh.exchanges.load(Ordering::SeqCst), 1);

    // dsh died and came back on the same port. This is the call `serve` and
    // `attempt` in main.rs make for every URL dsh prints.
    harness.dsh.restart();
    harness.shared.upstream.ready(
        &format!("http://{}/?token=second", upstream_authority(&harness))
            .parse()
            .unwrap(),
    );

    let after = harness.get("/", &[("Cookie", &jar)]);
    assert_eq!(after.status, 200, "the phone's session survived it");
    assert!(after.body.contains("dsh index"));
    assert_eq!(
        harness.dsh.exchanges.load(Ordering::SeqCst),
        2,
        "by exchanging the new launch token"
    );
}

/// Thirty requests at once is one page load, and the launch token must be
/// exchanged once between them rather than thirty times.
#[test]
fn a_page_load_exchanges_the_launch_token_once() {
    let Some(harness) = Harness::raise(true) else {
        return;
    };

    let nonce = harness.nonce();
    let cookie = harness
        .get(&format!("/?pair_token={nonce}"), &[])
        .device_cookie()
        .expect("a device cookie");
    let jar = format!("{}={cookie}", super::session::COOKIE);

    std::thread::scope(|scope| {
        for _ in 0..12 {
            scope.spawn(|| {
                assert_eq!(harness.get("/", &[("Cookie", &jar)]).status, 200);
            });
        }
    });

    assert_eq!(harness.dsh.exchanges.load(Ordering::SeqCst), 1);
}

/// The realtime channel, which is the whole reason the feature is worth having:
/// the thinking stream, the approval prompts and the answers all ride on it.
/// The 101 has to be forwarded in both directions and the socket then joined —
/// a `with_upgrades` left off either side is a WebSocket that connects and
/// never says anything.
#[test]
fn the_websocket_is_carried_through_byte_for_byte() {
    let Some(harness) = Harness::raise(true) else {
        return;
    };

    let nonce = harness.nonce();
    let cookie = harness
        .get(&format!("/?pair_token={nonce}"), &[])
        .device_cookie()
        .expect("a device cookie");

    let mut socket = TcpStream::connect(harness.at).expect("the gateway is listening");
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    socket
        .write_all(
            format!(
                "GET /api/remote.mux HTTP/1.1\r\n\
                 Host: {}\r\n\
                 Cookie: {}={cookie}\r\n\
                 Connection: Upgrade\r\n\
                 Upgrade: websocket\r\n\
                 Sec-WebSocket-Version: 13\r\n\
                 Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
                harness.authority,
                super::session::COOKIE,
            )
            .as_bytes(),
        )
        .expect("a written handshake");

    let mut reader = BufReader::new(socket.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).expect("a status line");
    assert!(
        line.contains("101"),
        "the upgrade was not forwarded: {line}"
    );

    let mut upgrade = None;
    let mut leaked = false;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).expect("a header line");
        let header = header.trim_end().to_ascii_lowercase();
        if header.is_empty() {
            break;
        }
        if let Some(rest) = header.strip_prefix("upgrade:") {
            upgrade = Some(rest.trim().to_string());
        }
        if header.starts_with("set-cookie:") {
            leaked = true;
        }
    }

    // The headers that make it an upgrade are not dropped as hop-by-hop, and
    // the one that must never travel still is.
    assert_eq!(upgrade.as_deref(), Some("websocket"));
    assert!(
        !leaked,
        "a 101 is still a response dsh's cookie must not ride"
    );

    // Now it is a pipe. Nothing here knows what a WebSocket frame is, and
    // neither does the gateway.
    socket.write_all(b"not a frame, just bytes").unwrap();
    let mut echoed = [0u8; 23];
    reader
        .read_exact(&mut echoed)
        .expect("the bytes came back through the gateway");
    assert_eq!(&echoed, b"not a frame, just bytes");
}

/// Kicking a device takes effect on its next request, without anything having
/// to reach out to the phone.
#[test]
fn a_kicked_device_stops_getting_through() {
    let Some(harness) = Harness::raise(true) else {
        return;
    };

    let nonce = harness.nonce();
    let cookie = harness
        .get(&format!("/?pair_token={nonce}"), &[])
        .device_cookie()
        .expect("a device cookie");
    let jar = format!("{}={cookie}", super::session::COOKIE);

    assert_eq!(harness.get("/", &[("Cookie", &jar)]).status, 200);

    let device = harness.shared.store.devices().pop().expect("one device");
    harness.shared.store.revoke(&device.id);

    assert_eq!(harness.get("/", &[("Cookie", &jar)]).status, 401);
}

/// With dsh gone there is nothing to forward to, and the phone is told that
/// rather than handed whatever a connection to a closed port looks like.
#[test]
fn a_phone_is_told_when_dsh_is_not_running() {
    let Some(harness) = Harness::raise(true) else {
        return;
    };

    let nonce = harness.nonce();
    let cookie = harness
        .get(&format!("/?pair_token={nonce}"), &[])
        .device_cookie()
        .expect("a device cookie");
    let jar = format!("{}={cookie}", super::session::COOKIE);

    harness.shared.upstream.gone();

    let answer = harness.get("/", &[("Cookie", &jar)]);
    assert_eq!(answer.status, 503);
    assert!(!answer.body.is_empty(), "with something readable on it");
}

/// The stub's authority, read back off the upstream the harness set.
fn upstream_authority(harness: &Harness) -> String {
    harness
        .shared
        .upstream
        .authority()
        .expect("the stub is the upstream")
}
