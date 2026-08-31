//! The dsh this app runs: finding it, and getting one onto the machine when
//! there is none.
//!
//! Nothing about dsh ships inside the app and nothing here unpacks it. dsh is an
//! ordinary npm install into a directory the app owns, so updating it is one npm
//! command rather than a download into a staging directory and a rename on the
//! next launch — and the copy the app starts is the same copy the launcher on
//! the user's PATH starts.
//!
//! Both the installing and the updating live in a script beside the app —
//! `resources/install-deps.ps1` on Windows, `resources/install-deps.sh` on macOS
//! and Linux. This module decides *whether* to run one and reports what it
//! prints onto the loading page. Keeping one implementation per platform matters
//! more than keeping it in Rust: the script has to fetch and verify a Node
//! archive and measure a list of registry mirrors before walking it, and a
//! second copy of all that would drift.
//!
//! It is also the only thing that runs them. The Windows installer used to run
//! `-Mode install` itself, from `NSIS_HOOK_POSTINSTALL`, which is why Windows
//! was the one platform where the script ran again on an upgrade — and so the
//! one platform a bumped Node version reached. That call is gone; [`gate`] does
//! it at first launch everywhere, and [`provisioned`] is how it tells a runtime
//! this release built from one an older release left behind.
//!
//! Nothing here searches for anything, and that is the whole design.
//!
//! The app owns its runtime outright — a pinned Node and a local install of dsh
//! under `<app data>/runtime` — so every path is a join onto one constant, and
//! `dsh.rs` and `install-deps.ps1` derive the same paths independently without
//! either having to tell the other what it did.
//!
//! It used to go the other way. dsh was `npm install -g`, which put it in
//! whichever global prefix the machine's Node had configured, so where it landed
//! could not be predicted — only recorded, in `bootstrap.json`, and read back on
//! the next launch. That one fact is what the deleted half of this file was:
//! a marker parser, a three-level search path, three functions telling npm's
//! Windows and Unix layouts apart, six deciding whether a prefix could be
//! written to, and a symlink-liveness check because a `-g` shim outlives the
//! Node it points at. Every one of them was downstream of not knowing where dsh
//! was.
//!
//! It was also the user's problem, not just ours: switching Node with nvm, fnm,
//! asdf or volta moved the prefix, and the dsh the app had recorded stopped
//! existing. A pinned Node cannot be switched out from under us, and it pins the
//! ABI that dsh's native modules were built against besides.
//!
//! One directory does reach outside: `<app data>/bin`, holding a single launcher
//! that names our Node and our dsh by absolute path, goes on the front of the
//! user's PATH so that `dsh` works in their terminal — and means the app's copy.
//! It holds one file rather than a whole Node distribution, so the only command
//! it can shadow is a `dsh`, and `-Mode uninstall` takes the entry back off. On
//! Windows that is the whole of what can be done: the machine's PATH comes
//! before the user's and only HKCU is writable without elevation, so a `dsh`
//! installed machine-wide still answers first. `install-deps.ps1` writes and
//! owns that file; see [`terminal`] for the app's own way into a shell.

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use semver::Version;
use tauri::{AppHandle, Manager};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The package the whole thing is about.
const PACKAGE: &str = "@deepseek-ai/dsh";

// There are no shim-name lists any more. `DSH`, `PNPM` and `NODE` each held the
// two names npm's shims go by — `dsh.cmd` and `dsh`, and so on — because finding
// a tool meant walking a search path and trying each name against every
// directory on it. Nothing is searched for now: `entry`, `node` and `pnpm_ready`
// each name one file at one path.

/// What a dsh install costs over the wire. Quoted to the user before they agree
/// to it, because it is a lot. Measured, not estimated: 587 packages, 185 MB of
/// tarballs, four minutes on a 2 MB/s link.
fn download_size() -> &'static str {
    t!("约 185 MB", "about 185 MB")
}

/// What it takes up once it is on disk, which is not the download above: 185 MB
/// of tarballs unpack to a tree of some 30,000 files, and the pinned Node sits
/// beside it. Quoted before deleting it, for the same reason the download is
/// quoted before fetching it — and the uninstaller in `installer-hooks.nsh`
/// quotes the same number.
fn installed_size() -> &'static str {
    t!("约 360 MB", "about 360 MB")
}

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

/// Where everything the app installed lives: `<app data>/runtime`.
///
/// This one constant is what replaced `bootstrap.json` and the search it fed.
/// The old scheme installed dsh into whichever npm global prefix the machine's
/// Node happened to have — a location nothing can predict, so it had to be
/// written down, read back, and fallen back from, and the reading is what this
/// app got wrong silently for every machine the script installed a Node onto.
///
/// Nothing is written down now because there is nothing to remember. The
/// runtime is at one path, derived from the same application data directory
/// `install-deps.ps1` derives it from, and everything below is a join onto it.
/// The two have to agree; see the layout comment at the top of that script.
pub fn runtime(app: &AppHandle) -> Option<PathBuf> {
    Some(app_dir(app)?.join("runtime"))
}

/// The directory holding our Node's executable: `<runtime>/node` on Windows and
/// `<runtime>/node/bin` everywhere else, which is where Node's own archives put
/// it. This is what goes on a child's PATH — see [`apply_path`].
fn node_dir(app: &AppHandle) -> Option<PathBuf> {
    let dir = runtime(app)?.join("node");
    Some(if cfg!(windows) { dir } else { dir.join("bin") })
}

/// Our Node, and the only Node this app ever runs whatever else the machine has.
///
/// Owning it is what makes every path here a constant, and it settles a second
/// thing the old scheme could not: dsh's native modules — koffi, node-pty — are
/// built against one Node's ABI and refuse to load on another's. A pinned Node
/// pins the ABI for the life of the install.
pub fn node(app: &AppHandle) -> Option<PathBuf> {
    let node = node_dir(app)?.join(if cfg!(windows) { "node.exe" } else { "node" });
    node.is_file().then_some(node)
}

/// `<runtime>/node_modules`, where a local install puts everything.
///
/// The same layout on every platform, which a `-g` install is not: that one puts
/// the package under `lib/node_modules` and the shim in `bin` on Unix, and both
/// directly under the prefix on Windows. Telling those two apart was the whole
/// job of `shim_dir`, `package_root` and `root_of`, none of which survives.
fn modules(app: &AppHandle) -> Option<PathBuf> {
    Some(runtime(app)?.join("node_modules"))
}

/// dsh's entry point, read off the `bin` field of the manifest npm installed.
///
/// npm's own shim at `<runtime>/node_modules/.bin/dsh` is deliberately not used.
/// Its body falls back to whatever `node` is on PATH when there is no `node.exe`
/// beside it — and in this tree there is not — so going through it would put
/// back exactly the coupling this layout exists to remove, at the one point
/// that is exposed to the user. `install-deps.ps1` writes the terminal launcher
/// from the same field for the same reason; see `Get-DshEntry` there.
fn entry(app: &AppHandle) -> Option<PathBuf> {
    let dir = modules(app)?.join(PACKAGE);
    let manifest = std::fs::read_to_string(dir.join("package.json")).ok()?;
    let manifest: serde_json::Value = serde_json::from_str(&manifest).ok()?;

    let entry = dir.join(bin_field(manifest.get("bin")?)?);
    entry.is_file().then_some(entry)
}

/// The `dsh` entry out of a manifest's `bin`, whichever way round it is written.
///
/// npm accepts `"bin": "./cli.js"` for a package with one binary named after
/// itself, and `"bin": { "dsh": "./cli.js" }` when it names them. Both are read
/// rather than one being assumed, because which one dsh ships is dsh's choice
/// and it has not always been the same. Split out from [`entry`] so the parse
/// can be tested without an installed package.
fn bin_field(bin: &serde_json::Value) -> Option<&str> {
    let named = match bin {
        serde_json::Value::String(only) => only.as_str(),
        object => object.get("dsh")?.as_str()?,
    };
    // npm's own spelling is `./cli.js`. `Path::join` keeps the `.` as a
    // component, which resolves but reads badly in every error message.
    Some(named.trim_start_matches("./"))
}

// The launcher at `<app data>/bin/dsh.cmd` is the user's way in, not the app's:
// this module goes straight to [`entry`] with [`node`], and [`child_path`] puts
// the directory on a child's PATH by name. Nothing here needs to resolve the
// file itself, so nothing does — `install-deps.ps1` writes it and owns it.

/// Whether pnpm is in the runtime. dsh forwards every plugin install to it; see
/// `plugins.rs`.
///
/// One file test, where this used to be a three-state answer: there, there but
/// broken, or missing. The middle state existed because a global install leaves
/// a symlink into the prefix, and a Node switched by nvm/fnm/asdf/volta leaves
/// it pointing at nothing — so a plain `is_file` reported *absent* for a name
/// that was sitting right there, and the caller either reinstalled over a broken
/// link or told the user there was no pnpm at all. There are no symlinks in this
/// tree and nothing outside it can move, so the question has two answers again.
pub fn pnpm_ready(app: &AppHandle) -> bool {
    modules(app).is_some_and(|dir| dir.join("pnpm").join("package.json").is_file())
}

/// An explicit dsh, for a developer who wants the app to run their checkout. It
/// wins outright, and nothing offers to update it.
fn pinned() -> Option<PathBuf> {
    std::env::var_os("DSH_BIN").map(PathBuf::from)
}

/// What the settings panel shows: this app's version, the dsh it runs, the Node
/// underneath, and which dsh the user's own terminal answers with. As JSON,
/// because that is how the panel is fed.
///
/// Any of them can be empty — `dsh` and `node` both are on a launch that has not
/// provisioned a runtime yet — which the panel draws as "unknown" rather than
/// treating as a failure to report.
///
/// The terminal one is the slow field: it asks a shell or the registry, so this
/// belongs on the thread `crate::open_settings` already spawns and not in front
/// of a window.
pub fn facts(app: &AppHandle) -> String {
    // The stamp sits beside the Node rather than under its `bin`: the install
    // script writes it at the root of the tree it unpacked. [`node_dir`] points
    // one level deeper than that on Unix, so it is not the thing to join onto.
    let node = runtime(app)
        .map(|dir| dir.join("node").join(".dsh-node-version"))
        .and_then(|stamp| std::fs::read_to_string(stamp).ok())
        .map(|version| version.trim().to_string())
        .unwrap_or_default();

    let found = terminal_dsh();
    let ours = found.as_deref().is_some_and(|found| same_file(found, launcher(app)));

    serde_json::json!({
        "app": app.package_info().version.to_string(),
        "dsh": current(app).map(|dsh| dsh.version.to_string()).unwrap_or_default(),
        "node": node,
        "terminal": {
            "path": found.as_ref().map(|path| path.display().to_string()).unwrap_or_default(),
            "ours": ours,
            // Only for one that is not ours: ours is the `dsh` field above, and
            // this is a process start apiece.
            "version": found
                .filter(|_| !ours)
                .and_then(|path| version_of(path.as_os_str()))
                .map(|version| version.to_string())
                .unwrap_or_default(),
        },
    })
    .to_string()
}

/// The launcher `install-deps` writes and puts on the user's PATH. One file, at
/// one path, on every platform; see `Write-Launcher` / `write_launcher`.
fn launcher(app: &AppHandle) -> Option<PathBuf> {
    let name = if cfg!(windows) { "dsh.cmd" } else { "dsh" };
    Some(app_dir(app)?.join("bin").join(name))
}

/// Whether two paths name the same file on disk.
///
/// Resolved rather than compared as text: on macOS and Linux the `dsh` a shell
/// finds is `~/.local/bin/dsh`, which is a symlink to the launcher, and on
/// Windows the two spellings can differ in case alone.
fn same_file(found: &Path, ours: Option<PathBuf>) -> bool {
    let Some(ours) = ours else { return false };
    match (found.canonicalize(), ours.canonicalize()) {
        (Ok(found), Ok(ours)) => found == ours,
        _ => false,
    }
}

/// How long the probe below gets. It is a shell reading the user's own profile,
/// which is theirs to make as slow as they like; the panel draws without it and
/// fills the row in when it arrives.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// What a `dsh` typed into the user's own terminal resolves to.
///
/// Deliberately not `env::var("PATH")`. This process's environment was captured
/// when the app started, so it is missing the entry `install-deps` writes during
/// a first launch — the one launch where the answer matters most — and on macOS
/// a GUI launch never had the user's login PATH in the first place. So the
/// question goes to something that reads the environment fresh.
///
/// Nothing is done with the answer but show it. The app runs its own dsh by
/// absolute path and does not consult PATH, and a dsh that is not ours is not
/// ours to touch — see the note in `Add-Path` about what prepending can and
/// cannot win.
#[cfg(windows)]
fn terminal_dsh() -> Option<PathBuf> {
    // The machine's PATH followed by the user's, which is the order Windows
    // composes them in and the reason our own entry cannot outrank a `dsh`
    // installed machine-wide. `GetEnvironmentVariable` expands `REG_EXPAND_SZ`
    // for us; that is right here, where the value is being resolved rather than
    // written back.
    //
    // `dsh.cmd` before `dsh.ps1` because the .cmd is the one that can be spawned
    // for a `--version`. Within a single directory PowerShell would prefer the
    // .ps1, but npm writes all of them for the same install, so the answer to
    // "which dsh" is the same either way.
    const PROBE: &str = "\
        $path = [Environment]::GetEnvironmentVariable('Path','Machine') + ';' + \
                [Environment]::GetEnvironmentVariable('Path','User'); \
        foreach ($dir in ($path -split ';')) { \
            if (-not $dir) { continue } \
            foreach ($name in 'dsh.cmd','dsh.exe','dsh.bat','dsh.ps1','dsh') { \
                $file = Join-Path $dir $name; \
                if (Test-Path -LiteralPath $file -PathType Leaf) { $file; exit } \
            } \
        }";

    let mut command = Command::new("powershell.exe");
    command
        .args(["-NoProfile", "-NonInteractive", "-Command", PROBE])
        .stdin(Stdio::null());
    command.creation_flags(CREATE_NO_WINDOW);

    found(printed(command, PROBE_TIMEOUT))
}

/// The login shell's own answer, because only it knows what the user's profile
/// does to PATH: `~/.local/bin`, where the launcher is linked, is not on a
/// stock macOS PATH until something puts it there.
///
/// `-l` for the login files and `-i` for the interactive ones — `~/.zshrc` is
/// read for the second, `~/.zprofile` for the first, and a user can have either.
/// A shell that refuses the combination (dash has no `-l`) is asked again
/// without it rather than reported as "no dsh at all".
#[cfg(not(windows))]
fn terminal_dsh() -> Option<PathBuf> {
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());

    let ask = |flags: &str| {
        let mut command = Command::new(&shell);
        command.args([flags, "command -v dsh"]).stdin(Stdio::null());
        found(printed(command, PROBE_TIMEOUT))
    };

    ask("-lic").or_else(|| ask("-c"))
}

/// The probe's output as a path, and `None` for the empty line a probe that
/// found nothing prints.
fn found(printed: Option<String>) -> Option<PathBuf> {
    let path = printed?;
    let path = path.trim();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// The dsh command this launch will run.
pub struct Install {
    /// The executable to spawn: our Node, or whatever `DSH_BIN` named.
    program: PathBuf,
    /// What goes before dsh's own arguments — the entry script, when `program`
    /// is our Node. `None` for a `DSH_BIN`, which is a dsh already.
    prelude: Option<PathBuf>,
    pub version: Version,
}

impl Install {
    /// A command that runs this dsh, for the caller to add arguments to.
    ///
    /// Built here rather than by each caller, because the two halves have to
    /// stay together: spawning `program` without `prelude` starts a bare Node
    /// REPL, which fails in a way that says nothing about why.
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        if let Some(entry) = &self.prelude {
            command.arg(entry);
        }
        command
    }

    /// Whether this is the runtime the app installed, and so the one an update
    /// may replace. A `DSH_BIN` is the developer's own business.
    pub fn ours(&self) -> bool {
        self.prelude.is_some()
    }
}

/// Find it, the same way for every caller, so that the version being checked is
/// the version that will run.
///
/// `None` means the machine has no runtime yet, which [`gate`] answers by
/// installing one. There is no third state to report: the tree is ours, so it is
/// either there or it is not, and a half-written one fails the manifest read.
pub fn current(app: &AppHandle) -> Option<Install> {
    if let Some(bin) = pinned() {
        return Some(Install {
            version: version_of(bin.as_os_str())?,
            program: bin,
            prelude: None,
        });
    }

    Some(Install {
        program: node(app)?,
        prelude: Some(entry(app)?),
        version: manifest_version(app)?,
    })
}

/// The `version` field of the installed package's manifest — a file read rather
/// than a `dsh --version`, which costs a Node startup on the way to the window.
fn manifest_version(app: &AppHandle) -> Option<Version> {
    let manifest = modules(app)?.join(PACKAGE).join("package.json");
    let manifest = std::fs::read_to_string(manifest).ok()?;
    let manifest: serde_json::Value = serde_json::from_str(&manifest).ok()?;
    Version::parse(manifest.get("version")?.as_str()?).ok()
}

/// Ask a dsh what version it is.
///
/// Only for `DSH_BIN`, which can point at a checkout laid out however the
/// developer likes and so has no manifest where one would expect it. The
/// runtime's version is read off its own manifest above.
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
    // Two ways to end up in `provision`, and they run the same command.
    //
    // There is no runtime: a fresh machine, or a `tauri dev` build. Nothing is
    // looked for and nothing is borrowed — `-Mode install` fetches the Node it
    // needs and builds the tree from nothing, on purpose.
    //
    // Or there is one, and an older release of this app laid it down. See
    // [`provisioned`]: what the script installs is free to change between
    // releases, and this is what makes that reach a machine that already has a
    // working runtime.
    //
    // A `DSH_BIN` is neither. It is the developer's own checkout, there is no
    // runtime of ours behind it, and provisioning one would fetch 185 MB that
    // nothing is going to run.
    let Some(installed) = current(app).filter(|dsh| !dsh.ours() || provisioned(app)) else {
        return provision(app, report);
    };

    if checked_recently(app) {
        return true;
    }

    report(t!("正在检查 dsh 更新…", "Checking for a dsh update…"), -1.0);

    let Some(latest) = latest(app) else {
        eprintln!(
            "dsh-desktop: could not ask for the latest dsh; using the installed {}",
            installed.version
        );
        return true;
    };
    mark_checked(app);

    if latest <= installed.version || skipped(app).is_some_and(|skipped| skipped == latest) {
        return true;
    }

    // A `DSH_BIN` is the developer's checkout and not ours to replace. Telling
    // them is the whole of it, and recording it as skipped keeps that to once
    // per release rather than every time the six hours are up.
    if !installed.ours() {
        tell(app, &installed.version, &latest);
        skip(app, &latest);
        return true;
    }

    if !ask(app, &installed.version, &latest) {
        skip(app, &latest);
        return true;
    }

    // Nothing is running yet, so there is nothing to stop and nothing to restart
    // for: npm replaces the tree in place and the boot carries straight on into
    // the new version.
    update(app, &installed.version, report)
}

/// What the window shows while this is waiting on npm, and `""` when the wait is
/// over. See [`crate::controls::busy`] — the caller owns the window, this only
/// knows when there is something to wait for.
pub type Saying<'a> = dyn Fn(&str) + 'a;

/// A dsh update the user asked for from the menu, rather than one a launch
/// happened to find. Answers with the version being replaced once they have
/// agreed to it, for the caller to hand to [`update`] with dsh stopped.
///
/// There is no prefix to hand back any more: [`update`] installs into the
/// runtime, which is where dsh already is and the only place it can be.
///
/// Every outcome is reported, including "nothing to do": a menu item that does
/// nothing visible looks broken. A version turned down earlier is offered again
/// — the whole point of this is being asked.
pub fn requested(app: &AppHandle, saying: &Saying) -> Option<Version> {
    // `latest` is a `npm view`, which is the network and up to CHECK_TIMEOUT of
    // it. Everything after it is instant, so the line comes down here rather
    // than in front of each of the dialogs below.
    saying(t!("正在检查 dsh 更新…", "Checking for a dsh update…"));
    let found = current(app).map(|installed| {
        let latest = latest(app);
        (installed, latest)
    });
    saying("");

    let Some((installed, latest)) = found else {
        note(
            app,
            t!("找不到 dsh", "No dsh found"),
            t!(
                "这台机器上还没有装好的 dsh。重启应用会再装一次。",
                "There is no working dsh on this machine. Restarting the app installs one."
            ),
        );
        return None;
    };

    let Some(latest) = latest else {
        note(
            app,
            t!("检查 dsh 更新失败", "Could not check for a dsh update"),
            t!(
                "无法查询 dsh 的最新版本，通常是网络或代理的问题。",
                "The latest dsh version could not be looked up, which is usually the network or a proxy."
            ),
        );
        return None;
    };
    mark_checked(app);

    if latest <= installed.version {
        note(
            app,
            t!("dsh 已是最新版本", "dsh is up to date"),
            &t!(
                "当前的 dsh {} 已经是最新的。",
                "The installed dsh {} is the latest there is.",
                installed.version
            ),
        );
        return None;
    }

    if !installed.ours() {
        tell(app, &installed.version, &latest);
        return None;
    }

    if !confirm(app, &installed.version, &latest) {
        return None;
    }

    Some(installed.version)
}

/// Replace the dsh in the runtime with the newest release, reporting progress
/// onto the loading page. `false` means the app quit while npm was still
/// running.
///
/// No prefix is passed and none is worked out. `install` and `update` are the
/// same npm command into the same directory, which is why this is now the
/// script's shortest mode — where it used to have to be told which of the
/// machine's global prefixes held the dsh being replaced, because installing
/// into the wrong one built a second copy nothing would ever run.
///
/// Failure is reported here and answers `true` all the same — there is a working
/// dsh on disk either way, which is the one the caller goes on to run.
pub fn update(app: &AppHandle, installed: &Version, report: &Report) -> bool {
    let args = [OsStr::new("-Mode"), OsStr::new("update")];

    match run(app, &args, report) {
        Ok(true) => {
            report("", -1.0);
            true
        }
        // Cut short because the app is quitting. Nothing to report, and by now
        // nowhere left to report it.
        Ok(false) => false,
        Err(error) => {
            eprintln!("dsh-desktop: updating dsh failed: {error}");
            report("", -1.0);
            note(
                app,
                t!("dsh 更新失败", "Updating dsh failed"),
                &t!(
                    "更新 dsh 时出错，将继续使用当前的 {}。\n\n{}",
                    "Something went wrong updating dsh; the installed {} stays in use.\n\n{}",
                    installed,
                    error
                ),
            );
            true
        }
    }
}

/// Ask before deleting it. Blocking, like every other dialog here.
///
/// Everything this is about to do is spelled out, because none of it is
/// recoverable by clicking again: what goes, what stays, and that the app is
/// leaving. The default is the one that changes nothing.
pub fn confirm_removal(app: &AppHandle) -> bool {
    crate::dialog::confirm(
        app,
        crate::dialog::Ask {
            title: t!("移除 dsh 运行时", "Remove the dsh runtime").to_string(),
            body: t!(
                "将删除应用自己安装的 Node 和 dsh（约 {}），以及终端里的 dsh 命令。\n\n\
                 你自己安装的 Node 和 dsh 不受影响，dsh 的配置和会话记录也会保留。\n\n\
                 正在运行的 dsh 会先停下，应用随后退出。下次启动时会重新安装一份。",
                "This deletes the Node and dsh the app installed ({}), and the dsh \
                 command in your terminal.\n\n\
                 A Node or a dsh you installed yourself is untouched, and dsh's own \
                 settings and sessions are kept.\n\n\
                 The running dsh is stopped first and the app then quits. The next \
                 launch installs a fresh copy.",
                installed_size()
            ),
            choices: vec![
                crate::dialog::Choice::new("cancel", t!("取消", "Cancel")),
                crate::dialog::Choice::primary("remove", t!("移除并退出", "Remove and quit")),
            ],
            answered: Box::new(|_, _| {}),
        },
        "remove",
    )
}

/// Delete the runtime: `<app data>/runtime`, the launcher, and the PATH entry or
/// symlink that pointed at it. `false` if the app quit while it was running.
///
/// The same `-Mode uninstall` the Windows uninstaller offers, reached from the
/// settings panel so that macOS and Linux — where nothing has ever called it —
/// have a way to it at all. What it can name is the app's own directory and
/// nothing else; see the function in either script.
pub fn remove(app: &AppHandle, report: &Report) -> bool {
    report(t!("正在移除 dsh 运行时…", "Removing the dsh runtime…"), -1.0);

    match run(app, &[OsStr::new("-Mode"), OsStr::new("uninstall")], report) {
        Ok(true) => true,
        Ok(false) => false,
        Err(error) => {
            eprintln!("dsh-desktop: removing the runtime failed: {error}");
            report("", -1.0);
            note(
                app,
                t!("移除失败", "Could not remove it"),
                &t!(
                    "删除 dsh 运行时时出错。\n\n{}",
                    "Something went wrong deleting the dsh runtime.\n\n{}",
                    error
                ),
            );
            false
        }
    }
}

/// Build the runtime this release wants, on every platform, at first launch.
///
/// This is the only caller of `-Mode install` there is. The Windows installer
/// used to run it too, from `NSIS_HOOK_POSTINSTALL`, which made Windows the one
/// platform where the script re-ran on an app upgrade — and so the one platform
/// a bumped `NODE_VERSION` ever reached. Taking that out left one code path, one
/// place an install can fail, and one progress bar to report it on.
///
/// `false` if the app quit while it was running.
fn provision(app: &AppHandle, report: &Report) -> bool {
    report(t!("正在准备运行环境…", "Preparing the runtime…"), -1.0);

    match run(app, &[OsStr::new("-Mode"), OsStr::new("install")], report) {
        Ok(true) => {
            mark_provisioned(app);
            report("", -1.0);
            true
        }
        Ok(false) => false,
        Err(error) => {
            eprintln!("dsh-desktop: installing dsh failed: {error}");
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
fn run(app: &AppHandle, args: &[&OsStr], report: &Report) -> Result<bool, String> {
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

    // Claimed before the spawn, so that two callers cannot both end up in the
    // slot below with one of the children left unowned. `main` keeps them from
    // reaching this at once at all; this is the backstop that makes the slot's
    // invariant true rather than merely likely.
    let mut running = RUNNING.lock().unwrap();
    if running.is_some() {
        return Err(t!(
            "已经有一个 dsh 安装或更新在进行中",
            "a dsh install or update is already running"
        )
        .to_string());
    }

    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or(t!("无法读取脚本输出", "the script's output cannot be read"))?;

    #[cfg(windows)]
    let job = crate::server::Job::hold(&child);

    *running = Some(Running {
        child,
        #[cfg(windows)]
        _job: job,
    });
    drop(running);

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
pub fn npm(app: &AppHandle) -> Option<Command> {
    let node = node(app)?;
    // The npm that Node's own archive ships, in the place that archive puts it:
    // beside `node.exe` on Windows, and under `lib` on the Unix tarballs. Two
    // fixed spellings rather than a search — the whole of what this function
    // used to be was working out which of the machine's Nodes had an npm and
    // where that npm kept its entry point.
    let root = runtime(app)?.join("node");
    let cli = if cfg!(windows) { root } else { root.join("lib") }
        .join("node_modules")
        .join("npm")
        .join("bin")
        .join("npm-cli.js");
    if !cli.is_file() {
        return None;
    }

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
    crate::dialog::confirm(
        app,
        crate::dialog::Ask {
            title: t!("dsh 有可用更新", "A dsh update is available").to_string(),
            body: t!(
                "dsh 有新版本 {}（当前 {}）。\n\n\
                 下载约需 {}。更新期间应用会等待，完成后直接启动。",
                "dsh {} is available (this machine has {}).\n\n\
                 The download is {}. The app waits for it and then starts.",
                latest,
                installed,
                download_size()
            ),
            choices: vec![
                crate::dialog::Choice::new("skip", t!("跳过此版本", "Skip this version")),
                crate::dialog::Choice::primary("update", t!("更新", "Update")),
            ],
            // Replaced by `confirm`; it is the channel send that answers.
            answered: Box::new(|_, _| {}),
        },
        "update",
    )
}

/// The same question from the tray, where dsh is already serving the window.
///
/// Also blocking: the answer decides whether the server comes down, and the
/// caller has nothing to do until it arrives.
fn confirm(app: &AppHandle, installed: &Version, latest: &Version) -> bool {
    crate::dialog::confirm(
        app,
        crate::dialog::Ask {
            title: t!("更新 dsh", "Update dsh").to_string(),
            body: t!(
                "dsh 有新版本 {}（当前 {}）。\n\n\
                 下载约需 {}。更新前会先关闭正在运行的 dsh —— \
                 正在进行的会话会被中断 —— 完成后自动重新启动。",
                "dsh {} is available (this machine has {}).\n\n\
                 The download is {}. The running dsh is stopped first — a session in \
                 progress will be interrupted — and started again when it is done.",
                latest,
                installed,
                download_size()
            ),
            choices: vec![
                crate::dialog::Choice::new("cancel", t!("取消", "Cancel")),
                crate::dialog::Choice::primary("update", t!("更新", "Update")),
            ],
            answered: Box::new(|_, _| {}),
        },
        "update",
    )
}

/// Tell the user about an update to a dsh the app is not running out of its own
/// runtime, and leave it at that.
///
/// The only way to reach this is `DSH_BIN`, which is a developer pointing the
/// app at their own checkout. Updating it would overwrite work in progress, and
/// the app has no idea what put it there — so the command goes in the message
/// instead. Every other install is in the runtime, where an update is just the
/// same npm command again.
fn tell(app: &AppHandle, installed: &Version, latest: &Version) {
    note(
        app,
        t!("dsh 有可用更新", "A dsh update is available"),
        &t!(
            "dsh 有新版本 {}（当前 {}）。\n\n\
             当前运行的是 DSH_BIN 指定的 dsh，不是应用自己安装的那份，\
             应用不会去改动它。要更新的话，用你当初安装它的方式，\
             在终端里执行：\n\nnpm install -g {}@latest",
            "dsh {} is available (this machine has {}).\n\n\
             The app is running the dsh that DSH_BIN points at rather than its \
             own, so it will not touch it. To update that one, use whatever you \
             installed it with:\n\nnpm install -g {}@latest",
            latest,
            installed,
            PACKAGE
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
        eprintln!("dsh-desktop: could not remember the skipped dsh version: {error}");
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
        eprintln!("dsh-desktop: could not record the dsh update check: {error}");
    }
}

fn checked_file(app: &AppHandle) -> Option<PathBuf> {
    Some(app_dir(app)?.join("dsh-checked"))
}

/// Whether the runtime on disk was laid down by *this* build of the app.
///
/// The install script installs whatever it currently says to install: a pinned
/// Node version, a launcher with a particular body, a set of directories on
/// PATH, whatever migration the release needed. Every one of those can change,
/// and none of them changes on a machine the script does not run on again.
///
/// Which used to mean they only reached Windows. `NSIS_HOOK_POSTINSTALL` re-ran
/// `-Mode install` on every upgrade, and nothing did on macOS or Linux — a
/// runtime there was provisioned once, on the day of first launch, and then
/// left alone forever. A bumped `NODE_VERSION` would have split the platforms
/// silently.
///
/// So the app records which version built the tree and re-runs the script when
/// that stops matching. Re-running is cheap: the Node download is skipped when
/// the stamp beside it already names the pinned version, and npm's cache is
/// per-user, so the packages are usually already there.
///
/// The record lives inside the runtime rather than beside it, so that removing
/// the runtime removes the claim that one exists; see `-Mode uninstall`.
fn provisioned(app: &AppHandle) -> bool {
    let Some(path) = provisioned_file(app) else {
        return false;
    };
    let wanted = app.package_info().version.to_string();

    std::fs::read_to_string(path).is_ok_and(|by| by.trim() == wanted)
}

/// Write down which version built it, once it is built.
fn mark_provisioned(app: &AppHandle) {
    let Some(path) = provisioned_file(app) else { return };

    if let Err(error) = std::fs::write(&path, app.package_info().version.to_string()) {
        // The cost is one more `-Mode install` on the next launch, which is
        // idempotent and mostly cache hits. Not worth interrupting anyone over.
        eprintln!("dsh-desktop: could not record what provisioned the runtime: {error}");
    }
}

fn provisioned_file(app: &AppHandle) -> Option<PathBuf> {
    Some(runtime(app)?.join(".provisioned"))
}

/// `%LOCALAPPDATA%\<identifier>`, `~/Library/Application Support/<identifier>`,
/// `~/.local/share/<identifier>`. Both bootstrap scripts and
/// `installer-hooks.nsh` build the same path out of the platform's own variable
/// and the bundle identifier; they all have to agree.
pub fn app_dir(app: &AppHandle) -> Option<PathBuf> {
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

/// A dialog with nothing to answer. Public because the tray has one thing to say
/// that this module knows nothing about — see `update_dsh` in `main.rs`.
pub fn note(app: &AppHandle, title: &str, detail: &str) {
    crate::dialog::ask(
        app,
        crate::dialog::Ask {
            title: title.to_string(),
            body: detail.to_string(),
            choices: vec![crate::dialog::Choice::primary("ok", t!("知道了", "OK"))],
            answered: Box::new(|_, _| {}),
        },
    );
}

/// Put the runtime's own directories at the front of a child's PATH.
///
/// Two constants, where this used to be a three-level search with the inherited
/// PATH behind it. dsh shells out to `node` for workers and plugin tooling, and
/// it runs pnpm for every plugin install; both have to reach ours rather than
/// whichever Node a version manager happens to have active, because that is the
/// one dsh's native modules were built against.
///
/// The inherited PATH still follows, so everything else the user has stays
/// reachable — this decides which `node` wins, not which commands exist.
pub fn apply_path(app: &AppHandle, command: &mut Command) {
    if let Ok(path) = std::env::join_paths(child_path(app)) {
        command.env("PATH", path);
    }
}

/// The PATH a child of ours gets: three directories of the runtime's, then
/// whatever this process inherited.
///
/// One definition, because [`apply_path`] and [`terminal`] both need it and a
/// terminal whose PATH disagreed with the app's would run a different dsh than
/// the window does — which is the confusing half of every bug report this
/// module has ever produced.
fn child_path(app: &AppHandle) -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    // The launcher first, so a bare `dsh` here is the one the window runs. It
    // has to come before the Node directory rather than after it: on Windows
    // that directory *is* npm's global prefix, so an `npm i -g` typed into the
    // terminal this opens lands a second `dsh` shim in it — and a shim ahead of
    // our launcher would be the one answering, in this terminal only, which is
    // the sort of difference nobody would ever guess at.
    if let Some(dir) = app_dir(app) {
        dirs.push(dir.join("bin"));
    }
    // Then our Node, so that a `node` spelled bare by dsh, by a plugin's install
    // script, or by npm's own shim resolves to the Node its native modules were
    // built against rather than to whatever a version manager has active. The
    // launcher directory holds one file and none of these three names, so
    // putting it in front costs this nothing.
    if let Some(node) = node_dir(app) {
        dirs.push(node);
    }
    // npm's shims for the local install, which is how dsh finds `pnpm`.
    if let Some(modules) = modules(app) {
        dirs.push(modules.join(".bin"));
    }

    // Behind ours rather than instead of it: this decides which `node` wins,
    // not which commands exist, and everything else the user has stays
    // reachable.
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    dirs.extend(std::env::split_paths(&inherited));

    dirs
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

/// Open a terminal that has dsh in it.
///
/// Less load-bearing than it used to be. The install script puts
/// `<app data>/bin` on the user's PATH, so `dsh` is a command they can type in
/// any shell — this used to be the *only* way to reach the CLI, because nothing
/// was written to PATH at all and a machine where the app had installed its own
/// Node had a working dsh the user could not name.
///
/// It is still worth having. Writing the entry can fail — a locked registry, a
/// PATH already at the length limit — and on Windows it cannot outrank a dsh on
/// the machine PATH however it is written; this reaches the app's copy either
/// way, because the PATH it hands the shell is built here rather than read.
///
/// What it opens is one terminal with the same PATH the app's own children get,
/// for as long as it is open, and nothing left behind when it closes.
///
/// The environment travels by inheritance rather than as a command to run: the
/// child is spawned with the PATH already set and the shell it opens inherits
/// it. Writing it into a command line instead would mean quoting a list of
/// Windows paths through two levels of `cmd`.
pub fn terminal(app: &AppHandle) -> Result<(), String> {
    #[cfg(windows)]
    {
        // `start` reads its first quoted argument as a window title, so it is
        // given one rather than eating the command. `dsh --version` is the
        // greeting: ASCII, instant, and it answers the only question this
        // terminal was opened to settle.
        let mut command = Command::new("cmd");
        command.args(["/c", "start", "dsh", "cmd", "/k", "dsh --version"]);
        apply_path(app, &mut command);
        command.spawn().map(|_| ()).map_err(|error| error.to_string())
    }

    #[cfg(target_os = "macos")]
    {
        // Terminal.app takes a file to run, not an environment, so the
        // environment is the file. Rewritten on every open rather than cached:
        // the paths move when dsh is reinstalled somewhere else.
        let script = app_dir(app)
            .ok_or_else(|| t!("找不到应用数据目录", "no application data directory").to_string())?
            .join("dsh-shell.command");
        if let Some(parent) = script.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let path = std::env::join_paths(child_path(app))
            .map_err(|error| error.to_string())?
            .to_string_lossy()
            .into_owned();
        // Not a login shell: one would re-read the user's profile, which is
        // free to put its own Node back in front of ours.
        let body = format!(
            "#!/bin/sh\nPATH={}\nexport PATH\nexec \"${{SHELL:-/bin/sh}}\"\n",
            shell_quote(&path)
        );
        std::fs::write(&script, body).map_err(|error| error.to_string())?;

        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .map_err(|error| error.to_string())?;

        Command::new("open")
            .args([OsStr::new("-a"), OsStr::new("Terminal"), script.as_os_str()])
            .spawn()
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // No single answer here, so the list is walked until one of them
        // starts. `x-terminal-emulator` comes first because on Debian it is
        // whatever the user already chose.
        const TERMINALS: [&str; 6] = [
            "x-terminal-emulator",
            "gnome-terminal",
            "konsole",
            "xfce4-terminal",
            "alacritty",
            "xterm",
        ];

        for name in TERMINALS {
            let mut command = Command::new(name);
            apply_path(app, &mut command);
            if command.spawn().is_ok() {
                return Ok(());
            }
        }

        Err(t!(
            "没有找到可用的终端程序。",
            "no terminal emulator could be started."
        )
        .to_string())
    }
}

/// One argument, safe to paste into a `sh` script. Single quotes take everything
/// literally, and the only thing they cannot hold is a single quote.
#[cfg(target_os = "macos")]
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::bin_field;

    /// Most of what used to be tested here is gone with what it covered: a
    /// marker parser that had to strip a BOM, a `prefix_of` that told npm's two
    /// global layouts apart, a `present`/`executable` pair whose whole job was
    /// to call a dangling symlink "there but broken", and six functions probing
    /// prefixes for writability. None of those questions can be asked any more
    /// — the runtime sits at a constant path, it is a local install with one
    /// layout on every platform, it contains no symlinks, and the app owns the
    /// directory it is in.
    ///
    /// What is left is the one thing still genuinely parsed on this side: the
    /// `bin` field of a manifest we did not write. `install-deps.ps1` reads the
    /// same field to build the terminal launcher — see `Get-DshEntry` — so the
    /// two have to agree about what it can look like.

    #[test]
    fn reads_a_bin_field_written_as_a_bare_string() {
        let manifest: serde_json::Value = serde_json::from_str(r#"{"bin": "./cli.js"}"#).unwrap();
        assert_eq!(bin_field(manifest.get("bin").unwrap()), Some("cli.js"));
    }

    #[test]
    fn reads_a_bin_field_written_as_an_object() {
        let manifest: serde_json::Value =
            serde_json::from_str(r#"{"bin": {"dsh": "./bin/dsh.js"}}"#).unwrap();
        assert_eq!(bin_field(manifest.get("bin").unwrap()), Some("bin/dsh.js"));
    }

    /// A package that names binaries but not one called `dsh` is not a dsh.
    /// Saying so here is what makes [`super::current`] report an unusable
    /// runtime rather than hand back an entry point belonging to something else.
    #[test]
    fn refuses_an_object_that_names_no_dsh() {
        let manifest: serde_json::Value =
            serde_json::from_str(r#"{"bin": {"dshx": "./cli.js"}}"#).unwrap();
        assert_eq!(bin_field(manifest.get("bin").unwrap()), None);
    }

    /// `./` is npm's spelling of "here" and not part of the path, but only at
    /// the front. Trimming every `./` would corrupt a nested one.
    #[test]
    fn strips_only_the_leading_dot_slash() {
        let manifest: serde_json::Value =
            serde_json::from_str(r#"{"bin": "lib/./cli.js"}"#).unwrap();
        assert_eq!(bin_field(manifest.get("bin").unwrap()), Some("lib/./cli.js"));
    }

    /// A `bin` that is neither a string nor an object — npm would reject the
    /// package, but the manifest is not ours and a panic here is a window that
    /// never opens.
    #[test]
    fn refuses_a_bin_field_that_is_not_a_name_at_all() {
        let manifest: serde_json::Value = serde_json::from_str(r#"{"bin": ["./cli.js"]}"#).unwrap();
        assert_eq!(bin_field(manifest.get("bin").unwrap()), None);
    }
}
