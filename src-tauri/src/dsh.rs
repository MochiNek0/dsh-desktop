//! The dsh this app runs: finding it, and getting one onto the machine when
//! there is none.
//!
//! Nothing about dsh ships inside the app and nothing here unpacks it. dsh is a
//! global npm install — `npm install -g @deepseek-ai/dsh` — so the copy the app
//! starts is the same copy the user's terminal gets, and updating it is one npm
//! command rather than a download into a staging directory and a rename on the
//! next launch.
//!
//! Both the installing and the updating live in a script beside the app —
//! `resources/install-deps.ps1` on Windows, which the NSIS installer also runs
//! (see `src-tauri/installer-hooks.nsh`), and `resources/install-deps.sh` on
//! macOS and Linux, where there is no installer hook to share it with and the
//! first launch is the only thing that runs it. This module decides *whether* to
//! run one and reports what it prints onto the loading page. Keeping one
//! implementation per platform matters more than keeping it in Rust: the script
//! has to detect Node, fetch and verify a Node archive, and walk a list of
//! registry mirrors, and a second copy of all that would drift.
//!
//! Finding dsh cannot go through the process's own PATH. The installer adds
//! Node's directory to the user's PATH and then launches this app, which
//! inherited its environment before any of that happened — so the search below
//! starts from what the script wrote down in `bootstrap.json` and falls back to
//! PATH, rather than the other way round.

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
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

/// How long the startup check waits for npm before the app stops waiting on it.
///
/// This one runs before dsh does, so it is time the user spends looking at the
/// loading page. Offline, or behind a proxy that never answers, `npm view`
/// would sit there for far longer than anyone will forgive — and the answer was
/// never load-bearing: there is a working dsh on disk either way.
///
/// Measured at 10s against registry.npmjs.org on a connection that was
/// otherwise fine — most of it npm starting up — so this leaves half again as
/// much headroom. What it costs is bounded by the failure it is there for:
/// offline fails in well under a second, because the name does not resolve.
const CHECK_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a dsh gets to answer `--version`. Nothing about it touches the
/// network — this is a local process starting — so the only thing this bounds
/// is a dsh that is broken enough to hang, and it still sits between the user
/// and their window.
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

/// How long an answer from the registry is treated as still true.
///
/// Without this the check *waits for the answer* — up to [`CHECK_TIMEOUT`] of a
/// loading page, before dsh has been allowed to start, on every single launch.
/// Once every few hours is enough to put a new release in front of someone the
/// same day they could have had it, and the other launches go straight through.
///
/// Only a check that got an answer counts, so a failure retries on the next
/// launch rather than being remembered for six hours.
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// How often a running child is looked in on. Nothing waits on the answer
/// except a progress bar and, at the end, the boot.
const POLL: Duration = Duration::from_millis(200);

/// The bootstrap script running right now, if one is. Kept reachable so that
/// quitting the app takes it down rather than leaving npm writing to disk with
/// no owner — see [`stop`].
static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

/// A child of ours, and what makes sure it does not outlive us.
struct Running {
    child: Child,
    /// Held for as long as it runs; see [`crate::server::Job`]. This is the
    /// backstop for a crash or a force-kill, where [`stop`] never runs.
    #[cfg(windows)]
    _job: Option<crate::server::Job>,
}

/// What `install-deps.ps1` wrote down about what it installed. Absent on a
/// machine where it has never run, or has never had anything to do.
///
/// This is how the app finds a Node that is on the user's PATH but not on this
/// process's — see the module docs — and how it tells a dsh it installed from
/// one that was already there.
#[derive(Default)]
struct Bootstrap {
    /// The Node the script settled on, ours or the machine's.
    node: Option<PathBuf>,
    /// npm's entry point beside it.
    npm: Option<PathBuf>,
    /// Where `npm install -g` puts things, which is also where `dsh.cmd` and
    /// the `node_modules` holding dsh live.
    prefix: Option<PathBuf>,
    /// Whether the dsh on this machine is one the script installed. A dsh that
    /// was already here is reported on and never written to; see [`tell`].
    ours: bool,
}

fn bootstrap(app: &AppHandle) -> Bootstrap {
    let Some(path) = app_dir(app).map(|dir| dir.join("bootstrap.json")) else {
        return Bootstrap::default();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Bootstrap::default();
    };
    let Ok(state) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Bootstrap::default();
    };

    let read = |key: &str| {
        state
            .get(key)
            .and_then(|value| value.as_str())
            .map(PathBuf::from)
    };

    Bootstrap {
        node: read("nodeExe"),
        npm: read("npmCli"),
        prefix: read("prefix"),
        ours: state.get("dsh").and_then(|value| value.as_str()) == Some("managed"),
    }
}

/// Every directory a command of ours might be in, most specific first: what the
/// script installed, then whatever this process inherited.
///
/// npm puts a global package's shims in the prefix itself on Windows and in
/// `<prefix>/bin` everywhere else, so what the marker records is npm's prefix
/// and the directory to search for is derived from it.
fn search_path(app: &AppHandle) -> Vec<PathBuf> {
    let state = bootstrap(app);
    let shims = state.prefix.map(|prefix| {
        if cfg!(windows) {
            prefix
        } else {
            prefix.join("bin")
        }
    });
    let node_dir = state
        .node
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf);

    let inherited = std::env::var_os("PATH").unwrap_or_default();
    [shims, node_dir]
        .into_iter()
        .flatten()
        .chain(std::env::split_paths(&inherited))
        .collect()
}

/// The first of `names` that exists in [`search_path`], as an absolute path.
///
/// Resolved here rather than left to `Command::new`, which searches the PATH
/// this process started with — the one that predates anything the installer did.
fn look_up(app: &AppHandle, names: &[&str]) -> Option<PathBuf> {
    search_path(app).into_iter().find_map(|dir| {
        names
            .iter()
            .map(|name| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}

/// The dsh command this launch will run.
pub struct Install {
    /// What to execute. On Windows this is npm's `dsh.cmd` shim, which std
    /// routes through cmd.exe with the correct argument quoting.
    pub bin: PathBuf,
    /// The directory holding `node_modules`, which the warm-up list's paths are
    /// relative to. See [`crate::warm`].
    pub root: PathBuf,
    pub version: Version,
}

/// Find it, the same way for every caller, so that the version being checked is
/// the version that will run.
///
/// 1. `DSH_BIN` — an explicit choice, so it wins outright.
/// 2. `dsh` in the npm prefix the bootstrap script recorded, or on PATH.
///
/// `None` means the machine has no dsh at all, which [`gate`] answers by
/// installing one.
pub fn current(app: &AppHandle) -> Option<Install> {
    let bin = match std::env::var_os("DSH_BIN") {
        Some(bin) => PathBuf::from(bin),
        None => look_up(app, &["dsh.cmd", "dsh"])?,
    };

    let root = root_of(&bin);
    let version = manifest_version(&root).or_else(|| version_of(bin.as_os_str()))?;

    Some(Install { bin, root, version })
}

/// The directory holding the `node_modules` a global install put dsh in.
///
/// On Windows npm puts the shim in the prefix and the package under the
/// `node_modules` beside it, so the shim's own directory is it. Everywhere else
/// the shim is a symlink in `<prefix>/bin` and the package is under
/// `<prefix>/lib/node_modules`, which is one level up and across.
///
/// Falling back to the shim's directory covers a `DSH_BIN` pointing at a tree
/// laid out some third way; the callers all treat a root with nothing under it
/// as nothing to do.
fn root_of(bin: &Path) -> PathBuf {
    let dir = bin.parent().unwrap_or(Path::new(".")).to_path_buf();

    #[cfg(not(windows))]
    if let Some(lib) = dir.parent().map(|prefix| prefix.join("lib")) {
        if lib.join("node_modules/@deepseek-ai/dsh").is_dir() {
            return lib;
        }
    }

    dir
}

/// The `version` field of the installed package's manifest — a file read rather
/// than a `dsh --version`, which costs a Node startup.
fn manifest_version(root: &Path) -> Option<Version> {
    let manifest = root.join("node_modules/@deepseek-ai/dsh/package.json");
    let manifest = std::fs::read_to_string(manifest).ok()?;
    let manifest: serde_json::Value = serde_json::from_str(&manifest).ok()?;
    Version::parse(manifest.get("version")?.as_str()?).ok()
}

/// Ask a dsh what version it is. The fallback for a `DSH_BIN` pointing at
/// something whose tree is laid out differently — a version manager's shim, a
/// checkout — where the manifest is not where npm would have put it.
fn version_of(bin: &OsStr) -> Option<Version> {
    let mut command = Command::new(bin);
    command.arg("--version").stdin(Stdio::null());

    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    Version::parse(&printed(command, VERSION_TIMEOUT)?).ok()
}

/// What the loading page is told while the checks and the install they can lead
/// to are running: a line of status text, and a percentage — negative to put
/// the progress bar away.
pub type Report<'a> = dyn Fn(&str, f64) + 'a;

/// Settle which dsh this launch runs, before one is started.
///
/// Returns `true` to go ahead and boot, which is every outcome except a user
/// who quit while an install was still running.
///
/// Everything that can hold this up is bounded except the user: the check has
/// [`CHECK_TIMEOUT`], and a check that fails or times out boots what is already
/// on disk. The dialogs block, so this must run off the main thread.
pub fn gate(app: &AppHandle, report: &Report) -> bool {
    let Some(installed) = current(app) else {
        // Nothing to run. The installer either failed to get dsh onto the
        // machine or never ran at all, and this is a `tauri dev` build — either
        // way the fix is the same, and the machine has an npm by now or is
        // about to get one.
        return bootstrap_now(app, report);
    };

    if checked_recently(app) {
        return true;
    }

    report("正在检查 dsh 更新…", -1.0);

    let Some(latest) = latest(app) else {
        eprintln!(
            "dsh-desktop: 无法查询 dsh 最新版本，使用已安装的 {}",
            installed.version
        );
        return true;
    };
    mark_checked(app);

    if latest <= installed.version || skipped(app).is_some_and(|skipped| skipped == latest) {
        return true;
    }

    // Not ours to replace, so telling the user is the whole of it — and
    // recording it as skipped is what keeps it to once per release rather than
    // every time the six hours are up.
    if !bootstrap(app).ours {
        tell(app, &installed.version, &latest);
        skip(app, &latest);
        return true;
    }

    if !ask(app, &installed.version, &latest) {
        skip(app, &latest);
        return true;
    }

    match run(app, &["-Mode", "update"], report) {
        // The new version is in place already: npm replaced it while nothing
        // was running, so there is nothing to swap in and nothing to restart
        // for. The boot carries straight on into it.
        Ok(true) => {
            report("", -1.0);
            true
        }
        // Cut short because the app is quitting. Nothing to report, and by now
        // nowhere left to report it.
        Ok(false) => false,
        Err(error) => {
            eprintln!("dsh-desktop: 更新 dsh 到 {latest} 失败：{error}");
            report("", -1.0);
            note(
                app,
                "dsh 更新失败",
                &format!(
                    "更新到 dsh {latest} 时出错，将继续使用当前的 {}。\n\n{error}",
                    installed.version
                ),
            );
            true
        }
    }
}

/// Get a dsh onto a machine that has none, which may mean getting it a Node
/// first. `false` if the app quit while it was running.
fn bootstrap_now(app: &AppHandle, report: &Report) -> bool {
    report("正在准备运行环境…", -1.0);

    match run(app, &["-Mode", "install"], report) {
        Ok(true) => {
            report("", -1.0);
            true
        }
        Ok(false) => false,
        Err(error) => {
            eprintln!("dsh-desktop: 安装 dsh 失败：{error}");
            report("", -1.0);
            // Booting anyway: `server::start` is about to fail with a message
            // that says what to do, and one failure report is better than two.
            true
        }
    }
}

/// Run the bootstrap script with `args`, mirroring its progress onto the
/// loading page. `Ok(false)` means the app quit while it was still working.
///
/// The script emits `::status <text>` and `::progress <percent>` lines for
/// this, and everything else it prints is npm's own log, which goes to stderr
/// for whoever is watching the app from a console.
fn run(app: &AppHandle, args: &[&str], report: &Report) -> Result<bool, String> {
    let script = script(app).ok_or_else(|| format!("找不到安装脚本 {SCRIPT}"))?;

    let mut command = interpreter(&script);
    command
        .args(args)
        // Turns the plain log into `::` lines and switches stdout to UTF-8,
        // which is what the reader below expects.
        .arg("-Progress")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    // npm is a tree of its own, and this one runs for minutes.
    #[cfg(unix)]
    crate::server::group_leader(&mut command);

    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let stdout = child.stdout.take().ok_or("无法读取脚本输出")?;

    #[cfg(windows)]
    let job = crate::server::Job::hold(&child);

    *RUNNING.lock().unwrap() = Some(Running {
        child,
        #[cfg(windows)]
        _job: job,
    });

    // Read on this thread, so that the reporter does not have to be `Send` — it
    // writes to the window, which the caller owns.
    //
    // The two kinds of line are independent: a status without a percentage must
    // not disturb the bar, so the last one seen is repeated rather than made up.
    let mut failure = None;
    let mut percent = -1.0;
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if let Some(text) = line.strip_prefix("::status ") {
            report(text, percent);
        } else if let Some(reported) = line.strip_prefix("::progress ") {
            if let Ok(reported) = reported.trim().parse() {
                percent = reported;
                report("", percent);
            }
        } else if let Some(text) = line.strip_prefix("::error ") {
            failure = Some(text.to_string());
        } else {
            eprintln!("[bootstrap] {line}");
        }
    }

    // The pipe closed, so the script is finished or a syscall away from it.
    let status = loop {
        let mut running = RUNNING.lock().unwrap();
        // Taken by `stop`: the app is on its way out.
        let Some(active) = running.as_mut() else {
            return Ok(false);
        };

        let finished = match active.child.try_wait() {
            Ok(Some(status)) => Some(Ok(status)),
            Ok(None) => None,
            Err(error) => Some(Err(error.to_string())),
        };
        if let Some(result) = finished {
            *running = None;
            break result?;
        }

        drop(running);
        std::thread::sleep(POLL);
    };

    if status.success() {
        Ok(true)
    } else {
        Err(failure.unwrap_or_else(|| format!("脚本退出码 {status}")))
    }
}

/// Kill a bootstrap that is still running. Called on the way out: npm holds no
/// state worth saving, and left alone it would keep unpacking into a directory
/// this process no longer owns.
pub fn stop() {
    if let Some(mut running) = RUNNING.lock().unwrap().take() {
        crate::server::kill_tree(&mut running.child);
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

/// npm, run through Node rather than its shell shim, so there is no console
/// window and no dependency on how the machine resolves `npm`.
///
/// The pair the bootstrap script recorded comes first: on a machine where it
/// installed a Node of its own, that Node is the one whose global prefix holds
/// the dsh being asked about.
fn npm(app: &AppHandle) -> Option<Command> {
    let state = bootstrap(app);
    let (node, cli) = match (state.node, state.npm) {
        (Some(node), Some(cli)) if node.is_file() && cli.is_file() => (node, cli),
        _ => {
            // Beside the binary on Windows, and one level up under `lib`
            // everywhere else — the same two layouts `root_of` covers.
            let node = look_up(app, &["node.exe", "node"])?;
            let dir = node.parent()?;
            let cli = [
                dir.join("node_modules/npm/bin/npm-cli.js"),
                dir.parent()
                    .unwrap_or(dir)
                    .join("lib/node_modules/npm/bin/npm-cli.js"),
            ]
            .into_iter()
            .find(|candidate| candidate.is_file())?;
            (node, cli)
        }
    };

    let mut command = Command::new(node);
    command.arg(cli).stdin(Stdio::null());

    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    Some(command)
}

/// Run `command` and take the single line it prints, giving it `timeout` to do
/// so. `None` if it would not start, failed, or was still going when the time
/// ran out — every caller is asking a question it can do without an answer to.
///
/// One short line is the whole contract, but [`version_of`] runs whatever
/// `DSH_BIN` names. So the pipe is drained on a thread of its own rather than
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
             下载约需 {DOWNLOAD_SIZE}。更新期间应用会等待，完成后直接启动。"
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
/// Whatever put that dsh there owns it — their own npm's global prefix, a
/// version manager's shims, a package manager that is not npm — and running
/// `npm install -g` with the npm this app has at hand would install a second
/// dsh into a prefix PATH may never reach, next to the working one it was
/// supposed to replace.
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
    Some(app_dir(app)?.join("dsh-skipped"))
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
    Some(app_dir(app)?.join("dsh-checked"))
}

/// `%LOCALAPPDATA%\<identifier>`, `~/Library/Application Support/<identifier>`,
/// `~/.local/share/<identifier>`. Both bootstrap scripts and
/// `installer-hooks.nsh` build the same path out of the platform's own variable
/// and the bundle identifier; they all have to agree.
fn app_dir(app: &AppHandle) -> Option<PathBuf> {
    Some(simplified(app.path().app_local_data_dir().ok()?))
}

pub fn resources(app: &AppHandle) -> Option<PathBuf> {
    Some(simplified(app.path().resource_dir().ok()?).join("resources"))
}

/// The bootstrap script for this platform. Both are staged into `resources/` by
/// `scripts/bundle-runtime.mjs` and shipped by the bundler; they take the same
/// arguments and print the same `::` lines.
#[cfg(windows)]
const SCRIPT: &str = "install-deps.ps1";
#[cfg(not(windows))]
const SCRIPT: &str = "install-deps.sh";

fn script(app: &AppHandle) -> Option<PathBuf> {
    let script = resources(app)?.join(SCRIPT);
    script.is_file().then_some(script)
}

/// What to run the script with.
#[cfg(windows)]
fn interpreter(script: &Path) -> Command {
    let mut command = Command::new("powershell.exe");
    command
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(script);
    command
}

/// `/bin/sh` rather than the script itself: a resource copied into a `.app` or
/// unpacked from a `.deb` does not reliably keep its executable bit, and there
/// is nothing in the script that a stock `/bin/sh` cannot run.
#[cfg(not(windows))]
fn interpreter(script: &Path) -> Command {
    let mut command = Command::new("/bin/sh");
    command.arg(script);
    command
}

fn note(app: &AppHandle, title: &str, detail: &str) {
    app.dialog().message(detail).title(title).show(|_| {});
}

/// Put the directories dsh needs at the front of a child's PATH.
///
/// Two things depend on this. The Node the bootstrap script may have installed
/// is on the user's PATH but not on this process's, so without it `dsh.cmd`
/// would run and fail to find the `node` it shells out to. And dsh shells out
/// to `node` again for workers and plugin tooling, which should reach the same
/// one the app is running it with.
pub fn apply_path(app: &AppHandle, command: &mut Command) {
    if let Ok(path) = std::env::join_paths(search_path(app)) {
        command.env("PATH", path);
    }
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
