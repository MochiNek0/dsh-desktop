//! The third channel: `cloudflared`, and with it the first address on this
//! gateway that is not on somebody's private network.
//!
//! Everything else in [`super::tunnel`] answers out of this machine's own
//! network cards. This one launches a process, waits for it to reach
//! Cloudflare's edge, and hands the phone a hostname that anybody on the
//! internet can resolve. That difference is why this is a file of its own and
//! not two more arms of a `match`: a subprocess has a lifetime, a public
//! address has a guard, and neither belongs in a module about reading IP
//! addresses off adapters.
//!
//! ## Named, and quick as a way of trying it
//!
//! A **named tunnel** is the one to stay on: the user's own hostname, their own
//! Cloudflare account, and a token this app stores. The hostname does not move,
//! so a paired phone stays paired across restarts and the home-screen icon goes
//! on pointing somewhere. That is the whole of what M1 bought, and it is the
//! reason the other mode is not the default.
//!
//! A **quick tunnel** (`--url`, no account) is a `*.trycloudflare.com` hostname
//! that lasts until the process stops. Every restart is a new origin: every
//! paired device is thrown off by the browser, and the home-screen icon points
//! at a name that no longer resolves. Cloudflare documents it as unsuitable for
//! production and gives it no SLA. So it is here to answer "does this work at
//! all on my machine" without an account — and while it is the active channel
//! the gateway refuses to serve a manifest, because an installed icon on an
//! address with hours to live is worse than no icon. See
//! [`super::proxy`].
//!
//! ## What the token is, and where it lives
//!
//! `app_dir/cloudflare.token`, beside the signing key and for the same reason —
//! not in `desktop.json`, which is hand-edited, rewritten key by key, and the
//! sort of file that ends up pasted into a bug report.
//!
//! The threat model is *not* the signing key's, though, and the difference
//! matters. The argument that excuses the signing key — any process running as
//! this user could already drive the local dsh, so the key is a slower road to
//! somewhere it can already go — does not hold here. This token operates a
//! tunnel in the user's Cloudflare account. It opens a door on a machine this
//! one does not own, and reading it is not equivalent to anything a local
//! process could already do. If this app ever grows a `keyring` dependency, it
//! is for this and not for the signing key.
//!
//! ## The ingress points at us, and we do not get to choose it
//!
//! A token-run tunnel is *remotely managed*: the ingress rules live in the
//! Cloudflare dashboard, and `cloudflared` is told them when it connects. So
//! this app cannot pass `--url` and have it mean anything, and it does not
//! pretend to — the card shows the exact `http://localhost:<port>` to put in
//! the dashboard, and the port it shows is the one M1 made stable.

use std::io::{BufRead, BufReader};
#[cfg(windows)]
use std::os::windows::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use http::HeaderMap;
use tauri::AppHandle;

use super::tunnel::{RemoteTunnel, Scheme, TunnelError, TunnelState, TunnelType};

/// The file the token is kept in, beside `desktop.json` rather than in it.
const TOKEN_FILE: &str = "cloudflare.token";

/// What a named tunnel needs before it can be asked to start.
#[derive(Clone)]
pub struct Account {
    /// The connector token from the Cloudflare dashboard. Never logged, never
    /// drawn on the card, and redacted out of anything `cloudflared` says.
    pub token: String,
    /// The hostname the dashboard's ingress rule publishes, bare: no scheme,
    /// no path, lowercased.
    pub hostname: String,
}

/// A Cloudflare tunnel, in either of its two modes.
pub struct CloudflareTunnel {
    /// [`TunnelType::Cloudflare`] or [`TunnelType::CloudflareQuick`].
    kind: TunnelType,
    /// Where `cloudflared` was found, or `None` on a machine without one.
    /// Resolved when the tunnel was built rather than when it is started, so
    /// that the card can say "install this" before anybody presses anything.
    binary: Option<PathBuf>,
    /// `None` for the quick mode, and for a named tunnel nobody has configured.
    account: Option<Account>,
    child: Option<std::process::Child>,
    /// Windows' backstop against an orphan. See [`crate::server::Job`].
    #[cfg(windows)]
    job: Option<crate::server::Job>,
    /// Where this run has got to.
    ///
    /// Behind an `Arc` because the thread reading `cloudflared`'s stderr is
    /// what moves it, and replaced wholesale on every start rather than reset:
    /// the reader of a run that has been stopped goes on holding *its* `Arc`,
    /// and a shared one would let it write a `Failed` from the old process over
    /// the `Starting` of the new one.
    state: Arc<Mutex<TunnelState>>,
}

impl CloudflareTunnel {
    /// Build one, reading what it needs off the disk.
    ///
    /// Nothing is launched and nothing fails here: a machine with no
    /// `cloudflared` and no token still produces a tunnel, which answers
    /// [`TunnelError::NoCloudflared`] or [`TunnelError::NoToken`] the
    /// moment it is started. That is what lets the card draw the Cloudflare
    /// panel — with the install text, or the two empty fields — without the
    /// user first pressing a button that cannot work.
    pub fn new(app: Option<&AppHandle>, kind: TunnelType) -> Self {
        Self {
            kind,
            binary: binary(app),
            account: match kind {
                TunnelType::Cloudflare => app.and_then(account),
                _ => None,
            },
            child: None,
            #[cfg(windows)]
            job: None,
            state: Arc::new(Mutex::new(TunnelState::Stopped)),
        }
    }

    /// A tunnel that is already up, with no process behind it.
    ///
    /// For [`super::tests`], which drives the real listener end to end and
    /// needs a public channel to drive it on. Everything the gateway asks a
    /// tunnel — its type, its scheme, its authorities, whether it is public —
    /// is answered out of these two fields, so a fake is the whole of what
    /// those tests need and `cloudflared` is not.
    #[cfg(test)]
    pub fn pretending(kind: TunnelType, hostname: &str) -> Self {
        Self {
            kind,
            binary: None,
            account: None,
            child: None,
            #[cfg(windows)]
            job: None,
            state: Arc::new(Mutex::new(TunnelState::Running {
                base_url: format!("https://{hostname}"),
            })),
        }
    }

    /// Kill the child and wait for it, so that nothing is left holding the
    /// connection after this returns.
    fn take_down(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let _ = child.kill();
        // Waited for, not abandoned: on Windows the job object below is only
        // released once every process in it is gone, and on Unix an unreaped
        // child is a zombie for the life of the app.
        let _ = child.wait();

        #[cfg(windows)]
        {
            self.job = None;
        }
    }
}

impl RemoteTunnel for CloudflareTunnel {
    fn start(&mut self, local_gateway_port: u16) -> Result<(), TunnelError> {
        self.take_down();

        let binary = self.binary.clone().ok_or(TunnelError::NoCloudflared)?;
        if self.kind == TunnelType::Cloudflare && self.account.is_none() {
            return Err(TunnelError::NoToken);
        }

        let mut command = Command::new(binary);
        // `--no-autoupdate`: cloudflared replaces its own binary and restarts
        // itself by default, which for a child this app is holding a handle to
        // is a tunnel that disappears mid-session. Updating it is the job of
        // whatever installed it.
        command.arg("--no-autoupdate");
        match &self.account {
            // Remotely managed: the ingress is in the dashboard, so there is no
            // URL to pass. See the module docs.
            Some(account) => {
                command.args(["tunnel", "run", "--token", &account.token]);
            }
            None => {
                command.args([
                    "tunnel",
                    "--url",
                    &format!("http://127.0.0.1:{local_gateway_port}"),
                ]);
            }
        }

        // Nothing reads stdout, and a pipe nobody drains is a process that
        // blocks once it fills. stderr is where cloudflared logs.
        command.stdin(Stdio::null());
        command.stdout(Stdio::null());
        command.stderr(Stdio::piped());

        #[cfg(windows)]
        command.creation_flags(crate::server::CREATE_NO_WINDOW);
        #[cfg(unix)]
        crate::server::group_leader(&mut command);

        let mut child = crate::server::tethered(command)
            .map_err(|error| TunnelError::WouldNotStart(error.to_string()))?;

        #[cfg(windows)]
        {
            self.job = crate::server::Job::hold(&child);
        }

        let state = Arc::new(Mutex::new(TunnelState::Starting));
        if let Some(stderr) = child.stderr.take() {
            watch(
                stderr,
                state.clone(),
                self.account.clone(),
                self.kind == TunnelType::Cloudflare,
            );
        }

        self.state = state;
        self.child = Some(child);
        Ok(())
    }

    fn stop(&mut self) -> Result<(), TunnelError> {
        self.take_down();
        // A fresh cell rather than a write into the old one: the reader thread
        // of the run just killed is about to see EOF and record a failure, and
        // it must not record it here.
        self.state = Arc::new(Mutex::new(TunnelState::Stopped));
        Ok(())
    }

    fn state(&self) -> TunnelState {
        self.state.lock().unwrap().clone()
    }

    fn tunnel_type(&self) -> TunnelType {
        self.kind
    }

    /// HTTPS, always, and not because anything here observed a handshake.
    ///
    /// The phone's browser spoke TLS to Cloudflare's edge; what reaches this
    /// listener is plaintext over loopback and always was. This is the fact the
    /// device cookie's `Secure` attribute is decided by, and it is a fact about
    /// the channel rather than about any byte on the wire — which is exactly
    /// why it is a method here and not a header read in the proxy.
    fn scheme(&self) -> Scheme {
        Scheme::Https
    }

    /// The phone's address, out of `CF-Connecting-IP` — but only on a
    /// connection that came from this machine.
    ///
    /// The header is Cloudflare's own and the edge overwrites whatever the
    /// client sent, so on a request that really arrived through the tunnel it
    /// is trustworthy. The catch is that "the Cloudflare channel is active" is
    /// not the same statement as "this request came through it": the listener
    /// is bound to `0.0.0.0`, so anything on the same Wi-Fi can open the port
    /// and write the header itself. Believed there, it would hand a phone the
    /// power to pick which address its wrong short codes are counted against,
    /// and to choose what the desktop's approval dialog shows the human.
    ///
    /// So the peer has to be loopback, which is where `cloudflared` — a child
    /// of this process — talks from. [`super::proxy`] refuses non-loopback
    /// peers outright while a public tunnel is up; this is the same rule held
    /// twice, because the two are about different things and the one that
    /// breaks first should not be the one making a header trustworthy.
    fn client_ip(&self, headers: &HeaderMap, peer: std::net::IpAddr) -> Option<std::net::IpAddr> {
        if !peer.is_loopback() {
            return None;
        }
        headers
            .get("cf-connecting-ip")?
            .to_str()
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// The one hostname, and no port on it: the phone reached `https://` on
    /// 443, so its `Host` header carries the name alone.
    ///
    /// Empty until the tunnel is actually up. A fence that trusted the
    /// hostname while `cloudflared` was still connecting would be open on a
    /// tunnel that may yet fail to exist.
    fn authorities(&self) -> Vec<String> {
        match self.state() {
            TunnelState::Running { base_url } => base_url
                .strip_prefix("https://")
                .map(|host| vec![host.trim_end_matches('/').to_ascii_lowercase()])
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }
}

impl Drop for CloudflareTunnel {
    /// The last of the three backstops, and the only one that runs on an
    /// ordinary drop: a tunnel replaced by [`super::Remote::switch`] would
    /// otherwise leave its `cloudflared` connected to Cloudflare with nothing
    /// on this end.
    fn drop(&mut self) {
        self.take_down();
    }
}

/// Read what `cloudflared` says until it stops saying anything.
///
/// Two jobs, and the second is the reason this is not simply piped to our own
/// stderr. `cloudflared` logs the tunnel's connection metadata, and this app
/// holds a token that must not end up in a terminal, a log file or a bug
/// report. So nothing is mirrored: the lines are classified, the one that
/// matters becomes a [`TunnelState`], and the last one is kept — with the token
/// replaced by a placeholder, an exact substitution because the token is a
/// string this app has in its hand — so that a failure can say something more
/// useful than "it did not work".
fn watch(
    stderr: std::process::ChildStderr,
    state: Arc<Mutex<TunnelState>>,
    account: Option<Account>,
    named: bool,
) {
    std::thread::spawn(move || {
        let hostname = account.as_ref().map(|account| account.hostname.clone());
        let token = account.map(|account| account.token);
        let mut last = String::new();

        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if let Some(base_url) = up(&line, hostname.as_deref()) {
                let mut held = state.lock().unwrap();
                if !matches!(*held, TunnelState::Running { .. }) {
                    eprintln!("dsh-desktop: the cloudflare tunnel is up at {base_url}");
                    *held = TunnelState::Running { base_url };
                }
                continue;
            }
            last = redact(&line, token.as_deref());
        }

        // EOF on stderr is `cloudflared` gone. Whether it ever came up decides
        // what this says, not whether it says anything: a tunnel that was
        // running and is not any more has to stop being an authority, or the
        // fence stays open on a channel with nothing behind it.
        let mut held = state.lock().unwrap();
        if matches!(*held, TunnelState::Stopped) {
            return;
        }
        let was_up = matches!(*held, TunnelState::Running { .. });
        *held = TunnelState::Failed(reason(was_up, named, &last));
        eprintln!("dsh-desktop: the cloudflare tunnel stopped");
    });
}

/// What to tell the user when the process ends.
fn reason(was_up: bool, named: bool, last: &str) -> String {
    let lede = if was_up {
        t!(
            "cloudflared 退出了，隧道已经断开。",
            "cloudflared exited, so the tunnel is down."
        )
    } else if named {
        t!(
            "cloudflared 没能连上 Cloudflare。检查一下 token 是不是完整、这台电脑能不能上外网。",
            "cloudflared could not reach Cloudflare. Check that the token was pasted in full \
             and that this computer can reach the internet."
        )
    } else {
        t!(
            "cloudflared 没能建起临时隧道。多半是连不上 Cloudflare。",
            "cloudflared could not raise a quick tunnel, which usually means it could not \
             reach Cloudflare."
        )
    };

    if last.is_empty() {
        return lede.to_string();
    }
    format!("{lede} {last}")
}

/// Whether this line means the tunnel is now carrying traffic, and where to.
///
/// The two modes learn it differently, and only one of them learns it from the
/// log. A quick tunnel is *given* its hostname by Cloudflare and prints it; a
/// named tunnel's hostname was known before the process started — it is the one
/// in the dashboard, which the user typed into this app — so what is waited for
/// there is a registered connection, not an address.
fn up(line: &str, hostname: Option<&str>) -> Option<String> {
    match hostname {
        Some(hostname) => registered(line).then(|| format!("https://{hostname}")),
        None => quick_url(line),
    }
}

/// `INF Registered tunnel connection connIndex=0 …`, in whichever of the
/// spellings the installed `cloudflared` uses.
fn registered(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("registered tunnel connection") || lower.contains("connection registered")
}

/// The `https://<something>.trycloudflare.com` out of the banner a quick tunnel
/// prints, or `None` for every other line.
///
/// The suffix is checked rather than merely "an https URL": that banner is
/// drawn in a box of `|` characters and sits among lines that carry Cloudflare
/// documentation links, and publishing one of those as the gateway's address
/// would send the phone to a help page.
fn quick_url(line: &str) -> Option<String> {
    let at = line.find("https://")?;
    let rest = &line[at..];
    let end = rest
        .find(|character: char| character.is_whitespace() || character == '|' || character == '"')
        .unwrap_or(rest.len());
    let url = rest[..end].trim_end_matches('/');

    url.ends_with(".trycloudflare.com").then(|| url.to_string())
}

/// Take the token out of a line before it is kept anywhere.
///
/// An exact substitution, not a pattern: the token is a string this process is
/// holding, so there is no guessing at what one looks like and no line that
/// contains it can slip through for being shaped unexpectedly.
fn redact(line: &str, token: Option<&str>) -> String {
    let line = line.trim();
    match token {
        Some(token) if !token.is_empty() => line.replace(token, "<token>"),
        _ => line.to_string(),
    }
}

/// Where `cloudflared` is, if it is anywhere.
///
/// The PATH first, because a `cloudflared` the user installed is one something
/// on their machine is keeping up to date; then this app's own data directory,
/// which is where a future build that offers to fetch the binary would put it.
///
/// Deliberately not a download. Fetching an executable over the network and
/// running it is a supply-chain hole unless the signature is checked, the
/// binary comes down quarantined on macOS, and GitHub Releases is not reliably
/// reachable from China — three problems each larger than this channel. The
/// card shows the install command for the platform instead.
pub fn binary(app: Option<&AppHandle>) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "cloudflared.exe"
    } else {
        "cloudflared"
    };

    if let Some(found) = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    }) {
        return Some(found);
    }

    let beside = app.and_then(crate::dsh::app_dir)?.join(name);
    beside.is_file().then_some(beside)
}

/// The token and hostname a named tunnel was configured with, or `None` when
/// either half is missing.
///
/// Both or neither: a hostname with no token launches nothing, and a token with
/// no hostname leaves the fence with no authority to admit and every request
/// refused. Half a configuration is the same as none, and saying so here means
/// no caller has to decide what to do with it.
pub fn account(app: &AppHandle) -> Option<Account> {
    let token = std::fs::read_to_string(crate::dsh::app_dir(app)?.join(TOKEN_FILE))
        .ok()?
        .trim()
        .to_string();
    let hostname = crate::settings::cloudflare_hostname(app)?;

    (!token.is_empty() && !hostname.is_empty()).then_some(Account { token, hostname })
}

/// Write both halves down. The hostname goes in `desktop.json` — it is not a
/// secret, and a user looking at the file should be able to see which host
/// their phone is being sent to — and the token goes in a file of its own.
pub fn remember(app: &AppHandle, token: &str, hostname: &str) -> Result<(), String> {
    let hostname = tidy_hostname(hostname);
    if token.trim().is_empty() || hostname.is_empty() {
        return Err(t!(
            "token 和域名都要填。",
            "Both the token and the hostname are needed."
        )
        .to_string());
    }

    let path = crate::dsh::app_dir(app)
        .ok_or_else(|| {
            t!(
                "找不到应用数据目录，没地方保存。",
                "There is no application data directory to save this in."
            )
            .to_string()
        })?
        .join(TOKEN_FILE);

    store(&path, token.trim()).map_err(|error| error.to_string())?;
    crate::settings::set_cloudflare_hostname(app, &hostname);
    Ok(())
}

/// Write the token readable by this user and nobody else.
///
/// The same shape as the signing key's own writer in [`super::session`], and
/// the mode is set at creation for the same reason: any other order leaves a
/// window in which the file exists and is world-readable.
fn store(path: &std::path::Path, token: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }

    std::io::Write::write_all(&mut options.open(path)?, token.as_bytes())
}

/// What the user pasted, as a bare host.
///
/// People paste what their browser shows them, which is a URL with a scheme on
/// it and often a trailing slash. Both are stripped rather than rejected: the
/// alternative is an error message about punctuation on a field where the user
/// has already given the right answer.
fn tidy_hostname(typed: &str) -> String {
    typed
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// How to get `cloudflared` onto this machine, in the words of the package
/// manager the platform actually has.
pub fn install_hint() -> String {
    if cfg!(windows) {
        t!(
            "用 winget 装：winget install --id Cloudflare.cloudflared；或者从 GitHub Releases \
             下载 cloudflared.exe，放进 PATH 里。",
            "Install it with winget: winget install --id Cloudflare.cloudflared — or download \
             cloudflared.exe from GitHub Releases and put it on the PATH."
        )
    } else if cfg!(target_os = "macos") {
        t!(
            "用 Homebrew 装：brew install cloudflared。",
            "Install it with Homebrew: brew install cloudflared."
        )
    } else {
        t!(
            "按 Cloudflare 文档装 cloudflared 包（Debian/Ubuntu 有 .deb，其他发行版有二进制）。",
            "Install the cloudflared package as Cloudflare documents it — a .deb on \
             Debian and Ubuntu, a binary elsewhere."
        )
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The banner a quick tunnel prints, and the lines around it that must not
    /// be mistaken for it.
    #[test]
    fn the_quick_hostname_is_read_off_the_banner() {
        assert_eq!(
            quick_url("|  https://dull-mice-invent-softly.trycloudflare.com    |"),
            Some("https://dull-mice-invent-softly.trycloudflare.com".to_string())
        );
        assert_eq!(
            quick_url("INF +--- https://a-b-c.trycloudflare.com/ ---+"),
            Some("https://a-b-c.trycloudflare.com".to_string())
        );

        // Every other https URL cloudflared prints, and it prints several.
        for line in [
            "INF Thank you for trying Cloudflare Tunnel.",
            "see https://developers.cloudflare.com/cloudflare-one/ for docs",
            "INF Requesting new quick Tunnel on trycloudflare.com...",
            "https://dash.cloudflare.com/argotunnel",
        ] {
            assert_eq!(quick_url(line), None, "{line}");
        }
    }

    /// A named tunnel's address was known before the process started, so what
    /// the log is read for is a connection and not a hostname.
    #[test]
    fn a_named_tunnel_waits_for_a_registered_connection() {
        let host = Some("dsh.example.com");

        assert_eq!(
            up(
                "2026-09-19T10:00:00Z INF Registered tunnel connection connIndex=0 \
                 connection=abc location=sjc",
                host
            ),
            Some("https://dsh.example.com".to_string())
        );
        assert_eq!(up("INF Starting tunnel tunnelID=abc", host), None);
        // And it never reads a hostname out of the log, even one that is there.
        assert_eq!(up("|  https://x.trycloudflare.com  |", host), None);
    }

    /// The one thing that must not survive into a log line this app keeps.
    #[test]
    fn the_token_is_taken_out_of_anything_kept() {
        let token = "eyJhIjoiMTIzIiwidCI6IjQ1NiJ9";
        assert_eq!(
            redact(&format!("ERR bad token {token} rejected"), Some(token)),
            "ERR bad token <token> rejected"
        );
        // Whitespace goes too: these end up in a sentence on the card.
        assert_eq!(redact("  ERR something  ", Some(token)), "ERR something");
        assert_eq!(redact("ERR something", None), "ERR something");
        assert_eq!(redact("ERR something", Some("")), "ERR something");
    }

    /// What people paste is whatever their address bar showed them.
    #[test]
    fn the_hostname_is_taken_as_pasted() {
        for typed in [
            "dsh.example.com",
            "https://dsh.example.com",
            "https://dsh.example.com/",
            "http://DSH.Example.com/path",
            "  dsh.example.com  ",
        ] {
            assert_eq!(tidy_hostname(typed), "dsh.example.com", "{typed}");
        }
        assert_eq!(tidy_hostname(""), "");
        assert_eq!(tidy_hostname("   "), "");
    }

    /// A tunnel nobody has started is not an authority for anything, whatever
    /// hostname it was configured with. The window between asking for a tunnel
    /// and having one is seconds long here, and a fence open across it would be
    /// open on a channel that may never come up.
    #[test]
    fn a_tunnel_that_is_not_up_yet_trusts_nothing() {
        let tunnel = CloudflareTunnel {
            kind: TunnelType::Cloudflare,
            binary: None,
            account: Some(Account {
                token: "t".into(),
                hostname: "dsh.example.com".into(),
            }),
            child: None,
            #[cfg(windows)]
            job: None,
            state: Arc::new(Mutex::new(TunnelState::Starting)),
        };

        assert!(tunnel.authorities().is_empty());
        assert!(tunnel.public());
        assert!(tunnel.scheme().secure());

        *tunnel.state.lock().unwrap() = TunnelState::Running {
            base_url: "https://dsh.example.com".into(),
        };
        assert_eq!(tunnel.authorities(), vec!["dsh.example.com".to_string()]);
    }

    /// The header is Cloudflare's, and it is believed on exactly one kind of
    /// connection: the one `cloudflared` itself makes, from this machine. A
    /// device on the same Wi-Fi can reach this listener directly — it is bound
    /// to every interface — and must not get to name itself.
    #[test]
    fn the_forwarded_address_is_believed_only_from_loopback() {
        let tunnel = CloudflareTunnel::new(None, TunnelType::CloudflareQuick);
        let mut headers = HeaderMap::new();
        headers.insert("cf-connecting-ip", "203.0.113.7".parse().unwrap());

        assert_eq!(
            tunnel.client_ip(&headers, "127.0.0.1".parse().unwrap()),
            Some("203.0.113.7".parse().unwrap())
        );
        assert_eq!(
            tunnel.client_ip(&headers, "192.168.1.9".parse().unwrap()),
            None
        );

        // And a header that is not an address is not one.
        let mut nonsense = HeaderMap::new();
        nonsense.insert("cf-connecting-ip", "not-an-address".parse().unwrap());
        assert_eq!(
            tunnel.client_ip(&nonsense, "127.0.0.1".parse().unwrap()),
            None
        );
        assert_eq!(
            tunnel.client_ip(&HeaderMap::new(), "127.0.0.1".parse().unwrap()),
            None
        );
    }

    /// A named tunnel with nothing configured refuses before it launches
    /// anything, and says which half of the problem it is.
    #[test]
    fn an_unconfigured_named_tunnel_will_not_start() {
        let mut tunnel = CloudflareTunnel::new(None, TunnelType::Cloudflare);
        tunnel.binary = Some(PathBuf::from("cloudflared"));

        assert!(matches!(tunnel.start(59000), Err(TunnelError::NoToken)));
        assert_eq!(tunnel.state(), TunnelState::Stopped);
    }

    /// And one with no binary refuses before it looks at anything else, because
    /// that is the failure with something to do about it.
    #[test]
    fn a_missing_binary_is_the_first_thing_reported() {
        let mut tunnel = CloudflareTunnel::new(None, TunnelType::Cloudflare);
        tunnel.binary = None;

        assert!(matches!(
            tunnel.start(59000),
            Err(TunnelError::NoCloudflared)
        ));
    }
}
