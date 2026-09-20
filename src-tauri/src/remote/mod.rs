//! Letting a phone at the dsh session on this machine, without letting the
//! network at it.
//!
//! The shape is one listener on `0.0.0.0` in front of the loopback port dsh
//! already has, and a handshake with a human in it. Nothing about dsh changes:
//! no flag, no patch, no file on disk. dsh goes on believing it is being talked
//! to from this machine's own browser, because as far as the bytes reaching it
//! are concerned it is.
//!
//! ```text
//!   phone ──▶ 0.0.0.0:<gateway>   trust fence ─ device cookie ─ rewrite
//!                    │
//!                    └──▶ 127.0.0.1:<dsh>      dsh's own cookie, held here
//! ```
//!
//! The pieces, each its own file:
//!
//! - [`tunnel`] — how the phone reaches us, and which `Host` values that makes
//!   legitimate. One active at a time; the interface is what kept the third
//!   from being a rewrite.
//! - [`mod@cloudflare`] — that third one. It is apart from the others because it
//!   launches a process and because it is the only channel that puts this
//!   machine on the public internet, which is a thing with a guard around it:
//!   an idle timer, a standing warning in the tray, a question at exit, and a
//!   rule in [`proxy`] that will take nothing but a loopback peer while it is
//!   up.
//! - [`trust`] — the browser fence dsh has and this rewrite disarms, rebuilt on
//!   the near side. Not optional; read the module.
//! - [`session`] — the pairing nonce and the device cookie.
//! - [`upstream`] — dsh's own cookie, got once per dsh process and never shown
//!   to anyone.
//! - [`proxy`] — the listener, the forwarding, and the WebSocket.
//!
//! Plus [`card`], the desktop side of it, and [`firewall`], which is about the
//! one failure that otherwise has no symptom at all.
//!
//! ## What it costs to get this wrong
//!
//! A dsh session is an agent with a shell on the user's machine. A gateway that
//! lets the wrong device in has not leaked a document; it has handed over the
//! computer. That is why there are two independent gates — the trust fence,
//! which is about where a request came from, and the desktop dialog, which is
//! about whether a human said yes — and why neither is skippable by the other.

mod card;
pub mod cloudflare;
mod firewall;
mod patch;
mod proxy;
mod session;
mod style;
mod trust;
mod tunnel;
mod upstream;

#[cfg(test)]
mod tests;

/// Out for [`crate::settings`] to store and [`crate::controls`] to parse, so
/// that the name of a channel is spelled in one place. See
/// [`TunnelType::name`].
pub use tunnel::TunnelType;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager, Url};

use cloudflare::CloudflareTunnel;
use session::SessionStore;
use tunnel::{LanTunnel, RemoteTunnel, TailscaleTunnel, TunnelState};
use upstream::Upstream;

/// How long the card waits before suggesting the firewall.
///
/// Counted from the moment the code goes on screen, and reset by nothing: what
/// it is waiting for is a single inbound connection, successful or not, which
/// is the first thing that happens when a phone opens the URL. Twelve seconds
/// is long enough to find the phone, unlock it and open the camera; a hint that
/// arrived sooner would be accusing the firewall of what is really just a user
/// walking across the room.
const SILENCE: Duration = Duration::from_secs(12);

/// How long [`Shared::authorities`] may answer from the last enumeration.
///
/// A second. Long enough that one page load's requests share a single walk of
/// the machine's network adapters, short enough that nobody notices it when a
/// laptop lands on a new network — and short enough that it is never the thing
/// a stale fence is blamed on.
const AUTHORITIES_TTL: Duration = Duration::from_secs(1);

/// How long a public tunnel may stand with nothing using it before it is taken
/// down on its own.
///
/// Thirty minutes, and the reason this exists at all is the asymmetry between
/// the channels. A LAN gateway left running overnight is reachable by whoever
/// is on the same Wi-Fi; a Cloudflare tunnel left running overnight is
/// reachable by the internet, and what is behind it is a shell on this machine.
/// Forgetting to switch it off is the ordinary human failure, so the feature
/// does not rely on nobody ever forgetting.
///
/// "Nothing using it" is both halves of the question, because either one alone
/// is wrong: a phone sitting in a session holds a WebSocket open and sends no
/// requests for hours, and a phone that has closed its tab leaves a device on
/// the list forever. So it is *no live connection* and *no request* — see
/// [`Shared::idle`].
const PUBLIC_IDLE: Duration = Duration::from_secs(30 * 60);

/// How often that is checked. A minute is far finer than the thing it is
/// measuring and costs two atomic reads.
const IDLE_TICK: Duration = Duration::from_secs(60);

/// How long a tunnel may be [`TunnelState::Starting`] before the card stops
/// waiting for it.
///
/// `cloudflared` takes a few seconds to register a connection on a good
/// network, and rather longer on a bad one. Forty-five seconds is past the
/// point where a user is still willing to watch, and the message that follows
/// is better than a card that says "starting" forever.
const STARTUP: Duration = Duration::from_secs(45);

/// How often the card looks while a tunnel is coming up.
const STARTUP_TICK: Duration = Duration::from_millis(300);

/// Who decides whether a device that redeemed a nonce gets in.
///
/// In the app it is a human at the desktop, asked through [`crate::dialog`].
/// The second arm exists so that [`tests`] can drive the whole gateway — fence,
/// handshake, token exchange, WebSocket — with no window behind it, which is
/// the only way to exercise those paths at all: everything above them needs a
/// running Tauri app, and everything below them is where the mistakes are.
enum Approve {
    Desktop(AppHandle),
    #[cfg(test)]
    Fixed(bool),
}

impl Approve {
    /// The window to draw the card on, when there is one.
    fn app(&self) -> Option<&AppHandle> {
        match self {
            Self::Desktop(app) => Some(app),
            #[cfg(test)]
            Self::Fixed(_) => None,
        }
    }

    /// Put the question, and answer on the channel.
    fn ask(&self, label: &str, address: &str) -> tokio::sync::oneshot::Receiver<bool> {
        match self {
            Self::Desktop(app) => pairing_requested(app, label, address),
            #[cfg(test)]
            Self::Fixed(answer) => {
                let (send, receive) = tokio::sync::oneshot::channel();
                let _ = send.send(*answer);
                receive
            }
        }
    }
}

/// Everything the listener's tasks share. One `Arc`, handed to every connection.
pub struct Shared {
    approve: Approve,
    store: SessionStore,
    upstream: Upstream,
    /// The one active tunnel. A box rather than a type parameter: the channel
    /// is switched while the app runs — see [`Remote::switch`] — and there is
    /// never more than one, because `base_url` is the single authority every
    /// pairing URL and every entry in [`Shared::authorities`] is derived from.
    tunnel: Mutex<Box<dyn RemoteTunnel>>,
    /// The last answer [`Shared::authorities`] gave, and when it gave it.
    authorities: Mutex<Option<(std::time::Instant, Vec<String>)>>,
    /// Wrong short codes, per address. The QR's nonce is not counted — 128 bits
    /// is not a thing anyone guesses — so this is only ever touched by the
    /// typed entrance. See [`trust::Guesses`].
    guesses: trust::Guesses,
    /// Held across the launch-token exchange so that a page load's thirty
    /// simultaneous requests do it once between them; see
    /// `proxy::ensure_cookie`.
    exchange: tokio::sync::Mutex<()>,
    /// How many connections the listener has accepted. Only ever compared
    /// against zero — see [`SILENCE`] — so nothing depends on it being exact.
    seen: AtomicU64,
    /// How many WebSockets are open through the gateway right now.
    ///
    /// The only thing in this app that knows a device is *connected* rather
    /// than merely paired. The device list is a record of who has been let in;
    /// this is a count of who is holding a socket, which is what the idle timer
    /// has to ask about — a phone in a session sends no HTTP requests for as
    /// long as it is listening. See the WebSocket handover in [`proxy`].
    live: AtomicU64,
    /// When the last request arrived. See [`PUBLIC_IDLE`].
    last: Mutex<Instant>,
}

impl Shared {
    /// The `Host` values a request may carry right now, from the tunnel that
    /// would have carried it.
    ///
    /// Cached for [`AUTHORITIES_TTL`], because [`trust::provenance`] asks on
    /// every single request — thirty of them for one page load — and the LAN's
    /// answer walks every network adapter on the machine to produce it. The TTL
    /// is what keeps the other half of the promise: the set is a fact about the
    /// network this laptop is on *now*, so a machine that moved to another
    /// Wi-Fi has to be let back in without being restarted.
    fn authorities(&self) -> Vec<String> {
        let now = std::time::Instant::now();

        if let Some((taken, cached)) = self.authorities.lock().unwrap().as_ref() {
            if now.duration_since(*taken) < AUTHORITIES_TTL {
                return cached.clone();
            }
        }

        // Not while holding the cache: this is the slow call, and the lock it
        // wants is the tunnel's.
        let fresh = self.tunnel.lock().unwrap().authorities();
        *self.authorities.lock().unwrap() = Some((now, fresh.clone()));
        fresh
    }

    /// Throw the cached answer away. For the moments when waiting a second for
    /// it to lapse would mean a fence that is open on a tunnel that is gone.
    fn forget_authorities(&self) {
        *self.authorities.lock().unwrap() = None;
    }

    /// Whether a cookie issued now may carry `Secure`. See [`tunnel::Scheme`].
    fn secure(&self) -> bool {
        self.tunnel.lock().unwrap().scheme().secure()
    }

    /// Where the active tunnel has got to. See [`TunnelState`].
    fn state(&self) -> TunnelState {
        self.tunnel.lock().unwrap().state()
    }

    /// Whether the active channel reaches the public internet.
    /// See [`RemoteTunnel::public`].
    fn public(&self) -> bool {
        self.tunnel.lock().unwrap().public()
    }

    /// A request arrived. What the idle timer counts from.
    fn touched(&self) {
        *self.last.lock().unwrap() = Instant::now();
    }

    /// Whether nothing at all has used this gateway for `how_long`.
    ///
    /// Both halves, and neither is sufficient. A live WebSocket is a phone in
    /// a session, which makes no requests while it waits for the agent to
    /// think; a quiet stretch with no socket open is a gateway nobody is on.
    fn idle(&self, how_long: Duration) -> bool {
        self.live.load(Ordering::Relaxed) == 0 && self.last.lock().unwrap().elapsed() >= how_long
    }

    /// Who is actually asking, as the active tunnel accounts for it.
    ///
    /// The socket's peer is the fallback and today it is also always the
    /// answer, because neither tunnel in M2 has a proxy in front of it. The
    /// call goes through the tunnel all the same: the day one does, the two
    /// places that care — the rate limiter and the desktop dialog — are already
    /// asking the party that knows. See [`RemoteTunnel::client_ip`].
    fn client_ip(&self, headers: &http::HeaderMap, peer: std::net::SocketAddr) -> std::net::IpAddr {
        self.tunnel
            .lock()
            .unwrap()
            .client_ip(headers, peer.ip())
            .unwrap_or_else(|| peer.ip())
    }
}

/// The app-wide handle, in Tauri's state.
pub struct Remote {
    shared: Arc<Shared>,
    running: Mutex<Option<Running>>,
    /// The nonce currently on the card, or `None` when no card is up. What
    /// makes a redraw possible without minting a second one.
    showing: Mutex<Option<Showing>>,
    /// Whether a card is on screen at all.
    ///
    /// Not the same question as `showing`, and the difference is new in M3: a
    /// tunnel that takes seconds to come up has a card with no nonce on it,
    /// drawn and waiting. `showing` still means "there is a live nonce printed
    /// on the card", which is what [`remint`] is about; this is what the
    /// watchers ask before they redraw something the user has since closed.
    card: AtomicBool,
    /// Something to tell the user that did not come from what they just did —
    /// the idle timer taking a public tunnel down, say. Shown once and cleared.
    note: Mutex<Option<String>>,
    /// Whether an idle watch is already running. See [`watch_idle`], which is
    /// reached from every redraw of a running public tunnel and must start one
    /// thread rather than one per draw.
    idle_watch: AtomicBool,
    /// Asked once — it costs a second and a megabyte of text — and remembered.
    firewall: OnceLock<firewall::Firewall>,
}

/// An unstarted tunnel of the asked-for kind.
///
/// The one place a [`TunnelType`] becomes an implementation, so that the stored
/// setting, the card's buttons and [`Remote::switch`] all agree on what the
/// name means.
///
/// The handle is what the Cloudflare tunnels read their token, their hostname
/// and the path to `cloudflared` out of. `None` is the test harness, where
/// there is no app and the only tunnels raised are the two that need nothing
/// from one.
fn raise(app: Option<&AppHandle>, kind: TunnelType) -> Box<dyn RemoteTunnel> {
    match kind {
        TunnelType::Lan => Box::new(LanTunnel::default()),
        TunnelType::Tailscale => Box::new(TailscaleTunnel::default()),
        TunnelType::Cloudflare => Box::new(CloudflareTunnel::new(app)),
    }
}

/// The nonce on the card, in both of the shapes the card draws it in: one for
/// the QR and the copy button, one for the phone that would rather type.
#[derive(Clone)]
struct Showing {
    url: String,
    code: String,
}

/// The listener, while there is one.
///
/// Note what is *not* here any more: the address. It used to be kept beside the
/// port and replaced whenever the channel moved, which made two things that had
/// to agree about where the phone was being sent. Since M3 the tunnel can
/// arrive at an address seconds after being asked to — and leave it again
/// without anybody calling anything — so the tunnel's own
/// [`RemoteTunnel::state`] is the single answer, and this is only the socket.
struct Running {
    /// The port the socket is bound to, so that a new tunnel can be told to
    /// publish the listener that is already up rather than needing a new one.
    port: u16,
    /// Dropped to stop the accept loop. See [`proxy::serve`].
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Remote {
    pub fn new(app: AppHandle) -> Self {
        // Read before the handle is moved into `approve`, and read here rather
        // than at the first `start`: the channel decides which tunnel exists,
        // and `resume` raises one before any card has been opened to pick it.
        let channel = crate::settings::channel(&app);

        Self {
            shared: Arc::new(Shared {
                store: SessionStore::persisted(&app),
                tunnel: Mutex::new(raise(Some(&app), channel)),
                approve: Approve::Desktop(app),
                upstream: Upstream::default(),
                authorities: Mutex::new(None),
                guesses: trust::Guesses::default(),
                exchange: tokio::sync::Mutex::new(()),
                seen: AtomicU64::new(0),
                live: AtomicU64::new(0),
                last: Mutex::new(Instant::now()),
            }),
            running: Mutex::new(None),
            showing: Mutex::new(None),
            card: AtomicBool::new(false),
            note: Mutex::new(None),
            idle_watch: AtomicBool::new(false),
            firewall: OnceLock::new(),
        }
    }

    /// Bind and start serving, or answer with the address already being served.
    ///
    /// Nothing else needs to know the port in advance — that is the part of
    /// method A the specification calls out, the alternative being a
    /// `--trusted-host` handed to dsh at launch, which would make the gateway's
    /// port something dsh has to be restarted to change. What that argument
    /// settles is that the port need not be *agreed*; it says nothing about
    /// whether it should *move*, and the phone is the party that suffers when
    /// it does. See [`crate::settings::gateway_port`].
    ///
    /// So: ask for the port last time ended on, and take whatever the OS gives
    /// if that one is spoken for.
    ///
    /// Answers `Ok` as soon as the tunnel has been *asked* to start, which is
    /// not the same as its being up — see [`RemoteTunnel::start`]. The address
    /// comes out of [`Shared::state`] when there is one, and the card is what
    /// waits; see [`watch_startup`].
    ///
    /// Called a second time on a gateway that is already bound, this restarts
    /// the tunnel if there is nothing behind it — which is the way back from a
    /// Cloudflare token that was wrong, and from the idle timer having taken a
    /// public tunnel down. A tunnel that is up, or on its way up, is left
    /// alone.
    fn start(&self) -> Result<(), String> {
        let mut running = self.running.lock().unwrap();

        if let Some(live) = running.as_ref() {
            let port = live.port;
            let outcome = {
                let mut tunnel = self.shared.tunnel.lock().unwrap();
                match tunnel.state() {
                    TunnelState::Running { .. } | TunnelState::Starting => Ok(()),
                    _ => tunnel.start(port).map_err(|error| error.to_string()),
                }
            };
            self.shared.forget_authorities();
            return outcome;
        }

        let remembered = self
            .shared
            .approve
            .app()
            .and_then(crate::settings::gateway_port);

        let listener = bind(remembered).map_err(|error| error.to_string())?;

        let port = listener
            .local_addr()
            .map_err(|error| error.to_string())?
            .port();

        // Written on every launch that did not land where it meant to: the
        // first one, and any after it that found the port taken. Writing the
        // port back unconditionally would rewrite the file on every single
        // launch for no change.
        if Some(port) != remembered {
            if let Some(app) = self.shared.approve.app() {
                crate::settings::set_gateway_port(app, port);
            }
        }

        // After the bind, because the tunnel is told which port to publish. A
        // machine with no network — or with no `cloudflared` — fails here, with
        // the socket already open, which is why nothing is recorded as running
        // until this has succeeded: the listener is dropped on the way out and
        // the next attempt starts from a clean bind.
        let over = {
            let mut tunnel = self.shared.tunnel.lock().unwrap();
            tunnel.start(port).map_err(|error| error.to_string())?;
            tunnel.tunnel_type()
        };

        // The set the fence holds is the old tunnel's, or an empty one from
        // before this started. Either way it is not this port's.
        self.shared.forget_authorities();

        // The one line this writes anywhere. A socket bound to every interface
        // on the machine is worth saying out loud, and the channel it is bound
        // for is the part that changes. The address is not here: over a tunnel
        // there is not one yet, and the line that reports it belongs to
        // whichever tunnel arrives at one.
        eprintln!("dsh-desktop: the phone gateway is listening on {port} over {over:?}");

        let (stop, stopped) = tokio::sync::oneshot::channel();
        tauri::async_runtime::spawn(proxy::serve(listener, self.shared.clone(), stopped));

        // A fresh idle window: the clock this starts is what takes a public
        // tunnel down again, and it must not be inherited from whenever the
        // last request before this happened to arrive.
        self.shared.touched();

        *running = Some(Running {
            port,
            shutdown: Some(stop),
        });
        Ok(())
    }

    /// Move to another channel, on the socket that is already bound.
    ///
    /// The listener does not move and does not need to: both of these tunnels
    /// publish an address for the same `0.0.0.0` socket, and *which* address is
    /// the whole of the difference between them. So the old tunnel is stopped,
    /// the new one is started on the same port, and the base URL every pairing
    /// URL is built from is replaced.
    ///
    /// The paired devices are left alone. Their cookies are scoped by the
    /// browser to the host they were issued on, so none of them will be sent to
    /// the new address and every phone has to pair again there — but that is
    /// the browser's doing, not a revocation, and it runs backwards too: a user
    /// who switches to the tailnet and home again finds the LAN pairings still
    /// good. Throwing them away here would turn a reversible change into a
    /// permanent one. What the user is told before this happens is in
    /// [`channel`].
    ///
    /// A channel that will not start leaves the gateway bound and publishing
    /// nothing, which the fence reads as "no authorities" and refuses
    /// everything. That is the correct answer to "put me on a tailnet this
    /// machine is not on", and switching back undoes it.
    fn switch(&self, kind: TunnelType) -> Result<(), String> {
        let running = self.running.lock().unwrap();
        let started = {
            let mut tunnel = self.shared.tunnel.lock().unwrap();

            // Stopped before it is replaced, and not merely dropped: a
            // Cloudflare tunnel's `stop` kills a process and waits for it, and
            // the one thing worse than a tunnel that will not start is two of
            // them answering for the same machine.
            let _ = tunnel.stop();
            *tunnel = raise(self.shared.approve.app(), kind);

            running
                .as_ref()
                .map(|live| tunnel.start(live.port).map_err(|error| error.to_string()))
        };

        drop(running);
        self.shared.forget_authorities();
        // The rate limiter's addresses only mean anything relative to a
        // channel; see [`trust::Guesses::forget_all`].
        self.shared.guesses.forget_all();

        // `None` is nothing bound, so there is nothing to publish and nothing
        // to repair: the next `start` raises this tunnel instead.
        started.unwrap_or(Ok(()))
    }

    /// Which channel is up right now, for the card to draw the switch in the
    /// position it is actually in.
    fn channel(&self) -> TunnelType {
        self.shared.tunnel.lock().unwrap().tunnel_type()
    }

    /// Take the tunnel down and leave the socket where it is.
    ///
    /// The public guard's actual lever, used by the idle timer and by the two
    /// buttons that mean "stop being reachable from the internet". The listener
    /// is deliberately left bound: what is dangerous is the tunnel, and with it
    /// gone the fence holds no authorities and refuses everything anyway — so
    /// tearing down the socket as well would buy nothing and would cost the
    /// user a second firewall prompt when they switch back to the LAN.
    ///
    /// The channel is not changed. A user who took the tunnel down still has
    /// Cloudflare selected, and the card offers to start it again; what keeps
    /// the next launch from quietly raising it is [`resume`], which will not
    /// raise a public channel at all.
    fn halt(&self, why: Option<String>) {
        let _ = self.shared.tunnel.lock().unwrap().stop();
        self.shared.forget_authorities();
        self.shared.guesses.forget_all();
        // The nonce on the card is a URL at the address that has just stopped
        // answering. Leaving it there would put a QR code on screen that sends
        // a phone to nothing — and the card would go on looking live.
        *self.showing.lock().unwrap() = None;
        *self.note.lock().unwrap() = why;
    }

    /// Stop serving. Whether the devices go with it is the user's call.
    ///
    /// This used to throw them all off unconditionally, which was not so much a
    /// policy as a description: the key was in memory, so closing the app ended
    /// every session whatever this function did. Now that the key outlives the
    /// process, the line has to say what it means — and what it means by
    /// default is that the phone works tomorrow. See
    /// [`crate::settings::forget_pairings_on_exit`].
    fn stop(&self) {
        if let Some(mut running) = self.running.lock().unwrap().take() {
            if let Some(stop) = running.shutdown.take() {
                let _ = stop.send(());
            }
        }
        let _ = self.shared.tunnel.lock().unwrap().stop();
        // Now, rather than a second from now: a fence still holding the stopped
        // tunnel's addresses is a fence that is open for no tunnel at all.
        self.shared.forget_authorities();

        if self
            .shared
            .approve
            .app()
            .is_some_and(crate::settings::forget_pairings_on_exit)
        {
            self.shared.store.revoke_all();
        }

        *self.showing.lock().unwrap() = None;
        self.card.store(false, Ordering::Relaxed);
    }
}

/// The listener: on the port we asked for, or on one the OS picked.
///
/// A remembered port that will not bind is the ordinary case, not a failure —
/// something else took it while the app was closed, or another copy of this app
/// is up. Falling back is what keeps that from being the end of the feature,
/// and it costs the home-screen icon on that machine until the port is free
/// again, which is strictly better than not starting.
///
/// Only the fallback's error is returned. A machine where binding `0` fails has
/// no usable network stack at all, and that is the failure worth showing on the
/// card — not "port 59123 is busy", which is not something the user can act on.
fn bind(remembered: Option<u16>) -> std::io::Result<tokio::net::TcpListener> {
    let at = |port| {
        tauri::async_runtime::block_on(tokio::net::TcpListener::bind((
            std::net::Ipv4Addr::UNSPECIFIED,
            port,
        )))
    };

    if let Some(port) = remembered {
        if let Ok(listener) = at(port) {
            return Ok(listener);
        }
    }

    at(0)
}

/// dsh is serving at this URL — the first launch and every restart after it.
///
/// The URL carries the launch token, which is what the gateway exchanges for a
/// session of its own. Called even when the gateway is not running: the next one
/// to start then has somewhere to forward to without waiting for dsh to restart.
pub fn dsh_ready(app: &AppHandle, url: &Url) {
    if let Some(remote) = app.try_state::<Remote>() {
        remote.shared.upstream.ready(url);
    }
}

/// dsh stopped, and nothing should be forwarded until it is back.
pub fn dsh_gone(app: &AppHandle) {
    if let Some(remote) = app.try_state::<Remote>() {
        remote.shared.upstream.gone();
    }
}

/// The app is closing: take the listener down.
///
/// The socket is the part that has to go. One still bound while the window is
/// gone is a port answering for an app that no longer exists. The sessions are
/// the part that now does not — they are on the disk, and they are meant to be
/// there when the app comes back. See [`Remote::stop`] for the switch that says
/// otherwise.
pub fn shutdown(app: &AppHandle) {
    if let Some(remote) = app.try_state::<Remote>() {
        remote.stop();
    }
}

/// Launch: put the gateway back up for the phones that are still paired to it.
///
/// Persisting the pairing was only half of what "the phone still works
/// tomorrow" needs. The other half is a socket: a device whose cookie survived
/// the restart still has nowhere to send it until something binds the port, and
/// until this, the only thing that ever did was the titlebar button. A phone
/// opening its home-screen icon on a machine where nobody had pressed it got
/// the browser's connection-refused page — reached before a line of this app
/// runs, so not a page this app can explain. That is the failure the whole of
/// M1 was written to remove, and it was still there.
///
/// Guarded on the device list, so nothing changes for anyone who has not used
/// the feature: no port, no firewall prompt, nothing on the network. And
/// guarded on the switch, because a user who asked for every phone to be
/// dropped at exit did not ask for a door to be standing open at launch.
///
/// No nonce is minted and no card goes up. What this raises is the gateway, for
/// devices that are already through the fence; anything new still comes in past
/// a human at the desktop.
pub fn resume(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let Some(remote) = app.try_state::<Remote>() else {
            return;
        };

        if crate::settings::forget_pairings_on_exit(&app) {
            return;
        }
        if remote.shared.store.devices().is_empty() {
            return;
        }
        // And the third gate, which is M3's: a channel that reaches the public
        // internet is never raised without somebody asking for it in this
        // session. Everything else here is about a phone still working
        // tomorrow; putting this machine back on the internet because it was
        // on it when the app last closed is a different promise, and not one
        // the user made. The card is where it comes back up.
        if remote.channel().public() {
            eprintln!(
                "dsh-desktop: not resuming the {} channel on its own; open the card to raise it",
                remote.channel().name()
            );
            return;
        }

        // Nowhere to report a failure to: there is no card up and the user did
        // not ask for anything. The line `start` prints is the record, and the
        // card says why the next time it is opened.
        if let Err(why) = remote.start() {
            eprintln!("dsh-desktop: the phone gateway could not be resumed: {why}");
        }
    });
}

/// The titlebar button: raise the gateway if it is not up, mint a nonce, and
/// put the card on screen.
///
/// On a thread of its own because the first call binds a socket and asks the
/// tunnel for an address, and because this is called from the webview's
/// navigation handler, on the main thread, with the webview waiting on it. See
/// [`crate::controls::perform`].
pub fn open(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let Some(remote) = app.try_state::<Remote>() else {
            return;
        };

        let started = remote.start();
        let up = started.is_ok();
        present(&app, &remote, started);

        // Both of these take a while and neither should hold the card back: the
        // firewall check shells out, and the silence watch is twelve seconds by
        // definition.
        if up {
            watch_firewall(&app);
            watch_silence(&app);
        }
    });
}

/// The card's start button, on a channel that is not running.
///
/// Reached three ways, all of them a public tunnel that is down while the
/// gateway is not: a token that was wrong and has been fixed, a `cloudflared`
/// that fell over, and the idle timer having done its job. See [`Remote::halt`]
/// for why the socket is still there to start a tunnel on.
pub fn start_channel(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let Some(remote) = app.try_state::<Remote>() else {
            return;
        };

        *remote.note.lock().unwrap() = None;
        let started = remote.start();
        present(&app, &remote, started);
    });
}

/// Stop being reachable from the internet, now.
///
/// Both the card's button and the tray's item, which is the specification's
/// "click straight through to switching it off" — the standing indicator is no
/// use if acting on it means finding a card first. See [`Remote::halt`].
pub fn stop_public(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let Some(remote) = app.try_state::<Remote>() else {
            return;
        };

        remote.halt(Some(
            t!(
                "公网通道已经关闭。这台电脑现在只能从这台机器自己的网络访问。",
                "The public tunnel is off. This computer is only reachable from its own \
                 network again."
            )
            .to_string(),
        ));
        redraw(&app, &remote, None);
        crate::refresh_tray(&app);
    });
}

/// The hostname this machine is answering on from the public internet, if it
/// is answering on one.
///
/// Read by the tray, which draws the standing warning, and by the question at
/// exit. `None` covers every ordinary case: the LAN, the tailnet, a public
/// channel that is selected but not up, and no gateway at all.
pub fn exposed(app: &AppHandle) -> Option<String> {
    let remote = app.try_state::<Remote>()?;
    if !remote.shared.public() {
        return None;
    }
    let base = remote.shared.state().base_url()?.to_string();
    Some(base.trim_start_matches("https://").to_string())
}

/// Put the card up on whatever the gateway's state now is: a fresh nonce over
/// the address it is publishing, the fact that it is still coming up, or the
/// reason there is no address.
///
/// The ways to arrive here are the titlebar button, a channel switch, the start
/// button and the watcher below, and they all want the same card — which is why
/// the nonce is minted here rather than by whichever of them happened to run.
fn present(app: &AppHandle, remote: &Remote, started: Result<(), String>) {
    remote.card.store(true, Ordering::Relaxed);

    // The tunnel may have arrived somewhere new since the last draw, and the
    // tray's standing warning is the one thing that has to be right whether or
    // not anybody is looking at this card.
    crate::refresh_tray(app);

    let state = match started {
        Ok(()) => remote.shared.state(),
        // What could be known before anything was launched: no network card,
        // no tailnet, no `cloudflared`, no token.
        Err(why) => TunnelState::Failed(why),
    };

    match state {
        TunnelState::Running { base_url } => {
            let minted = remote.shared.store.mint_pair();
            *remote.showing.lock().unwrap() = Some(Showing {
                url: format!("{base_url}/?pair_token={}", minted.token),
                code: minted.code,
            });
            redraw(app, remote, None);

            // Started here rather than beside the tunnel because this is the
            // one place that knows a public tunnel has actually arrived
            // somewhere — `start` returns before that is true.
            if remote.shared.public() {
                watch_idle(app);
            }
        }
        // No nonce yet, because there is nowhere to send anyone. The card goes
        // up all the same, saying so — the alternative is a button that looks
        // like it did nothing for the five seconds `cloudflared` takes.
        TunnelState::Starting => {
            *remote.showing.lock().unwrap() = None;
            waiting(app, remote, None);
            watch_startup(app);
        }
        TunnelState::Failed(why) => {
            *remote.showing.lock().unwrap() = None;
            waiting(app, remote, Some(why));
        }
        // Selected but not running: the idle timer, or the button that stops a
        // public tunnel. `note` is what says which.
        TunnelState::Stopped => {
            *remote.showing.lock().unwrap() = None;
            waiting(app, remote, None);
        }
    }
}

/// The card with no nonce on it: coming up, stopped, or broken.
///
/// The device list is still drawn, and still paired. A channel that will not
/// start has thrown nobody off, and those rows are what say how much is waiting
/// on it starting.
fn waiting(app: &AppHandle, remote: &Remote, error: Option<String>) {
    let channel = remote.channel();
    let starting = matches!(remote.shared.state(), TunnelState::Starting);
    // Read into a local rather than inline below: a guard taken inside a struct
    // literal lives until the end of the whole statement, and the two calls
    // beside it take the other two locks in this module.
    let hint = remote.note.lock().unwrap().clone();

    card::show(
        app,
        &card::View {
            url: None,
            code: None,
            error,
            starting,
            devices: remote.shared.store.devices(),
            hint,
            channel,
            setup: setup(app, remote, channel),
            public: exposed(app),
            style_patch: style::enabled(),
            forget_on_exit: crate::settings::forget_pairings_on_exit(app),
        },
    );
}

/// What the Cloudflare panel on the card needs, or `None` on a channel that has
/// nothing to configure.
///
/// Read off the disk rather than out of the tunnel. The tunnel holds the same
/// three facts, but only behind the trait — and reaching through it would mean
/// either a downcast or three more methods on an interface that three other
/// things implement and do not have them.
fn setup(app: &AppHandle, remote: &Remote, kind: TunnelType) -> Option<card::Setup> {
    if !kind.public() {
        return None;
    }

    Some(card::Setup {
        install: cloudflare::binary(Some(app))
            .is_none()
            .then(cloudflare::install_hint),
        hostname: crate::settings::cloudflare_hostname(app),
        // The one number the dashboard's ingress rule has to carry. Stable
        // across restarts since M1, which is what makes it worth printing.
        origin: remote
            .running
            .lock()
            .unwrap()
            .as_ref()
            .map(|live| format!("http://localhost:{}", live.port)),
    })
}

/// Watch a tunnel that is on its way up, and draw the card again when it gets
/// somewhere.
///
/// Polling, and deliberately: the thing being waited on is a process reading
/// its way to a connection, so the alternative is a channel threaded from a
/// reader thread through a trait object and out to the window — for an event
/// that happens once, seconds from now, on one of three channels.
///
/// Gives up at [`STARTUP`], and says so rather than leaving the word "starting"
/// on screen. The tunnel is left where it is: `cloudflared` may still be
/// retrying, and a user who watches it finally connect and presses the button
/// again finds it already up.
fn watch_startup(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let until = Instant::now() + STARTUP;

        loop {
            std::thread::sleep(STARTUP_TICK);

            let Some(remote) = app.try_state::<Remote>() else {
                return;
            };
            // The user closed the card. Putting it back up because a tunnel
            // they stopped waiting for finally connected is the same mistake
            // `remint` is written to avoid.
            if !remote.card.load(Ordering::Relaxed) {
                return;
            }

            if !matches!(remote.shared.state(), TunnelState::Starting) {
                return present(&app, &remote, Ok(()));
            }

            if Instant::now() >= until {
                return waiting(
                    &app,
                    &remote,
                    Some(
                        t!(
                            "cloudflared 起了，但是一直没连上 Cloudflare。检查一下网络和 token，\
                             或者先切回局域网。",
                            "cloudflared started but has not reached Cloudflare. Check the \
                             network and the token, or switch back to the local network."
                        )
                        .to_string(),
                    ),
                );
            }
        }
    });
}

/// The idle timer: take a public tunnel down when nothing has used it for
/// [`PUBLIC_IDLE`].
///
/// One thread for the life of the tunnel, started when a public channel is
/// raised and ending as soon as the channel is no longer public or no longer
/// running — so the LAN and the tailnet never have one of these at all, and
/// there is never more than one per tunnel.
///
/// It reports itself. A tunnel that vanished without explanation is a phone
/// that stops working for no reason anybody can see, which is worse than the
/// exposure this is preventing.
fn watch_idle(app: &AppHandle) {
    let Some(remote) = app.try_state::<Remote>() else {
        return;
    };
    // One at a time. Every redraw of a running public tunnel reaches here, and
    // a thread per redraw would be a thread per device that connects.
    if remote.idle_watch.swap(true, Ordering::Relaxed) {
        return;
    }

    let app = app.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(IDLE_TICK);

            let Some(remote) = app.try_state::<Remote>() else {
                return;
            };
            // Switched away, stopped by hand, or fallen over on its own.
            // Whatever happened, this watch is over; the next public tunnel
            // that comes up starts another.
            if !remote.shared.public() || remote.shared.state().base_url().is_none() {
                remote.idle_watch.store(false, Ordering::Relaxed);
                return;
            }
            if !remote.shared.idle(PUBLIC_IDLE) {
                continue;
            }

            let minutes = PUBLIC_IDLE.as_secs() / 60;
            eprintln!("dsh-desktop: stopping the public tunnel after {minutes} idle minutes");
            remote.halt(Some(
                t!(
                    "{} 分钟没有设备连接，公网通道已经自动关闭。需要的话在这里重新打开。",
                    "Nothing has used the public tunnel for {} minutes, so it has been \
                     switched off. Start it again here when you need it.",
                    minutes
                )
                .to_string(),
            ));
            redraw(&app, &remote, None);
            crate::refresh_tray(&app);
            remote.idle_watch.store(false, Ordering::Relaxed);
            return;
        }
    });
}

/// The card's channel switch.
///
/// Every paired phone has to scan again on the new channel — the browser scopes
/// the device cookie to the host it was issued on, and the host is exactly what
/// changes — so a switch with devices on the list asks first. Nothing is
/// revoked either way; see [`Remote::switch`].
pub fn channel(app: &AppHandle, kind: TunnelType) {
    let Some(remote) = app.try_state::<Remote>() else {
        return;
    };
    if remote.channel() == kind {
        return;
    }

    let waiting = remote.shared.store.devices().len();
    if waiting == 0 && !kind.public() {
        return switch_to(app, kind);
    }

    // Two things to say and they stack: the pairings that will have to be
    // redone, and — on the way to Cloudflare — what the channel actually is.
    // The second one is not a formality. Every other channel in this app is
    // reachable by people who are already somewhere the user let them be; this
    // one is reachable by everyone, and what is behind it is a shell.
    let mut body = String::new();
    if kind.public() {
        body.push_str(t!(
            "Cloudflare 通道会把这台电脑放到公网上：拿到地址并且通过配对的设备，在任何网络下都能进来。\
             而 dsh 会话等于这台机器上的一个终端。闲置 30 分钟会自动关闭，托盘里也会一直显示它开着。",
            "The Cloudflare channel puts this computer on the public internet: a paired device \
             with the address can reach it from any network at all. A dsh session is a terminal \
             on this machine. It switches itself off after 30 idle minutes, and the tray says \
             so for as long as it is up."
        ));
    }
    if waiting > 0 {
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str(&t!(
            "换一个通道，手机看到的地址就变了，而配对是跟着地址走的：现在的 {} 台设备都要重新扫一次码。\
             它们不会被吊销——换回来的话，原来的配对还在。",
            "The phone reaches a different address over a different channel, and a pairing \
             follows the address: all {} of the paired devices will have to scan again. \
             Nothing is revoked — switch back and the old pairings still hold.",
            waiting
        ));
    }

    let app_for_answer = app.clone();
    crate::dialog::ask(
        app,
        crate::dialog::Ask {
            title: if kind.public() {
                t!(
                    "把这台电脑放到公网上？",
                    "Put this computer on the internet?"
                )
                .to_string()
            } else {
                t!("切换连接通道", "Change the channel").to_string()
            },
            body,
            choices: vec![
                crate::dialog::Choice::new("keep", t!("取消", "Cancel")),
                crate::dialog::Choice::primary("switch", t!("切换", "Switch")),
            ],
            // A cancel needs no redraw: the card draws the channel it was told
            // about, and it was told nothing.
            answered: Box::new(move |_app, id| {
                if id == "switch" {
                    switch_to(&app_for_answer, kind);
                }
            }),
        },
    );
}

/// Change channel and put the card back up on the result.
///
/// On a thread for the same reason [`open`] is: this runs from the webview's
/// navigation handler or from a dialog's answer, and raising a tunnel walks
/// every network adapter on the machine.
fn switch_to(app: &AppHandle, kind: TunnelType) {
    let app = app.clone();
    std::thread::spawn(move || {
        let Some(remote) = app.try_state::<Remote>() else {
            return;
        };

        // Written before the attempt, not after it: the user asked for this
        // channel, and a tailnet that is not up yet is a reason to show them
        // why — not a reason to quietly put them back on the LAN and have the
        // switch snap back under their hand.
        crate::settings::set_channel(&app, kind);

        let started = remote.switch(kind).and_then(|()| remote.start());
        present(&app, &remote, started);
    });
}

/// The card was closed. The gateway stays up — the phones already on it are
/// still working, and that is the point of the feature.
pub fn close(app: &AppHandle) {
    if let Some(remote) = app.try_state::<Remote>() {
        *remote.showing.lock().unwrap() = None;
        remote.card.store(false, Ordering::Relaxed);
        // Read and gone: what it had to say was about something that already
        // happened, and a card opened tomorrow should not still be reporting
        // it.
        *remote.note.lock().unwrap() = None;
    }
    card::hide(app);
}

/// The Cloudflare panel's save button: a token and a hostname, as typed.
///
/// Written down first and acted on second, and only acted on at all when
/// Cloudflare is the channel the user is looking at — the same order
/// [`switch_to`] uses, and for the same reason. Somebody filling this in from
/// the tailnet is configuring the thing for later, not asking to be put on the
/// internet now.
pub fn cloudflare(app: &AppHandle, token: &str, hostname: &str) {
    let app = app.clone();
    let token = token.to_string();
    let hostname = hostname.to_string();

    std::thread::spawn(move || {
        let Some(remote) = app.try_state::<Remote>() else {
            return;
        };

        if let Err(why) = cloudflare::remember(&app, &token, &hostname) {
            *remote.note.lock().unwrap() = None;
            return waiting(&app, &remote, Some(why));
        }

        if remote.channel() != TunnelType::Cloudflare {
            return waiting(&app, &remote, None);
        }

        // The tunnel in hand was built from the old token, so this is a switch
        // to the channel it is already on: `switch` is what rebuilds it, and
        // rebuilding is what reads the file that was just written.
        *remote.note.lock().unwrap() = None;
        let started = remote
            .switch(TunnelType::Cloudflare)
            .and_then(|()| remote.start());
        present(&app, &remote, started);
    });
}

/// The app is being asked to quit while a public tunnel is up.
///
/// `true` holds the exit open. A tunnel that is taken down silently at exit is
/// the right outcome and the wrong way to reach it: the user cannot tell the
/// difference between having switched it off and having left it on, and the
/// next time they leave it on they will assume the same. So the question is
/// put, once — the latch is what lets the answer through — and quitting is
/// still one click away.
///
/// Only for the public channels. Closing the app on the LAN takes a socket down
/// and nothing else, which nobody needs to be asked about.
pub fn confirm_exit(app: &AppHandle) -> bool {
    /// Set by the answer, so the `app.exit` it triggers is not asked again.
    static LEAVING: AtomicBool = AtomicBool::new(false);

    if LEAVING.load(Ordering::Relaxed) {
        return false;
    }
    let Some(host) = exposed(app) else {
        return false;
    };

    // The window is very likely parked in the tray, and a dialog drawn into a
    // hidden webview is a question nobody is shown and nobody answers.
    crate::reveal(app);

    let app_for_answer = app.clone();
    crate::dialog::ask(
        app,
        crate::dialog::Ask {
            title: t!("公网通道还开着", "The public tunnel is still up").to_string(),
            body: t!(
                "这台电脑现在能从公网通过 {} 访问。退出会把通道一起关掉——先确认一下这就是你想要的。",
                "This computer is currently reachable from the internet at {}. Quitting takes \
                 the tunnel down with it; this is the moment to be sure that is what you want.",
                host
            ),
            choices: vec![
                crate::dialog::Choice::new("stay", t!("留下", "Stay open")),
                crate::dialog::Choice::primary("quit", t!("关闭通道并退出", "Close it and quit")),
            ],
            answered: Box::new(move |_app, id| {
                if id == "quit" {
                    LEAVING.store(true, Ordering::Relaxed);
                    app_for_answer.exit(0);
                }
            }),
        },
    );

    true
}

/// Throw one device off.
pub fn kick(app: &AppHandle, id: &str) {
    let Some(remote) = app.try_state::<Remote>() else {
        return;
    };
    remote.shared.store.revoke(id);
    redraw(app, &remote, None);
}

/// Throw them all off and change the key, so that nothing signed before now
/// verifies again. See [`SessionStore::revoke_all`].
pub fn kick_all(app: &AppHandle) {
    let Some(remote) = app.try_state::<Remote>() else {
        return;
    };
    remote.shared.store.revoke_all();

    // The nonce on the card went with the rest of them, so the code on screen
    // is now one nothing will redeem. A fresh one, rather than a card that
    // looks live and is not.
    remint(&remote);
    redraw(app, &remote, None);
}

/// The button under the six characters: throw away the nonce on the card and
/// put a new one up.
pub fn refresh(app: &AppHandle) {
    let Some(remote) = app.try_state::<Remote>() else {
        return;
    };
    remint(&remote);
    redraw(app, &remote, None);
}

/// A nonce was just redeemed, and the card is still printing it.
///
/// The one on screen is spent the moment the phone reaches [`proxy::pair`] —
/// before the dialog goes up, deliberately, so that a refusal cannot be retried
/// — which leaves the card showing a QR and six characters that nothing will
/// take. So the redraw that follows a handshake carries a new nonce, whatever
/// the human goes on to answer.
fn spent(shared: &Shared) {
    let Some(app) = shared.approve.app() else {
        return;
    };
    if let Some(remote) = app.try_state::<Remote>() {
        remint(&remote);
        redraw(app, &remote, None);
    }
}

/// Mint a nonce and make it the one the card is showing.
///
/// Does nothing when no card is up. `showing` is what says whether there is
/// one — see [`close`] — so filling it here would put a card back on screen
/// that the user closed, which is what a phone pairing unobserved would
/// otherwise do.
///
/// The nonce being replaced is not revoked: it is single-use and five minutes
/// from expiring anyway, and [`SessionStore::mint_pair`] leaves outstanding
/// ones alone on purpose — a camera already pointed at the old code is the case
/// that would break.
fn remint(remote: &Remote) {
    if remote.showing.lock().unwrap().is_none() {
        return;
    }

    let minted = remote.shared.store.mint_pair();
    let base = remote.shared.state().base_url().map(str::to_string);
    let next = base.map(|base| Showing {
        url: format!("{base}/?pair_token={}", minted.token),
        code: minted.code,
    });
    *remote.showing.lock().unwrap() = next;
}

/// A phone redeemed a nonce and is waiting at the far end of a held-open
/// request. Ask the human.
///
/// The dialog is this app's own — see [`crate::dialog`] — so it looks the same
/// on all three platforms and does not block the thread that raised it. The
/// answer comes back on the channel; a window that could not show the dialog
/// drops the sender, which the proxy reads as a refusal.
fn pairing_requested(
    app: &AppHandle,
    label: &str,
    address: &str,
) -> tokio::sync::oneshot::Receiver<bool> {
    let (answered, answer) = tokio::sync::oneshot::channel();

    crate::dialog::ask(
        app,
        crate::dialog::Ask {
            title: t!("检测到移动设备请求连接", "A phone is asking to connect").to_string(),
            body: t!(
                "{} （{}）扫了配对码。允许之后，它能看到你的会话、也能替你回答 dsh 的提问——\
                 和坐在这台电脑前是一样的权限。",
                "{} ({}) scanned the pairing code. Allowing it gives that device your \
                 sessions and the ability to answer dsh's questions — the same reach as \
                 sitting at this computer.",
                label,
                address
            ),
            choices: vec![
                crate::dialog::Choice::new("deny", t!("拒绝", "Deny")),
                crate::dialog::Choice::primary("allow", t!("允许连接", "Allow")),
            ],
            answered: Box::new(move |_app, id| {
                let _ = answered.send(id == "allow");
            }),
        },
    );

    answer
}

/// A device got in. Put it on the card, if there is a card and a window under
/// it.
fn paired(shared: &Shared) {
    let Some(app) = shared.approve.app() else {
        return;
    };
    if let Some(remote) = app.try_state::<Remote>() {
        redraw(app, &remote, None);
    }
}

/// Push the current state of the world to the card.
///
/// Does nothing when no card is up: the state it would draw is read out of the
/// store each time rather than kept anywhere, so there is never a stale view to
/// catch up.
fn redraw(app: &AppHandle, remote: &Remote, hint: Option<String>) {
    let Some(showing) = remote.showing.lock().unwrap().clone() else {
        // No nonce. There may still be a card — a tunnel coming up, or one the
        // idle timer just took down — and it is the thing with something new on
        // it, so it is redrawn rather than left holding the last state.
        if remote.card.load(Ordering::Relaxed) {
            waiting(app, remote, None);
        }
        return;
    };

    let hint = hint
        .or_else(|| remote.note.lock().unwrap().clone())
        .or_else(|| remote.firewall.get().copied().and_then(card::firewall_hint));
    let channel = remote.channel();

    card::show(
        app,
        &card::View {
            url: Some(showing.url),
            code: Some(showing.code),
            error: None,
            starting: false,
            devices: remote.shared.store.devices(),
            hint,
            channel,
            setup: setup(app, remote, channel),
            public: exposed(app),
            style_patch: style::enabled(),
            forget_on_exit: crate::settings::forget_pairings_on_exit(app),
        },
    );
}

/// Turn the phone's stylesheet patch on or off.
///
/// The card sends the state its box is now in, so this is not a toggle and does
/// not read the flag first: two clicks racing each other settle on whichever
/// arrived last rather than on a parity.
///
/// A phone already looking at a page keeps the styling it loaded with — the
/// patch rides in on dsh's index, and there is no way to take a `<style>` back
/// out of a document this app does not script. The card says so; see
/// `stylePatchWhy` in [`card::text`].
pub fn style(app: &AppHandle, on: bool) {
    let Some(remote) = app.try_state::<Remote>() else {
        return;
    };

    // The error goes on the card rather than into a dialog: the box the user
    // just ticked is right there, and redrawing puts it back where it was.
    let hint = style::set(on).err();
    redraw(app, &remote, hint);

    // Turning it on is the one moment someone is waiting to see it work, so
    // this is the round that reports its own failures. `refresh` does nothing
    // when the switch went the other way.
    patch::refresh(app, true);
}

/// Decide whether closing the app throws every paired phone off.
///
/// Recorded and no more: the phones on the card stay where they are, because
/// this is a choice about what happens at exit and acting on it now would be
/// answering a question nobody asked. The redraw is only to put the box back
/// where the user just clicked it.
pub fn forget_on_exit(app: &AppHandle, on: bool) {
    crate::settings::set_forget_pairings_on_exit(app, on);

    if let Some(remote) = app.try_state::<Remote>() {
        redraw(app, &remote, None);
    }
}

/// Fetch the published stylesheet, if the patch is switched on.
///
/// Here rather than called directly so that startup names one thing in
/// `remote` instead of reaching into its private modules. See [`patch`].
pub fn refresh_patch(app: &AppHandle, loud: bool) {
    patch::refresh(app, loud);
}

/// Ask the firewall what it knows, once, and redraw if it had something to say.
fn watch_firewall(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let Some(remote) = app.try_state::<Remote>() else {
            return;
        };
        if remote.firewall.get().is_some() {
            return;
        }

        let _ = remote.firewall.set(firewall::asked());
        redraw(&app, &remote, None);
    });
}

/// If nothing has so much as connected by the time the code has been up for
/// [`SILENCE`], say what that usually means.
fn watch_silence(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(SILENCE);

        let Some(remote) = app.try_state::<Remote>() else {
            return;
        };
        if remote.shared.seen.load(Ordering::Relaxed) > 0 {
            return;
        }

        redraw(&app, &remote, Some(card::silence_hint()));
    });
}

/// The card's own script, for the window's initialization scripts.
pub fn script() -> String {
    card::script()
}
