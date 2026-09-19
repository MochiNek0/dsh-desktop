//! The browser trust fence, rebuilt on this side of the proxy.
//!
//! dsh ships one of these — `api-request-trust` in `dsh-client-connection` —
//! and it is the only thing standing between a dsh session and any web page the
//! user happens to have open: it refuses a request whose `Host` is neither
//! loopback nor a declared trusted host, refuses one whose `Origin` disagrees
//! with its `Host`, and refuses anything the browser labelled
//! `sec-fetch-site: cross-site`.
//!
//! This gateway forwards to dsh with the `Host` and `Origin` rewritten to
//! `127.0.0.1:<dsh port>` — see [`crate::remote::upstream`] for why — and the
//! moment it does that, dsh's fence passes everything. It is not weakened for
//! traffic that does not come through here; it is simply no longer being asked
//! anything by traffic that does. So it is rebuilt here, in front of the
//! rewrite, against the headers the phone's browser actually sent.
//!
//! This is not a formality. The two attacks the original fence is for both work
//! against a plain reverse proxy that skips it:
//!
//! - **DNS rebinding.** A page on `evil.example` whose name is re-resolved to
//!   `192.168.1.100` can make same-origin requests to whatever is listening
//!   there. The browser sends `Host: evil.example`, which is why checking the
//!   `Host` against the addresses this machine actually has is the whole
//!   defence — and why [`RemoteTunnel::authorities`] rather than a hardcoded
//!   list.
//! - **Cross-site requests.** Any page in the phone's browser can `fetch` at
//!   the gateway, and the device cookie would ride along on it if the cookie
//!   were not `SameSite=Strict`. It is, and this is the second belt: a request
//!   the browser itself labelled cross-site is refused before its cookies are
//!   even looked at.
//!
//! [`RemoteTunnel::authorities`]: crate::remote::tunnel::RemoteTunnel::authorities

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// Why a request was turned away, in the words that go to the terminal.
///
/// A `&'static str` rather than a formatted message: this is written on the
/// refusal path, which is the path a machine that is being scanned spends all
/// its time on, and the one thing it must not do is allocate per probe.
pub type Refusal = &'static str;

/// Whether a request may be considered at all, before anything about who sent
/// it is looked at.
///
/// Split from the cookie check because the two answer differently: this one
/// ends in 403 and says nothing about sessions, while a request that passes it
/// and carries no cookie ends in 401. That is dsh's own division, and keeping it
/// means a phone whose session expired sees the same thing it would have seen
/// talking to dsh directly.
pub fn provenance(
    host: Option<&str>,
    origin: Option<&str>,
    fetch_site: Option<&str>,
    authorities: &[String],
) -> Result<(), Refusal> {
    // First, because it is the cheapest and because it is the only one of the
    // three the browser fills in on its own: a page cannot set it, and a client
    // that is not a browser does not send it at all.
    if fetch_site.is_some_and(|site| site.eq_ignore_ascii_case("cross-site")) {
        return Err("sec-fetch-site was cross-site");
    }

    let Some(host) = host else {
        return Err("no Host header");
    };
    let host = host.trim().to_ascii_lowercase();

    // A tunnel that has not started yet publishes no authorities, so this is
    // also what keeps the door shut between binding the socket and having an
    // address to publish.
    if !authorities.contains(&host) {
        return Err("the Host is not an address of this machine");
    }

    // Absent is allowed — an address typed into the bar, or a QR code followed
    // from the camera, is a navigation with no Origin at all. Present and
    // disagreeing is not, and `null` is a disagreement rather than an absence:
    // it is what a sandboxed frame sends, and nothing this gateway serves is
    // meant to be read out of one.
    if let Some(origin) = origin {
        if authority_of(origin).is_none_or(|origin| origin != host) {
            return Err("the Origin does not match the Host");
        }
    }

    Ok(())
}

/// The `host:port` out of an `Origin`, lowercased.
///
/// `None` for `null` and for anything that is not `scheme://authority`, both of
/// which fail the comparison above rather than being waved through.
fn authority_of(origin: &str) -> Option<String> {
    let (_, authority) = origin.trim().split_once("://")?;
    // An Origin has no path, but a client that sent one would otherwise have
    // the path compared against a port.
    let authority = authority.split('/').next()?;
    (!authority.is_empty()).then(|| authority.to_ascii_lowercase())
}

/// What to call a device on the desktop card.
///
/// Read out of the user agent, which is the only thing a phone says about
/// itself. It is also a string the client chooses, so nothing is decided by it:
/// it is drawn on a card a human reads, next to the address the connection
/// actually came from, and the human is the one deciding.
///
/// The list is ordered rather than a lookup — "iPhone" and "iPad" both carry
/// `Mobile`, and an Android tablet carries `Android` without `Mobile` — and it
/// ends at a word rather than at the raw header, because a modern user agent is
/// a hundred characters of Mozilla-compatible fiction and none of it belongs on
/// a card.
pub fn label(user_agent: Option<&str>) -> String {
    let agent = user_agent.unwrap_or_default();
    for (marker, name) in [
        ("iPhone", "iPhone"),
        ("iPad", "iPad"),
        ("Android", "Android"),
        ("Macintosh", "Mac"),
        ("Windows", "Windows"),
        ("Linux", "Linux"),
    ] {
        if agent.contains(marker) {
            return name.to_string();
        }
    }
    t!("未知设备", "Unknown device").to_string()
}

/// How many wrong short codes one address may offer inside [`WINDOW`].
///
/// Ten, which is more than anyone mistypes six characters and far fewer than
/// anyone needs to search a billion of them. The limit is not what makes the
/// code safe — see [`crate::remote::session`] for the four things that do — it
/// is what keeps the search from being free.
const GUESSES: u32 = 10;

/// The span they are counted over.
const WINDOW: Duration = Duration::from_secs(60);

/// And what a run of them costs.
///
/// Five minutes, which is [`PAIR_TTL`] — so an address that spent its guesses
/// on one code waits out that code entirely, and comes back to a card the user
/// has had to refresh anyway.
///
/// [`PAIR_TTL`]: crate::remote::session
const COOLDOWN: Duration = Duration::from_secs(5 * 60);

/// Wrong short codes, counted per address.
///
/// ## The address has to be the phone's
///
/// Both of today's tunnels hand the gateway a real socket, so the peer address
/// on it is the device that opened it. Behind a reverse proxy — which is what
/// `cloudflared` and `tailscale serve` both are — it stops being, and every
/// request arrives from `127.0.0.1`.
///
/// Keying on that would not merely weaken this; it would invert it. One
/// attacker's failures would count against the loopback address that *every*
/// device shares, and the tenth wrong guess would lock out the user's own
/// phone. A rate limit that the attacker aims at the victim is worse than no
/// rate limit.
///
/// So this is no longer fed from the socket. What the caller passes is
/// [`RemoteTunnel::client_ip`] falling back to the peer — the active tunnel's
/// own answer to who is asking — and a tunnel with a proxy in front of it is
/// the one party that can say which header to believe.
///
/// [`RemoteTunnel::client_ip`]: crate::remote::tunnel::RemoteTunnel::client_ip
#[derive(Default)]
pub struct Guesses {
    runs: std::sync::Mutex<HashMap<IpAddr, Run>>,
}

/// One address's recent wrong answers.
struct Run {
    wrong: u32,
    /// When the count started. The window slides by being restarted, not by
    /// keeping timestamps: what is being measured is "a burst", and a burst is
    /// adequately described by when it began and how big it got.
    since: Instant,
    /// Set when the run went over, and cleared by nothing — the entry itself is
    /// dropped once this has passed.
    until: Option<Instant>,
}

impl Guesses {
    /// Whether this address has to wait before it may guess again.
    pub fn blocked(&self, who: IpAddr) -> bool {
        self.runs
            .lock()
            .unwrap()
            .get(&who)
            .and_then(|run| run.until)
            .is_some_and(|until| until > Instant::now())
    }

    /// Record a wrong code, and start the wait if that was one too many.
    pub fn wrong(&self, who: IpAddr) {
        let now = Instant::now();
        let mut runs = self.runs.lock().unwrap();

        // Before the insert, so that an address that guessed twice last week is
        // not still in here. Nothing else prunes: the map is written only on
        // this path, which is the path that would otherwise grow it.
        runs.retain(|_, run| lively(run, now));

        let run = runs.entry(who).or_insert(Run {
            wrong: 0,
            since: now,
            until: None,
        });

        if now.duration_since(run.since) > WINDOW {
            run.wrong = 0;
            run.since = now;
        }

        run.wrong += 1;
        if run.wrong >= GUESSES {
            run.until = Some(now + COOLDOWN);
        }
    }

    /// A right one. The run is forgotten, so a user who fumbled the code twice
    /// before getting it does not carry those two into their next pairing.
    pub fn right(&self, who: IpAddr) {
        self.runs.lock().unwrap().remove(&who);
    }
}

/// Whether an entry is still worth keeping: inside its window, or still owed a
/// wait.
fn lively(run: &Run, now: Instant) -> bool {
    run.until.is_some_and(|until| until > now) || now.duration_since(run.since) <= WINDOW
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authorities() -> Vec<String> {
        vec![
            "192.168.1.100:59000".to_string(),
            "10.0.0.5:59000".to_string(),
        ]
    }

    #[test]
    fn the_phone_on_the_same_wifi_gets_through() {
        assert!(provenance(
            Some("192.168.1.100:59000"),
            None,
            Some("none"),
            &authorities()
        )
        .is_ok());
    }

    /// Every address this machine has, not just the one on the QR code: a phone
    /// that was given one of them and a laptop that reaches another are the same
    /// gateway.
    #[test]
    fn any_address_of_this_machine_is_this_machine() {
        assert!(provenance(Some("10.0.0.5:59000"), None, None, &authorities()).is_ok());
    }

    /// The rebinding case, which is the reason this module exists: the name
    /// resolves here, the request arrives here, and the `Host` says otherwise.
    #[test]
    fn a_name_that_merely_resolves_here_does_not() {
        for host in [
            "evil.example:59000",
            "evil.example",
            "localhost:59000",
            "127.0.0.1:59000",
        ] {
            assert!(
                provenance(Some(host), None, None, &authorities()).is_err(),
                "{host} must be refused"
            );
        }
    }

    /// The port is part of the authority. Another service on this machine is
    /// not this one.
    #[test]
    fn the_port_is_part_of_it() {
        assert!(provenance(Some("192.168.1.100:3080"), None, None, &authorities()).is_err());
        assert!(provenance(Some("192.168.1.100"), None, None, &authorities()).is_err());
    }

    #[test]
    fn a_cross_site_request_is_refused_whatever_else_it_says() {
        assert!(provenance(
            Some("192.168.1.100:59000"),
            Some("http://192.168.1.100:59000"),
            Some("cross-site"),
            &authorities()
        )
        .is_err());
    }

    /// The labels the browser does send on ordinary traffic. `same-origin` is
    /// every XHR the dsh page makes; `none` is the address bar and the QR code.
    #[test]
    fn the_ordinary_labels_are_let_through() {
        for site in ["same-origin", "same-site", "none", "None", ""] {
            assert!(
                provenance(
                    Some("192.168.1.100:59000"),
                    None,
                    Some(site),
                    &authorities()
                )
                .is_ok(),
                "sec-fetch-site: {site:?} is not a refusal"
            );
        }
    }

    #[test]
    fn an_origin_must_agree_with_the_host() {
        let host = Some("192.168.1.100:59000");
        assert!(provenance(
            host,
            Some("http://192.168.1.100:59000"),
            None,
            &authorities()
        )
        .is_ok());

        for origin in [
            "http://evil.example",
            "http://192.168.1.100:3080",
            "http://10.0.0.5:59000",
            "null",
            "",
        ] {
            assert!(
                provenance(host, Some(origin), None, &authorities()).is_err(),
                "Origin {origin} does not agree with the Host"
            );
        }
    }

    /// The scheme is not compared, and that is deliberate rather than an
    /// oversight: Phase 2 terminates TLS at a tunnel and forwards plain HTTP,
    /// so the `Origin` the phone sends says `https` while the request this
    /// process reads does not. The authority is the part that identifies the
    /// gateway.
    #[test]
    fn the_scheme_in_the_origin_is_not_what_is_compared() {
        assert_eq!(
            authority_of("https://192.168.1.100:59000"),
            authority_of("http://192.168.1.100:59000")
        );
    }

    /// A tunnel that has not published an address yet trusts nothing, so the
    /// gateway is shut rather than open in the moment before it is ready.
    #[test]
    fn nothing_is_trusted_before_a_tunnel_says_so() {
        assert!(provenance(Some("192.168.1.100:59000"), None, None, &[]).is_err());
    }

    #[test]
    fn a_request_with_no_host_is_not_a_request_for_us() {
        assert!(provenance(None, None, None, &authorities()).is_err());
    }

    #[test]
    fn devices_are_named_after_what_they_say_they_are() {
        let iphone = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) \
                      AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1";
        assert_eq!(label(Some(iphone)), "iPhone");
        assert_eq!(
            label(Some("Mozilla/5.0 (iPad; CPU OS 17_5 like Mac OS X)")),
            "iPad"
        );
        assert_eq!(
            label(Some("Mozilla/5.0 (Linux; Android 14; Pixel 8)")),
            "Android"
        );
        assert_eq!(
            label(Some("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)")),
            "Mac"
        );
        assert_eq!(
            label(Some("Mozilla/5.0 (Windows NT 10.0; Win64; x64)")),
            "Windows"
        );
    }

    /// An Android phone's user agent contains `Linux` as well, and says
    /// `Android` first only in the order this list is written in.
    #[test]
    fn android_is_not_reported_as_linux() {
        assert_eq!(label(Some("Mozilla/5.0 (Linux; Android 14)")), "Android");
    }

    #[test]
    fn a_client_that_says_nothing_still_gets_a_name() {
        assert!(!label(None).is_empty());
        assert!(!label(Some("")).is_empty());
    }

    fn phone() -> IpAddr {
        "192.168.1.9".parse().unwrap()
    }

    fn other() -> IpAddr {
        "192.168.1.10".parse().unwrap()
    }

    /// Nine wrong codes is somebody typing badly. The tenth is the limit.
    #[test]
    fn a_run_of_wrong_codes_ends_in_a_wait() {
        let guesses = Guesses::default();
        assert!(!guesses.blocked(phone()));

        for _ in 0..GUESSES - 1 {
            guesses.wrong(phone());
            assert!(!guesses.blocked(phone()), "still within the allowance");
        }

        guesses.wrong(phone());
        assert!(guesses.blocked(phone()));
    }

    /// And it is the guesser who waits, not everyone. This is the whole reason
    /// the address has to be the phone's own — see the type's docs.
    #[test]
    fn the_wait_falls_only_on_the_address_that_earned_it() {
        let guesses = Guesses::default();
        for _ in 0..GUESSES {
            guesses.wrong(phone());
        }

        assert!(guesses.blocked(phone()));
        assert!(!guesses.blocked(other()), "a second phone is unaffected");
    }

    /// A user who fumbled the code twice and then got it does not carry those
    /// two into the next pairing.
    #[test]
    fn getting_it_right_forgets_the_run() {
        let guesses = Guesses::default();
        guesses.wrong(phone());
        guesses.wrong(phone());
        guesses.right(phone());

        for _ in 0..GUESSES - 1 {
            guesses.wrong(phone());
        }
        assert!(
            !guesses.blocked(phone()),
            "the count started again at the right answer"
        );
    }

    /// The count is a burst, not a lifetime total: wrong answers spread wider
    /// than the window do not add up.
    #[test]
    fn the_window_slides() {
        let guesses = Guesses::default();

        {
            let mut runs = guesses.runs.lock().unwrap();
            runs.insert(
                phone(),
                Run {
                    wrong: GUESSES - 1,
                    since: Instant::now() - WINDOW - Duration::from_secs(1),
                    until: None,
                },
            );
        }

        guesses.wrong(phone());
        assert!(
            !guesses.blocked(phone()),
            "the stale burst was dropped rather than added to"
        );
    }

    /// Entries that are neither inside a window nor owed a wait are dropped, so
    /// the map does not grow one row per address that ever mistyped.
    #[test]
    fn the_map_forgets_addresses_that_are_done() {
        let guesses = Guesses::default();

        {
            let mut runs = guesses.runs.lock().unwrap();
            runs.insert(
                other(),
                Run {
                    wrong: 1,
                    since: Instant::now() - WINDOW - Duration::from_secs(1),
                    until: None,
                },
            );
        }

        guesses.wrong(phone());
        assert!(!guesses.runs.lock().unwrap().contains_key(&other()));
    }
}
