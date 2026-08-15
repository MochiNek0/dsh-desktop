//! The dsh installations the app manages, and how they get replaced.
//!
//! Two of them exist. The one in the bundle came with the installer: always
//! present, always works, never changes. The one in app data was downloaded
//! later because npm had something newer. Whichever has the higher version is
//! the one that runs — so a fresh install works offline, and an app that has
//! been running for a while tracks the registry.
//!
//! Nothing here is load-bearing: every failure leaves the bundled copy in
//! place, which is a working dsh by construction.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use semver::Version;
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The package the whole thing is about.
const PACKAGE: &str = "@deepseek-ai/dsh";

/// Roughly what a dsh install weighs once npm is done with it. Quoted to the
/// user before they agree to download it, because it is a lot.
const DOWNLOAD_SIZE: &str = "约 255 MB";

/// How often the download thread looks in on npm. It runs for minutes, and the
/// only thing waiting on the answer is a notification.
const POLL: Duration = Duration::from_millis(200);

/// The install running right now, if one is. Kept reachable so that quitting
/// the app takes it down rather than leaving it writing to disk with no owner —
/// see [`stop`].
static INSTALLING: Mutex<Option<Installing>> = Mutex::new(None);

/// An npm process installing dsh, and what makes sure it does not outlive us.
struct Installing {
    child: Child,
    /// Held for as long as the install runs; see [`crate::server::Job`]. This is
    /// the backstop for a crash or a force-kill, where [`stop`] never runs.
    #[cfg(windows)]
    _job: Option<crate::server::Job>,
}

/// One dsh installation on disk.
pub struct Install {
    /// The Node runtime that runs it — always the bundled one, since a machine
    /// without dsh probably has no Node either.
    node: PathBuf,
    entry: PathBuf,
    pub version: Version,
}

impl Install {
    /// Read the installation rooted at `dir`, where `dir/node_modules` is an
    /// npm tree containing dsh. `None` if it is missing or half-written.
    fn at(dir: &Path, node: &Path) -> Option<Self> {
        let package = dir.join("node_modules/@deepseek-ai/dsh");
        let entry = package.join("lib/bin.js");
        if !entry.is_file() {
            return None;
        }

        // The entry point is there, so this is meant to be a usable install. If
        // the version will not read, something is wrong with it — say so, or
        // the app silently falls through to the next candidate and the reason
        // is invisible.
        let Some(version) = Self::version(&package) else {
            eprintln!(
                "dsh-desktop: 读不出 {} 的版本号，忽略这份安装",
                package.display()
            );
            return None;
        };

        Some(Self {
            node: node.to_path_buf(),
            entry,
            version,
        })
    }

    /// The `version` field of an npm package directory's manifest.
    fn version(package: &Path) -> Option<Version> {
        let manifest = std::fs::read_to_string(package.join("package.json")).ok()?;
        let manifest: serde_json::Value = serde_json::from_str(&manifest).ok()?;
        Version::parse(manifest.get("version")?.as_str()?).ok()
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.node);
        command.arg(&self.entry);

        // dsh shells out to `node` for workers and plugin tooling; point those
        // at the runtime we shipped rather than whatever the machine has.
        if let Some(dir) = self.node.parent() {
            prepend_to_path(&mut command, dir);
        }

        command
    }
}

/// The dsh that should run: the newer of the bundled and downloaded copies.
pub fn current(app: &AppHandle) -> Option<Install> {
    let node = node(app)?;
    let bundled = Install::at(&bundled_dir(app)?, &node);

    match (Install::at(&downloaded_dir(app)?, &node), bundled) {
        (Some(downloaded), Some(bundled)) if downloaded.version > bundled.version => Some(downloaded),
        (downloaded, None) => downloaded,
        (_, bundled) => bundled,
    }
}

/// Move a finished download into place. Call this before anything launches dsh:
/// at that point nothing holds the old directory open, which is the only moment
/// swapping it is safe on Windows.
///
/// The staging directory existing is the whole test, because [`install`] only
/// gives the tree that name once npm has exited cleanly. A download that was
/// cut short is sitting under another name and is ignored here — swapping a
/// half-written tree in would destroy a perfectly good one.
pub fn promote(app: &AppHandle) {
    let (Some(staged), Some(live)) = (staging_dir(app), downloaded_dir(app)) else {
        return;
    };
    if !staged.is_dir() {
        return;
    }

    let discarded = live.with_extension("old");
    let _ = std::fs::remove_dir_all(&discarded);

    if live.is_dir() && std::fs::rename(&live, &discarded).is_err() {
        return;
    }
    if let Err(error) = std::fs::rename(&staged, &live) {
        eprintln!("dsh-desktop: 无法启用已下载的 dsh：{error}");
        let _ = std::fs::rename(&discarded, &live);
        return;
    }

    // Best effort: a locked leftover just gets cleared on a later launch.
    let _ = std::fs::remove_dir_all(&discarded);
}

/// Ask npm what the newest dsh is and, if we do not have it, offer to fetch it.
/// Runs in the background: the app is already usable on the version it has.
pub fn check(app: &AppHandle) {
    let app = app.clone();

    // A plain thread, not the async runtime: `npm view` blocks, and the install
    // that may follow blocks for minutes. Neither belongs on the executor the
    // app updater is sharing.
    std::thread::spawn(move || {
        let Some(installed) = current(&app).map(|install| install.version) else {
            return;
        };

        let Some(latest) = latest(&app) else {
            eprintln!("dsh-desktop: 无法查询 dsh 最新版本");
            return;
        };

        if latest <= installed {
            return;
        }
        if skipped(&app).is_some_and(|skipped| skipped == latest) {
            return;
        }

        offer(app, installed, latest);
    });
}

/// `npm view` rather than a request of our own: it reads the user's `.npmrc`,
/// so a private registry or a corporate proxy keeps working.
fn latest(app: &AppHandle) -> Option<Version> {
    let output = npm(app)?
        .args(["view", PACKAGE, "version"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;

    Version::parse(String::from_utf8_lossy(&output.stdout).trim()).ok()
}

fn offer(app: AppHandle, installed: Version, latest: Version) {
    let answered = app.clone();
    let version = latest.clone();

    app.dialog()
        .message(&format!(
            "dsh 有新版本 {latest}（当前 {installed}）。\n\n\
             下载约需 {DOWNLOAD_SIZE}，在后台进行，下次启动时生效。"
        ))
        .title("dsh 有可用更新")
        .buttons(MessageDialogButtons::OkCancelCustom(
            "下载".into(),
            "跳过此版本".into(),
        ))
        .show(move |yes| {
            if yes {
                download(answered, version);
            } else {
                skip(&answered, &version);
            }
        });
}

/// The version the user turned down. Re-asking on every launch for a download
/// this size wears thin fast, so a refusal sticks — but only to that version:
/// the next release asks again.
fn skipped(app: &AppHandle) -> Option<Version> {
    let recorded = std::fs::read_to_string(skip_file(app)?).ok()?;
    Version::parse(recorded.trim()).ok()
}

fn skip(app: &AppHandle, version: &Version) {
    let Some(path) = skip_file(app) else { return };

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(error) = std::fs::write(&path, version.to_string()) {
        // Not worth interrupting anyone over; the cost is being asked again.
        eprintln!("dsh-desktop: 无法记住跳过的 dsh 版本：{error}");
    }
}

fn skip_file(app: &AppHandle) -> Option<PathBuf> {
    Some(simplified(app.path().app_local_data_dir().ok()?).join("dsh-skipped"))
}

/// Install into a staging directory. The running dsh keeps its files; the swap
/// happens in [`promote`] on the next launch.
fn download(app: AppHandle, version: Version) {
    std::thread::spawn(move || {
        match install(&app, &version) {
            Ok(true) => note(
                &app,
                "dsh 已更新",
                &format!("dsh {version} 已下载完成，下次启动 dsh desktop 时生效。"),
            ),
            // Cut short because the app is quitting. Nothing to report, and by
            // now nowhere left to report it.
            Ok(false) => {}
            Err(error) => {
                eprintln!("dsh-desktop: 下载 dsh {version} 失败：{error}");
                note(
                    &app,
                    "dsh 更新失败",
                    &format!("下载 dsh {version} 时出错，将继续使用当前版本。\n\n{error}"),
                );
            }
        }
    });
}

/// Run the install. npm writes into a directory [`promote`] does not look at,
/// which is renamed to the one it does only after npm exits cleanly — so an
/// interrupted download is never mistaken for a finished one.
///
/// `Ok(false)` means the app quit while npm was still working and the install
/// was killed with it.
fn install(app: &AppHandle, version: &Version) -> Result<bool, String> {
    let (Some(partial), Some(staged)) = (partial_dir(app), staging_dir(app)) else {
        return Err("找不到应用数据目录".into());
    };

    let _ = std::fs::remove_dir_all(&partial);
    std::fs::create_dir_all(&partial).map_err(|error| error.to_string())?;

    let child = npm(app)
        .ok_or("内置的 npm 不可用")?
        .current_dir(&partial)
        .args(["install", "--omit=dev", "--no-audit", "--no-fund"])
        .arg(format!("{PACKAGE}@{version}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| error.to_string())?;

    #[cfg(windows)]
    let job = crate::server::Job::hold(&child);

    *INSTALLING.lock().unwrap() = Some(Installing {
        child,
        #[cfg(windows)]
        _job: job,
    });

    let Some(status) = wait() else {
        return Ok(false);
    };

    let status = status.map_err(|error| error.to_string())?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(format!("npm 退出码 {status}"));
    }

    // Complete at last: give the tree the name `promote` acts on.
    let _ = std::fs::remove_dir_all(&staged);
    std::fs::rename(&partial, &staged).map_err(|error| error.to_string())?;

    Ok(true)
}

/// Wait for the tracked npm to finish, letting go of the lock between checks so
/// [`stop`] can still reach it. `None` once it has: the app is on its way out.
fn wait() -> Option<std::io::Result<ExitStatus>> {
    loop {
        {
            let mut installing = INSTALLING.lock().unwrap();
            let finished = match installing.as_mut()?.child.try_wait() {
                Ok(None) => None,
                Ok(Some(status)) => Some(Ok(status)),
                Err(error) => Some(Err(error)),
            };

            if let Some(result) = finished {
                *installing = None;
                return Some(result);
            }
        }

        std::thread::sleep(POLL);
    }
}

/// Kill a download that is still running. Called on the way out: npm holds no
/// state worth saving, and left alone it would keep unpacking into a directory
/// this process no longer owns.
pub fn stop() {
    if let Some(mut installing) = INSTALLING.lock().unwrap().take() {
        crate::server::kill_tree(&mut installing.child);
    }
}

/// npm, run through the bundled Node rather than its shell shim, so there is no
/// console window and no dependency on how the machine resolves `npm`.
fn npm(app: &AppHandle) -> Option<Command> {
    let node = node(app)?;
    let cli = node.parent()?.join("node_modules/npm/bin/npm-cli.js");
    if !cli.is_file() {
        return None;
    }

    let mut command = Command::new(node);
    command.arg(cli).stdin(Stdio::null());

    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    Some(command)
}

fn node(app: &AppHandle) -> Option<PathBuf> {
    let node = resources(app)?
        .join("runtime")
        .join(if cfg!(windows) { "node.exe" } else { "node" });

    node.is_file().then_some(node)
}

fn resources(app: &AppHandle) -> Option<PathBuf> {
    Some(simplified(app.path().resource_dir().ok()?).join("resources"))
}

fn bundled_dir(app: &AppHandle) -> Option<PathBuf> {
    Some(resources(app)?.join("dsh"))
}

fn downloaded_dir(app: &AppHandle) -> Option<PathBuf> {
    Some(simplified(app.path().app_local_data_dir().ok()?).join("dsh"))
}

fn staging_dir(app: &AppHandle) -> Option<PathBuf> {
    Some(downloaded_dir(app)?.with_extension("next"))
}

/// Where npm actually writes. Only ever renamed to [`staging_dir`], and only
/// once the install is complete; a leftover here is scrap the next download
/// clears.
fn partial_dir(app: &AppHandle) -> Option<PathBuf> {
    Some(downloaded_dir(app)?.with_extension("partial"))
}

fn note(app: &AppHandle, title: &str, detail: &str) {
    app.dialog().message(detail).title(title).show(|_| {});
}

/// Drop the `\\?\` prefix Tauri's path APIs come back with on Windows. Rust's
/// own file APIs accept a verbatim path, but Node's module resolver parses one
/// as the bare drive `C:` and refuses to load the entry point.
///
/// Only plain drive paths are unwrapped — `\\?\UNC\server\share` means
/// something different and is left alone.
fn simplified(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    if let Some(rest) = path.to_str().and_then(|path| path.strip_prefix(r"\\?\")) {
        if rest.as_bytes().get(1) == Some(&b':') {
            return PathBuf::from(rest);
        }
    }

    path
}

/// Put `dir` at the front of the child's PATH, keeping ours behind it.
fn prepend_to_path(command: &mut Command, dir: &Path) {
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let entries = std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(&inherited));

    if let Ok(path) = std::env::join_paths(entries) {
        command.env("PATH", path);
    }
}
