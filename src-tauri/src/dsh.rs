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
//! has to detect Node, fetch and verify a Node archive, and measure a list of
//! registry mirrors before walking it, and a second copy of all that would
//! drift.
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

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The package the whole thing is about.
const PACKAGE: &str = "@deepseek-ai/dsh";

/// The names npm's shims go by, most specific first: the `.cmd` wrapper npm
/// writes on Windows, then the bare name it uses everywhere else.
///
/// Named once and shared, because a typo in one copy is a tool the app decides
/// is missing — and the copies are far apart: [`current`] resolves the dsh this
/// launch runs, while `plugins.rs` asks [`tool`] about the same two names to
/// tell a broken shim from an absent one. Listing both on every platform is
/// deliberate: [`look_up`] joins each onto directories that only hold one of
/// them, so the wrong name simply never matches.
pub const DSH: &[&str] = &["dsh.cmd", "dsh"];
/// pnpm's, which dsh forwards every plugin install to; see `plugins.rs`.
pub const PNPM: &[&str] = &["pnpm.cmd", "pnpm"];
/// Node's, which is what [`npm`] runs npm's own entry point with.
const NODE: &[&str] = &["node.exe", "node"];

/// What a dsh install costs over the wire. Quoted to the user before they agree
/// to it, because it is a lot. Measured, not estimated: 587 packages, 185 MB of
/// tarballs, four minutes on a 2 MB/s link.
fn download_size() -> &'static str {
    t!("约 185 MB", "about 185 MB")
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

/// What `install-deps.ps1` wrote down about what it installed. Absent on a
/// machine where it has never run, or has never had anything to do.
///
/// This is how the app finds a Node that is on the user's PATH but not on this
/// process's — see the module docs.
#[derive(Default)]
struct Bootstrap {
    /// The Node the script settled on, ours or the machine's.
    node: Option<PathBuf>,
    /// npm's entry point beside it.
    npm: Option<PathBuf>,
    /// Where `npm install -g` puts things, which is also where `dsh.cmd` and
    /// the `node_modules` holding dsh live.
    prefix: Option<PathBuf>,
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
    }
}

/// Where npm puts a global package's shims for a given prefix: the prefix itself
/// on Windows, and `<prefix>/bin` everywhere else.
///
/// One definition, because two places depend on agreeing with it — the directory
/// [`search_path`] searches, and the directory a `--prefix` install has to land
/// its shims in for that search to find them. They were the same expression
/// written twice, which is how they come apart.
pub fn shim_dir(prefix: &Path) -> PathBuf {
    if cfg!(windows) {
        prefix.to_path_buf()
    } else {
        prefix.join("bin")
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
    let shims = state.prefix.as_deref().map(shim_dir);
    let node_dir = state
        .node
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf);

    // The app's own prefix, which is where [`tool_prefix`] puts a tool when
    // neither the recorded nor the installed prefix can be written to. Listed
    // unconditionally and without creating it: a tool installed there has to be
    // findable afterwards, and on the machines that never need it this is one
    // directory that does not exist and matches nothing.
    //
    // Last of the three, so a pnpm beside dsh still wins over a stale copy here.
    let own = app_dir(app).map(|dir| shim_dir(&dir.join("npm")));

    let inherited = std::env::var_os("PATH").unwrap_or_default();
    [shims, node_dir, own]
        .into_iter()
        .flatten()
        .chain(std::env::split_paths(&inherited))
        .collect()
}

/// The first of `names` that exists in [`search_path`], as an absolute path.
///
/// Resolved here rather than left to `Command::new`, which searches the PATH
/// this process started with — the one that predates anything the installer did.
pub fn look_up(app: &AppHandle, names: &[&str]) -> Option<PathBuf> {
    search_path(app).into_iter().find_map(|dir| {
        names
            .iter()
            .map(|name| dir.join(name))
            .find(|candidate| present(candidate))
    })
}

/// Whether npm left an entry at `path` — the question [`look_up`] is actually
/// asking, which is not the same as `is_file`.
///
/// On Windows a global shim is a plain `.cmd` file and the two agree. Everywhere
/// else it is a symlink into `<prefix>/lib/node_modules`, and `is_file` follows
/// it: a shim whose target is gone — a Node switched by nvm/fnm/asdf/volta, a
/// half-removed global package, a distro Node upgraded underneath — reads as
/// *absent*, so this said "no pnpm" and "no dsh" on a machine that has both
/// names sitting right there in the prefix.
///
/// Answering that with the file gone is worse than answering it wrongly: the
/// caller reinstalls over a broken link, or reports that there is no dsh at all
/// while `dsh` works in the user's terminal. `symlink_metadata` does not follow
/// the link, so a dangling shim is found, handed back, and allowed to fail as
/// what it is — see [`executable`], which is where a broken one gets caught with
/// something specific to say.
fn present(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        // A directory named `pnpm` is not the pnpm we are looking for.
        Ok(metadata) => !metadata.is_dir(),
        Err(_) => false,
    }
}

/// Whether `path` resolves to something that can actually be run.
///
/// [`present`] deliberately accepts a dangling symlink, so somebody has to be
/// the one that notices. This is it: `metadata` follows the link, so this is
/// false exactly when the shim points at nothing — and on Unix it also checks
/// the executable bit, because a global bin that lost it fails to spawn with an
/// error no more helpful than the dangling case.
fn executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if metadata.is_dir() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return metadata.permissions().mode() & 0o111 != 0;
    }
    #[cfg(not(unix))]
    true
}

/// What [`look_up`] found, as the three states a caller actually has to tell
/// apart.
///
/// A pair of `Option`-returning calls — one for "is it there", one for "is it
/// broken" — makes the interesting state the *absence* of one answer and the
/// presence of another, which every caller has to reassemble correctly and no
/// single call can answer. It also walks the search path twice, so the two
/// answers can describe different files if the directory changes in between.
/// One enum from one walk removes both problems.
pub enum Tool {
    /// There and runnable.
    ///
    /// No path: every caller so far only needs to know that it is usable,
    /// because what actually runs it is dsh through [`apply_path`] rather than
    /// this app spawning it directly. Carrying one nobody reads is a promise
    /// that the value has been checked for the caller's purpose, which it has
    /// not.
    Ready,
    /// There but not runnable, with the reason spelled out for the user. The
    /// state that used to be invisible: the name is in the prefix, so nothing
    /// reinstalls it, and every attempt to run it fails far from the cause.
    Broken(String),
    /// Not on the search path at all.
    Missing,
}

/// Resolve one tool by its platform names, in one walk of the search path.
pub fn tool(app: &AppHandle, names: &[&str]) -> Tool {
    let Some(path) = look_up(app, names) else {
        return Tool::Missing;
    };
    if executable(&path) {
        return Tool::Ready;
    }

    let shown = path.display();
    Tool::Broken(match std::fs::read_link(&path) {
        // The common one: npm's symlink outliving what it pointed at.
        //
        // `read_link` hands back the target exactly as stored, and npm writes a
        // relative one (`../lib/node_modules/pnpm/bin/pnpm.cjs`). Testing that
        // as-is would resolve it against this process's working directory —
        // wherever the app happens to have been started — and call a perfectly
        // good shim dangling. So it is resolved against the link's own directory,
        // which is what the kernel does.
        Ok(target) if !resolve(&path, &target).exists() => t!(
            "{} 指向的目标已经不存在（{}）。这通常是切换过 Node 版本（nvm / fnm / asdf / volta）或全局包被删掉一半留下的断链。",
            "{} points at something that no longer exists ({}). That is usually a dangling link left by switching Node versions (nvm/fnm/asdf/volta) or a half-removed global package.",
            shown,
            target.display()
        ),
        Ok(target) => t!(
            "{} 指向 {}，但它不可执行。",
            "{} points at {}, which is not executable.",
            shown,
            target.display()
        ),
        Err(_) => t!(
            "{} 存在，但不可执行。",
            "{} exists but is not executable.",
            shown
        ),
    })
}

/// A symlink's target as an absolute path: relative to the directory the link
/// sits in, which is how the kernel reads it. An already-absolute target is
/// returned unchanged.
fn resolve(link: &Path, target: &Path) -> PathBuf {
    if target.is_absolute() {
        return target.to_path_buf();
    }
    link.parent().unwrap_or(Path::new(".")).join(target)
}

/// The dsh command this launch will run.
pub struct Install {
    /// What to execute. On Windows this is npm's `dsh.cmd` shim, which std
    /// routes through cmd.exe with the correct argument quoting.
    pub bin: PathBuf,
    pub version: Version,
    /// The npm global prefix this dsh sits in, when the tree around it is one
    /// `npm install -g` built; see [`prefix_of`]. Whether an update can actually
    /// be written there is [`updatable`]'s question.
    prefix: Option<PathBuf>,
}

/// Find it, the same way for every caller, so that the version being checked is
/// the version that will run.
///
/// 1. `DSH_BIN` — an explicit choice, so it wins outright.
/// 2. `dsh` in the npm prefix the bootstrap script recorded, or on PATH.
///
/// `None` means the machine has no dsh at all, which [`gate`] answers by
/// installing one — including the case where the name is in the prefix but the
/// shim behind it is broken, which is reinstalled over rather than left as a dsh
/// that cannot run. [`tool`] is what turns that into something to say.
pub fn current(app: &AppHandle) -> Option<Install> {
    let bin = match std::env::var_os("DSH_BIN") {
        Some(bin) => PathBuf::from(bin),
        None => {
            let found = look_up(app, DSH)?;
            // A dangling shim is not a dsh. Saying so here means `gate`
            // reinstalls instead of handing back an install whose every command
            // fails, and `version_of` below is not asked to run a broken link.
            if !executable(&found) {
                eprintln!(
                    "dsh-desktop: {} is not runnable; treating this machine as having no dsh",
                    found.display()
                );
                return None;
            }
            found
        }
    };

    let version = manifest_version(&root_of(&bin)).or_else(|| version_of(bin.as_os_str()))?;
    let prefix = prefix_of(&bin);

    Some(Install {
        bin,
        version,
        prefix,
    })
}

/// The npm global prefix holding `bin`, if the tree around it is the one a
/// global install builds.
///
/// On Windows the shim sits in the prefix with the package under the
/// `node_modules` beside it; everywhere else the shim is in `<prefix>/bin` and
/// the package under `<prefix>/lib/node_modules`. Something laid out a third way
/// is a version manager's shim, a pnpm link, or a `DSH_BIN` pointing into a
/// checkout, and `npm install -g --prefix` aimed at it would build a *second*
/// dsh in a directory nothing on PATH looks at rather than replacing the one
/// that is there.
fn prefix_of(bin: &Path) -> Option<PathBuf> {
    let dir = bin.parent()?;
    let manifest = Path::new("node_modules").join(PACKAGE).join("package.json");

    if dir.join(&manifest).is_file() {
        return Some(dir.to_path_buf());
    }

    let prefix = dir.parent()?;
    prefix
        .join("lib")
        .join(&manifest)
        .is_file()
        .then(|| prefix.to_path_buf())
}

/// The prefix to install an update into: the one this dsh is in, once npm has
/// somewhere it can actually write. A `sudo npm i -g` into `/usr/local`, or a
/// distribution's `/usr`, answers `None` — asking for a password is no more on
/// the table here than it is in the install script.
///
/// Probed rather than read off a permission bit: Windows has ACLs rather than a
/// mode, and on Unix the answer depends on the user's groups, so the only
/// reliable form of the question is the one npm is about to ask anyway. It is
/// not part of [`current`] because that one only wants to know where dsh is, and
/// writing to answer it would be a side effect on every launch.
fn updatable(installed: &Install) -> Option<&Path> {
    let prefix = installed.prefix.as_deref()?;
    writable(&package_root(prefix)).then_some(prefix)
}

/// Whether a file can be created in `dir` — the only reliable form of the
/// question, for the reasons in [`updatable`]. Shared with
/// [`unwritable_prefix`] so the update path and the plugin path cannot come to
/// different conclusions about the same directory.
fn writable(dir: &Path) -> bool {
    // The pid keeps two copies of the app out of each other's way. There is only
    // ever meant to be one — see the single-instance plugin — but a file left
    // behind by a crash would otherwise be a permanent "no".
    let probe = dir.join(format!(".dsh-write-probe-{}", std::process::id()));
    let allowed = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    allowed
}

/// The npm global prefix a `-g` install would land in, when this process cannot
/// write to it — `None` when it can, or when there is no prefix to check.
///
/// This is the Linux failure the plugin panel used to report as a bare npm exit
/// code: a distribution's Node keeps its global prefix under `/usr`, so
/// `npm install -g pnpm` fails with `EACCES` for reasons that have nothing to do
/// with the network or the registry. Windows never sees it, because npm's prefix
/// there is a per-user directory under `%APPDATA%`.
///
/// The probe is aimed at the deepest directory that already exists, since npm
/// creates the rest: probing a missing `lib/node_modules` would report every
/// fresh prefix as unwritable.
pub fn unwritable_prefix(app: &AppHandle) -> Option<PathBuf> {
    let prefix = bootstrap(app)
        .prefix
        .or_else(|| current(app).and_then(|install| install.prefix))?;

    // A prefix that is not there at all is not reported as unwritable:
    // "missing" is a different problem, and the advice below — move npm's
    // prefix — would not address it.
    if !prefix.is_dir() {
        return None;
    }

    // The deepest level that exists, which is the one npm would write into and
    // so the one worth naming. `usable_prefix` asks the same question of the
    // same directory; this reports *which* directory rather than yes or no, so
    // they stay in agreement by construction.
    let existing = deepest_existing(&prefix)?;
    (!writable(&existing)).then_some(existing)
}

/// The deepest directory under `prefix` that already exists, walking up from the
/// `node_modules` a `-g` install lands in. `None` when `prefix` itself is gone.
///
/// npm creates the levels below, so probing a missing `lib/node_modules` would
/// reject a perfectly good fresh prefix. The walk stops at `prefix` because
/// stepping above it reaches directories npm would never touch — accepting one
/// of those is how a nonexistent prefix passes for a usable one.
fn deepest_existing(prefix: &Path) -> Option<PathBuf> {
    package_root(prefix)
        .ancestors()
        .take_while(|dir| dir.starts_with(prefix))
        .find(|dir| dir.is_dir())
        .map(Path::to_path_buf)
}

/// Where a `-g` install of a tool this app needs should go, and whether that had
/// to be chosen rather than left to npm.
///
/// `install-deps.sh` already solves this for dsh itself — see `find_prefix`
/// there: it takes npm's configured global prefix when the user can write to it,
/// and otherwise falls back to a prefix of the app's own under the application
/// data directory, because installing as the user who will run it is the point
/// and asking for a password is not on the table.
///
/// Nothing was applying that reasoning to pnpm. `npm install -g pnpm` let npm
/// pick, so on a Mac with the nodejs.org pkg or a Linux box with a distribution
/// Node the install went at `/usr/local` or `/usr` and failed with `EACCES` —
/// while the dsh beside it had been carefully placed somewhere writable. Worse,
/// npm could succeed into a prefix that is not the one [`search_path`] looks in,
/// leaving a pnpm that exists and cannot be found.
///
/// So the prefix is chosen here, explicitly, and handed to npm as `--prefix`:
///
/// 1. The prefix the bootstrap script recorded, when it is writable. That is
///    where dsh is, so pnpm lands beside it and [`search_path`] finds it.
/// 2. The prefix the running dsh sits in, when that is writable — covers a dsh
///    the user installed themselves, with no marker to read.
/// 3. The app's own prefix under the data directory. Always writable, because
///    the app owns it.
///
/// `None` only when there is no application data directory to fall back to,
/// which is a machine this app cannot run on anyway.
pub fn tool_prefix(app: &AppHandle) -> Option<PathBuf> {
    let recorded = bootstrap(app).prefix;
    let installed = current(app).and_then(|install| install.prefix);

    if let Some(usable) = first_writable_prefix([recorded, installed]) {
        return Some(usable);
    }

    // Ours, and made if it is not there. This is the same directory the shell
    // script falls back to, so a machine that took that path keeps one prefix
    // rather than growing a second.
    let own = app_dir(app)?.join("npm");
    if let Err(error) = std::fs::create_dir_all(&own) {
        eprintln!(
            "dsh-desktop: could not create a prefix at {}: {error}",
            own.display()
        );
        return None;
    }
    Some(own)
}

/// The first candidate a `-g` install could actually write into. Split out from
/// [`tool_prefix`] so the choice can be tested without an `AppHandle`.
fn first_writable_prefix<C>(candidates: C) -> Option<PathBuf>
where
    C: IntoIterator<Item = Option<PathBuf>>,
{
    candidates
        .into_iter()
        .flatten()
        .find(|candidate| usable_prefix(candidate))
}

/// Whether a `-g` install aimed at `prefix` would land somewhere this process
/// can write.
///
/// The prefix itself has to exist. Walking up to *any* writable ancestor — which
/// is what an unguarded `ancestors()` does — accepts a prefix that is not there
/// at all because its parent happens to be writable, and npm would then create a
/// tree nothing else in this app looks at. Only the levels npm itself creates
/// (`lib/node_modules`, or `node_modules` on Windows) may be missing, so the
/// probe walks up from the package root and stops at the prefix.
fn usable_prefix(prefix: &Path) -> bool {
    deepest_existing(prefix).is_some_and(|dir| writable(&dir))
}

/// The `node_modules` an update lands in: under `lib` on the layout npm uses
/// everywhere but Windows, and directly under the prefix on that one. Answered
/// by looking rather than by `cfg`, because it is the layout [`prefix_of`]
/// matched that decides.
fn package_root(prefix: &Path) -> PathBuf {
    let lib = prefix.join("lib").join("node_modules");
    if lib.join(PACKAGE).is_dir() {
        lib
    } else {
        prefix.join("node_modules")
    }
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

    // Nothing here can aim an install at that tree, so telling the user is the
    // whole of it — and recording it as skipped is what keeps that to once per
    // release rather than every time the six hours are up.
    let Some(prefix) = updatable(&installed) else {
        tell(app, &installed.version, &latest);
        skip(app, &latest);
        return true;
    };

    if !ask(app, &installed.version, &latest) {
        skip(app, &latest);
        return true;
    }

    // Nothing is running yet, so there is nothing to stop and nothing to restart
    // for: npm replaces the tree in place and the boot carries straight on into
    // the new version.
    update(app, prefix, &installed.version, report)
}

/// What the window shows while this is waiting on npm, and `""` when the wait is
/// over. See [`crate::controls::busy`] — the caller owns the window, this only
/// knows when there is something to wait for.
pub type Saying<'a> = dyn Fn(&str) + 'a;

/// A dsh update the user asked for from the menu, rather than one a launch
/// happened to find. Answers with the prefix to install into once they have
/// agreed to it, for the caller to hand to [`update`] with dsh stopped.
///
/// Every outcome is reported, including "nothing to do": a menu item that does
/// nothing visible looks broken. A version turned down earlier is offered again
/// — the whole point of this is being asked.
pub fn requested(app: &AppHandle, saying: &Saying) -> Option<(PathBuf, Version)> {
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

    let Some(prefix) = updatable(&installed).map(Path::to_path_buf) else {
        tell(app, &installed.version, &latest);
        return None;
    };

    if !confirm(app, &installed.version, &latest) {
        return None;
    }

    Some((prefix, installed.version))
}

/// Replace the dsh in `prefix` with the newest release, reporting progress onto
/// the loading page. `false` means the app quit while npm was still running.
///
/// The prefix is passed to the script rather than left to the npm it runs with:
/// a dsh the user installed themselves lives in their own global prefix, and an
/// npm of ours would default to a different one and install a second copy there.
///
/// Failure is reported here and answers `true` all the same — there is a working
/// dsh on disk either way, which is the one the caller goes on to run.
pub fn update(app: &AppHandle, prefix: &Path, installed: &Version, report: &Report) -> bool {
    let args = [
        OsStr::new("-Mode"),
        OsStr::new("update"),
        OsStr::new("-Prefix"),
        prefix.as_os_str(),
    ];

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

/// Get a dsh onto a machine that has none, which may mean getting it a Node
/// first. `false` if the app quit while it was running.
fn bootstrap_now(app: &AppHandle, report: &Report) -> bool {
    report(t!("正在准备运行环境…", "Preparing the runtime…"), -1.0);

    match run(app, &[OsStr::new("-Mode"), OsStr::new("install")], report) {
        Ok(true) => {
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
    let state = bootstrap(app);
    let (node, cli) = match (state.node, state.npm) {
        (Some(node), Some(cli)) if node.is_file() && cli.is_file() => (node, cli),
        _ => {
            // Beside the binary on Windows, and one level up under `lib`
            // everywhere else — the same two layouts `root_of` covers.
            //
            // `executable` rather than bare presence: `look_up` deliberately
            // reports a dangling shim so it can be named, and a Node that cannot
            // run is no use to the caller, which is about to spawn it. Bailing
            // here turns it into "no npm to install pnpm with" rather than a
            // spawn failure from inside the install.
            let node = look_up(app, NODE).filter(|node| executable(node))?;
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

/// Tell the user about an update to a dsh that is not laid out as one
/// `npm install -g` this app can point npm at, and leave it at that.
///
/// Nothing here offers to apply it, because doing so would not land where the
/// working copy is: see [`prefix_of`] for what rules a tree out. So the command
/// goes in the message instead, for the user to run against whatever they
/// actually installed it with.
fn tell(app: &AppHandle, installed: &Version, latest: &Version) {
    note(
        app,
        t!("dsh 有可用更新", "A dsh update is available"),
        &t!(
            "dsh 有新版本 {}（当前 {}）。\n\n\
             这份 dsh 不在应用能写的 npm 全局目录里（比如用版本管理器装的，\
             或者装在只有管理员能写的地方），应用不会去改动它。要更新的话，\
             用你当初安装它的方式，在终端里执行：\n\nnpm install -g {}@latest",
            "dsh {} is available (this machine has {}).\n\n\
             This dsh is not in an npm global directory the app can write to — a \
             version manager put it there, or it needs administrator rights — so \
             the app will not touch it. To update it, use whatever you installed \
             it with:\n\nnpm install -g {}@latest",
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

/// Open a terminal that has dsh in it.
///
/// The app knows where dsh and its Node are — it resolved them at startup and
/// hands them to every child it runs (see [`apply_path`]). The user's shell does
/// not, and deliberately: nothing here writes to their PATH, because a desktop
/// app editing the environment of every terminal on the machine is a change they
/// did not ask for and cannot see. The cost of that decision is that on a machine
/// where this app installed Node itself, `dsh` is a command the user cannot type.
///
/// This is the way out. One terminal with the same PATH the app's own children
/// get, for as long as it is open, and nothing left behind when it closes. It is
/// how the CLI gets used at all on such a machine — `dsh plugin`, `dsh` itself,
/// anything the window does not put a button on.
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

        let path = std::env::join_paths(search_path(app))
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
    use super::{
        executable, first_writable_prefix, package_root, prefix_of, present, resolve, shim_dir,
        writable, PACKAGE,
    };
    use std::path::{Path, PathBuf};

    /// A directory of this test's own, gone again when the test ends. Both
    /// layouts below are built on disk, because what [`prefix_of`] answers is a
    /// question about which files are where.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let unique = format!("dsh-prefix-{name}-{}", std::process::id());
            let dir = std::env::temp_dir().join(unique);
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("a scratch directory under the temp dir");
            Self(dir)
        }

        /// An empty shim, and the package tree npm would have put beside it.
        fn install(&self, shim: &str, root: &str) -> PathBuf {
            let package = self.0.join(root).join("node_modules").join(PACKAGE);
            std::fs::create_dir_all(&package).expect("the package directory");
            std::fs::write(package.join("package.json"), b"{}").expect("the manifest");
            self.shim(shim)
        }

        fn shim(&self, shim: &str) -> PathBuf {
            let bin = self.0.join(shim);
            std::fs::create_dir_all(bin.parent().expect("the shim has a directory"))
                .expect("the shim directory");
            std::fs::write(&bin, b"").expect("the shim");
            bin
        }

        fn at(&self, path: &str) -> PathBuf {
            self.0.join(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn takes_the_directory_a_windows_shim_sits_in() {
        let scratch = Scratch::new("windows");
        let shim = scratch.install("prefix/dsh.cmd", "prefix");

        assert_eq!(prefix_of(&shim), Some(scratch.at("prefix")));
    }

    #[test]
    fn takes_the_directory_above_a_unix_bin() {
        let scratch = Scratch::new("unix");
        let shim = scratch.install("prefix/bin/dsh", "prefix/lib");

        assert_eq!(prefix_of(&shim), Some(scratch.at("prefix")));
    }

    /// A version manager's shim, or a `DSH_BIN` pointing into a checkout: there
    /// is no global install around it for npm to replace.
    #[test]
    fn refuses_a_shim_with_no_package_around_it() {
        let scratch = Scratch::new("bare");
        let shim = scratch.shim("elsewhere/dsh");

        assert_eq!(prefix_of(&shim), None);
    }

    #[test]
    fn refuses_a_shim_at_the_root_of_the_filesystem() {
        assert_eq!(prefix_of(Path::new("/")), None);
    }

    /// The bug this pair is here for: npm's global shims are symlinks on every
    /// platform but Windows, and `is_file` follows them. A shim whose target is
    /// gone — a Node switched by a version manager, a half-removed global
    /// package — read as *absent*, so the app announced there was no pnpm and no
    /// dsh on a machine where both names were sitting in the prefix, then
    /// reinstalled over the top and reported the wrong cause when that failed.
    ///
    /// Unix-only because it is about symlink semantics; the Windows shim is a
    /// plain file and was never affected.
    #[cfg(unix)]
    #[test]
    fn finds_a_shim_whose_symlink_target_is_gone() {
        let scratch = Scratch::new("dangling");
        let target = scratch.at("lib/node_modules/pnpm/bin/pnpm.cjs");
        let link = scratch.at("bin/pnpm");
        std::fs::create_dir_all(link.parent().expect("the bin directory"))
            .expect("the bin directory");
        std::os::unix::fs::symlink(&target, &link).expect("the shim symlink");

        assert!(!target.exists(), "the target is deliberately absent");
        assert!(
            present(&link),
            "a dangling shim is still an entry npm left behind"
        );
        assert!(
            !executable(&link),
            "and it is exactly what cannot be run, which is what gets reported"
        );
    }

    /// The healthy Unix layout: a symlink to a real executable is both found and
    /// runnable, so nothing reinstalls over a working pnpm.
    #[cfg(unix)]
    #[test]
    fn accepts_a_shim_that_is_a_symlink_to_something_runnable() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new("linked");
        let target = scratch.shim("lib/node_modules/pnpm/bin/pnpm.cjs");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
            .expect("the executable bit");

        let link = scratch.at("bin/pnpm");
        std::fs::create_dir_all(link.parent().expect("the bin directory"))
            .expect("the bin directory");
        std::os::unix::fs::symlink(&target, &link).expect("the shim symlink");

        assert!(present(&link));
        assert!(executable(&link));
    }

    /// A global bin that lost its executable bit fails to spawn with an error no
    /// clearer than the dangling case, so it is caught the same way.
    #[cfg(unix)]
    #[test]
    fn rejects_a_shim_without_its_executable_bit() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new("unreadable");
        let shim = scratch.shim("bin/pnpm");
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o644))
            .expect("the permissions");

        assert!(present(&shim), "it is there");
        assert!(!executable(&shim), "but it cannot be run");
    }

    /// A directory named `pnpm` on the search path is not the pnpm anybody wants
    /// to execute.
    #[test]
    fn ignores_a_directory_with_the_name_of_a_shim() {
        let scratch = Scratch::new("dir");
        let dir = scratch.at("bin/pnpm");
        std::fs::create_dir_all(&dir).expect("the directory");

        assert!(!present(&dir));
        assert!(!executable(&dir));
    }

    #[test]
    fn a_missing_path_is_neither_present_nor_executable() {
        let scratch = Scratch::new("absent");
        let nothing = scratch.at("bin/pnpm");

        assert!(!present(&nothing));
        assert!(!executable(&nothing));
    }

    /// npm writes its shim targets as relative paths, so the liveness test has
    /// to resolve them against the link's directory rather than this process's
    /// working directory — which would call a healthy shim dangling depending on
    /// where the app was started from.
    #[test]
    fn resolves_a_relative_symlink_target_against_the_link() {
        let link = Path::new("/usr/local/bin/pnpm");

        assert_eq!(
            resolve(link, Path::new("../lib/node_modules/pnpm/bin/pnpm.cjs")),
            Path::new("/usr/local/bin/../lib/node_modules/pnpm/bin/pnpm.cjs")
        );
        // An absolute target is already the answer.
        assert_eq!(
            resolve(link, Path::new("/opt/pnpm/bin/pnpm.cjs")),
            Path::new("/opt/pnpm/bin/pnpm.cjs")
        );
    }

    /// The same thing on disk: a relative link to a real file is not reported as
    /// broken, whatever the working directory is.
    #[cfg(unix)]
    #[test]
    fn a_relative_shim_to_a_real_target_is_not_broken() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new("relative");
        let target = scratch.shim("lib/node_modules/pnpm/bin/pnpm.cjs");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
            .expect("the executable bit");

        let link = scratch.at("bin/pnpm");
        std::fs::create_dir_all(link.parent().expect("the bin directory"))
            .expect("the bin directory");
        std::os::unix::fs::symlink("../lib/node_modules/pnpm/bin/pnpm.cjs", &link)
            .expect("the relative shim symlink");

        let stored = std::fs::read_link(&link).expect("the target");
        assert!(stored.is_relative(), "npm stores a relative target");
        assert!(
            resolve(&link, &stored).exists(),
            "and it resolves to the real file"
        );
        assert!(present(&link) && executable(&link));
    }

    /// The prefix choice behind `--prefix`: the first candidate that can be
    /// written to wins, a nonexistent one is skipped rather than created, and
    /// nothing usable answers `None` so the caller falls back to its own prefix.
    ///
    /// This is what stops the macOS/Linux `EACCES`: pnpm goes where dsh already
    /// is when that is writable, and never into a `/usr` prefix chosen by npm.
    #[test]
    fn takes_the_first_prefix_it_can_write_to() {
        let scratch = Scratch::new("toolprefix");
        let good = scratch.at("writable");
        std::fs::create_dir_all(&good).expect("the writable prefix");

        // A prefix that is not there is not a candidate — even though its
        // ancestors are writable, which an unguarded `ancestors()` walk would
        // have accepted, sending npm off to build a tree nothing looks at. This
        // assertion is here because the first version of the code did exactly
        // that and this test caught it.
        let missing = scratch.at("no/such/prefix");

        assert_eq!(
            first_writable_prefix([Some(missing.clone()), Some(good.clone())]),
            Some(good.clone()),
            "an unusable candidate is skipped for the next one"
        );
        assert_eq!(
            first_writable_prefix([Some(good.clone()), Some(missing.clone())]),
            Some(good),
            "and the first usable one wins"
        );
        assert_eq!(
            first_writable_prefix([None, Some(missing)]),
            None,
            "nothing usable means the caller uses its own prefix"
        );
        assert_eq!(first_writable_prefix([]), None);
    }

    /// A prefix that exists but has no `lib/node_modules` yet is still usable —
    /// npm creates those itself. Rejecting it would push every fresh install
    /// into the fallback prefix and defeat the point of installing beside dsh.
    #[test]
    fn accepts_a_prefix_npm_has_not_filled_in_yet() {
        let scratch = Scratch::new("freshprefix");
        let fresh = scratch.at("fresh");
        std::fs::create_dir_all(&fresh).expect("an empty prefix");

        assert!(
            !package_root(&fresh).is_dir(),
            "the package root is deliberately absent"
        );
        assert_eq!(
            first_writable_prefix([Some(fresh.clone())]),
            Some(fresh),
            "an empty but writable prefix is where npm should go"
        );
    }

    /// The one definition both the search path and the `--prefix` install rely
    /// on. If these two ever disagree, a tool is installed into a directory
    /// nothing looks in — which is the failure `tool_prefix` exists to prevent,
    /// so it is pinned per platform rather than left to a `cfg!` written twice.
    #[test]
    fn puts_shims_where_npm_puts_them() {
        let prefix = Path::new("/some/prefix");

        if cfg!(windows) {
            assert_eq!(shim_dir(prefix), prefix);
        } else {
            assert_eq!(shim_dir(prefix), prefix.join("bin"));
        }
    }

    /// The probe behind the `EACCES` message: it answers for a directory that
    /// exists and refuses one that does not, so a fresh prefix is not reported as
    /// unwritable.
    #[test]
    fn probes_a_directory_it_can_write_to() {
        let scratch = Scratch::new("probe");

        assert!(writable(&scratch.0));
        assert!(!writable(&scratch.at("does/not/exist")));
    }

    /// What the write probe is aimed at, which is the directory npm replaces.
    #[test]
    fn puts_the_package_root_where_the_package_is() {
        let scratch = Scratch::new("root");
        scratch.install("prefix/bin/dsh", "prefix/lib");

        let prefix = scratch.at("prefix");
        assert_eq!(
            package_root(&prefix),
            prefix.join("lib").join("node_modules")
        );
        assert_eq!(
            package_root(&scratch.at("nothing-here")),
            scratch.at("nothing-here").join("node_modules")
        );
    }
}
