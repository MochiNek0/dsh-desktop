//! Who the gateway lets in, and the two secrets that decide it.
//!
//! Everything behind the gateway is the dsh session on this machine, which is
//! to say a shell on it. So the only device that gets through is one a human
//! sitting at the desktop said yes to, and the only proof it offers afterwards
//! is a cookie this process signed. Both halves live here.
//!
//! ## The pairing nonce
//!
//! The QR code carries `?pair_token=<nonce>`: 16 random bytes, good for
//! [`PAIR_TTL`], and spent the first time it is redeemed. It authorises
//! nothing on its own — redeeming it only buys the right to *ask*, and what
//! answers is the dialog on the desktop. Its whole job is to keep the question
//! from being asked by anything that merely found the port open, because a
//! question anyone on the network can raise is a doorbell anyone on the network
//! can ring.
//!
//! ## And the six characters beside it
//!
//! The same nonce, spelled so that a person can type it. One [`Pair`] holds
//! both: redeeming either spends the entry, so the code and the QR expire
//! together and neither outlives the other's use.
//!
//! It exists because the camera is the wrong tool for the commonest failure.
//! A phone that has been paired and has lost its cookie — the key rotated,
//! [`SESSION_TTL`] ran out, "disconnect everything" was pressed, iOS handed the
//! home-screen window a cookie jar of its own — is a phone already looking at
//! this gateway's 401 page. Sending that user back to the computer for a
//! camera, when the feature exists so that they need not be at the computer, is
//! the wrong answer to the easiest question. Six characters typed into the page
//! they already have open is a same-origin navigation: no camera, so no secure
//! context to want, so it works on the plain-HTTP LAN where a scanner cannot;
//! and on iOS it stays inside the standalone window instead of throwing the
//! user back into Safari with a second icon to show for it.
//!
//! What it costs is search space — 2^128 becomes about 2^30 — and that is
//! affordable only because of what the nonce is *for*, above. Four things stand
//! behind it: [`PAIR_TTL`], single use, [`crate::remote::trust::Guesses`] on
//! wrong ones, and the human at the desktop who still has to say yes.
//!
//! ## The device cookie
//!
//! `dsh_mobile_session=v1.<id>.<issued>.<mac>`, where the mac is HMAC-SHA256
//! over everything before it under the secret described below. The id names a
//! row in [`Inner::devices`], so a device that has been kicked fails on the row
//! rather than on the signature, and "disconnect everything" is one call that
//! empties the list *and* rotates the secret — after which every cookie this
//! process ever signed is scrap.
//!
//! The mac is compared with `subtle` rather than `==`. The comparison is over a
//! value the caller controls, against a secret they are trying to guess, which
//! is the exact shape a timing oracle is built out of.
//!
//! ## The secret is on the disk, and that is a decision
//!
//! It was not, once. The secret was minted at startup and written nowhere, and
//! the security model was the clean one: closing the app revoked everything,
//! because the only copy of the key went with the process.
//!
//! What that model cost was the feature. [`SESSION_TTL`] says thirty days and
//! the reasoning under it says a pairing nobody has to redo is the whole point
//! — and both were false, because every launch minted a new key and every
//! phone came back to a 401. Worse than the re-scan was where it left the
//! phone: the gateway also took a new port each launch, so a device that had
//! been added to a home screen was pointing at an origin with nothing behind
//! it. Not a 401 this app could explain — the browser's own connection-refused
//! page, reached before a line of ours runs.
//!
//! So the key is written down, and the model is now: **for thirty days, or
//! until the user disconnects everything, whoever can read that file can forge
//! a cookie into this machine's dsh session.**
//!
//! Two things make that trade a reasonable one rather than a quiet
//! downgrade. The first is that the file sits in [`crate::dsh::app_dir`],
//! user-only by the operating system's own doing, and the process that can read
//! it is by definition a process running as this user — which is already a
//! process that can read dsh's launch token off the command line, talk to dsh
//! on loopback and drive the session directly. The key is not the first thing
//! such a process gets; it is a slower route to something it already has.
//!
//! The second is that the user gets a say:
//! [`crate::settings::forget_pairings_on_exit`] puts the old model back, and
//! the card says which one is switched on.
//!
//! An OS credential store was the other candidate. On the platform most of this
//! app's users are on it is the same guarantee — Windows Credential Manager is
//! readable by any process of the same user, with no per-program check — and on
//! Linux it would mean carrying a D-Bus client for one 32-byte value. The file
//! is 0600 where a mode means anything, and inherits a user-only directory
//! where it does not.
//!
//! ## What is deliberately not in the cookie
//!
//! The address the device connected from. Phase 1 could bind to it — the phone
//! is on the same Wi-Fi and its lease is not going to move mid-session — but
//! Phase 2 puts the same session behind a tunnel, where the address the gateway
//! sees is the tunnel's and changes without the device having gone anywhere.
//! Binding to it now would be a check that quietly stops meaning anything
//! later, which is worse than not having written it.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde_json::Value;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tauri::AppHandle;

/// The name the cookie goes out under. Ours, not dsh's — see
/// [`crate::remote::upstream`] for the one dsh mints, which never leaves this
/// process.
pub const COOKIE: &str = "dsh_mobile_session";

/// How long a pairing nonce is worth redeeming.
///
/// Five minutes is the window between putting a QR code on screen and pointing
/// a camera at it. Longer would leave a live nonce in a screenshot or on a
/// projector; shorter would expire under someone hunting for the phone they
/// left in the other room.
const PAIR_TTL: Duration = Duration::from_secs(5 * 60);

/// How long a device stays authorised without being kicked.
///
/// Thirty days, which is what dsh gives its own browser cookie. The point of
/// the feature is that the phone in a pocket still works tomorrow morning; a
/// session that has to be re-paired every launch is one nobody would turn on.
const SESSION_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// A device that got past the desktop dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// The name in the cookie, and the handle the desktop kicks it by.
    pub id: String,
    /// What to call it on the desktop card. Worked out from the user agent by
    /// [`crate::remote::trust::label`]; never trusted for anything but reading.
    pub label: String,
    /// Where it connected from, as the gateway saw it at the time. Shown, not
    /// checked — see the module docs.
    pub address: String,
    /// Seconds since the epoch, for the card's "connected at".
    pub since: u64,
}

/// One outstanding nonce, in both of its spellings.
struct Pair {
    /// What the QR code carries.
    token: String,
    /// What the card prints beside it, and what a person types. See the module
    /// docs for why this is not simply a shorter token.
    code: String,
    /// When both stop being redeemable.
    until: SystemTime,
}

/// A nonce as the card needs it: one to put in the URL, one to print.
pub struct Pairing {
    pub token: String,
    pub code: String,
}

/// The gateway's whole notion of who is allowed in.
#[derive(Default)]
pub struct SessionStore {
    inner: Mutex<Inner>,
    /// Where the device list is written back to. `Some` for the store the app
    /// runs on; `None` for one that keeps everything in memory — which is what
    /// the tests get, and what a machine whose key could not be written falls
    /// back to. See [`SessionStore::persisted`].
    save: Option<AppHandle>,
}

struct Inner {
    /// What device cookies are signed with. Rotated by
    /// [`SessionStore::revoke_all`].
    secret: [u8; 32],
    /// Outstanding pairing nonces.
    pairs: Vec<Pair>,
    devices: Vec<Device>,
    /// Where device ids come from. A counter rather than more randomness: the
    /// id is not a secret — the mac beside it is — and a number that never
    /// repeats is what keeps a kicked device's cookie from being revived by the
    /// next device to pair.
    next: u64,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            secret: secret(),
            pairs: Vec::new(),
            devices: Vec::new(),
            next: 1,
        }
    }
}

impl Inner {
    /// The device list as it goes into `desktop.json`.
    fn stored(&self) -> Value {
        serde_json::json!({
            "next": self.next,
            "devices": self
                .devices
                .iter()
                .map(|device| serde_json::json!({
                    "id": device.id,
                    "label": device.label,
                    "address": device.address,
                    "since": device.since,
                }))
                .collect::<Vec<_>>(),
        })
    }
}

impl SessionStore {
    /// The store the app runs on: last launch's key and last launch's devices,
    /// and everything written back as it changes.
    ///
    /// A key that cannot be read *or* written — no app directory, a read-only
    /// disk — falls all the way back to [`Default`], which is the old
    /// behaviour: a key for this process only, and nothing on disk. The device
    /// list is not loaded in that case, and deliberately so. Those rows would
    /// name devices whose cookies were signed under a key this process does not
    /// have, so every one of them would fail to verify while sitting on the
    /// card looking connected.
    pub fn persisted(app: &AppHandle) -> Self {
        let Some(secret) = remembered_secret(app) else {
            return Self::default();
        };

        let (devices, next) = restore(crate::settings::pairing(app));

        Self {
            inner: Mutex::new(Inner {
                secret,
                pairs: Vec::new(),
                devices,
                next,
            }),
            save: Some(app.clone()),
        }
    }

    /// Write the device list down, if this store has anywhere to write it.
    ///
    /// Takes the value rather than the lock. The caller has already let go of
    /// the mutex, because a file write is not something to hold one across.
    fn remember(&self, state: Value) {
        if let Some(app) = &self.save {
            crate::settings::set_pairing(app, state);
        }
    }

    /// Mint a nonce for a QR code that is about to go on screen.
    ///
    /// Every card that opens gets its own, and the ones already outstanding are
    /// left alone: closing and reopening the card while a phone is mid-scan
    /// should not invalidate the code the camera is looking at.
    pub fn mint_pair(&self) -> Pairing {
        let mut inner = self.inner.lock().unwrap();
        let now = SystemTime::now();
        inner.pairs.retain(|pair| pair.until > now);

        let pair = Pair {
            token: random(),
            code: code(),
            until: now + PAIR_TTL,
        };
        let minted = Pairing {
            token: pair.token.clone(),
            code: pair.code.clone(),
        };

        inner.pairs.push(pair);
        minted
    }

    /// Spend a nonce by the token the QR code carried. `true` when it was live,
    /// and it is gone either way.
    pub fn redeem_pair(&self, offered: &str) -> bool {
        self.redeem(|pair| pair.token.as_bytes().ct_eq(offered.as_bytes()))
    }

    /// Spend the same nonce by the six characters someone typed.
    ///
    /// Read the way the alphabet is written rather than the way the keyboard
    /// left it — see [`normalize`] — so a code entered in lower case, or with
    /// the space someone put in the middle to keep their place, is the code
    /// that was minted.
    pub fn redeem_code(&self, typed: &str) -> bool {
        let offered = normalize(typed);

        // Before the comparison, and not a leak: every code is this long, so
        // the only thing the early return tells the caller is something they
        // typed themselves.
        if offered.len() != CODE_LEN {
            return false;
        }

        self.redeem(|pair| pair.code.as_bytes().ct_eq(offered.as_bytes()))
    }

    /// Sweep the expired, take the one that matches, and say whether there was
    /// one.
    ///
    /// Every outstanding nonce is compared, rather than the scan stopping at
    /// the one that matched: the list is one or two entries long, and a scan
    /// that returns early is the thing this is trying not to be.
    fn redeem(&self, matches: impl Fn(&Pair) -> subtle::Choice) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let now = SystemTime::now();
        inner.pairs.retain(|pair| pair.until > now);

        let mut found = None;
        for (index, pair) in inner.pairs.iter().enumerate() {
            if bool::from(matches(pair)) {
                found = Some(index);
            }
        }

        match found {
            Some(index) => {
                inner.pairs.remove(index);
                true
            }
            None => false,
        }
    }

    /// Take a device in, and hand back the cookie value it proves itself with.
    pub fn authorize(&self, label: String, address: String) -> (Device, String) {
        let (device, cookie, state) = {
            let mut inner = self.inner.lock().unwrap();

            let id = format!("d{}", inner.next);
            inner.next += 1;

            let device = Device {
                id,
                label,
                address,
                since: epoch_secs(SystemTime::now()),
            };
            let cookie = sign(&inner.secret, &device.id, device.since);
            inner.devices.push(device.clone());
            (device, cookie, inner.stored())
        };

        self.remember(state);
        (device, cookie)
    }

    /// Whether a cookie value names a device that is still allowed in.
    ///
    /// Three ways to fail, all of them the same answer: the shape is wrong, the
    /// mac does not verify under the current secret, or it verifies and names a
    /// device that has been kicked or has aged out.
    pub fn verify(&self, offered: &str) -> Option<Device> {
        let inner = self.inner.lock().unwrap();
        let (id, issued) = check(&inner.secret, offered)?;

        let device = inner.devices.iter().find(|device| device.id == id)?;
        let age = epoch_secs(SystemTime::now()).saturating_sub(issued);
        (age <= SESSION_TTL.as_secs()).then(|| device.clone())
    }

    /// Who is on the card, newest last.
    pub fn devices(&self) -> Vec<Device> {
        self.inner.lock().unwrap().devices.clone()
    }

    /// Kick one device. Its cookie stops verifying on the next request.
    pub fn revoke(&self, id: &str) {
        let state = {
            let mut inner = self.inner.lock().unwrap();
            inner.devices.retain(|device| device.id != id);
            inner.stored()
        };

        self.remember(state);
    }

    /// Kick everything and change the locks.
    ///
    /// The rotation is what makes this different from revoking each device in
    /// turn: after it, a cookie signed by this process before the call cannot be
    /// verified even if a device id it names were somehow to come back. It is
    /// also what the desktop's "refresh the key" button is — the two are one
    /// action, because a key nothing is signed with is not a state worth being
    /// able to be in.
    /// The rotation goes to the disk too. A key left there while this process
    /// runs on a new one is a key the next launch would read back, and "change
    /// the locks" that a restart undoes is not what the button says.
    pub fn revoke_all(&self) {
        let (rotated, state) = {
            let mut inner = self.inner.lock().unwrap();
            inner.devices.clear();
            inner.pairs.clear();
            inner.secret = secret();
            (inner.secret, inner.stored())
        };

        if let Some(path) = self.save.as_ref().and_then(key_file) {
            // Best effort, and not a hole when it fails: the empty device list
            // written just below goes to the same directory, and a cookie
            // signed under the old key names a row that is no longer in it.
            let _ = store_secret(&path, &rotated);
        }

        self.remember(state);
    }
}

/// Where the signing key lives between launches.
///
/// Beside `desktop.json` in [`crate::dsh::app_dir`] rather than inside it. The
/// settings file is hand-editable by design, gets read and rewritten key by
/// key, and is the sort of thing a user pastes into a bug report; a key that
/// signs its way into a shell should not be sitting in the middle of it.
fn key_file(app: &AppHandle) -> Option<PathBuf> {
    Some(crate::dsh::app_dir(app)?.join("gateway.key"))
}

/// Last launch's key, or a fresh one written down for the next launch.
///
/// `None` only when there is nowhere to keep it, which is the caller's signal
/// to run the way this module used to — see [`SessionStore::persisted`].
fn remembered_secret(app: &AppHandle) -> Option<[u8; 32]> {
    let path = key_file(app)?;

    if let Some(key) = std::fs::read_to_string(&path)
        .ok()
        .as_deref()
        .and_then(parse_secret)
    {
        return Some(key);
    }

    // No file, or one holding something that is not a key. Either way what is
    // there cannot verify anything, so it is replaced rather than worked
    // around — and every device paired under whatever it used to hold is
    // already gone, which is the same place a first launch starts from.
    let fresh = secret();
    store_secret(&path, &fresh).ok()?;
    Some(fresh)
}

/// Exactly 32 bytes of base64, or nothing. A file holding the right number of
/// characters of the wrong thing still decodes, and that is fine: what comes
/// out is a key, and the only cookies it verifies are ones it signed.
fn parse_secret(text: &str) -> Option<[u8; 32]> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text.trim())
        .ok()?
        .try_into()
        .ok()
}

/// Write the key, readable by this user and nobody else.
///
/// The mode is set at creation, which is the only moment it can be set without
/// a window where the file exists and is world-readable. Windows has no mode to
/// set and does not need one: the directory this lands in is under the user's
/// own profile, which is user-only by the installer's doing and the system's.
fn store_secret(path: &Path, key: &[u8; 32]) -> std::io::Result<()> {
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

    std::io::Write::write_all(&mut options.open(path)?, b64(key).as_bytes())
}

/// The device list out of `desktop.json`, and the counter to carry on from.
///
/// Anything unreadable is no devices, the same forgiveness every other key in
/// that file gets: the cost of getting this wrong is a re-scan, and the cost of
/// refusing to start is the app.
fn restore(stored: Option<Value>) -> (Vec<Device>, u64) {
    let Some(stored) = stored else {
        return (Vec::new(), 1);
    };

    let devices: Vec<Device> = stored
        .get("devices")
        .and_then(Value::as_array)
        .map(|rows| rows.iter().filter_map(row).collect())
        .unwrap_or_default();

    // Never below one past the highest id on the list. The counter's whole job
    // is that an id is never handed out twice — see [`Inner::next`] — and a
    // file that lost the counter but kept the rows would otherwise start again
    // at `d1` and mint a second device holding a kicked device's name.
    let highest = devices
        .iter()
        .filter_map(|device| device.id.strip_prefix('d')?.parse::<u64>().ok())
        .max()
        .unwrap_or(0);

    let next = stored
        .get("next")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .max(highest + 1);

    (devices, next)
}

/// One row, or nothing. A row missing any field is dropped rather than filled
/// in: a device with no id is one nothing can kick.
fn row(stored: &Value) -> Option<Device> {
    Some(Device {
        id: stored.get("id")?.as_str()?.to_string(),
        label: stored.get("label")?.as_str()?.to_string(),
        address: stored.get("address")?.as_str()?.to_string(),
        since: stored.get("since")?.as_u64()?,
    })
}

/// `v1.<id>.<issued>.<mac>`.
///
/// The version prefix is there so that a later build can change what is signed
/// without a cookie from this one verifying under the new rule by accident.
fn sign(secret: &[u8; 32], id: &str, issued: u64) -> String {
    let payload = format!("v1.{id}.{issued}");
    let mac = b64(&hmac(secret, payload.as_bytes()));
    format!("{payload}.{mac}")
}

/// The id and the issuing time out of a cookie whose mac verifies.
fn check(secret: &[u8; 32], offered: &str) -> Option<(String, u64)> {
    let (payload, mac) = offered.rsplit_once('.')?;
    let expected = b64(&hmac(secret, payload.as_bytes()));

    // Before anything is parsed out of it: what follows reads fields, and a
    // field read out of an unverified string is a field an attacker wrote.
    if !bool::from(expected.as_bytes().ct_eq(mac.as_bytes())) {
        return None;
    }

    let mut parts = payload.split('.');
    if parts.next()? != "v1" {
        return None;
    }
    let id = parts.next()?.to_string();
    let issued = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((id, issued))
}

/// HMAC-SHA256, by hand.
///
/// RFC 2104 over `sha2`, which is already in the tree. The alternative is the
/// `hmac` crate and the ones behind it, for the dozen lines below — see the RFC
/// 4231 vectors in the tests, which are what makes writing them out an
/// acceptable thing to have done.
fn hmac(key: &[u8; 32], message: &[u8]) -> [u8; 32] {
    // 64 is SHA-256's block size. A 32-byte key is shorter than it, so the key
    // is padded rather than hashed down — the shortening branch of the RFC
    // cannot be reached from anything here, and is not written.
    let mut inner = [0x36u8; 64];
    let mut outer = [0x5cu8; 64];
    for index in 0..key.len() {
        inner[index] ^= key[index];
        outer[index] ^= key[index];
    }

    let mut hash = Sha256::new();
    hash.update(inner);
    hash.update(message);
    let first = hash.finalize();

    let mut hash = Sha256::new();
    hash.update(outer);
    hash.update(first);
    hash.finalize().into()
}

/// 16 bytes of the platform's randomness, base64url.
///
/// Not a UUID and not a counter: this is the one value in the pairing handshake
/// an attacker on the network has to guess, and 128 bits is the width at which
/// guessing stops being a strategy.
fn random() -> String {
    let mut bytes = [0u8; 16];
    fill(&mut bytes);
    b64(&bytes)
}

/// The alphabet the short code is written in.
///
/// Crockford's base32: the ten digits, and the twenty-two letters left once
/// `I`, `L`, `O` and `U` are taken out. The first three go because a person
/// reading them off a screen types `1`, `1` and `0`; `U` goes so that the
/// generator cannot spell anything at the user.
///
/// Thirty-two exactly, and that is what makes a byte fold into a character
/// without bias: 256 is a whole number of 32s, so `% 32` is uniform. An
/// alphabet of any other size would need rejection sampling, and the version of
/// this that quietly skips it is the version where some codes are likelier than
/// others.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// How many characters of it.
///
/// Six, which is 32^6 — near enough 1.07 billion. Small next to the token's
/// 2^128, and see the module docs for what stands behind it instead. Fewer
/// would be typeable and guessable; more stops being something a person reads
/// off a screen in one glance, which is the entire point of having it.
const CODE_LEN: usize = 6;

/// Six characters of the platform's randomness.
fn code() -> String {
    let mut bytes = [0u8; CODE_LEN];
    fill(&mut bytes);
    bytes
        .iter()
        .map(|byte| ALPHABET[(byte % 32) as usize] as char)
        .collect()
}

/// What someone typed, as the alphabet spells it.
///
/// Upper-cased, with `I` and `L` read as `1` and `O` as `0` — Crockford's own
/// rule, and the reason those letters are absent rather than merely avoided —
/// and with everything outside the alphabet dropped, so a code entered with the
/// hyphen or the space that made it readable still matches the one minted.
///
/// Dropping rather than refusing is deliberate. A stricter reader would refuse
/// codes that are *right*, and every one of those costs the user a trip back to
/// the computer to read the same six characters again.
fn normalize(typed: &str) -> String {
    typed
        .chars()
        .filter_map(|character| match character.to_ascii_uppercase() {
            'I' | 'L' => Some('1'),
            'O' => Some('0'),
            other if other.is_ascii() && ALPHABET.contains(&(other as u8)) => Some(other),
            _ => None,
        })
        .collect()
}

/// The cookie-signing secret. Same source, twice the width.
fn secret() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    fill(&mut bytes);
    bytes
}

/// Randomness, or nothing at all.
///
/// `getrandom` reads the OS generator, and its documented failure is a machine
/// without one — which on the three platforms this ships to does not happen. If
/// it ever did, carrying on with a buffer of zeroes would mean a pairing nonce
/// an attacker can type and a cookie secret they can compute, so this is the one
/// place in the app that would rather stop.
fn fill(bytes: &mut [u8]) {
    getrandom::fill(bytes).expect("the operating system has no randomness to give");
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn epoch_secs(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 4231's first two vectors, which are the whole reason [`hmac`] is
    /// allowed to be written by hand. Both use a short key, so both run through
    /// the padding branch — the one the cookie takes.
    #[test]
    fn hmac_matches_the_published_vectors() {
        let mut key = [0u8; 32];
        key[..20].copy_from_slice(&[0x0b; 20]);
        assert_eq!(
            hex(&hmac(&key, b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );

        let mut key = [0u8; 32];
        key[..4].copy_from_slice(b"Jefe");
        assert_eq!(
            hex(&hmac(&key, b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn a_cookie_it_signed_is_one_it_takes_back() {
        let store = SessionStore::default();
        let (device, cookie) = store.authorize("iPhone".into(), "192.168.1.9".into());

        assert_eq!(store.verify(&cookie), Some(device));
    }

    /// Every way a forged cookie is forged: the mac changed, the payload changed
    /// under a mac that was right for the old one, and shapes that are not a
    /// cookie at all.
    #[test]
    fn nothing_else_verifies() {
        let store = SessionStore::default();
        let (_, cookie) = store.authorize("iPhone".into(), "192.168.1.9".into());
        let (payload, mac) = cookie.rsplit_once('.').unwrap();

        // One character, changed to a different one whatever it was. A
        // `replace('A', "B")` here passed or failed depending on whether that
        // run's random secret happened to produce a mac with an `A` in it.
        let mut flipped = mac.to_string();
        flipped.replace_range(0..1, if mac.starts_with('A') { "B" } else { "A" });

        for forged in [
            String::new(),
            "v1.d1.0.".to_string(),
            "v1.d1.0".to_string(),
            format!("{payload}.{flipped}"),
            format!("v1.d2.0.{mac}"),
            format!("{payload}.{payload}.{mac}"),
        ] {
            assert_eq!(store.verify(&forged), None, "{forged} must not verify");
        }
    }

    /// A kicked device's cookie still carries a valid signature. What stops it
    /// is that the row it names is gone.
    #[test]
    fn a_kicked_device_stops_getting_in() {
        let store = SessionStore::default();
        let (device, cookie) = store.authorize("iPhone".into(), "192.168.1.9".into());

        store.revoke(&device.id);
        assert_eq!(store.verify(&cookie), None);
    }

    /// And after a rotation, so does one whose row would have been there: the
    /// ids do not restart, so this is about the secret rather than the list.
    #[test]
    fn rotating_the_secret_invalidates_everything() {
        let store = SessionStore::default();
        let (_, cookie) = store.authorize("iPhone".into(), "192.168.1.9".into());

        store.revoke_all();
        let (_, fresh) = store.authorize("iPhone".into(), "192.168.1.9".into());

        assert_eq!(store.verify(&cookie), None, "the old cookie is scrap");
        assert!(store.verify(&fresh).is_some(), "and the new one is not");
    }

    /// Two devices that paired separately are two rows, and kicking one leaves
    /// the other alone. The ids have to differ for that, which is what the
    /// counter is for.
    #[test]
    fn devices_are_kicked_one_at_a_time() {
        let store = SessionStore::default();
        let (phone, phone_cookie) = store.authorize("iPhone".into(), "192.168.1.9".into());
        let (pad, pad_cookie) = store.authorize("iPad".into(), "192.168.1.10".into());
        assert_ne!(phone.id, pad.id);

        store.revoke(&phone.id);
        assert_eq!(store.verify(&phone_cookie), None);
        assert!(store.verify(&pad_cookie).is_some());
        assert_eq!(store.devices(), vec![pad]);
    }

    /// Redeemed once, and gone — including for the request that arrives a moment
    /// behind the one that spent it.
    #[test]
    fn a_pairing_nonce_is_spent_by_the_first_taker() {
        let store = SessionStore::default();
        let minted = store.mint_pair();

        assert!(store.redeem_pair(&minted.token));
        assert!(!store.redeem_pair(&minted.token), "it was spent");
    }

    #[test]
    fn a_nonce_nobody_minted_is_worth_nothing() {
        let store = SessionStore::default();
        store.mint_pair();

        assert!(!store.redeem_pair(""));
        assert!(!store.redeem_pair("guessed"));
    }

    /// Two cards open at once, and the code the camera is pointed at is the
    /// older one. Minting must not invalidate what is already on a screen.
    #[test]
    fn an_outstanding_nonce_survives_the_next_one() {
        let store = SessionStore::default();
        let first = store.mint_pair();
        let second = store.mint_pair();

        assert!(store.redeem_pair(&first.token));
        assert!(store.redeem_pair(&second.token));
    }

    /// Nonces are 128 bits from the OS. Two in a row being equal is the failure
    /// that would make the whole handshake decorative.
    #[test]
    fn nonces_do_not_repeat() {
        let store = SessionStore::default();
        assert_ne!(store.mint_pair().token, store.mint_pair().token);
    }

    /// Either spelling opens it, and either spelling closes it: they are one
    /// nonce, not two.
    #[test]
    fn the_code_and_the_token_are_the_same_nonce() {
        let store = SessionStore::default();
        let minted = store.mint_pair();

        assert!(store.redeem_code(&minted.code));
        assert!(
            !store.redeem_pair(&minted.token),
            "spending the code spent the token with it"
        );

        let other = store.mint_pair();
        assert!(store.redeem_pair(&other.token));
        assert!(!store.redeem_code(&other.code), "and the other way round");
    }

    /// Crockford's alphabet, and the four letters it leaves out. Thirty-two
    /// exactly is what makes `% 32` unbiased — an alphabet that drifted to
    /// thirty-three would make some codes likelier than others, silently.
    #[test]
    fn the_alphabet_is_thirty_two_unmistakable_characters() {
        assert_eq!(ALPHABET.len(), 32);

        let mut seen: Vec<u8> = ALPHABET.to_vec();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 32, "no character appears twice");

        for absent in [b'I', b'L', b'O', b'U'] {
            assert!(
                !ALPHABET.contains(&absent),
                "{} is read back as something else",
                absent as char
            );
        }
    }

    /// Minted codes are the right width and drawn from that alphabet.
    #[test]
    fn a_minted_code_is_six_characters_of_it() {
        for _ in 0..64 {
            let minted = code();
            assert_eq!(minted.chars().count(), CODE_LEN);
            assert!(minted.bytes().all(|byte| ALPHABET.contains(&byte)), "{minted}");
        }
    }

    /// What a person types, read as what the card printed. Every line here is a
    /// user who would otherwise have walked back to the computer for nothing.
    #[test]
    fn the_code_is_read_the_way_it_is_typed() {
        assert_eq!(normalize("ab3k9z"), "AB3K9Z", "lower case");
        assert_eq!(normalize("AB3 K9Z"), "AB3K9Z", "a space to keep their place");
        assert_eq!(normalize("AB3-K9Z"), "AB3K9Z", "a hyphen, for the same reason");
        assert_eq!(normalize(" AB3K9Z\n"), "AB3K9Z", "what a paste drags along");
        // Crockford's own rule, and the reason those letters are absent.
        assert_eq!(normalize("IL0"), "110");
        assert_eq!(normalize("OoIi"), "0011");
        // Non-ASCII must not be truncated into the alphabet on the way past.
        assert_eq!(normalize("AB3K9Z：好"), "AB3K9Z");
    }

    /// A code of the wrong length never reaches the comparison, and `U` is not
    /// quietly read as anything — it is simply not in the alphabet.
    #[test]
    fn a_code_that_is_not_one_redeems_nothing() {
        let store = SessionStore::default();
        store.mint_pair();

        assert!(!store.redeem_code(""));
        assert!(!store.redeem_code("ABC"));
        assert!(!store.redeem_code("ABCDEFGH"));
        assert!(!store.redeem_code("UUUUUU"), "U is dropped, not folded");
    }

    /// What a launch writes is what the next launch reads. The ids matter most:
    /// they are what a cookie names, so a list that comes back under different
    /// ones is a list every paired phone has fallen off.
    #[test]
    fn the_device_list_survives_the_round_trip() {
        let store = SessionStore::default();
        store.authorize("iPhone".into(), "192.168.1.9".into());
        store.authorize("Pixel".into(), "192.168.1.10".into());

        let written = store.inner.lock().unwrap().stored();
        let (devices, next) = restore(Some(written));

        assert_eq!(devices, store.devices());
        assert_eq!(next, 3, "and the counter carries on rather than restarting");
    }

    /// A cookie signed before a restart verifies after one, which is the whole
    /// point. Rebuilt by hand because a real [`SessionStore::persisted`] needs
    /// an `AppHandle` a test cannot build.
    #[test]
    fn a_cookie_signed_before_a_restart_still_verifies_after_it() {
        let before = SessionStore::default();
        let (_, cookie) = before.authorize("iPhone".into(), "192.168.1.9".into());

        let (secret, written) = {
            let inner = before.inner.lock().unwrap();
            (inner.secret, inner.stored())
        };

        let (devices, next) = restore(Some(written));
        let after = SessionStore {
            inner: Mutex::new(Inner {
                secret,
                pairs: Vec::new(),
                devices,
                next,
            }),
            save: None,
        };

        assert!(after.verify(&cookie).is_some());
    }

    /// Every way the stored list can be unusable ends at no devices — the same
    /// forgiveness `desktop.json`'s other keys get, and the same cost: a
    /// re-scan.
    #[test]
    fn an_unreadable_device_list_is_no_devices() {
        assert_eq!(restore(None), (Vec::new(), 1));
        assert_eq!(restore(Some(serde_json::json!({}))), (Vec::new(), 1));
        assert_eq!(restore(Some(serde_json::json!("nonsense"))), (Vec::new(), 1));
        assert_eq!(
            restore(Some(serde_json::json!({"devices": "not a list"}))),
            (Vec::new(), 1)
        );
    }

    /// A row missing a field is dropped, not filled in. The rest of the list is
    /// still good, and throwing all of it away would cost the user every phone
    /// over one bad line.
    #[test]
    fn a_row_that_is_missing_something_is_dropped_alone() {
        let (devices, _) = restore(Some(serde_json::json!({
            "next": 3,
            "devices": [
                {"id": "d1", "label": "iPhone", "address": "192.168.1.9", "since": 1},
                {"id": "d2", "label": "Pixel", "since": 2},
            ],
        })));

        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, "d1");
    }

    /// The counter is never below one past the highest id on the list. A file
    /// that kept the rows but lost the counter would otherwise mint a second
    /// device under a name a cookie already carries — see [`Inner::next`].
    #[test]
    fn the_counter_never_hands_out_an_id_twice() {
        let rows = serde_json::json!({
            "devices": [
                {"id": "d7", "label": "iPhone", "address": "192.168.1.9", "since": 1},
            ],
        });

        assert_eq!(restore(Some(rows.clone())).1, 8, "no counter at all");

        let mut behind = rows;
        behind["next"] = serde_json::json!(2);
        assert_eq!(restore(Some(behind)).1, 8, "a counter that fell behind");
    }

    /// The key is written the way it is read. A file this build cannot parse is
    /// a file every paired device has been kicked by, so it must not happen by
    /// accident.
    #[test]
    fn the_key_file_round_trips() {
        let key = secret();
        assert_eq!(parse_secret(&b64(&key)), Some(key));
        // As it comes back off a disk that ended the line.
        assert_eq!(parse_secret(&format!("{}\n", b64(&key))), Some(key));
    }

    /// And anything else reads as no key, which mints a fresh one.
    #[test]
    fn a_key_file_holding_something_else_is_not_a_key() {
        assert_eq!(parse_secret(""), None);
        assert_eq!(parse_secret("not base64 at all !!"), None);
        // Valid base64, wrong width.
        assert_eq!(parse_secret(&b64(&[0u8; 16])), None);
        assert_eq!(parse_secret(&b64(&[0u8; 64])), None);
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}
