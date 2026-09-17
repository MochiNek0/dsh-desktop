//! Where the stylesheet comes from when it is not the built-in one.
//!
//! The patch in `plugin/lib/index.js` describes dsh's markup, so it goes stale
//! on dsh's release schedule rather than on this app's. Shipping the only copy
//! inside the binary means every fix for a newer dsh costs a release, an
//! installer, and a download from everyone who wants it — for a stylesheet.
//! This is the way out: a signed stylesheet published next to the app's own
//! updates, fetched here, written to the file the plugin already prefers over
//! its built-in copy. See [`super::style::sheet`].
//!
//! ## Why the desktop fetches it and not the phone
//!
//! The obvious shape is a `<link>` in the page, and it is the wrong one.
//!
//! Only this side knows which dsh is running. The phone is handed a page by
//! whatever dsh the user installed — `npm install -g @deepseek-ai/dsh`, no
//! version pinned anywhere in this repo — so a stylesheet the phone fetches by
//! itself has to be one file that fits every dsh in the field at once. This
//! side has [`crate::dsh::current`] and can ask for the patch that matches.
//!
//! The other two follow from where the fetch happens rather than from what it
//! fetches. A phone-side link is a render-blocking dependency on the internet
//! for a feature whose whole point is a phone and a PC on the same Wi-Fi, which
//! needs no internet at all; and it would put a request on the user's phone,
//! from the user's IP, every time they open the page. Fetched here and cached
//! as a file, the phone never leaves the LAN and the request happens once a
//! launch.
//!
//! ## Why it is signed
//!
//! The stylesheet lands in a page that contains dsh's permission prompts, and
//! CSS is enough to move, hide or cover a control — an invisible box over the
//! refuse button is a tap that lands on approve. So the file is a capability,
//! not decoration, and an unsigned one would be the weakest link in a product
//! that already pays for signed updates: whoever took the host could not push
//! an app update, but could push this. It is verified with [`PUBKEY`], which is
//! the key `tauri.conf.json` already trusts for updates —
//! `the_key_is_the_one_that_signs_releases` holds the two together — so nothing
//! reaches the page on the word of the host alone.

use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use serde_json::Value;
use tauri::{AppHandle, Manager};

/// Where the list of published stylesheets lives.
///
/// The app's own updater already points at this host (`updater.endpoints` in
/// `tauri.conf.json`), so this adds a path to a host the app depends on rather
/// than a second one to keep alive.
const MANIFEST: &str = "https://dsh-desktop.cc.cd/mobile-css/patches.json";

/// The minisign key every published stylesheet is signed with, base64 as
/// `tauri.conf.json` writes it.
const PUBKEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDhCOTkyNUJDNjA1OTBFQ0QKUldUTkRsbGd2Q1daaTJNUnBUTmN0d291b1A5SGlpUG05My81bXdkOGlCU2o0Y0Y1N1pUdFBQUkIK";

/// A stylesheet for one dialog. Anything near this is a mistake somewhere, and
/// the check is here so that a mistake costs a skipped update rather than the
/// memory it asks for.
const LIMIT: usize = 256 * 1024;

/// Long enough for a slow phone-tethered connection, short enough that a host
/// that has gone away does not leave a thread waiting on it all session.
const PATIENCE: Duration = Duration::from_secs(10);

/// What one round of this actually did, for deciding whether to say anything.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// A new stylesheet is on disk. The phone picks it up on its next load.
    Wrote,
    /// What is published is what is already here.
    Same,
    /// The manifest has nothing for this dsh. Not a failure: it is how a dsh
    /// too new to have been looked at yet is left alone rather than given a
    /// patch written for a different one.
    Nothing,
}

/// Fetch the published stylesheet for the dsh that is running, if the patch is
/// switched on at all.
///
/// Returns immediately; the work is a task. `loud` is whether the user is
/// waiting on an answer — they just ticked the box — in which case a failure
/// goes on the card. A failure at launch says nothing: they did not ask, the
/// old stylesheet still works, and the one thing worse than a stale patch is a
/// window that greets you with a network error.
pub fn refresh(app: &AppHandle, loud: bool) {
    // No network at all unless the patch is wanted. Someone who never turns it
    // on never talks to the host.
    if !super::style::enabled() {
        return;
    }

    let dsh = crate::dsh::current(app).map(|install| install.version.to_string());
    let sheet = super::style::sheet();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let told = fetch(MANIFEST, dsh.as_deref(), &sheet).await;
        let Some(remote) = app.try_state::<super::Remote>() else {
            return;
        };

        let hint = match told {
            // Worth a line either way: the phone has to be reloaded before any
            // of this is visible, and nothing else would say so.
            Ok(Outcome::Wrote) => Some(
                t!(
                    "样式补丁已更新，刷新手机页面生效。",
                    "Stylesheet patch updated. Reload the page on the phone."
                )
                .to_string(),
            ),
            Ok(_) => None,
            Err(why) if loud => Some(why),
            Err(_) => None,
        };

        if hint.is_some() {
            super::redraw(&app, &remote, hint);
        }
    });
}

/// One round, start to finish, with no handle to anything.
///
/// Takes the two things it would otherwise reach for, so that the whole path —
/// fetch, parse, pick, verify, write — can be driven against a local server by
/// `a_stylesheet_the_key_did_not_sign_changes_nothing`.
async fn fetch(manifest_at: &str, dsh: Option<&str>, sheet: &Path) -> Result<Outcome, String> {
    // reqwest is here on `rustls-no-provider`, which is what the updater asked
    // for, and on that feature building a client *panics* unless the process
    // has a provider installed. The updater installs one before it makes its
    // own client, so in a running app this would usually be someone else's
    // doing — usually, and only in the order those two happen to run in. It is
    // installed here instead, where it is needed. `Err` is "already installed",
    // which is the common case and not a problem.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let client = reqwest::Client::builder()
        .timeout(PATIENCE)
        .build()
        .map_err(|error| trouble(error.to_string()))?;

    // A manifest that is not there is not a failure, it is an absent offer:
    // nothing has been published for anyone yet, which is true of every launch
    // before the first stylesheet goes up and would otherwise greet the first
    // person to tick the box with a 404. Every other status is a real answer
    // from a host that exists, and says so.
    let Some(manifest) = get(&client, manifest_at).await? else {
        return Ok(Outcome::Nothing);
    };
    let manifest: Value = serde_json::from_slice(&manifest)
        .map_err(|error| trouble(format!("{manifest_at}: {error}")))?;

    let Some((url, signature)) = pick(&manifest, dsh) else {
        return Ok(Outcome::Nothing);
    };

    // A stylesheet the manifest names and the host does not have is a broken
    // publish rather than an absent one, so this 404 is an error.
    let css = get(&client, &url)
        .await?
        .ok_or_else(|| trouble(format!("{url}: not there")))?;
    verify(&css, &signature)?;

    write(sheet, &css)
}

/// One GET, with the size cap applied before the body is in memory where the
/// host is honest about it, and after where it is not.
///
/// `None` is a 404 — nothing published at that name — which the caller reads as
/// an absent offer rather than as a failure.
async fn get(client: &reqwest::Client, url: &str) -> Result<Option<Vec<u8>>, String> {
    let answer = client
        .get(url)
        .send()
        .await
        .map_err(|error| trouble(error.to_string()))?;

    if answer.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }

    let answer = answer
        .error_for_status()
        .map_err(|error| trouble(error.to_string()))?;

    if answer.content_length().is_some_and(|size| size > LIMIT as u64) {
        return Err(trouble(format!("{url}: too big")));
    }

    let body = answer
        .bytes()
        .await
        .map_err(|error| trouble(error.to_string()))?;

    if body.len() > LIMIT {
        return Err(trouble(format!("{url}: too big")));
    }
    Ok(Some(body.to_vec()))
}

/// The entry for a given dsh: the exact version if the manifest names it, else
/// whatever is published as the current one.
///
/// Exact names and a default rather than version ranges, though `semver` is
/// already here, because dsh ships prereleases — `0.1.5-rc.1` — and a
/// `VersionReq` does not match a prerelease unless the comparator carries one
/// of its own. `>=0.1.5` and even `*` both quietly fail to match every rc user
/// there is, which is a footgun aimed at the people publishing the manifest.
/// Naming the version that needs its own stylesheet cannot go quiet that way.
fn pick(manifest: &Value, dsh: Option<&str>) -> Option<(String, String)> {
    let named = dsh
        .and_then(|version| manifest.get("byDsh")?.get(version))
        .or_else(|| manifest.get("default"))?;

    let url = named.get("css")?.as_str()?;
    let signature = named.get("signature")?.as_str()?;
    Some((url.to_string(), signature.to_string()))
}

/// Check a stylesheet against [`PUBKEY`].
///
/// Both strings are base64 of a minisign file, which is the shape
/// `tauri.conf.json` and `latest.json` already use, so publishing one of these
/// is `tauri signer sign` and a copy-paste — the same two steps as publishing a
/// release.
fn verify(css: &[u8], signature: &str) -> Result<(), String> {
    let plain = |what: &str, text: &str| {
        base64::engine::general_purpose::STANDARD
            .decode(text)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .ok_or_else(|| trouble(format!("{what} is not base64")))
    };

    let key = minisign_verify::PublicKey::decode(&plain("the key", PUBKEY)?)
        .map_err(|error| trouble(error.to_string()))?;
    let signature =
        minisign_verify::Signature::decode(&plain("the signature", signature)?)
            .map_err(|error| trouble(error.to_string()))?;

    key.verify(css, &signature, false).map_err(|_| {
        t!(
            "样式补丁的签名不对，已忽略。",
            "the stylesheet patch is not correctly signed; ignored"
        )
        .to_string()
    })
}

/// Put it where the plugin looks, or leave what is there alone.
///
/// Through a sibling and a rename, so that a write interrupted halfway cannot
/// leave the plugin reading half a stylesheet: the plugin stats this file on
/// every index render, which is to say it can read it in the middle of this.
fn write(path: &Path, css: &[u8]) -> Result<Outcome, String> {
    if std::fs::read(path).is_ok_and(|already| already == css) {
        return Ok(Outcome::Same);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| trouble(error.to_string()))?;
    }

    let sibling = path.with_extension("css.new");
    std::fs::write(&sibling, css).map_err(|error| trouble(error.to_string()))?;
    std::fs::rename(&sibling, path).map_err(|error| {
        let _ = std::fs::remove_file(&sibling);
        trouble(error.to_string())
    })?;
    Ok(Outcome::Wrote)
}

/// One sentence for the card, with the detail after it.
///
/// The detail is there because the alternative — a tidy "update failed" — is
/// the kind of message that costs a round trip to find out whether the host is
/// down, the file is malformed, or the machine is offline.
fn trouble(detail: String) -> String {
    t!(
        "没能更新样式补丁：{}",
        "could not update the stylesheet patch: {}",
        detail
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stylesheet and a signature over it, made with a throwaway key by
    /// `tauri signer`, which is the tool that signs the real ones. Fixtures
    /// rather than a key generated here because this crate can only verify:
    /// the point is to exercise the decoding path the real thing uses.
    const TEST_PUBKEY: &str = include_str!("../../tests/patch-key.pub.b64");
    const TEST_SIGNATURE: &str = include_str!("../../tests/patch-sheet.sig.b64");
    const TEST_CSS: &[u8] = include_bytes!("../../tests/patch-sheet.css");

    fn verify_with(key: &str, css: &[u8], signature: &str) -> Result<(), String> {
        let plain = |text: &str| {
            String::from_utf8(
                base64::engine::general_purpose::STANDARD
                    .decode(text.trim())
                    .unwrap(),
            )
            .unwrap()
        };
        let key = minisign_verify::PublicKey::decode(&plain(key)).unwrap();
        let signature = minisign_verify::Signature::decode(&plain(signature)).unwrap();
        key.verify(css, &signature, false).map_err(|e| e.to_string())
    }

    #[test]
    fn a_stylesheet_signed_with_the_key_verifies() {
        assert!(verify_with(TEST_PUBKEY, TEST_CSS, TEST_SIGNATURE).is_ok());
    }

    #[test]
    fn one_changed_byte_does_not() {
        let mut tampered = TEST_CSS.to_vec();
        // The kind of edit the signature is here to catch: still valid CSS.
        tampered.extend_from_slice(b"\n.x{position:fixed;inset:0}\n");
        assert!(verify_with(TEST_PUBKEY, &tampered, TEST_SIGNATURE).is_err());
    }

    #[test]
    fn a_signature_by_another_key_does_not() {
        // The shipped key against a sheet signed by the test key: the right
        // shape, the wrong signer, which is what a taken host would produce.
        assert!(verify(TEST_CSS, TEST_SIGNATURE).is_err());
    }

    #[test]
    fn nothing_that_is_not_base64_gets_as_far_as_the_key() {
        let why = verify(TEST_CSS, "not base64 at all!!").unwrap_err();
        assert!(why.contains("base64"), "{why}");
    }

    #[test]
    fn the_key_is_the_one_that_signs_releases() {
        // Drift here would mean stylesheets signed with a key the app no longer
        // trusts, which looks exactly like an attack and is not one.
        let conf = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tauri.conf.json"
        ))
        .unwrap();
        let conf: Value = serde_json::from_str(&conf).unwrap();
        let updater = conf["plugins"]["updater"]["pubkey"].as_str().unwrap();
        assert_eq!(updater.trim(), PUBKEY);
    }

    #[test]
    fn the_exact_dsh_wins_over_the_default() {
        let manifest: Value = serde_json::from_str(
            r#"{
              "default": { "css": "new.css", "signature": "s2" },
              "byDsh": { "0.1.5-rc.1": { "css": "old.css", "signature": "s1" } }
            }"#,
        )
        .unwrap();

        assert_eq!(
            pick(&manifest, Some("0.1.5-rc.1")),
            Some(("old.css".into(), "s1".into()))
        );
        // A prerelease the manifest does not name: the default, where a
        // `VersionReq` of `*` would have matched nothing at all.
        assert_eq!(
            pick(&manifest, Some("0.1.6-rc.3")),
            Some(("new.css".into(), "s2".into()))
        );
        assert_eq!(
            pick(&manifest, None),
            Some(("new.css".into(), "s2".into()))
        );
    }

    #[test]
    fn a_manifest_with_nothing_to_offer_asks_for_nothing() {
        let empty: Value = serde_json::from_str("{}").unwrap();
        assert_eq!(pick(&empty, Some("0.1.5-rc.1")), None);

        // Named, but half-written: not a stylesheet, so not a download.
        let partial: Value =
            serde_json::from_str(r#"{ "default": { "css": "a.css" } }"#).unwrap();
        assert_eq!(pick(&partial, None), None);
    }

    /// A directory of this test's own; the real one is under `$DSH_HOME`.
    fn elsewhere(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dsh-desktop-patch-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn the_same_stylesheet_twice_is_written_once() {
        let path = elsewhere("same").join("mobile.css");

        assert_eq!(write(&path, b"a{}").unwrap(), Outcome::Wrote);
        assert_eq!(std::fs::read(&path).unwrap(), b"a{}");
        assert_eq!(write(&path, b"a{}").unwrap(), Outcome::Same);
        assert_eq!(write(&path, b"b{}").unwrap(), Outcome::Wrote);
        assert_eq!(std::fs::read(&path).unwrap(), b"b{}");

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn the_sibling_it_writes_through_is_not_left_behind() {
        let path = elsewhere("sibling").join("mobile.css");
        write(&path, b"a{}").unwrap();

        let strays: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
            .filter(|name| name != "mobile.css")
            .collect();
        assert!(strays.is_empty(), "{strays:?}");

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A host serving a manifest and whatever it points at, on loopback.
    ///
    /// The manifest has to name the address it is served from, which is only
    /// known once the port is bound, so `{at}` in the template is filled in
    /// here rather than by the caller.
    async fn host(template: &str, css: &'static [u8]) -> std::net::SocketAddr {
        use bytes::Bytes;
        use http::{Request, Response, StatusCode};
        use http_body_util::Full;
        use hyper::body::Incoming;
        use hyper::service::service_fn;
        use hyper_util::rt::TokioIo;

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("a free loopback port");
        let at = listener.local_addr().expect("a bound listener");
        let manifest = std::sync::Arc::new(template.replace("{at}", &at.to_string()));

        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let manifest = manifest.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |request: Request<Incoming>| {
                        let manifest = manifest.clone();
                        async move {
                            let body = match request.uri().path() {
                                "/patches.json" => Bytes::from(manifest.to_string()),
                                "/sheet.css" => Bytes::from_static(css),
                                _ => {
                                    return Ok::<_, std::convert::Infallible>(
                                        Response::builder()
                                            .status(StatusCode::NOT_FOUND)
                                            .body(Full::new(Bytes::new()))
                                            .unwrap(),
                                    )
                                }
                            };
                            Ok(Response::new(Full::new(body)))
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(socket), service)
                        .await;
                });
            }
        });
        at
    }

    #[tokio::test]
    async fn a_stylesheet_the_key_did_not_sign_changes_nothing() {
        // The fixture is signed, correctly, by a key this app does not trust —
        // which is what a taken host would serve, and the one case where being
        // wrong means CSS of someone else's choosing over dsh's permission
        // prompts.
        let at = host(
            &format!(
                r#"{{ "default": {{ "css": "http://{{at}}/sheet.css", "signature": {sig} }} }}"#,
                sig = serde_json::to_string(TEST_SIGNATURE.trim()).unwrap()
            ),
            TEST_CSS,
        )
        .await;

        let sheet = elsewhere("unsigned").join("mobile.css");
        let told = fetch(
            &format!("http://{at}/patches.json"),
            Some("0.1.5-rc.1"),
            &sheet,
        )
        .await;

        assert!(told.is_err(), "{told:?}");
        assert!(!sheet.exists(), "a sheet it could not verify was written");
        let _ = std::fs::remove_dir_all(sheet.parent().unwrap());
    }

    #[tokio::test]
    async fn a_host_with_no_manifest_yet_is_not_an_error() {
        // Every launch before the first stylesheet is published looks like
        // this, and the person who ticked the box should not be told off for it.
        let at = host("{}", TEST_CSS).await;
        let sheet = elsewhere("unpublished").join("mobile.css");

        let told = fetch(&format!("http://{at}/nothing-here.json"), None, &sheet).await;

        assert_eq!(told.unwrap(), Outcome::Nothing);
        assert!(!sheet.exists());
        let _ = std::fs::remove_dir_all(sheet.parent().unwrap());
    }

    #[tokio::test]
    async fn a_manifest_naming_a_stylesheet_that_is_not_there_is_an_error() {
        // The other half of the same 404: the host exists and the manifest is
        // wrong, which is worth saying out loud rather than passing over.
        let at = host(
            r#"{ "default": { "css": "http://{at}/gone.css", "signature": "x" } }"#,
            TEST_CSS,
        )
        .await;
        let sheet = elsewhere("broken-publish").join("mobile.css");

        let told = fetch(&format!("http://{at}/patches.json"), None, &sheet).await;

        assert!(told.is_err(), "{told:?}");
        assert!(!sheet.exists());
        let _ = std::fs::remove_dir_all(sheet.parent().unwrap());
    }

    #[tokio::test]
    async fn a_manifest_with_nothing_in_it_asks_for_no_stylesheet() {
        let at = host("{}", TEST_CSS).await;
        let sheet = elsewhere("nothing").join("mobile.css");

        let told = fetch(
            &format!("http://{at}/patches.json"),
            Some("0.1.5-rc.1"),
            &sheet,
        )
        .await;

        assert_eq!(told.unwrap(), Outcome::Nothing);
        assert!(!sheet.exists());
        let _ = std::fs::remove_dir_all(sheet.parent().unwrap());
    }

    #[test]
    fn the_names_are_the_ones_the_plugin_looks_for() {
        // `override()` in plugin/lib/index.js. A disagreement is silent: the
        // app would write a stylesheet nobody reads.
        let plugin = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../plugin/lib/index.js"
        ))
        .unwrap();
        assert!(plugin.contains("'mobile.css'"), "the plugin moved its name");
        assert!(super::super::style::sheet().ends_with("mobile.css"));
    }
}
