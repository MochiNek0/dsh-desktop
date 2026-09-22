//! The handful of choices this app makes on its own, kept next to the ones the
//! installer makes.
//!
//! Almost nothing belongs here. The theme is dsh's, read out of its
//! `settings.yaml` (see [`crate::theme`]); the login item is the operating
//! system's, and is asked about rather than recorded. What is left is the
//! preferences that are this window's alone, and today that is one: whether
//! this app raises notifications at all.
//!
//! ## Where it lives
//!
//! `desktop.json` in [`crate::dsh::app_dir`] — Tauri's `app_local_data_dir`,
//! which is `%LOCALAPPDATA%\<identifier>` on Windows, `~/Library/Application
//! Support/<identifier>` on macOS and `$XDG_DATA_HOME/<identifier>` (in
//! practice `~/.local/share/<identifier>`) on Linux.
//!
//! The same directory on all three, and the same one `install-deps.sh` picks
//! with its own `Darwin` / `*` branch, so this file lands beside the
//! `bootstrap.json` that script writes — and, on Windows, inside what the
//! installer hooks clean up. Deliberately *not* in `$DSH_HOME`: that directory
//! is dsh's own, it survives an uninstall on purpose, and a desktop-only
//! preference has no business in a file dsh parses.
//!
//! ## Why by hand, and why so forgiving
//!
//! A store plugin would bring a dependency, a schema and a migration story for
//! what is one boolean. Instead the file is read with `serde_json` — already
//! here for the preset list — and every failure answers with the default:
//! unreadable, truncated by a power cut mid-write, hand-edited into invalid
//! JSON, or written by a future version that keeps something else in it.
//!
//! That is the whole design rule. A preference file is not worth an error
//! dialog, and it is certainly not worth refusing to start over. Unknown keys
//! are preserved through a write rather than dropped, so a newer build's
//! settings survive being opened by an older one.

use std::path::PathBuf;

use serde_json::{Map, Value};
use tauri::AppHandle;

/// Whether this app raises notifications at all. On unless it was turned off:
/// the feature exists because the window spends turns in the tray, and a
/// notification setting that defaults to silent is one nobody discovers.
///
/// Deliberately not `notifyOnTurnEnd`, which is what this was called first. The
/// gate it drives sits in [`crate::notify::show`], the one place *every*
/// notification passes through — a plugin's as much as this app's own — so a
/// name that promised only the finished-turn toast was describing a narrower
/// switch than the one actually wired up. The menu item says the same thing:
/// "Notifications", not "Notify when a turn finishes".
///
/// What the preference cannot do is turn anything on by itself. Every
/// notification this app raises starts as a signal from the client plugin in
/// `plugin/`, so [`crate::notify::show`] asks
/// [`crate::plugins::signalling`] first and the menu draws the switch
/// unavailable when the answer is no. The stored value is left alone in that
/// state rather than forced off: a user who turns notifications on, removes
/// the plugin and puts it back should find the switch where they left it.
///
/// The old name is not read as a fallback. Both the switch and the rename
/// landed before v0.1.7, so no released build ever wrote `notifyOnTurnEnd` and
/// there is no file in the wild holding one — the fallback only ever covered a
/// dev build of the branch it was written on.
const NOTIFY_KEY: &str = "notifications";

const NOTIFY_DEFAULT: bool = true;

/// Read the preference. Any problem reading it is the default.
pub fn notifications(app: &AppHandle) -> bool {
    let document = read(app);
    document
        .get(NOTIFY_KEY)
        .and_then(Value::as_bool)
        .unwrap_or(NOTIFY_DEFAULT)
}

/// Flip it, and answer with what it now is.
///
/// Returns the value that was written rather than re-reading, so a caller can
/// repaint the checkmark even on the disk error path — where the toggle did not
/// survive, but the menu should still not show a lie about this session.
pub fn toggle_notifications(app: &AppHandle) -> bool {
    let wanted = !notifications(app);
    write(app, NOTIFY_KEY, Value::Bool(wanted));
    wanted
}

/// Which registry an install of dsh is taken from, when the machine has an
/// opinion of its own about that.
///
/// npm reads a registry out of the user's `.npmrc`, and a private mirror or a
/// corporate proxy is there for a reason — it may be the only route out of the
/// network at all. So the installer has always deferred to it and said so in
/// its log. What it could not do was ask: a mirror that answers but lags behind
/// serves an old `dsh@latest` and the install succeeds, so nothing ever fails
/// over to a source that has the current one, and the user is never told which
/// of the two they got.
///
/// This is that question, asked once and remembered. [`RegistrySource::Own`]
/// keeps the machine's own configured registry; [`RegistrySource::Auto`] hands
/// the choice to the installer's own list, which measures the mirrors and takes
/// the fastest. Absent means nobody has been asked yet — which is not the same
/// as either answer, and is why this reads as an `Option`.
///
/// ## The choice is stored, not the address
///
/// `own` rather than the URL that was configured when the question was asked.
/// A user who later points their `.npmrc` somewhere else is still a user who
/// said "mine", and re-asking them because the address moved would be asking
/// the wrong question. It also means the file cannot go stale against a
/// registry that no longer exists.
const REGISTRY_KEY: &str = "registry";

/// Where `dsh` is installed from. See [`REGISTRY_KEY`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RegistrySource {
    /// The registry the machine's own npm configuration names.
    Own,
    /// Whichever of the installer's sources measures fastest.
    Auto,
}

impl RegistrySource {
    /// The spelling both installer scripts take for `-Registry`, and the one
    /// stored in `desktop.json`. One function so the two can never drift.
    pub fn as_str(self) -> &'static str {
        match self {
            RegistrySource::Own => "own",
            RegistrySource::Auto => "auto",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "own" => Some(RegistrySource::Own),
            "auto" => Some(RegistrySource::Auto),
            _ => None,
        }
    }
}

/// The answer, or `None` when the question has not been put yet.
///
/// A value this build does not recognise reads as `None` — the same forgiveness
/// every other key here gets, and it means a source a future version adds
/// degrades to asking again rather than to a panic.
pub fn registry(app: &AppHandle) -> Option<RegistrySource> {
    read(app)
        .get(REGISTRY_KEY)
        .and_then(Value::as_str)
        .and_then(RegistrySource::parse)
}

/// Write the answer down. Called once when the question is first put, and again
/// whenever the user changes it from the menu.
pub fn set_registry(app: &AppHandle, source: RegistrySource) {
    write(app, REGISTRY_KEY, Value::String(source.as_str().to_string()));
}

/// Which line of dsh releases this app installs and updates to.
///
/// npm publishes dsh under two tags that matter here. `latest` is the
/// release-candidate line, which is what every install has always taken and
/// what a machine that has never been asked still takes. `alpha` runs ahead of
/// it, and is there for a user who wants what is being worked on now rather
/// than what is being stabilised.
///
/// ## Alpha is not merely "newer"
///
/// The two lines are not a ladder with alpha on the higher rung. Work lands in
/// alpha that may never reach a release candidate in the shape it landed in,
/// and one of the things it is free to change is the format dsh writes its
/// sessions in. A session written by an alpha is not a session an rc promises
/// to be able to open, which makes going back the problem rather than going
/// forward.
///
/// ## Why the two lines still share a home
///
/// The obvious answer to that is to give alpha a `$DSH_HOME` of its own, and it
/// was tried. It does not hold. Only one dsh can be installed globally, so
/// after a switch the `dsh` the user types in their own terminal is the alpha
/// one — and nothing this app does reaches that terminal's environment, by an
/// older decision this one is not going to overturn (see
/// [`crate::dsh::terminal`]). That dsh resolves `$DSH_HOME` for itself, lands
/// in the shared home, and writes there. So the separation held only for
/// sessions started through this window, while the dialog was promising that
/// the other home was untouched — a promise the first terminal the user opened
/// would break.
///
/// A promise that cannot be kept is worse than none. So the channel moves which
/// dsh is installed and nothing else: one home, one dsh, and a dialog that says
/// what the risk is and where the directory to back up lives. See
/// `channel_question` in `main.rs`, which is the whole of the mitigation.
///
/// ## Why the default is not `Option`
///
/// Unlike [`REGISTRY_KEY`], where "nobody has been asked" is a state worth
/// telling apart from either answer, there is always a channel in use and the
/// safe one is known. So a file with nothing in it, and a file holding a name
/// this build does not recognise, both read as [`Channel::Rc`] — never as
/// alpha. A preference that cannot be parsed must not be able to move anyone
/// onto the line that writes sessions the other one cannot read.
const DSH_CHANNEL_KEY: &str = "dshChannel";

/// The line of dsh releases in use. See [`DSH_CHANNEL_KEY`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Channel {
    /// The release candidates, published under npm's `latest`.
    Rc,
    /// The alpha line, published under npm's `alpha`.
    Alpha,
}

/// What a machine nobody has asked is on, and what anything unreadable reads
/// as.
const DSH_CHANNEL_DEFAULT: Channel = Channel::Rc;

impl Channel {
    /// The spelling both installer scripts take for `-Channel`, and the one
    /// stored in `desktop.json`. One function so the two can never drift, the
    /// same way [`RegistrySource::as_str`] is one.
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Rc => "rc",
            Channel::Alpha => "alpha",
        }
    }

    /// The npm dist-tag this channel installs from.
    ///
    /// Deliberately not the same string as [`Channel::as_str`]: the rc line is
    /// published under `latest`, not under `rc`, and writing `rc` into an
    /// `npm install -g @deepseek-ai/dsh@rc` would name a tag that does not
    /// exist. The scripts hold the other half of this mapping and a test pins
    /// the two together.
    pub fn tag(self) -> &'static str {
        match self {
            Channel::Rc => "latest",
            Channel::Alpha => "alpha",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "rc" => Some(Channel::Rc),
            "alpha" => Some(Channel::Alpha),
            _ => None,
        }
    }
}

/// The channel in use. Anything unreadable is [`DSH_CHANNEL_DEFAULT`]; see
/// [`DSH_CHANNEL_KEY`] for why this is not an `Option`.
pub fn dsh_channel(app: &AppHandle) -> Channel {
    read(app)
        .get(DSH_CHANNEL_KEY)
        .and_then(Value::as_str)
        .and_then(Channel::parse)
        .unwrap_or(DSH_CHANNEL_DEFAULT)
}

/// Write the channel the user switched to.
pub fn set_dsh_channel(app: &AppHandle, channel: Channel) {
    write(
        app,
        DSH_CHANNEL_KEY,
        Value::String(channel.as_str().to_string()),
    );
}

/// The port the phone gateway bound last time, so that it can ask for the same
/// one again.
///
/// The gateway binds port `0` and lets the operating system pick — nothing else
/// needs to know the number in advance, which is the whole advantage of the
/// forwarding method [`crate::remote`] uses. But "nobody needs to know it in
/// advance" is not the same as "it may change", and the phone at the other end
/// is the thing that does not get a say: a browser's notion of a site is
/// `scheme://host:port`, so a new port every launch is a new site every launch.
/// A cookie issued to the old one is not sent to the new one, and an icon
/// added to a home screen points at a port nothing is listening on — which
/// fails as the browser's own connection-refused page, before a single line of
/// ours runs, so there is nowhere to put "scan the code again".
///
/// Hence: remembered, asked for, and not insisted on. Something else holding
/// the port is not an error — the gateway falls back to `0` and writes down
/// whatever it got, which is the same state a first launch is in.
///
/// The firewall rule is not affected either way: [`crate::remote::firewall`]
/// matches on the program, not the port.
const GATEWAY_PORT_KEY: &str = "remoteGatewayPort";

/// The remembered port, or `None` on the first launch — and on any file that
/// holds something that is not a port.
///
/// `0` reads as `None` rather than as itself. It is the spelling of "you pick",
/// so a file containing it is a file asking for exactly what `None` already
/// means, and treating it as a port to request would be asking the OS to pick
/// and then recording that it was asked.
pub fn gateway_port(app: &AppHandle) -> Option<u16> {
    read(app)
        .get(GATEWAY_PORT_KEY)
        .and_then(Value::as_u64)
        .and_then(|port| u16::try_from(port).ok())
        .filter(|port| *port != 0)
}

/// Remember the port the gateway is actually on.
///
/// Called with what the listener reports rather than with what was asked for,
/// so the fallback path records the port it fell back to.
pub fn set_gateway_port(app: &AppHandle, port: u16) {
    write(app, GATEWAY_PORT_KEY, Value::from(port));
}

/// Which channel the phone reaches this machine over.
///
/// Stored by name rather than by number, because the number is an enum
/// discriminant and this file outlives the build that wrote it; see
/// [`crate::remote::TunnelType::name`].
const CHANNEL_KEY: &str = "remoteChannel";

/// The channel to raise, defaulting to the local network.
///
/// A name this build does not recognise falls back to the default rather than
/// refusing to start: the gateway on the LAN is always a usable answer, and a
/// settings file written by a newer version is not a reason to have no phone
/// connection at all.
pub fn channel(app: &AppHandle) -> crate::remote::TunnelType {
    read(app)
        .get(CHANNEL_KEY)
        .and_then(Value::as_str)
        .and_then(crate::remote::TunnelType::named)
        .unwrap_or(crate::remote::TunnelType::Lan)
}

/// Remember the channel the user switched to.
pub fn set_channel(app: &AppHandle, kind: crate::remote::TunnelType) {
    write(app, CHANNEL_KEY, Value::String(kind.name().to_string()));
}

/// The hostname a Cloudflare named tunnel publishes — `dsh.example.com`, bare.
///
/// Here rather than beside the token, and that split is deliberate. A hostname
/// is not a secret: it is the address the user's own phone is sent to, and
/// somebody reading this file to find out where their computer is answering
/// from should find it. The token that operates the tunnel is the half that
/// does not belong in a hand-editable file; see
/// [`mod@crate::remote::cloudflare`].
const CLOUDFLARE_HOST_KEY: &str = "remoteCloudflareHostname";

/// The configured hostname, or `None` when nobody has set one — and for an
/// empty string, which is what a cleared field leaves behind and is the same
/// thing as unset.
pub fn cloudflare_hostname(app: &AppHandle) -> Option<String> {
    read(app)
        .get(CLOUDFLARE_HOST_KEY)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|host| !host.is_empty())
}

/// Write it down. Given already tidied — scheme and path stripped, lowercased —
/// by the one caller that has the user's typing in its hand.
pub fn set_cloudflare_hostname(app: &AppHandle, hostname: &str) {
    write(
        app,
        CLOUDFLARE_HOST_KEY,
        Value::String(hostname.to_string()),
    );
}

/// The phones the gateway has let in, and the counter their ids come from.
///
/// State, not a preference — the only thing in this file that is, so it is
/// worth saying why it is here rather than in a file of its own. It is small,
/// it is this app's alone, it has to survive exactly as long as the other keys
/// here do, and it wants the same forgiveness: a device list that fails to
/// parse should cost the user a re-scan, not a launch. A second file with a
/// second reader, a second corruption story and a second thing to clean up on
/// uninstall would buy nothing for that.
///
/// The shape is [`crate::remote::session`]'s, and stays there — this file
/// stores the value without reading into it, the way it stores the others.
/// What does *not* go in here is the key those devices' cookies are signed
/// with: see that module for where that lives and why it is not this file.
const PAIRING_KEY: &str = "remotePairing";

/// What was written last time, whatever shape it is in.
///
/// Handed back unread. A document this build cannot make sense of is the
/// caller's problem to shrug at, and [`crate::remote::session`] does.
pub fn pairing(app: &AppHandle) -> Option<Value> {
    read(app).get(PAIRING_KEY).cloned()
}

/// Write the device list back. Called on every change to it — a device let in,
/// one kicked, all of them kicked.
pub fn set_pairing(app: &AppHandle, state: Value) {
    write(app, PAIRING_KEY, state);
}

/// Whether closing the app should throw every paired phone off.
///
/// Off by default, because the feature exists so that the phone in a pocket
/// still works tomorrow morning, and a pairing that ends when the window closes
/// is one that has to be redone before every single use.
///
/// It is here for the user who reads [`crate::remote::session`]'s module docs
/// and does not like what they say: with the switch off there is a key on this
/// disk that signs cookies into a shell, and the honest answer to someone who
/// would rather that key not outlive the session is a switch, not an argument.
const FORGET_ON_EXIT_KEY: &str = "remoteForgetPairingsOnExit";

const FORGET_ON_EXIT_DEFAULT: bool = false;

/// Read the switch. Any problem reading it is the default.
pub fn forget_pairings_on_exit(app: &AppHandle) -> bool {
    read(app)
        .get(FORGET_ON_EXIT_KEY)
        .and_then(Value::as_bool)
        .unwrap_or(FORGET_ON_EXIT_DEFAULT)
}

/// Set it to what the box on the card now shows — the state, not a flip. See
/// [`crate::controls::Action::RemoteForget`].
pub fn set_forget_pairings_on_exit(app: &AppHandle, on: bool) {
    write(app, FORGET_ON_EXIT_KEY, Value::Bool(on));
}

/// The whole document, or an empty one. Never `Err`: see the module docs.
fn read(app: &AppHandle) -> Map<String, Value> {
    let Some(path) = file(app) else {
        return Map::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Map::new();
    };
    match serde_json::from_str::<Value>(&text) {
        // A JSON document that is not an object — `null`, a list, a bare number
        // — is as unusable as a corrupt one, and is treated the same way.
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

/// Set one key, keeping every other key the file already had.
///
/// Read-modify-write rather than serialising a struct, so a preference this
/// build has never heard of — one a newer version wrote — is still there after
/// an older version toggles something beside it.
fn write(app: &AppHandle, key: &str, value: Value) {
    let Some(path) = file(app) else {
        return;
    };

    let mut document = read(app);
    document.insert(key.to_string(), value);

    let Ok(text) = serde_json::to_string_pretty(&Value::Object(document)) else {
        return;
    };

    // The directory is normally already there — the install script writes
    // `bootstrap.json` into it — but not on a machine where that never ran.
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    if let Err(error) = std::fs::write(&path, text) {
        // Losing a preference is not a reason to interrupt anyone; the setting
        // simply reverts next launch.
        eprintln!("dsh-desktop: could not save the settings: {error}");
    }
}

fn file(app: &AppHandle) -> Option<PathBuf> {
    Some(crate::dsh::app_dir(app)?.join("desktop.json"))
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Map, Value};

    /// The read half of `read`, without the `AppHandle` a test cannot build.
    /// A copy of it, and nothing pins the two together: a change to how the
    /// reader treats a document has to be made here as well or these tests go
    /// on passing against the old behaviour.
    fn parse(text: &str) -> Map<String, Value> {
        match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(map)) => map,
            _ => Map::new(),
        }
    }

    fn notify(text: &str) -> bool {
        let document = parse(text);
        document
            .get(super::NOTIFY_KEY)
            .and_then(Value::as_bool)
            .unwrap_or(super::NOTIFY_DEFAULT)
    }

    #[test]
    fn reads_the_preference() {
        assert!(!notify(r#"{"notifications": false}"#));
        assert!(notify(r#"{"notifications": true}"#));
    }

    /// The name this switch had before v0.1.7 is not a fallback: it reads as
    /// any other key this build does not know, which is to say the default.
    #[test]
    fn the_name_it_had_first_is_just_an_unknown_key() {
        assert!(notify(r#"{"notifyOnTurnEnd": false}"#));
    }

    /// Every way the file can be unusable ends at the default, because a
    /// preference is not worth failing a launch over.
    #[test]
    fn falls_back_to_the_default() {
        assert!(notify(""));
        assert!(notify("{"));
        assert!(notify("null"));
        assert!(notify("[1, 2, 3]"));
        assert!(notify("{}"));
        // Present, but not a boolean.
        assert!(notify(r#"{"notifications": "no"}"#));
    }

    /// On by default: the window spends a turn in the tray, and a notification
    /// nobody switched on is a notification nobody knows exists.
    #[test]
    fn defaults_to_notifying() {
        const { assert!(super::NOTIFY_DEFAULT) };
    }

    /// The read half of [`super::gateway_port`], mirrored the same way and with
    /// the same warning as `parse` above.
    fn port(text: &str) -> Option<u16> {
        parse(text)
            .get(super::GATEWAY_PORT_KEY)
            .and_then(Value::as_u64)
            .and_then(|port| u16::try_from(port).ok())
            .filter(|port| *port != 0)
    }

    #[test]
    fn reads_the_remembered_port() {
        assert_eq!(port(r#"{"remoteGatewayPort": 59123}"#), Some(59123));
    }

    /// Nothing recorded, or something recorded that is not a port a listener
    /// could be asked for. All one answer: let the operating system pick, which
    /// is what a first launch does anyway.
    #[test]
    fn anything_that_is_not_a_port_means_let_the_os_pick() {
        assert_eq!(port("{}"), None);
        assert_eq!(port("{"), None);
        assert_eq!(port(r#"{"remoteGatewayPort": "59123"}"#), None);
        assert_eq!(port(r#"{"remoteGatewayPort": -1}"#), None);
        // Past what a port number can be — a file written by something else,
        // or by hand.
        assert_eq!(port(r#"{"remoteGatewayPort": 70000}"#), None);
        // The spelling of "you pick", which is already what `None` says.
        assert_eq!(port(r#"{"remoteGatewayPort": 0}"#), None);
    }

    /// The read half of [`super::dsh_channel`], mirrored the same way and with
    /// the same warning as `parse` above.
    fn channel(text: &str) -> super::Channel {
        parse(text)
            .get(super::DSH_CHANNEL_KEY)
            .and_then(Value::as_str)
            .and_then(super::Channel::parse)
            .unwrap_or(super::DSH_CHANNEL_DEFAULT)
    }

    #[test]
    fn reads_the_channel() {
        assert_eq!(channel(r#"{"dshChannel": "rc"}"#), super::Channel::Rc);
        assert_eq!(channel(r#"{"dshChannel": "alpha"}"#), super::Channel::Alpha);
    }

    /// Every unreadable file lands on rc, and none of them on alpha. This is
    /// the one preference here where the fallback is a safety property rather
    /// than a convenience: alpha writes sessions an rc need not be able to
    /// open, so nothing but the word `alpha` may put anyone on it.
    #[test]
    fn nothing_unreadable_can_land_on_alpha() {
        for text in [
            "",
            "{",
            "null",
            "[1, 2, 3]",
            "{}",
            r#"{"dshChannel": "beta"}"#,
            r#"{"dshChannel": "ALPHA"}"#,
            r#"{"dshChannel": true}"#,
            r#"{"dshChannel": null}"#,
        ] {
            assert_eq!(channel(text), super::Channel::Rc, "for {text:?}");
        }
    }

    /// The rc line is published under npm's `latest`, not under `rc`. Spelling
    /// the stored name into an install would name a tag that does not exist,
    /// so the two strings are deliberately different and this says so.
    #[test]
    fn the_stored_name_is_not_the_npm_tag() {
        assert_eq!(super::Channel::Rc.as_str(), "rc");
        assert_eq!(super::Channel::Rc.tag(), "latest");
        assert_eq!(super::Channel::Alpha.as_str(), "alpha");
        assert_eq!(super::Channel::Alpha.tag(), "alpha");
    }

    /// A key this build does not know about survives a write of one that it
    /// does, so a newer version's settings are not thrown away by an older one.
    #[test]
    fn keeps_keys_it_does_not_understand() {
        let mut document = parse(r#"{"somethingNewer": {"nested": 1}}"#);
        document.insert(super::NOTIFY_KEY.to_string(), json!(false));

        assert_eq!(document.get("somethingNewer"), Some(&json!({"nested": 1})));
        assert_eq!(document.get(super::NOTIFY_KEY), Some(&json!(false)));
    }
}
