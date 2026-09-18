//! How the phone reaches the gateway, behind one interface so that it can stop
//! being the local network later without anything above noticing.
//!
//! Phase 1 has exactly one of these — [`LanTunnel`], which is `0.0.0.0` and the
//! machine's own Wi-Fi address. The trait exists anyway, and not as decoration:
//! two things above it would otherwise have "the LAN" written into them, and
//! both are things Phase 2 changes.
//!
//! The first is the QR code, which is a URL with a scheme in it. A Cloudflare
//! tunnel hands back `https://…`, and a pairing URL assembled from an address
//! and a hardcoded `http://` would be wrong the day that lands.
//!
//! The second is the trust fence. [`crate::remote::trust`] has to know which
//! `Host` values are this gateway's own, and that set is not a fact about the
//! machine — it is a fact about the channel the request came in on. On the LAN
//! it is every local IPv4 plus the bound port; through a tunnel it is one
//! hostname and no port at all. So the fence asks the tunnel rather than
//! enumerating network cards itself, which is why [`RemoteTunnel::authorities`]
//! is on the trait beside the three methods the specification names.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

/// Which channel the phone came in over. One arm today; the rest are the
/// Phase 2/3 entries of the evolution plan, and are not pretended to exist
/// until something implements them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TunnelType {
    /// The local network: bind `0.0.0.0`, hand out this machine's LAN address.
    Lan,
}

/// Why a tunnel could not be raised.
///
/// One variant, because on the LAN there is one way to fail: a laptop with the
/// Wi-Fi off has no address to publish and will not have one until it is back
/// on. An enum rather than a unit type all the same — a tunnel that shells out
/// to `cloudflared` has failures of its own to name, and this is the type they
/// will be named in.
#[derive(Debug)]
pub enum TunnelError {
    /// Nothing on this machine is on a network a phone could reach.
    NoAddress,
}

impl std::fmt::Display for TunnelError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoAddress => formatter.write_str(t!(
                "这台电脑没有连上任何局域网，手机没有地址可以连。",
                "This computer is not on a network, so there is no address for a phone to reach."
            )),
        }
    }
}

/// A way for a phone to reach the gateway.
pub trait RemoteTunnel: Send + Sync {
    /// Raise the tunnel over a gateway already listening on `local_gateway_port`,
    /// and answer with the base address a phone should be sent to —
    /// `http://192.168.1.100:59000` today, `https://…` once there is a tunnel
    /// that terminates TLS.
    fn start(&mut self, local_gateway_port: u16) -> Result<String, TunnelError>;

    /// Take it down again.
    fn stop(&mut self) -> Result<(), TunnelError>;

    fn tunnel_type(&self) -> TunnelType;

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
}

impl RemoteTunnel for LanTunnel {
    fn start(&mut self, local_gateway_port: u16) -> Result<String, TunnelError> {
        let address = best_address().ok_or(TunnelError::NoAddress)?;
        self.port = Some(local_gateway_port);
        Ok(format!("http://{address}:{local_gateway_port}"))
    }

    fn stop(&mut self) -> Result<(), TunnelError> {
        self.port = None;
        Ok(())
    }

    fn tunnel_type(&self) -> TunnelType {
        TunnelType::Lan
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
/// at all is out ([`reachable`]). Of the rest, the one the default route goes
/// through wins outright — that is the card the machine actually talks to the
/// world through, and no amount of reading adapter names is as good as asking
/// the routing table. What is left is ranked by name, which is what breaks the
/// tie on a machine with no route: a real card beats the virtual one Docker,
/// WSL or VMware installed, all of which sit on private addresses that look
/// exactly like a home network from here.
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
    "tap-windows",
    "teredo",
    "isatap",
    "bluetooth",
    "vpn",
    "clash",
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
            "Meta TUN",
            "sing-box tun",
            "Wintun Userspace Tunnel",
            "WireGuard Tunnel",
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
}
