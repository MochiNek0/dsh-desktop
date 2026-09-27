//! How the phone reaches the gateway, behind one interface so that it can stop
//! being the local network later without anything above noticing.
//!
//! Three of these, in two files. [`LanTunnel`] is `0.0.0.0` and the machine's
//! own Wi-Fi address; [`TailscaleTunnel`] is the same socket at a `100.x`
//! address that a phone off the Wi-Fi can still open; and
//! [`super::cloudflare::CloudflareTunnel`] — in a file of its own, because it
//! launches a process and puts this machine on the internet — is a hostname
//! anybody can resolve. One is active at a time (see
//! [`crate::remote::Remote::switch`]) and whichever it is, its
//! [`TunnelState::base_url`] is the only authority on where the phone was sent.
//!
//! The trait was here before the second implementation was, and not as
//! decoration: two things above it would otherwise have "the LAN" written into
//! them.
//!
//! The first is the QR code, which is a URL with a scheme in it. A Cloudflare
//! tunnel hands back `https://…`, and a pairing URL assembled from an address
//! and a hardcoded `http://` would have been wrong the day that landed.
//!
//! The second is the trust fence. [`crate::remote::trust`] has to know which
//! `Host` values are this gateway's own, and that set is not a fact about the
//! machine — it is a fact about the channel the request came in on. On the LAN
//! it is every local IPv4 plus the bound port; through a tunnel it is one
//! hostname and no port at all. So the fence asks the tunnel rather than
//! enumerating network cards itself, which is why [`RemoteTunnel::authorities`]
//! is on the trait beside the methods the specification names.
//!
//! [`RemoteTunnel::public`] is the newest of them and the one that is not about
//! reachability at all. Two of these channels can only be opened by somebody
//! the user has already let onto a network; the third can be opened by anyone.
//! Everything that treats those differently — the idle timer, the tray warning,
//! the question at exit, the loopback-only rule in [`super::proxy`] — asks that
//! one method, so that a fourth tunnel joins the guard by answering it rather
//! than by being added to a list somewhere.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

use http::HeaderMap;

/// Which channel the phone came in over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TunnelType {
    /// The local network: bind `0.0.0.0`, hand out this machine's LAN address.
    Lan,
    /// The tailnet: the same socket, at this machine's `100.x` address, reached
    /// by a device that has joined the same one. See [`TailscaleTunnel`].
    Tailscale,
    /// A Cloudflare named tunnel: the user's own hostname, their own token, and
    /// a `cloudflared` this app launches. See [`mod@super::cloudflare`].
    Cloudflare,
}

impl TunnelType {
    /// What this channel is called on the wire: in `desktop.json`, and in the
    /// verb the card signals. Deliberately not the [`Debug`] spelling — that
    /// one is free to change, and this one is written to a file that outlives
    /// the build that wrote it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Lan => "lan",
            Self::Tailscale => "tailscale",
            Self::Cloudflare => "cloudflare",
        }
    }

    /// The other way round. `None` for anything else, which is what a settings
    /// file from a newer build — or a hand-edited one — looks like from here.
    pub fn named(name: &str) -> Option<Self> {
        match name {
            "lan" => Some(Self::Lan),
            "tailscale" => Some(Self::Tailscale),
            "cloudflare" => Some(Self::Cloudflare),
            _ => None,
        }
    }

    /// Whether raising this channel puts the machine on the public internet.
    ///
    /// The one property the rest of the app branches on without knowing which
    /// tunnel it is talking to: the idle timer, the tray's standing warning,
    /// the question at exit and the loopback-only rule in [`super::proxy`] all
    /// read this. See [`RemoteTunnel::public`], which is where a tunnel answers
    /// for itself; this is the same answer before one has been built.
    pub fn public(self) -> bool {
        matches!(self, Self::Cloudflare)
    }
}

/// Where a tunnel is in coming up.
///
/// The LAN and the tailnet are `Running` or `Failed` by the time
/// [`RemoteTunnel::start`] returns — they read an answer off this machine's own
/// network cards and there is nothing to wait for. `Starting` exists for
/// `cloudflared`, which is a process that has to reach Cloudflare's edge and
/// register a connection before there is anywhere to send a phone, and which
/// can fail several seconds after being asked to start.
///
/// So `start` does not block on any of that. It reports the failures it can
/// know immediately — no binary, no token, no network card — and everything
/// after that arrives here, where the card polls for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TunnelState {
    /// Never started, or stopped again.
    Stopped,
    /// Asked to start, and not yet anywhere.
    Starting,
    /// Up, at this address.
    Running { base_url: String },
    /// It came up and then did not, or never came up. The string is for the
    /// card, so it is a sentence in the user's language.
    Failed(String),
}

impl TunnelState {
    /// The address to send a phone to, when there is one.
    pub fn base_url(&self) -> Option<&str> {
        match self {
            Self::Running { base_url } => Some(base_url),
            _ => None,
        }
    }
}

/// How the phone's browser addressed this gateway, and the only thing `Secure`
/// on the device cookie is ever decided by.
///
/// Not detected, and not detectable. This listener terminates nothing and
/// speaks plain HTTP whatever stands in front of it, so by the time a request
/// arrives the TLS — if there was any — is over and left no trace in the bytes.
/// The tunnel is the only party that knows, which is why this hangs off the
/// trait rather than being read somewhere in the proxy.
///
/// `X-Forwarded-Proto` is specifically not it. That is a header, which is to
/// say it is whatever the client put in it. Believed, it would hand a
/// plain-HTTP phone a `Secure` cookie the browser will then never send back —
/// and the same trust, pointed the other way, is what would let a downgrade
/// pass for a secure session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    Http,
    /// What a Cloudflare tunnel answers, and the first thing in this app to do
    /// so: the phone's browser spoke TLS to Cloudflare's edge, whatever this
    /// listener then received on loopback. It is the whole reason the device
    /// cookie can carry `Secure` at all — and, because a `Secure` origin is
    /// also a secure context, the reason a Service Worker can be registered
    /// there and the home-screen icon stops being a white page when this
    /// computer is off. See [`super::proxy`].
    Https,
}

impl Scheme {
    /// Whether a cookie issued over this scheme may carry `Secure`.
    pub fn secure(self) -> bool {
        matches!(self, Self::Https)
    }
}

/// Why a tunnel could not be raised.
///
/// Only the failures that are known *before* anything is running: no network
/// card, no tailnet, no binary, no token. A tunnel that starts and then falls
/// over reports that through [`TunnelState::Failed`] instead, because by then
/// [`RemoteTunnel::start`] has long returned.
#[derive(Debug)]
pub enum TunnelError {
    /// Nothing on this machine is on a network a phone could reach.
    NoAddress,
    /// The tailnet was asked for and this machine is not on one.
    NoTailnet,
    /// `cloudflared` is not on this machine, so there is nothing to launch.
    /// Carries nothing: what to do about it is per-platform, and the card is
    /// where that is spelled out.
    NoCloudflared,
    /// A named tunnel was asked for before a token and a hostname were given.
    NoToken,
    /// The tunnel process would not start at all — a binary that is not one, a
    /// permission denied. Carries the operating system's own words.
    WouldNotStart(String),
}

impl std::fmt::Display for TunnelError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoAddress => formatter.write_str(t!(
                "这台电脑没有连上任何局域网，手机没有地址可以连。",
                "This computer is not on a network, so there is no address for a phone to reach."
            )),
            Self::NoTailnet => formatter.write_str(t!(
                "没有检测到正在运行的 Tailscale。先让这台电脑和手机登录同一个 tailnet，再回到这里；\
                 或者切回局域网。",
                "No running Tailscale was found. Sign this computer and the phone into the same \
                 tailnet and come back, or switch to the local network."
            )),
            Self::NoCloudflared => formatter.write_str(t!(
                "这台电脑上没有找到 cloudflared。按下面的说明装好，再回到这里。",
                "cloudflared was not found on this computer. Install it as described below \
                 and come back."
            )),
            Self::NoToken => formatter.write_str(t!(
                "还没有填 Cloudflare 隧道的 token 和域名。",
                "The Cloudflare tunnel still needs a token and a hostname."
            )),
            Self::WouldNotStart(why) => write!(
                formatter,
                "{}{why}",
                t!("cloudflared 启动失败：", "cloudflared would not start: ")
            ),
        }
    }
}

/// A way for a phone to reach the gateway.
pub trait RemoteTunnel: Send + Sync {
    /// Raise the tunnel over a gateway already listening on
    /// `local_gateway_port`, and return without waiting for it to be up.
    ///
    /// The address a phone should be sent to comes out of [`state`] rather than
    /// out of here, and that is the whole shape of this method. Two of the
    /// three tunnels could perfectly well answer immediately — they read a
    /// network card and are `Running` before this returns — but `cloudflared`
    /// has to reach Cloudflare's edge and register a connection first, which
    /// takes seconds and can fail after the fact. A `start` that blocked on
    /// that would be a card with nothing on it for as long as it took, on the
    /// one channel where something *should* be on it saying so.
    ///
    /// So `Err` here means only what could be known without trying: no network
    /// card, no tailnet, no binary, no token. Everything else lands in
    /// [`TunnelState::Failed`].
    ///
    /// [`state`]: RemoteTunnel::state
    fn start(&mut self, local_gateway_port: u16) -> Result<(), TunnelError>;

    /// Take it down again.
    fn stop(&mut self) -> Result<(), TunnelError>;

    /// Where it has got to. See [`TunnelState`].
    fn state(&self) -> TunnelState;

    fn tunnel_type(&self) -> TunnelType;

    /// Whether this channel puts the machine on the public internet.
    ///
    /// Four things read it, and all four are the specification's public
    /// guard: the idle timer that takes the tunnel down on its own, the
    /// standing warning in the tray, the question asked at exit, and the rule
    /// in [`super::proxy`] that refuses any peer which is not loopback while a
    /// tunnel is up. Forgetting a LAN gateway costs the user whoever else is
    /// on their Wi-Fi; forgetting this one costs them the internet, and a dsh
    /// session is a shell.
    fn public(&self) -> bool {
        self.tunnel_type().public()
    }

    /// Which scheme the phone's browser used to get here. See [`Scheme`].
    fn scheme(&self) -> Scheme;

    /// The phone's own address, out of whatever forwarded-for header this
    /// tunnel vouches for — and `None` when this tunnel has no proxy in front
    /// of it, in which case the socket's peer *is* the phone.
    ///
    /// `peer` is passed in rather than being the caller's business precisely
    /// so that a tunnel can refuse to believe the header on a connection the
    /// proxy did not open. This listener is bound to `0.0.0.0`, so "a request
    /// arrived while the Cloudflare tunnel was the active channel" is not the
    /// same statement as "a request arrived *through* it": a machine on the
    /// same Wi-Fi can open the port directly and write whatever header it
    /// likes. See [`super::cloudflare::CloudflareTunnel::client_ip`].
    ///
    /// Two things downstream are about which *device* is talking, and both go
    /// silently wrong the moment a reverse proxy lands in front of the
    /// listener, because every request then arrives from `127.0.0.1`. The rate
    /// limiter on wrong short codes would count every device's attempts against
    /// the one address they all share, so one attacker's failures would lock
    /// out the user's own phone — a rate limit aimed at the victim, which is
    /// worse than none. And the desktop's approval dialog shows the human the
    /// address they are being asked to trust, which would become the loopback
    /// address of the machine asking.
    ///
    /// Trusting a header here is not trusting the client: it is trusting that
    /// the only thing which can open a connection to this listener over that
    /// tunnel is a proxy process this app launched itself. A tunnel with no
    /// such promise must answer `None` and must not look at the headers at
    /// all — which is the whole reason this is a method on the tunnel instead
    /// of a function reading `X-Forwarded-For` wherever somebody needs an
    /// address.
    fn client_ip(&self, headers: &HeaderMap, peer: IpAddr) -> Option<IpAddr>;

    /// The `Host` values a request arriving over this tunnel may legitimately
    /// carry, lowercased and with the port on them.
    ///
    /// Read on every request rather than settled at [`start`]: a laptop that
    /// moves between networks gets a different address without the gateway
    /// having restarted, and a fence still holding the old one would turn away
    /// the phone that is now on the same Wi-Fi as it.
    fn authorities(&self) -> Vec<String>;
}

/// The local network. `0.0.0.0` is already bound by the time this is started —
/// the gateway owns the socket — so all this has to do is work out which of the
/// machine's addresses to print on the QR code, and which ones to let through
/// the fence.
#[derive(Default)]
pub struct LanTunnel {
    port: Option<u16>,
    base: Option<String>,
}

impl RemoteTunnel for LanTunnel {
    fn start(&mut self, local_gateway_port: u16) -> Result<(), TunnelError> {
        let address = best_address().ok_or(TunnelError::NoAddress)?;
        self.port = Some(local_gateway_port);
        self.base = Some(format!("http://{address}:{local_gateway_port}"));
        Ok(())
    }

    fn stop(&mut self) -> Result<(), TunnelError> {
        self.port = None;
        self.base = None;
        Ok(())
    }

    /// Never `Starting`: the address was read off a network card before
    /// [`RemoteTunnel::start`] returned, and there is nothing in flight.
    fn state(&self) -> TunnelState {
        match &self.base {
            Some(base_url) => TunnelState::Running {
                base_url: base_url.clone(),
            },
            None => TunnelState::Stopped,
        }
    }

    fn tunnel_type(&self) -> TunnelType {
        TunnelType::Lan
    }

    fn scheme(&self) -> Scheme {
        Scheme::Http
    }

    /// Nothing, and the headers are not read.
    ///
    /// There is no proxy here: the phone opened this socket itself. A
    /// forwarded-for header on a request that arrived over the LAN was written
    /// by the phone, and honouring it would let a device choose which address
    /// its wrong guesses are counted against — which is to say, opt out of the
    /// rate limit, or point it at somebody else's phone.
    fn client_ip(&self, _headers: &HeaderMap, _peer: IpAddr) -> Option<IpAddr> {
        None
    }

    fn authorities(&self) -> Vec<String> {
        let Some(port) = self.port else {
            return Vec::new();
        };
        addresses()
            .into_iter()
            .map(|address| format!("{address}:{port}"))
            .collect()
    }
}

/// The tailnet: the same listener, reached at this machine's `100.x` address by
/// a device that has joined the same one.
///
/// It is the cheapest second tunnel there is, which is why it is the first.
/// Nothing is launched, nothing is downloaded, no account holds a token and
/// nothing fails several seconds after being asked: Tailscale is either running
/// on this machine or it is not, and the whole of this is working out which,
/// from the machine's own network cards. No call to `tailscale`, no LocalAPI,
/// no MagicDNS.
///
/// What it does not buy is TLS. `tailscale serve` would terminate it and answer
/// on a MagicDNS name, and that is the one thing standing between this app and
/// a PWA that survives the desktop being off — but it wants a CLI that is not
/// on the PATH on Windows, a LocalAPI whose token lives in the registry, and a
/// tailnet admin who has turned HTTPS on. What this buys is reachability: the
/// phone on mobile data, off the Wi-Fi, still gets in. See the roadmap.
#[derive(Default)]
pub struct TailscaleTunnel {
    port: Option<u16>,
    base: Option<String>,
}

impl RemoteTunnel for TailscaleTunnel {
    fn start(&mut self, local_gateway_port: u16) -> Result<(), TunnelError> {
        let address = tailscale_address().ok_or(TunnelError::NoTailnet)?;
        self.port = Some(local_gateway_port);
        self.base = Some(format!("http://{address}:{local_gateway_port}"));
        Ok(())
    }

    fn stop(&mut self) -> Result<(), TunnelError> {
        self.port = None;
        self.base = None;
        Ok(())
    }

    fn state(&self) -> TunnelState {
        match &self.base {
            Some(base_url) => TunnelState::Running {
                base_url: base_url.clone(),
            },
            None => TunnelState::Stopped,
        }
    }

    fn tunnel_type(&self) -> TunnelType {
        TunnelType::Tailscale
    }

    /// Plain HTTP, like the LAN. WireGuard has already encrypted every byte of
    /// it on the wire, but that is not what `Secure` is about: the browser
    /// knows only that it typed `http://`, and a `Secure` cookie on such an
    /// origin is one it will never send back.
    fn scheme(&self) -> Scheme {
        Scheme::Http
    }

    /// Nothing, for the same reason as the LAN: the phone dials this socket
    /// directly over WireGuard, so the peer address already *is* the phone —
    /// its `100.x` one.
    fn client_ip(&self, _headers: &HeaderMap, _peer: IpAddr) -> Option<IpAddr> {
        None
    }

    /// Re-read rather than remembered from [`start`], as the LAN's is: a
    /// machine that left the tailnet has no tailnet address, and the fence
    /// closing behind it is the correct answer.
    ///
    /// [`start`]: RemoteTunnel::start
    fn authorities(&self) -> Vec<String> {
        let Some(port) = self.port else {
            return Vec::new();
        };
        tailscale_address()
            .map(|address| vec![format!("{address}:{port}")])
            .unwrap_or_default()
    }
}

/// This machine's address on the tailnet, if it is on one.
///
/// Two conditions, and neither is sufficient alone.
///
/// The address has to be inside `100.64.0.0/10`, which is what Tailscale
/// assigns out of. But that range is not Tailscale's — it is RFC 6598
/// carrier-grade NAT, which is exactly what a Chinese mobile network hands a
/// tethered phone and what some ISPs hand a home router. A `100.x` address on
/// the Wi-Fi card is the ISP's, and publishing it as a tailnet address would
/// put a QR code on screen that nothing on earth can reach.
///
/// So the card holding it has to be named like Tailscale's as well:
/// `Tailscale` in the Windows friendly name, `tailscale0` on Linux, a `utun`
/// on macOS. That is not sufficient either — `utun` is every VPN and every
/// WireGuard client on macOS, and a Windows friendly name is whatever the user
/// renamed the adapter to — which is why both have to hold.
fn tailscale_address() -> Option<Ipv4Addr> {
    if_addrs::get_if_addrs()
        .ok()?
        .into_iter()
        .find_map(|interface| match interface.addr.ip() {
            IpAddr::V4(address) if is_tailnet(address) && named_like_tailscale(&interface.name) => {
                Some(address)
            }
            _ => None,
        })
}

/// `100.64.0.0/10`: the second octet from 64 to 127.
fn is_tailnet(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    octets[0] == 100 && (64..=127).contains(&octets[1])
}

fn named_like_tailscale(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("tailscale") || lower.starts_with("utun")
}

/// Every IPv4 address on this machine a phone on the same network could
/// plausibly be talking to.
///
/// Loopback is not one of them: a request whose `Host` is `127.0.0.1` did not
/// come from the phone, and letting it through the fence would hand the whole
/// of dsh to any page the *desktop's* browser happens to be showing.
pub fn addresses() -> Vec<Ipv4Addr> {
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };

    interfaces
        .into_iter()
        .filter_map(|interface| match interface.addr.ip() {
            IpAddr::V4(address) if reachable(address) => Some((interface.name, address)),
            _ => None,
        })
        .map(|(_, address)| address)
        .collect()
}

/// The one to put on the QR code.
///
/// Three things decide it, in this order. An address that is not on a network
/// at all is out ([`reachable`]). Of the rest, a real card beats a virtual one
/// ([`rank`] over [`VIRTUAL`]) — Docker, WSL and VMware all sit on private
/// addresses that look exactly like a home network from here. The routing
/// table breaks the tie *within* each of those two groups, and only there.
///
/// That ordering is the wrong way round from what it looks like it should be,
/// and the reason is that the question here is not "which card does this
/// machine talk to the world through" but "which address can the phone open a
/// connection to". A desktop running Clash, mihomo or sing-box in TUN mode
/// answers the first question with a virtual card that holds the default route
/// and answers the second one with nothing at all: the phone is not on that
/// machine's proxy core. Asking the routing table there returns an address the
/// QR code must never carry, which is exactly the bug this ordering fixes.
///
/// What it costs is the tie it used to break. On a machine whose route is
/// hijacked, every physical card ranks the same, and [`Iterator::max_by_key`]
/// yields the last of them — so a laptop with Wi-Fi and a dock both up
/// publishes whichever one `if_addrs` happens to enumerate second. Both are
/// normally on the same network and both normally work; when that stops being
/// true this is the line to come back to.
pub fn best_address() -> Option<Ipv4Addr> {
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return None;
    };
    let routed = routed_address();

    interfaces
        .into_iter()
        .filter_map(|interface| match interface.addr.ip() {
            IpAddr::V4(address) if reachable(address) => {
                Some((rank(&interface.name, Some(address) == routed), address))
            }
            _ => None,
        })
        .max_by_key(|(rank, _)| *rank)
        .map(|(_, address)| address)
}

/// Whether an address is one another machine on the network could open a
/// connection to.
///
/// Link-local goes with loopback: `169.254.x.x` is what Windows puts on a card
/// whose DHCP lease never arrived, and it is on screen exactly when the network
/// is not working — which is the moment a QR code carrying it would be at its
/// most confusing.
fn reachable(address: Ipv4Addr) -> bool {
    !address.is_loopback()
        && !address.is_link_local()
        && !address.is_unspecified()
        && !address.is_broadcast()
        && !address.is_multicast()
        && !is_benchmarking_or_fake_ip(address)
}

/// 198.18.0.0/15 is the RFC 2544 benchmark testing range, used by Clash, Mihomo,
/// Sing-box, etc. for TUN / Fake-IP mode. It is purely local to the proxy core
/// on this machine and never routable from an external LAN phone.
fn is_benchmarking_or_fake_ip(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    octets[0] == 198 && (octets[1] & 0xfe) == 18
}

fn is_virtual(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    VIRTUAL.iter().any(|marker| lower.contains(marker))
}

/// Higher is better. Pure, so the table below is something a test can hold.
///
/// Physical network cards (Wi-Fi, Ethernet) always beat virtual adapters
/// (Docker, WSL, Clash TUN, Tailscale) so that proxy or VPN software hijacking
/// the default route does not publish an address a LAN phone cannot reach.
fn rank(name: &str, routed: bool) -> u32 {
    let virtual_card = is_virtual(name);
    match (virtual_card, routed) {
        (false, true) => 4,
        (false, false) => 3,
        (true, true) => 2,
        (true, false) => 1,
    }
}

/// Adapter names that belong to something other than the network the phone is
/// on.
///
/// Matched as substrings against the friendly name, which on Windows is what
/// the Network Connections list shows — `vEthernet (WSL)`, `VMware Network
/// Adapter VMnet8`, `Hyper-V Virtual Ethernet Adapter`. Every one of them holds
/// a private address indistinguishable from a home network's, so the name is
/// the only thing there is to go on.
///
/// Tailscale is on the list for Phase 1 only, and for the QR code only: its
/// `100.x` address is perfectly reachable, but not by a phone that has not
/// joined the tailnet, and Phase 2 gives it a tunnel of its own that knows
/// better than this table does. It stays in [`addresses`] — the fence has no
/// reason to turn away a request that did arrive on it.
///
/// The second half of the list is the proxy stack a Chinese desktop is likely
/// to be running, in TUN mode, holding the default route: `clash` and `mihomo`
/// and `meta` are the friendly names the Clash family gives its adapter
/// (Clash Verge Rev, the current mainstream build, names it `Mihomo`), and
/// `sing-box`, `wintun` and `wireguard` cover the rest. `tun` and `tap` are
/// deliberately bare: they catch `utun0` on macOS and `tun0` on Linux, and on
/// Windows they catch whatever a wintun-based client decided to call itself
/// this release. `tap` subsumes the `tap-windows` entry that used to be here.
///
/// `meta` is the one short enough to worry about — a friendly name is whatever
/// the user renamed the card to, and four letters is not much. It stays
/// because the cost of a false positive is small and bounded: a card wrongly
/// called virtual is still in [`addresses`], so the fence still lets it
/// through, and it loses only to a *real* card, never to the TUN adapter this
/// list exists to demote.
const VIRTUAL: &[&str] = &[
    "vmware",
    "virtualbox",
    "vethernet",
    "hyper-v",
    "wsl",
    "docker",
    "tailscale",
    "zerotier",
    "loopback",
    "teredo",
    "isatap",
    "bluetooth",
    "vpn",
    "clash",
    "mihomo",
    "meta",
    "sing-box",
    "wintun",
    "wireguard",
    "tun",
    "tap",
];

/// The address the default route leaves through, or `None` on a machine with no
/// route at all.
///
/// A connected UDP socket, which sends nothing: `connect` on a datagram socket
/// only fixes the peer, and fixing it is what makes the kernel pick a source
/// address — the one it would use to reach the far side. That is the routing
/// table's own answer to "which card faces outward", and it costs two syscalls.
///
/// The address dialled is a public one so that the route chosen is the default
/// one rather than a subnet-local shortcut. Nothing is ever sent to it, and
/// nothing has to be listening.
fn routed_address() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    socket.connect(("8.8.8.8", 53)).ok()?;
    match socket.local_addr().ok()? {
        SocketAddr::V4(address) => Some(*address.ip()),
        SocketAddr::V6(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// For LAN pairing, a physical network card must always beat a virtual adapter,
    /// even if the virtual adapter (e.g. Clash TUN, VPN) has hijacked the default route.
    #[test]
    fn a_real_card_beats_a_routed_virtual_card() {
        assert!(rank("Wi-Fi", false) > rank("Tailscale", true));
        assert!(rank("Wi-Fi", false) > rank("Clash", true));
        assert!(rank("以太网", false) > rank("Meta", true));
        assert!(rank("Ethernet", false) > rank("wintun", true));
    }

    #[test]
    fn the_routed_card_wins_among_equals() {
        assert!(rank("Wi-Fi", true) > rank("Wi-Fi", false));
        assert!(rank("Tailscale", true) > rank("Tailscale", false));
        assert!(rank("Clash", true) > rank("Clash", false));
    }

    #[test]
    fn a_real_card_beats_a_virtual_one() {
        for name in ["Wi-Fi", "以太网", "Ethernet 2", "en0", "wlan0"] {
            assert!(
                rank(name, false) > rank("vEthernet (WSL)", false),
                "{name} is a real card"
            );
        }
    }

    /// The names are matched case-insensitively and as substrings, because what
    /// Windows shows is a sentence with the product name somewhere inside it.
    #[test]
    fn the_virtual_names_are_recognised_as_written() {
        for name in [
            "vEthernet (Default Switch)",
            "VMware Network Adapter VMnet1",
            "Hyper-V Virtual Ethernet Adapter",
            "Docker Desktop",
            "Tailscale",
            "ZeroTier One [abc]",
            "Clash Core Adapter",
            "Mihomo",
            "Meta TUN",
            "sing-box tun",
            "Wintun Userspace Tunnel",
            "WireGuard Tunnel",
            "TAP-Windows Adapter V9",
        ] {
            assert_eq!(rank(name, false), 1, "{name} is not the card to publish");
        }
    }

    /// What a QR code must never carry: an address that answers only on this
    /// machine, and the one Windows invents when DHCP fails.
    #[test]
    fn unreachable_addresses_are_not_offered() {
        for address in [
            Ipv4Addr::new(127, 0, 0, 1),
            Ipv4Addr::new(169, 254, 3, 4),
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::BROADCAST,
            Ipv4Addr::new(198, 18, 0, 1),
            Ipv4Addr::new(198, 19, 255, 254),
        ] {
            assert!(!reachable(address), "{address} is not reachable");
        }
    }

    #[test]
    fn ordinary_lan_addresses_are() {
        for address in [
            Ipv4Addr::new(192, 168, 1, 100),
            Ipv4Addr::new(10, 0, 0, 5),
            Ipv4Addr::new(172, 20, 3, 1),
            Ipv4Addr::new(100, 64, 1, 2),
        ] {
            assert!(reachable(address), "{address} is reachable");
        }
    }

    /// A tunnel that was never started has no authorities, which is what keeps
    /// the fence closed rather than open in the window between binding the
    /// socket and publishing an address.
    #[test]
    fn a_stopped_tunnel_trusts_nothing() {
        let mut tunnel = LanTunnel::default();
        assert!(tunnel.authorities().is_empty());

        if tunnel.start(59000).is_ok() {
            assert!(
                tunnel
                    .authorities()
                    .iter()
                    .all(|one| one.ends_with(":59000")),
                "every authority carries the bound port"
            );
            tunnel.stop().unwrap();
            assert!(tunnel.authorities().is_empty(), "and none survive the stop");
        }
    }

    /// The same, for the tunnel that may well not be able to start on the
    /// machine running the test.
    #[test]
    fn a_stopped_tailscale_tunnel_trusts_nothing() {
        let mut tunnel = TailscaleTunnel::default();
        assert!(tunnel.authorities().is_empty());

        match tunnel.start(59000) {
            Ok(()) => {
                let base = tunnel.state().base_url().expect("running").to_string();
                assert!(base.starts_with("http://100."), "{base} is a tailnet URL");
                tunnel.stop().unwrap();
                assert!(tunnel.authorities().is_empty());
                assert_eq!(tunnel.state(), TunnelState::Stopped);
            }
            // No tailnet on this machine, which is the ordinary case in CI and
            // is not a failure of anything here.
            Err(error) => assert!(matches!(error, TunnelError::NoTailnet)),
        }
    }

    /// The range Tailscale assigns out of, and the one an ISP hands a home
    /// router out of. They are the same range — which is the whole reason the
    /// adapter name has to agree before an address is published.
    #[test]
    fn the_tailnet_range_is_the_cgnat_range() {
        for address in ["100.64.0.1", "100.101.102.103", "100.127.255.255"] {
            assert!(is_tailnet(address.parse().unwrap()), "{address}");
        }
        for address in ["100.63.255.255", "100.128.0.1", "10.0.0.5", "192.168.1.9"] {
            assert!(!is_tailnet(address.parse().unwrap()), "{address}");
        }
    }

    #[test]
    fn the_adapter_has_to_be_named_like_tailscale_too() {
        for name in ["Tailscale", "tailscale0", "utun3", "Tailscale Tunnel"] {
            assert!(named_like_tailscale(name), "{name}");
        }
        for name in ["Wi-Fi", "以太网", "eth0", "Clash", "wintun", "vEthernet (WSL)"] {
            assert!(!named_like_tailscale(name), "{name}");
        }
    }

    /// The guarantee the rate limiter and the approval dialog are built on: a
    /// tunnel with no proxy in front of it does not read a forwarded-for
    /// header, so a phone cannot choose the address it is judged by.
    ///
    /// Written against every spelling a later tunnel might legitimately want to
    /// honour, because the failure this is guarding against is somebody adding
    /// a helpful general-purpose header reader.
    #[test]
    fn a_direct_tunnel_ignores_every_forwarded_for_header() {
        let mut headers = HeaderMap::new();
        for name in ["x-forwarded-for", "cf-connecting-ip", "x-real-ip"] {
            headers.insert(name, "203.0.113.7".parse().unwrap());
        }
        let peer: IpAddr = "192.168.1.9".parse().unwrap();

        assert_eq!(LanTunnel::default().client_ip(&headers, peer), None);
        assert_eq!(TailscaleTunnel::default().client_ip(&headers, peer), None);
    }

    /// And the one the cookie is built on. Neither of the two direct tunnels
    /// terminates TLS, and no header is allowed to say otherwise.
    #[test]
    fn neither_direct_tunnel_claims_a_secure_scheme() {
        assert!(!LanTunnel::default().scheme().secure());
        assert!(!TailscaleTunnel::default().scheme().secure());
        assert!(Scheme::Https.secure());
    }

    /// Which channel the public guard is about. Read before a tunnel exists —
    /// by the card, and by the question asked at exit — so the two answers have
    /// to be the same one.
    #[test]
    fn only_the_cloudflare_channel_is_public() {
        assert!(!TunnelType::Lan.public());
        assert!(!TunnelType::Tailscale.public());
        assert!(TunnelType::Cloudflare.public());

        assert!(!LanTunnel::default().public());
        assert!(!TailscaleTunnel::default().public());
    }

    /// The names are what `desktop.json` holds and what the card's verbs carry,
    /// so they round-trip or a stored channel silently becomes another one.
    #[test]
    fn every_channel_name_round_trips() {
        for kind in [
            TunnelType::Lan,
            TunnelType::Tailscale,
            TunnelType::Cloudflare,
        ] {
            assert_eq!(TunnelType::named(kind.name()), Some(kind), "{kind:?}");
        }
        assert_eq!(TunnelType::named("ngrok"), None);
        // The temporary `*.trycloudflare.com` channel this app used to offer.
        // A `desktop.json` that still names it falls back to the LAN rather
        // than to a channel that is no longer here; see `settings::channel`.
        assert_eq!(TunnelType::named("cloudflare-quick"), None);
        assert_eq!(TunnelType::named(""), None);
    }
}
