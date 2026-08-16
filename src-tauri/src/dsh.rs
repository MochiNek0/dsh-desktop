//! The dsh installation the app manages, and how it gets replaced.
//!
//! One of it, in one place: `%LOCALAPPDATA%\<identifier>\dsh`, put there by the
//! installer (see `src-tauri/installer-hooks.nsh`) and replaced in place by the
//! updates below. Nothing about dsh ships inside the app, which is what keeps
//! an app update from writing an older dsh over a newer one — the installer
//! finds the directory already populated and leaves it alone.
//!
//! Installing is the installer's job and only the installer's: a machine that
//! reaches this code without a dsh is one where that failed, and it is told to
//! run the installer again rather than quietly starting a 185 MB download in
//! front of a window the user just opened. What is left here is the update.
//!
//! A dsh the user installed themselves is checked too, and only checked: it is
//! reported on and never written to. See [`tell`] for why applying that one is
//! not this app's to do.
//!
//! The swap is a rename, and it happens in [`promote`] on the launch *after*
//! the download, before anything opens the tree. A download that was cut short
//! is sitting under another name and is never mistaken for a finished one, so
//! every failure leaves a working dsh in place.

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use semver::Version;
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The package the whole thing is about.
const PACKAGE: &str = "@deepseek-ai/dsh";

/// What a dsh install costs over the wire. Quoted to the user before they agree
/// to it, because it is a lot. Measured, not estimated: 587 packages, 185 MB of
/// tarballs, four minutes on a 2 MB/s link.
const DOWNLOAD_SIZE: &str = "约 185 MB";

/// What the same install weighs once npm has unpacked it, which is the figure
/// the progress bar divides by — npm reports nothing usable to drive one, so
/// what fills the bar is the staging tree's own weight against where it is
/// heading. Approximate by construction: the bar is held short of the end
/// rather than allowed to claim more than it knows.
const DOWNLOAD_BYTES: f64 = 327.0 * 1024.0 * 1024.0;

/// Registries to fall back through, in order, when the one npm resolves on its
/// own does not answer. Kept in step with `src-tauri/installer-hooks.nsh`,
/// which works the same way for the install this updates.
///
/// The user's own configuration is always tried first and is never overridden —
/// a private mirror or a corporate proxy is there for a reason. These only come
/// out when that has already failed.
const MIRRORS: [&str; 3] = [
    "https://registry.npmmirror.com/",
    "https://mirrors.cloud.tencent.com/npm/",
    "https://mirrors.huaweicloud.com/repository/npm/",
];

/// The furthest the measured bar goes. The last stretch belongs to npm's own
/// bookkeeping, which adds no bytes worth counting.
const PROGRESS_CEILING: f64 = 99.0;

/// How often the download thread looks in on npm. It runs for minutes, and the
/// only thing waiting on the answer is a notification.
const POLL: Duration = Duration::from_millis(200);

/// How often the staging tree is weighed for the progress bar. Each sample
/// walks tens of thousands of files, so it is deliberately far slower than
/// [`POLL`], which only costs a `try_wait`.
const WEIGH: Duration = Duration::from_secs(1);

/// How long the startup check waits for npm before the app stops waiting on it.
///
/// This one runs before dsh does, so it is time the user spends looking at the
/// loading page. Offline, or behind a proxy that never answers, `npm view`
/// would sit there for far longer than anyone will forgive — and the answer was
/// never load-bearing: there is a working dsh on disk either way.
///
/// It used to be three seconds, which was wrong in the expensive direction: a
/// `npm view` that is merely slow rather than broken gets cut off, and the app
/// then never offers an update that is genuinely there. Measured at 10s against
/// registry.npmjs.org on a connection that was otherwise fine — most of it npm
/// starting up — so this leaves half again as much headroom.
///
/// What it costs is bounded by the failure it is there for. Offline fails in
/// well under a second, because the name does not resolve; the full wait only
/// happens behind a proxy that accepts the connection and then says nothing.
const CHECK_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a dsh gets to answer `--version`. Nothing about it touches the
/// network — this is a local process starting — so the only thing this bounds
/// is a dsh that is broken enough to hang, and it still sits between the user
/// and their window.
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

/// How long an answer from the registry is treated as still true.
///
/// Raising [`CHECK_TIMEOUT`] to something the registry can actually answer
/// within had a cost the old three seconds hid: the check now usually *waits
/// for the answer* — ten seconds of a loading page, before dsh has been allowed
/// to start, on every single launch. Once every few hours is enough to put a
/// new release in front of someone the same day they could have had it, and the
/// other launches go straight through.
///
/// Only a check that got an answer counts, so a failure retries on the next
/// launch rather than being remembered for six hours.
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

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
    /// The directory holding `node_modules`, which the warm-up list's paths are
    /// relative to. See [`crate::warm`].
    pub root: PathBuf,
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
            root: dir.to_path_buf(),
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

/// The dsh this app manages, if the installer got one onto the machine.
///
/// `None` is not the end of the road: [`crate::server`] goes on to try `DSH_BIN`
/// and then whatever `dsh` is on PATH, so a user who installed dsh themselves
/// keeps working — and keeps it exactly as it is, since nothing here writes to
/// an install it did not make. See [`installed`].
pub fn current(app: &AppHandle) -> Option<Install> {
    Install::at(&dsh_dir(app)?, &node(app)?)
}

/// The dsh this launch is about to run, and whether it belongs to this app.
enum Installed {
    /// Under [`dsh_dir`], put there by the installer and replaced in place by
    /// the download below.
    Managed(Version),
    /// The user's own — `DSH_BIN`, or `dsh` on PATH. Reported on, never written
    /// to.
    Foreign(Version),
}

impl Installed {
    fn version(&self) -> &Version {
        match self {
            Self::Managed(version) | Self::Foreign(version) => version,
        }
    }
}

/// Find it the same way [`crate::server::command`] finds the one it starts, so
/// that the version being checked is the version that will run. Getting this
/// order wrong would mean offering an update to a copy nothing launches.
fn installed(app: &AppHandle) -> Option<Installed> {
    if let Some(bin) = std::env::var_os("DSH_BIN") {
        return version_of(&bin).map(Installed::Foreign);
    }
    if let Some(install) = current(app) {
        return Some(Installed::Managed(install.version));
    }
    version_of(crate::server::default_bin().as_ref()).map(Installed::Foreign)
}

/// Ask a dsh what version it is, rather than reading it off disk.
///
/// Where a foreign install keeps its files depends on what put it there — npm's
/// global prefix, a version manager's shims, something that is not npm at all —
/// and the name on PATH is the only part of that this app can count on knowing.
/// `dsh --version` prints the bare version and exits 0.
fn version_of(bin: &OsStr) -> Option<Version> {
    let mut command = Command::new(bin);
    command.arg("--version").stdin(Stdio::null());

    // On Windows this is `dsh.cmd`, which std runs through cmd.exe.
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    Version::parse(&printed(command, VERSION_TIMEOUT)?).ok()
}

/// Move a finished download into place. Call this before anything launches dsh:
/// at that point nothing holds the old directory open, which is the only moment
/// swapping it is safe on Windows.
///
/// The staging directory existing is the whole test, because [`install`] only
/// gives the tree that name once npm has exited cleanly. A download that was
/// cut short is sitting under another name and is ignored here — swapping a
/// half-written tree in would destroy a perfectly good one.
///
/// The target is [`dsh_dir`], the only dsh there is, so that a download replaces
/// it rather than joining it.
///
/// The tree a previous promotion displaced is cleared here too, whether or not
/// there is anything to promote this time.
pub fn promote(app: &AppHandle) {
    let (Some(staged), Some(live)) = (staging_dir(app), dsh_dir(app)) else {
        return;
    };

    if !staged.is_dir() {
        // Nothing to swap in, but [`discard`] dies with the app that started
        // it, so a quit part-way through one leaves a few hundred megabytes
        // behind. Nothing else ever comes back for it, so every launch does.
        discard(live.with_extension("old"));
        return;
    }

    if !swap(&staged, &live) {
        eprintln!("dsh-desktop: 无法启用已下载的 dsh，继续使用当前版本");
    }
}

/// Put `staged` where `live` is, keeping whatever was there until the new tree
/// is in place. `false` if the directory could not be written, which on Windows
/// is what a locked file or an unwritable install directory comes back as.
fn swap(staged: &Path, live: &Path) -> bool {
    let discarded = live.with_extension("old");

    // Synchronous, unlike the discard below: the renames that follow need the
    // name to be free.
    let _ = std::fs::remove_dir_all(&discarded);

    if live.is_dir() && std::fs::rename(live, &discarded).is_err() {
        return false;
    }
    if std::fs::rename(staged, live).is_err() {
        let _ = std::fs::rename(&discarded, live);
        return false;
    }

    discard(discarded);
    true
}

/// Delete a displaced dsh tree off the startup path. It is a couple of hundred
/// megabytes of small files, and it is under a name nothing looks at, so
/// nobody is waiting on it going away. Best effort: what this does not finish —
/// because it was locked, or because the app quit out from under it — the
/// [`promote`] on a later launch picks up.
fn discard(dir: PathBuf) {
    if !dir.is_dir() {
        return;
    }

    std::thread::spawn(move || {
        let _ = std::fs::remove_dir_all(&dir);
    });
}

/// What the loading page is told while the check and the download it can lead
/// to are running: a line of status text, and a percentage — negative to put
/// the progress bar away.
pub type Report<'a> = dyn Fn(&str, f64) + 'a;

/// Settle which dsh this launch runs, before one is started.
///
/// Returns `true` to go ahead and boot. `false` means the user took an update
/// and the app is on its way to restarting into it — there is nothing left to
/// start in this process.
///
/// Everything that can hold this up is bounded except the user: the check has
/// [`CHECK_TIMEOUT`], and a check that fails or times out boots what is already
/// on disk. The dialogs block, so this must run off the main thread.
pub fn gate(app: &AppHandle, report: &Report) -> bool {
    if checked_recently(app) {
        return true;
    }

    report("正在检查 dsh 更新…", -1.0);

    let Some(installed) = installed(app) else {
        // No dsh anywhere: the installer's fetch failed. Nothing to check and
        // nothing to offer — the boot failure that follows is where that gets
        // said, and it says to run the installer again.
        return true;
    };

    let Some(latest) = latest(app) else {
        eprintln!(
            "dsh-desktop: 无法查询 dsh 最新版本，使用已安装的 {}",
            installed.version()
        );
        return true;
    };
    mark_checked(app);

    if latest <= *installed.version() || skipped(app).is_some_and(|skipped| skipped == latest) {
        return true;
    }

    let installed = match installed {
        Installed::Managed(installed) => installed,
        // Not ours to replace, so telling the user is the whole of it — and
        // recording it as skipped is what keeps it to once per release rather
        // than every time the six hours are up.
        Installed::Foreign(installed) => {
            tell(app, &installed, &latest);
            skip(app, &latest);
            return true;
        }
    };

    if !ask(app, &installed, &latest) {
        skip(app, &latest);
        return true;
    }

    match install(app, &latest, report) {
        Ok(true) => {
            report("下载完成，正在重启…", 100.0);
            restart(app);
            false
        }
        // Cut short because the app is quitting. Nothing to report, and by now
        // nowhere left to report it.
        Ok(false) => false,
        Err(error) => {
            eprintln!("dsh-desktop: 下载 dsh {latest} 失败：{error}");
            report("", -1.0);
            note(
                app,
                "dsh 更新失败",
                &format!(
                    "下载 dsh {latest} 时出错，默认源和几个备用镜像都没有成功，\
                     将继续使用当前的 {installed}。\n\n{error}"
                ),
            );
            true
        }
    }
}

/// `npm view` rather than a request of our own: it reads the user's `.npmrc`,
/// so a private registry or a corporate proxy keeps working.
///
/// Given [`CHECK_TIMEOUT`] to answer, because this one is on the path to the
/// window. npm has no timeout of its own worth the name — behind a proxy that
/// black-holes the connection it will sit there for minutes.
fn latest(app: &AppHandle) -> Option<Version> {
    let mut npm = npm(app)?;
    npm.args(["view", PACKAGE, "version"]);
    Version::parse(&printed(npm, CHECK_TIMEOUT)?).ok()
}

/// Run `command` and take the single line it prints, giving it `timeout` to do
/// so. `None` if it would not start, failed, or was still going when the time
/// ran out — every caller is asking a question it can do without an answer to.
///
/// Both questions are asked with the loading page on screen, which is what the
/// deadline is for: neither `npm view` nor a dsh that hangs on startup has a
/// timeout of its own worth the name, and this is time the user spends waiting.
///
/// One short line is the whole contract, but only one of the two callers is
/// asking a program this app shipped: [`version_of`] runs whatever `dsh` the
/// user installed. So the pipe is drained on a thread of its own rather than
/// after the wait below — a child that fills it would block on the write while
/// this blocked on the exit, and nothing but the deadline would break the tie.
fn printed(mut command: Command, timeout: Duration) -> Option<String> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut printed = Vec::new();
        let _ = stdout.read_to_end(&mut printed);
        printed
    });

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) => return None,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
            Ok(None) => {
                crate::server::kill_tree(&mut child);
                return None;
            }
            Err(_) => return None,
        }
    }

    // The pipe closes with the process, so the reader is already done or a
    // syscall away from it. The early returns above leave it running, which
    // costs nothing: they all end with a process that is gone or killed.
    let printed = reader.join().ok()?;
    Some(String::from_utf8_lossy(&printed).trim().to_string())
}

/// Blocking, unlike every other dialog here: the answer decides what this
/// launch does next, and there is nothing sensible to do until it arrives.
fn ask(app: &AppHandle, installed: &Version, latest: &Version) -> bool {
    app.dialog()
        .message(&format!(
            "dsh 有新版本 {latest}（当前 {installed}）。\n\n\
             下载约需 {DOWNLOAD_SIZE}。更新期间应用会等待，完成后自动重启。"
        ))
        .title("dsh 有可用更新")
        .buttons(MessageDialogButtons::OkCancelCustom(
            "更新".into(),
            "跳过此版本".into(),
        ))
        .blocking_show()
}

/// Tell the user about an update to a dsh this app did not install, and leave it
/// at that.
///
/// Nothing here offers to apply it, because this app is in no position to.
/// Whatever put that dsh there owns it — npm's global prefix, a version
/// manager's shims, a package manager that is not npm — and the only npm at
/// hand is the one beside the bundled Node, whose global prefix is somewhere
/// else entirely. Running it would install a second dsh where PATH will never
/// look, next to the working one it was supposed to replace.
///
/// So the command goes in the message instead, for the user to run against
/// whatever they actually installed it with.
fn tell(app: &AppHandle, installed: &Version, latest: &Version) {
    note(
        app,
        "dsh 有可用更新",
        &format!(
            "dsh 有新版本 {latest}（当前 {installed}）。\n\n\
             这份 dsh 是你自己装的，应用不会去改动它。要更新的话，\
             在终端里执行：\n\nnpm install -g {PACKAGE}@latest"
        ),
    );
}

/// Restart into the dsh that was just downloaded, which [`promote`] swaps in on
/// the way back up.
///
/// Nothing is running to shut down — this happens before the server starts —
/// and `restart` skips the exit handler anyway.
fn restart(app: &AppHandle) {
    let restarting = app.clone();
    let _ = app.run_on_main_thread(move || restarting.restart());
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

/// Whether the registry was asked recently enough that asking again would only
/// cost the user the wait. See [`CHECK_INTERVAL`].
///
/// A clock that has moved backwards makes `elapsed` fail, which reads here as
/// "not recent" — one extra check, rather than a check suppressed until the
/// clock catches up.
fn checked_recently(app: &AppHandle) -> bool {
    let Some(path) = checked_file(app) else {
        return false;
    };

    std::fs::metadata(&path)
        .and_then(|file| file.modified())
        .is_ok_and(|at| at.elapsed().is_ok_and(|since| since < CHECK_INTERVAL))
}

/// Write down that the registry answered. The file's own timestamp is the
/// record, so there is nothing in it and nothing to parse.
fn mark_checked(app: &AppHandle) {
    let Some(path) = checked_file(app) else { return };

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(error) = std::fs::write(&path, b"") {
        // The cost is checking again next launch, which is what would have
        // happened anyway before any of this existed.
        eprintln!("dsh-desktop: 无法记录 dsh 更新检查时间：{error}");
    }
}

fn checked_file(app: &AppHandle) -> Option<PathBuf> {
    Some(simplified(app.path().app_local_data_dir().ok()?).join("dsh-checked"))
}

/// Run the install. npm writes into a directory [`promote`] does not look at,
/// which is renamed to the one it does only after npm exits cleanly — so an
/// interrupted download is never mistaken for a finished one.
///
/// `Ok(false)` means the app quit while npm was still working and the install
/// was killed with it.
fn install(app: &AppHandle, version: &Version, report: &Report) -> Result<bool, String> {
    let (Some(partial), Some(staged)) = (partial_dir(app), staging_dir(app)) else {
        return Err("找不到应用数据目录".into());
    };

    // `None` first: whatever npm resolves on its own, which is the user's
    // configuration if they have one. The mirrors are only for when that fails.
    let sources = std::iter::once(None).chain(MIRRORS.iter().copied().map(Some));
    let mut failure = String::new();

    for registry in sources {
        match attempt(app, version, report, &partial, registry) {
            Ok(true) => {
                // Complete at last: give the tree the name `promote` acts on.
                let _ = std::fs::remove_dir_all(&staged);
                std::fs::rename(&partial, &staged).map_err(|error| error.to_string())?;
                return Ok(true);
            }
            Ok(false) => return Ok(false),
            Err(error) => {
                eprintln!(
                    "dsh-desktop: 从 {} 下载 dsh 失败：{error}",
                    registry.unwrap_or("默认源")
                );
                failure = error;
            }
        }
    }

    // Every source failed. What the last one managed to unpack is a few hundred
    // megabytes of tree that nothing will ever look at again.
    let _ = std::fs::remove_dir_all(&partial);
    Err(failure)
}

/// One `npm install` into `partial`, from one registry. `Ok(false)` means the
/// app quit while it was running and there is nothing left to report to.
///
/// The tree is cleared first rather than resumed. A failed attempt can leave a
/// partly-written `node_modules` behind, and npm handed one of those decides
/// packages are already present — so a retry against a working mirror would
/// otherwise inherit the hole the broken one left.
fn attempt(
    app: &AppHandle,
    version: &Version,
    report: &Report,
    partial: &Path,
    registry: Option<&str>,
) -> Result<bool, String> {
    let _ = std::fs::remove_dir_all(partial);
    std::fs::create_dir_all(partial).map_err(|error| error.to_string())?;

    let mut npm = npm(app).ok_or("内置的 npm 不可用")?;
    npm.current_dir(partial)
        .args(["install", "--omit=dev", "--no-audit", "--no-fund"]);
    if let Some(registry) = registry {
        npm.arg(format!("--registry={registry}"));
    }

    let child = npm
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

    // Weighed from the polling loop rather than a thread of its own, so the
    // reporter does not have to be `Send` — it writes to the window, which the
    // caller owns.
    let downloading = match registry {
        Some(_) => format!("正在下载 dsh {version}（备用源）…"),
        None => format!("正在下载 dsh {version}…"),
    };
    let weighed = std::cell::Cell::new(Instant::now());
    report(&downloading, 0.0);

    let Some(status) = wait(&|| {
        if weighed.get().elapsed() < WEIGH {
            return;
        }
        weighed.set(Instant::now());
        report(&downloading, percent(partial));
    }) else {
        return Ok(false);
    };

    let status = status.map_err(|error| error.to_string())?;
    if !status.success() {
        return Err(format!("npm 退出码 {status}"));
    }

    Ok(true)
}

/// Wait for the tracked npm to finish, letting go of the lock between checks so
/// [`stop`] can still reach it. `None` once it has: the app is on its way out.
///
/// `tick` runs once per poll, outside the lock, for whatever wants to watch the
/// install go by.
fn wait(tick: &dyn Fn()) -> Option<std::io::Result<ExitStatus>> {
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

        tick();
        std::thread::sleep(POLL);
    }
}

/// How far along the install looks, from what it has written so far against
/// [`DOWNLOAD_BYTES`]. Held under [`PROGRESS_CEILING`]: the estimate is rough
/// in both directions, and a bar that sits at 100% while npm is still working
/// reads as a hang.
fn percent(partial: &Path) -> f64 {
    (weigh(partial) as f64 / DOWNLOAD_BYTES * 100.0).min(PROGRESS_CEILING)
}

/// The total size of every file under `dir`. Best effort — this races npm
/// writing into the same tree, and an entry that vanishes between the listing
/// and the `metadata` call is one npm is still moving around.
fn weigh(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };

    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => weigh(&entry.path()),
            Ok(kind) if kind.is_file() => entry.metadata().map_or(0, |data| data.len()),
            _ => 0,
        })
        .sum()
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

pub fn resources(app: &AppHandle) -> Option<PathBuf> {
    Some(simplified(app.path().resource_dir().ok()?).join("resources"))
}

/// The one dsh, in app data rather than next to the app.
///
/// App data is what the app's own installer does not overwrite. Were this in
/// `resources/`, every app update would put the dsh that installer was built
/// against over whatever the user had — downgrading it, and making the check
/// below immediately offer the 185 MB back again.
///
/// Everything a download touches is derived from this — [`staging_dir`] and
/// [`partial_dir`] are its siblings — so that the promotion is a rename inside
/// one directory. Across directories it would be a rename across volumes the
/// moment app data and the app land on different drives, and Windows fails
/// those outright rather than falling back to a copy.
///
/// `installer-hooks.nsh` builds this same path out of `$LOCALAPPDATA` and the
/// bundle identifier; the two have to agree.
fn dsh_dir(app: &AppHandle) -> Option<PathBuf> {
    Some(simplified(app.path().app_local_data_dir().ok()?).join("dsh"))
}

/// A finished download, waiting for the [`promote`] that swaps it in.
fn staging_dir(app: &AppHandle) -> Option<PathBuf> {
    Some(dsh_dir(app)?.with_extension("next"))
}

/// Where npm actually writes. Only ever renamed to [`staging_dir`], and only
/// once the install is complete; a leftover here is scrap the next download
/// clears.
fn partial_dir(app: &AppHandle) -> Option<PathBuf> {
    Some(dsh_dir(app)?.with_extension("partial"))
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
