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
//! ## The device cookie
//!
//! `dsh_mobile_session=v1.<id>.<issued>.<mac>`, where the mac is HMAC-SHA256
//! over everything before it under a secret minted at startup and never written
//! anywhere. The id names a row in [`Inner::devices`], so a device that has been
//! kicked fails on the row rather than on the signature, and "disconnect
//! everything" is one call that empties the list *and* rotates the secret —
//! after which every cookie this process ever signed is scrap.
//!
//! The mac is compared with `subtle` rather than `==`. The comparison is over a
//! value the caller controls, against a secret they are trying to guess, which
//! is the exact shape a timing oracle is built out of.
//!
//! ## What is deliberately not in the cookie
//!
//! The address the device connected from. Phase 1 could bind to it — the phone
//! is on the same Wi-Fi and its lease is not going to move mid-session — but
//! Phase 2 puts the same session behind a tunnel, where the address the gateway
//! sees is the tunnel's and changes without the device having gone anywhere.
//! Binding to it now would be a check that quietly stops meaning anything
//! later, which is worse than not having written it.

use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

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

/// The gateway's whole notion of who is allowed in.
#[derive(Default)]
pub struct SessionStore {
    inner: Mutex<Inner>,
}

struct Inner {
    /// What device cookies are signed with. Rotated by
    /// [`SessionStore::revoke_all`].
    secret: [u8; 32],
    /// Outstanding pairing nonces, with the moment each stops being redeemable.
    pairs: Vec<(String, SystemTime)>,
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

impl SessionStore {
    /// Mint a nonce for a QR code that is about to go on screen.
    ///
    /// Every card that opens gets its own, and the ones already outstanding are
    /// left alone: closing and reopening the card while a phone is mid-scan
    /// should not invalidate the code the camera is looking at.
    pub fn mint_pair(&self) -> String {
        let mut inner = self.inner.lock().unwrap();
        let now = SystemTime::now();
        inner.pairs.retain(|(_, until)| *until > now);

        let token = random();
        inner.pairs.push((token.clone(), now + PAIR_TTL));
        token
    }

    /// Spend a nonce. `true` when it was live, and it is gone either way.
    ///
    /// Every outstanding nonce is compared, rather than the scan stopping at
    /// the one that matched: the list is one or two entries long, and a scan
    /// that returns early is the thing this is trying not to be.
    pub fn redeem_pair(&self, offered: &str) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let now = SystemTime::now();
        inner.pairs.retain(|(_, until)| *until > now);

        let mut found = None;
        for (index, (token, _)) in inner.pairs.iter().enumerate() {
            if bool::from(token.as_bytes().ct_eq(offered.as_bytes())) {
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
        self.inner
            .lock()
            .unwrap()
            .devices
            .retain(|device| device.id != id);
    }

    /// Kick everything and change the locks.
    ///
    /// The rotation is what makes this different from revoking each device in
    /// turn: after it, a cookie signed by this process before the call cannot be
    /// verified even if a device id it names were somehow to come back. It is
    /// also what the desktop's "refresh the key" button is — the two are one
    /// action, because a key nothing is signed with is not a state worth being
    /// able to be in.
    pub fn revoke_all(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.devices.clear();
        inner.pairs.clear();
        inner.secret = secret();
    }
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
        let token = store.mint_pair();

        assert!(store.redeem_pair(&token));
        assert!(!store.redeem_pair(&token), "it was spent");
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

        assert!(store.redeem_pair(&first));
        assert!(store.redeem_pair(&second));
    }

    /// Nonces are 128 bits from the OS. Two in a row being equal is the failure
    /// that would make the whole handshake decorative.
    #[test]
    fn nonces_do_not_repeat() {
        let store = SessionStore::default();
        assert_ne!(store.mint_pair(), store.mint_pair());
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}
