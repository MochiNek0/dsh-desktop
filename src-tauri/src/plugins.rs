//! Profile plugins: listing what dsh can be extended with, and installing one
//! for a user who has no way to do it themselves.
//!
//! `dsh plugin --profile web <args…>` is a thin pnpm forwarder — it initializes
//! `$DSH_HOME/profiles/web`, runs `pnpm <args…>` in it, and reconciles the
//! `dsh.profile.bundles` list against what pnpm actually installed. Which means
//! installing a plugin needs three things on the machine: dsh, the Node under
//! it, and pnpm. All three are in the app's runtime, which `install-deps.ps1`
//! fills in — pnpm included, in the same npm command as dsh.
//!
//! Hence this module. It runs that command with the runtime's own paths, puts
//! pnpm there itself if the runtime somehow lacks it, and prints the whole thing
//! onto a panel drawn over whatever the window is showing (see `panel`). There
//! is nothing here the user could not do in a terminal — the launcher on their
//! PATH runs the same dsh — but a panel with a checklist beats reading a
//! `dsh plugin --profile web add` invocation off a README.
//!
//! ## The preset list
//!
//! `resources/preset-plugins.json` ships with the app and is the whole of what
//! the panel offers by name. Adding one is an edit to that file, not to this
//! module — which is the point: the list is going to move faster than the app,
//! and a plugin that turns out to be abandoned should be removable without a
//! release. Anything not on it goes in the panel's own text box, which passes
//! whatever is typed straight through to pnpm.
//!
//! ## What is not automated
//!
//! pnpm 10 and later refuse to run a dependency's build scripts until the
//! package is listed under `allowBuilds` in the profile's `pnpm-workspace.yaml`,
//! and every `github:` plugin builds on install. dsh prints the exact key to
//! add; this module puts that output on screen verbatim and offers to open the
//! directory holding the file, rather than writing the key itself. Doing it
//! automatically would mean this app deciding, on the user's behalf and without
//! showing them, that a package downloaded from a repository may run code during
//! installation.
//!
//! The same reasoning does *not* extend to `minimumReleaseAge`, pnpm's cooldown
//! on newly published versions — but what follows from it depends on who the
//! cooldown is actually stopping, because pnpm re-verifies every entry in the
//! lockfile before it does anything at all.
//!
//! - A *removal* lifts it outright: taking a dependency out cannot install
//!   anything, so the check has nothing to protect there. See [`remove`].
//! - An *install* that the cooldown blocks over a package on the command line
//!   keeps the refusal. That is the check doing its job — stopping a freshly
//!   compromised version — and the user is told what it is rather than having
//!   it switched off for them.
//! - An install blocked *only* over lockfile entries nobody asked about lifts
//!   it too. A version pinned earlier (a plugin that updated itself) fails
//!   every later install until it ages out, over a package the user is not
//!   touching; refusing there protects nothing and strands the panel. See
//!   [`install`].

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::channel;
use std::sync::Mutex;

use tauri::AppHandle;

// There is nothing left to import from `dsh` but the questions themselves.
//
// This used to be `use crate::dsh::{Tool, DSH, PNPM}` — a three-state enum and
// two lists of shim names — because a tool could be present, absent, or present
// with a dangling symlink behind it, and telling those apart meant naming the
// shims here as well as there. The runtime is the app's own directory now, so
// `dsh` answers both questions with a `bool` and there are no names to share.

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The profile the app's `dsh web` boots, and so the one a plugin has to be
/// installed into to be part of what the window shows.
const PROFILE: &str = "web";

/// The list that ships with the app.
const PRESETS: &str = "preset-plugins.json";

/// The install running right now, if one is. Held so that quitting takes pnpm
/// down rather than leaving it writing into the profile with no owner.
static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

struct Running {
    child: Child,
    /// See [`crate::server::Job`]: the backstop for a crash or a force-kill.
    #[cfg(windows)]
    _job: Option<crate::server::Job>,
}

/// One entry of `resources/preset-plugins.json`.
///
/// Deserialized by hand out of `serde_json::Value` rather than with a derive,
/// because a single malformed entry should cost that entry and not the list: a
/// preset file shipped with a typo would otherwise leave the panel empty.
struct Preset {
    id: String,
    /// What is handed to pnpm: a registry name, or a `github:owner/repo`.
    spec: String,
    /// The name the package installs under, which is what the profile manifest
    /// records and so what "already installed" is decided against. Only a
    /// registry spec is its own name; a `github:` one resolves to whatever the
    /// repository's manifest declares, and nothing here can work that out
    /// without fetching it.
    package: String,
    name: String,
    description: String,
    /// Which group of the panel it is drawn under. Free-form, because the panel
    /// decides what a group is called and in what order the groups come; an
    /// entry naming one the panel does not know falls in with the rest rather
    /// than disappearing. Defaults to `recommended`, so an entry written before
    /// there were groups still lands somewhere.
    section: String,
    /// Offered only on Windows — the one entry so far is a fix for a Windows
    /// failure, and listing it elsewhere is an invitation to install a no-op.
    windows_only: bool,
    /// Ticked when the panel opens.
    checked: bool,
    /// Drawn with a "fix" chip rather than the ordinary one.
    fix: bool,
    url: String,
}

fn presets(app: &AppHandle) -> Vec<Preset> {
    let Some(path) = preset_file(app) else {
        eprintln!("dsh-desktop: {PRESETS} is not where it should be; no presets to offer");
        return Vec::new();
    };

    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) => {
            eprintln!("dsh-desktop: could not read {}: {error}", path.display());
            return Vec::new();
        }
    };

    parse(&raw)
        .into_iter()
        .filter(|preset| !preset.windows_only || cfg!(windows))
        .collect()
}

/// The list as the file spells it, before the platform filter. Separate from
/// [`presets`] so the shipped file can be read by a test — a preset whose
/// `checked` is a string, or whose `id` is missing, is a mistake that only shows
/// up as a wrong-looking panel otherwise.
fn parse(raw: &str) -> Vec<Preset> {
    let parsed: serde_json::Value = match serde_json::from_str(raw) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("dsh-desktop: could not parse {PRESETS}: {error}");
            return Vec::new();
        }
    };

    let Some(entries) = parsed.as_array() else {
        eprintln!("dsh-desktop: {PRESETS} is not a list");
        return Vec::new();
    };

    entries
        .iter()
        .filter_map(|entry| {
            let text = |key: &str| entry.get(key)?.as_str().map(str::to_string);
            let flag = |key: &str| entry.get(key).and_then(serde_json::Value::as_bool) == Some(true);

            Some(Preset {
                package: text("package").or_else(|| text("id"))?,
                id: text("id")?,
                spec: text("spec")?,
                name: text("name")?,
                // The English half is optional: a preset that has not been
                // translated yet reads in Chinese rather than not at all.
                description: if crate::i18n::chinese() {
                    text("description").unwrap_or_default()
                } else {
                    text("descriptionEn")
                        .or_else(|| text("description"))
                        .unwrap_or_default()
                },
                section: text("section")
                    .map(|section| section.trim().to_string())
                    .filter(|section| !section.is_empty())
                    .unwrap_or_else(|| "recommended".to_string()),
                windows_only: flag("windowsOnly"),
                checked: flag("checked"),
                fix: flag("fix"),
                url: text("url").unwrap_or_default(),
            })
        })
        .collect()
}

/// Where the shipped list is: the bundled resource, or the one in the source
/// tree when this is a `tauri dev` build that has never been bundled.
fn preset_file(app: &AppHandle) -> Option<PathBuf> {
    if let Some(bundled) = crate::dsh::resources(app).map(|dir| dir.join(PRESETS)) {
        if bundled.is_file() {
            return Some(bundled);
        }
    }

    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("resources")
        .join(PRESETS);
    source.is_file().then_some(source)
}

/// The presets and their state, as the panel's `dshPlugins` hook wants them.
/// See `dist/index.html`.
pub fn listing(app: &AppHandle) -> String {
    let presets = presets(app);
    let installed = installed(app);

    // A preset already on the machine is not something to offer again; it is
    // listed below instead, where it can be taken off.
    let entries: Vec<serde_json::Value> = presets
        .iter()
        .filter(|preset| !installed.contains(&preset.package))
        .map(|preset| {
            serde_json::json!({
                "id": preset.id,
                "name": preset.name,
                "description": preset.description,
                "section": preset.section,
                "url": preset.url,
                "fix": preset.fix,
                "checked": preset.checked,
            })
        })
        .collect();

    // What pnpm put there, which is the whole of what it can take away again.
    // Carrying the preset's own label where the list knows one, so a plugin
    // reads the same on the way out as it did on the way in.
    let held: Vec<serde_json::Value> = dependencies(app)
        .into_iter()
        .map(|(name, version)| {
            let label = presets
                .iter()
                .find(|preset| preset.package == name)
                .map(|preset| preset.name.clone())
                .unwrap_or_else(|| name.clone());
            serde_json::json!({ "name": name, "label": label, "version": version })
        })
        .collect();

    serde_json::json!({
        "presets": entries,
        "installed": held,
        "directory": profile_dir(app),
    })
    .to_string()
}

/// What pnpm was asked to install: every dependency, with the range recorded.
///
/// `dsh.profile.bundles` is deliberately not read here, though [`installed`]
/// reads both: `@deepseek-ai/dsh-base` and `@deepseek-ai/dsh-web-app` are on
/// that list and are not plugins. Offering to remove the profile's own
/// foundation would be offering to break it.
fn dependencies(app: &AppHandle) -> Vec<(String, String)> {
    std::fs::read_to_string(profile_dir(app).join("package.json"))
        .map(|raw| dependencies_of(&raw))
        .unwrap_or_default()
}

fn dependencies_of(manifest: &str) -> Vec<(String, String)> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(manifest) else {
        return Vec::new();
    };

    value
        .get("dependencies")
        .and_then(serde_json::Value::as_object)
        .map(|deps| {
            deps.iter()
                .map(|(name, range)| {
                    (name.clone(), range.as_str().unwrap_or_default().to_string())
                })
                .collect()
        })
        .unwrap_or_default()
}

/// What the profile manifest says is in it.
///
/// Both halves of it: `dependencies` is what pnpm installed, and
/// `dsh.profile.bundles` is the layer stack dsh reconciled out of that — a
/// plugin is in the first and, once dsh has seen it declare `dsh.bundle`, the
/// second. Reading both means an entry matches whichever name it went in under.
fn installed(app: &AppHandle) -> HashSet<String> {
    let Ok(raw) = std::fs::read_to_string(profile_dir(app).join("package.json")) else {
        return HashSet::new();
    };
    let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return HashSet::new();
    };

    let dependencies = manifest
        .get("dependencies")
        .and_then(serde_json::Value::as_object)
        .map(|deps| deps.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();

    let bundles = manifest
        .pointer("/dsh/profile/bundles")
        .and_then(serde_json::Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|name| name.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    dependencies.into_iter().chain(bundles).collect()
}

/// `$DSH_HOME/profiles/web`, where a plugin ends up.
///
/// `DSH_HOME` is dsh's own variable and this app passes it through untouched, so
/// the answer has to be worked out the way dsh works it out — not read off
/// anything of ours.
pub fn profile_dir(app: &AppHandle) -> PathBuf {
    dsh_home(app).join("profiles").join(PROFILE)
}

fn dsh_home(app: &AppHandle) -> PathBuf {
    if let Some(home) = std::env::var_os("DSH_HOME") {
        return PathBuf::from(home);
    }

    let _ = app;
    #[allow(deprecated)]
    std::env::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".dsh")
}

/// What an install writes as it goes: one line of output, verbatim.
pub type Log<'a> = dyn Fn(&str) + 'a;

/// Install the presets named by `ids`, plus `extra` if the user typed one.
///
/// Blocking, and for minutes: pnpm is fetching packages. The caller runs it on a
/// thread with `dsh web` already stopped — pnpm is about to rewrite the profile
/// directory the running server loaded its plugins out of, and a half-written
/// one underneath a live server is worse than a wait.
pub fn install(
    app: &AppHandle,
    ids: &[String],
    extra: Option<&str>,
    log: &Log,
) -> Result<(), String> {
    let presets = presets(app);
    let mut specs: Vec<String> = Vec::new();

    for id in ids {
        let preset = presets
            .iter()
            .find(|preset| &preset.id == id)
            .ok_or_else(|| t!("清单里没有插件 {}", "no preset called {}", id))?;
        specs.push(preset.spec.clone());
    }
    if let Some(extra) = extra.map(str::trim).filter(|extra| !extra.is_empty()) {
        specs.push(extra.to_string());
    }

    if specs.is_empty() {
        return Err(t!("没有选择任何插件", "nothing was selected").to_string());
    }

    // One state, not three. `current` used to answer `None` both for a machine
    // with no dsh and for one whose shim was a symlink left dangling by a Node
    // version switch, and the second had to be named separately or the message
    // contradicted the working `dsh` in the user's terminal. The runtime holds
    // no symlinks and nothing outside it can move, so "not installed" is the
    // only way to be without one.
    let dsh = crate::dsh::current(app).ok_or_else(|| {
        t!(
            "这台机器上还没有装好的 dsh，插件没有可以装进去的地方。重启应用会再装一次。",
            "there is no working dsh on this machine for a plugin to go into. Restarting the app installs one."
        )
        .to_string()
    })?;

    ensure_pnpm(app, log)?;

    log(&t!("正在安装：{}", "Installing: {}", specs.join(" ")));

    let mut command = dsh.command();
    command.args(["plugin", "--profile", PROFILE, "add"]);
    // The profile directory is a pnpm workspace whose only package is itself —
    // `packages: [.]`, in the `pnpm-workspace.yaml` dsh writes when it first
    // initialises the profile. pnpm will not add a dependency at a workspace
    // root unless the caller says they meant it, and here they did: the
    // profile's dependencies are exactly what a plugin is.
    //
    // Conditional on the file being there, because `-w` outside a workspace is
    // an error of its own — and the file is dsh's to write, not ours.
    if profile_dir(app).join("pnpm-workspace.yaml").is_file() {
        command.arg("-w");
    }
    command.args(&specs);
    crate::dsh::apply_path(app, &mut command);

    let mut outcome = run(command, log)?;

    // A release-age refusal that names only packages this install did not ask
    // for is a stale lockfile, not a verdict on what is being installed. pnpm
    // re-verifies every lockfile entry before adding anything, so a version
    // that was pinned earlier — by a plugin updating itself, say — keeps
    // failing every later install until it ages out, over a package the user
    // is not touching. Retried once with the cooldown lifted, as a removal is.
    //
    // The distinction is the whole point: if the cooldown names something on
    // the command line, the check is doing its job and the refusal stands.
    if outcome.code != 0 && outcome.flagged(RELEASE_AGE) {
        let wanted: HashSet<String> = specs.iter().filter_map(|spec| spec_name(spec)).collect();
        let blamed = outcome.blamed();
        let stale: Vec<&String> = blamed.iter().filter(|name| !wanted.contains(*name)).collect();

        if !blamed.is_empty() && stale.len() == blamed.len() {
            let mut names: Vec<&str> = stale.iter().map(|name| name.as_str()).collect();
            names.sort_unstable();
            log(&t!(
                "被拦下的是 lockfile 里已有的 {}，不是这次要装的东西。跳过这项检查重试…",
                "What was blocked is {}, already in the lockfile — not anything being installed now. Retrying with that check skipped…",
                names.join("、")
            ));

            let mut retry = dsh.command();
            retry.args(["plugin", "--profile", PROFILE, "add"]);
            if profile_dir(app).join("pnpm-workspace.yaml").is_file() {
                retry.arg("-w");
            }
            retry.args(&specs);
            crate::dsh::apply_path(app, &mut retry);
            retry.env("PNPM_CONFIG_MINIMUM_RELEASE_AGE", "0");
            outcome = run(retry, log)?;
        }
    }

    match outcome.code {
        0 => {
            log(t!("插件安装完成。", "Plugins installed."));
            Ok(())
        }
        // 127 is what dsh answers with when pnpm is not on the PATH it was
        // given — see `crate::dsh::child_path`, which puts the runtime's
        // `node_modules/.bin` on it. Reaching it means `ensure_pnpm` claimed
        // success over a pnpm that then could not be executed, which the
        // runtime makes very unlikely: it checks the package is there rather
        // than trusting npm's exit code, and there is no shim to dangle.
        127 => Err(t!(
            "dsh 找不到 pnpm。插件安装需要 pnpm，自动安装它这一步没有成功。",
            "dsh could not find pnpm. Installing plugins needs it, and installing pnpm did not work."
        )
        .to_string()),
        _ => Err(diagnose(app, &outcome, false)),
    }
}

/// The pnpm error code for a package younger than the `minimumReleaseAge`
/// cooldown the machine is configured with.
const RELEASE_AGE: &str = "ERR_PNPM_MINIMUM_RELEASE_AGE_VIOLATION";

/// The package name a pnpm spec installs under, as far as it can be known
/// without fetching anything.
///
/// A registry spec is its own name once the version range is cut off. Anything
/// exotic — `github:owner/repo`, a tarball URL, a path — resolves to whatever
/// the fetched manifest declares, which nothing here can work out, so those
/// answer `None` and are treated as "not known to be ours".
fn spec_name(spec: &str) -> Option<String> {
    let spec = spec.trim();
    if spec.is_empty() || spec.contains(['/', '\\', ':']) && !spec.starts_with('@') {
        return None;
    }
    // A scoped name keeps its leading `@`; the range separator is a later one.
    let name = match spec.rfind('@') {
        Some(at) if at > 0 => &spec[..at],
        _ => spec,
    };
    let name = name.trim();
    (!name.is_empty() && !name.contains(' ')).then(|| name.to_string())
}

/// Turn a failed run into something the user can act on.
///
/// Every other failure in this module names a cause and a next step — a broken
/// pnpm shim, an unwritable npm prefix, the `allowBuilds` key to add. The
/// fallback did not: an exit code and "the output is above" leaves a user who
/// clicked a button in a panel to work out a pnpm policy from its log. The
/// codes pnpm names are the reliable part of that log, so they are what this
/// switches on.
fn diagnose(app: &AppHandle, outcome: &Outcome, removing: bool) -> String {
    if outcome.flagged(RELEASE_AGE) {
        let workspace = profile_dir(app).join("pnpm-workspace.yaml");
        let path = workspace.display();

        // Removal reaches here only after the retry with the policy lifted has
        // *also* failed, so the advice cannot be "turn the policy off" — that
        // was just tried. Naming the attempt is what stops the user from being
        // sent to do it again by hand.
        //
        // Both are kept to a couple of lines. This lands in the panel's footer
        // note, beside the buttons and directly under pnpm's own output — the
        // detail is already on screen, so a paragraph restating it only
        // crowds the row. What the note owes the user is the one thing the log
        // does not say: what to do next.
        return if removing {
            t!(
                "卸载仍被 pnpm 的 minimumReleaseAge 拦着——跳过这项检查重试过一次，还是失败。这项设置来自这台机器，可能在 {}，也可能在全局配置里。",
                "The removal is still blocked by pnpm's minimumReleaseAge — it was retried once with the check skipped and failed again. The setting comes from this machine: look in {}, or in a global pnpm config.",
                path
            )
        } else {
            // Not so on install: here the policy is doing its job, so this says
            // what it is before saying how to relax it.
            t!(
                "要装的包比 pnpm 的 minimumReleaseAge 冷却期新——这项检查用来挡住刚被投毒的版本。可以过一会儿再装，或在确认可信后往 {} 加一行 minimumReleaseAge: 0。",
                "Something being installed is newer than pnpm's minimumReleaseAge cooldown — the check that keeps a freshly compromised version out. Try again later, or, if you trust it, add minimumReleaseAge: 0 to {}.",
                path
            )
        };
    }

    t!(
        "dsh plugin 退出码 {}。上面是它的完整输出。",
        "dsh plugin exited with code {}. Its full output is above.",
        outcome.code
    )
}

/// Put pnpm where the dsh we are about to run will find it.
///
/// One `npm install` into the runtime, beside dsh, which is also where
/// `install-deps.ps1` puts it on a fresh machine — this is the repair path for a
/// runtime that predates that or lost it, not the ordinary way it arrives.
/// Downloading a standalone pnpm would mean a second installer in this app — a
/// fetch, a checksum, an archive to unpack, a mirror list to walk — to arrive at
/// a binary npm can place in one command.
///
/// What used to be here was three times this length, and none of it was about
/// pnpm. `npm install -g pnpm` let npm choose the destination, which gave two
/// distinct failures: an `EACCES` on a prefix under `/usr` that the user cannot
/// write to, and a *success* into a prefix that was not the one the app searched
/// — a pnpm that existed and could not be found. So a prefix had to be chosen
/// here (`tool_prefix`), probed for writability, and reported on afterwards
/// (`unwritable_prefix`), and the failure message had to say which of the two
/// directories it was talking about. The runtime is one directory the app owns
/// and can always write to, so all of that is gone and a failure here is a
/// network failure.
fn ensure_pnpm(app: &AppHandle, log: &Log) -> Result<(), String> {
    if crate::dsh::pnpm_ready(app) {
        return Ok(());
    }

    log(t!(
        "没有找到 pnpm，先安装它（dsh 的插件安装是转发给 pnpm 的）…",
        "No pnpm found; installing it first (dsh forwards plugin installs to pnpm)…"
    ));

    let runtime = crate::dsh::runtime(app).ok_or_else(|| {
        t!(
            "找不到应用数据目录，无法安装 pnpm。",
            "no application data directory to install pnpm into."
        )
        .to_string()
    })?;

    let mut npm = crate::dsh::npm(app).ok_or_else(|| {
        t!(
            "运行时里没有可用的 npm，无法安装 pnpm。重启应用会重新安装运行时。",
            "the runtime has no usable npm to install pnpm with. Restarting the app reinstalls it."
        )
        .to_string()
    })?;

    // Local, not `-g`, and aimed at the runtime — the same command the install
    // script runs, so pnpm lands in the one `node_modules` the app looks in.
    npm.args(["install", "--no-audit", "--no-fund", "pnpm"]);
    npm.arg("--prefix").arg(&runtime);

    // Only the exit code matters here: this runs npm, not pnpm, so there are no
    // `ERR_PNPM_*` codes to switch on.
    match run(npm, log)?.code {
        // npm exiting 0 is not the same as pnpm being there. The claim this
        // function makes is that the next step will find it, so that is what is
        // checked rather than what npm said.
        0 if crate::dsh::pnpm_ready(app) => {
            log(t!("pnpm 安装完成。", "pnpm installed."));
            Ok(())
        }
        0 => Err(t!(
            "npm 报告 pnpm 安装成功，但运行时目录里找不到它。",
            "npm reported pnpm installed, but it is not in the runtime directory."
        )
        .to_string()),
        code => Err(t!(
            "安装 pnpm 失败，npm 退出码 {}。目标目录是 {}，是应用自己的目录，所以这不是权限问题——上面是 npm 的完整输出，通常是网络或注册表访问失败。",
            "Installing pnpm failed; npm exited with code {}. The target was {}, a directory the app owns, so this is not a permission problem — npm's full output is above, and it is usually a network or registry failure.",
            code,
            runtime.display()
        )),
    }
}

/// What a finished child left behind: its exit code, the pnpm error codes its
/// output named, and the packages those errors named.
///
/// The output itself is not kept. A failing pnpm run can be thousands of lines
/// and all of them have already gone to the panel; what the callers need is the
/// much smaller question of *which* known failure this was, so only the
/// `ERR_PNPM_*` tokens are retained — see [`Outcome::flagged`] — along with the
/// package names a supply-chain refusal listed, which decide whether the
/// refusal is about this install at all. See [`Outcome::blamed`].
struct Outcome {
    code: i32,
    codes: HashSet<String>,
    blamed: HashSet<String>,
}

impl Outcome {
    /// Whether pnpm named this error code, e.g.
    /// `ERR_PNPM_MINIMUM_RELEASE_AGE_VIOLATION`.
    fn flagged(&self, code: &str) -> bool {
        self.codes.contains(code)
    }

    /// The packages a supply-chain refusal named, without their versions.
    fn blamed(&self) -> &HashSet<String> {
        &self.blamed
    }
}

/// pnpm prints its error codes as bare `ERR_PNPM_…` tokens, one per failure, in
/// a line that also carries prose. Matching the token rather than the whole
/// line keeps this working across pnpm's phrasing changes, and bounds what is
/// held from a run whose output is otherwise unbounded.
fn pnpm_codes(line: &str) -> impl Iterator<Item = String> + '_ {
    line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|word| word.starts_with("ERR_PNPM_"))
        .map(str::to_string)
}

/// The package a supply-chain refusal blames, if this line is one.
///
/// pnpm lists each rejected entry on its own indented line, in the shape
/// `name@version was published at <date>, within the minimumReleaseAge cutoff
/// (<date>)`. Only the name is taken: the question these answer is *which
/// package* is holding the install up, not which version of it.
///
/// Anchored on the ` was published at ` phrase rather than on indentation,
/// because indentation is shared with every other list pnpm prints. A line
/// that does not carry the phrase is not one of these.
fn pnpm_blamed(line: &str) -> Option<String> {
    let spec = line.trim().split(" was published at ").next()?.trim();
    if spec == line.trim() {
        return None;
    }

    // A scoped name keeps its leading `@`, so the version separator is the
    // *last* `@`, and only when something follows the first character.
    let name = match spec.rfind('@') {
        Some(at) if at > 0 => &spec[..at],
        _ => spec,
    };

    let name = name.trim();
    (!name.is_empty() && !name.contains(' ')).then(|| name.to_string())
}

/// Run a child to completion, putting every line either stream produces through
/// `log`, and answer with its exit code and the pnpm error codes it named.
///
/// Both streams, interleaved as they arrive: pnpm writes its progress to one and
/// its warnings — including the `allowBuilds` instruction this module
/// deliberately does not act on — to the other, and a user reading a failure
/// needs them in the order they happened.
fn run(mut command: Command, log: &Log) -> Result<Outcome, String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Both npm and pnpm turn colour off for a pipe on their own. This is for
        // the one that decides otherwise: the panel prints what it is given as
        // text, and an escape sequence there is line noise in front of the
        // message the user is trying to read.
        .env("NO_COLOR", "1");

    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    // pnpm is a process tree of its own, and this one runs for minutes.
    #[cfg(unix)]
    crate::server::group_leader(&mut command);

    // Claimed before the spawn, so two callers cannot both end up in the slot
    // with one of the children left unowned.
    let mut running = RUNNING.lock().unwrap();
    if running.is_some() {
        return Err(t!(
            "已经有一个插件安装在进行中。",
            "a plugin install is already running."
        )
        .to_string());
    }

    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let stdout = child.stdout.take().ok_or("stdout is piped")?;
    let stderr = child.stderr.take().ok_or("stderr is piped")?;

    #[cfg(windows)]
    let job = crate::server::Job::hold(&child);

    *running = Some(Running {
        child,
        #[cfg(windows)]
        _job: job,
    });
    drop(running);

    // One channel for both streams, so the reader below sees them in the order
    // they were written rather than one after the other. It closes when both
    // sending halves have been dropped, which is both streams at EOF.
    let (tx, rx) = channel();
    pump(stdout, tx.clone());
    pump(stderr, tx);

    let mut codes = HashSet::new();
    let mut blamed = HashSet::new();
    for line in rx {
        eprintln!("[plugin] {line}");
        codes.extend(pnpm_codes(&line));
        blamed.extend(pnpm_blamed(&line));
        log(&line);
    }

    let mut running = RUNNING.lock().unwrap();
    // Taken by `stop`: the app is on its way out and killed this.
    let Some(active) = running.as_mut() else {
        return Err(t!("安装已中断。", "the install was interrupted.").to_string());
    };
    // The pipes are both at EOF, so the child has exited or is a syscall away
    // from it, and this wait is the syscall.
    let status = active.child.wait().map_err(|error| error.to_string());
    *running = None;

    Ok(Outcome {
        code: status?.code().unwrap_or(-1),
        codes,
        blamed,
    })
}

/// How much of one line reaches the panel. Lines are split on newlines only, so
/// a progress display that redraws itself with carriage returns arrives as one
/// line as long as the run takes — and every line becomes a `window.eval`.
const LINE_LIMIT: usize = 400;

fn pump<R: Read + Send + 'static>(stream: R, tx: std::sync::mpsc::Sender<String>) {
    std::thread::spawn(move || {
        for mut line in BufReader::new(stream).lines().map_while(Result::ok) {
            if let Some((cut, _)) = line.char_indices().nth(LINE_LIMIT) {
                line.truncate(cut);
                line.push('…');
            }
            if tx.send(line).is_err() {
                return;
            }
        }
    });
}

/// Kill an install that is still running. Called on the way out, alongside the
/// bootstrap's own [`crate::dsh::stop`].
pub fn stop() {
    if let Some(mut running) = RUNNING.lock().unwrap().take() {
        crate::server::kill_tree(&mut running.child);
    }
}

/// Whether the panel has ever been shown. It opens once by itself — on a first
/// launch, and once for an existing install on the release that added it —
/// because a panel nobody knows about is a panel nobody opens, and the whole
/// point of it is the user who cannot reach these plugins any other way.
pub fn guided(app: &AppHandle) -> bool {
    marker(app).is_some_and(|path| path.exists())
}

pub fn mark_guided(app: &AppHandle) {
    let Some(path) = marker(app) else { return };

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(error) = std::fs::write(&path, b"") {
        // The cost is being shown the panel again next launch.
        eprintln!("dsh-desktop: could not record that the plugin panel was shown: {error}");
    }
}

fn marker(app: &AppHandle) -> Option<PathBuf> {
    Some(crate::dsh::app_dir(app)?.join("plugins-guided"))
}

/// The ids and free-text spec a `dsh-window://plugins-install` navigation
/// carries. Both may be empty, which the install rejects rather than this.
pub fn requested(url: &tauri::Url) -> (Vec<String>, Option<String>) {
    let mut ids = Vec::new();
    let mut spec = None;

    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "ids" => ids = commas(&value),
            "spec" => {
                let typed = value.trim().to_string();
                spec = (!typed.is_empty()).then_some(typed);
            }
            _ => {}
        }
    }

    (ids, spec)
}

/// The package names a `dsh-window://plugins-remove` navigation carries. What
/// is not installed is dropped by [`remove`], not here.
pub fn wanted_gone(url: &tauri::Url) -> Vec<String> {
    url.query_pairs()
        .find(|(key, _)| key == "names")
        .map(|(_, value)| commas(&value))
        .unwrap_or_default()
}

/// One comma-separated field of a panel navigation.
fn commas(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

/// Take plugins back out again, under the same two conditions the install runs
/// under: pnpm has to be there, and `dsh web` has to be down while the directory
/// it reads its plugins out of is rewritten.
///
/// No `-w` here. pnpm's refusal to touch a workspace root without one is
/// `add`'s alone — `remove` at the same root is not questioned.
pub fn remove(app: &AppHandle, names: &[String], log: &Log) -> Result<(), String> {
    let held: Vec<String> = dependencies(app)
        .into_iter()
        .map(|(name, _)| name)
        .collect();

    // Only what the manifest actually lists. The panel builds its list out of
    // that same manifest, so a name that is not on it did not come from the
    // panel — and `dsh plugin remove` is not the place to find out what else it
    // would have done with it.
    let names: Vec<&str> = names
        .iter()
        .filter(|name| held.iter().any(|held| held == *name))
        .map(String::as_str)
        .collect();

    if names.is_empty() {
        return Err(t!(
            "选中的插件不在这个 profile 里，没有可卸载的。",
            "nothing selected is installed in this profile."
        )
        .to_string());
    }

    let dsh = crate::dsh::current(app).ok_or_else(|| {
        t!(
            "这台机器上还没有装好的 dsh。重启应用会再装一次。",
            "there is no working dsh on this machine. Restarting the app installs one."
        )
        .to_string()
    })?;

    ensure_pnpm(app, log)?;

    log(&t!("正在卸载：{}", "Removing: {}", names.join(" ")));

    // Built twice, because the retry below needs a `Command` of its own — they
    // are not reusable once run.
    let removal = || {
        let mut command = dsh.command();
        command.args(["plugin", "--profile", PROFILE, "remove"]);
        command.args(&names);
        crate::dsh::apply_path(app, &mut command);
        command
    };

    let mut outcome = run(removal(), log)?;

    // A removal stopped by the release-age cooldown is retried once with the
    // policy lifted for this one child, because the policy cannot be protecting
    // anything here: removing a dependency only ever takes installed code away.
    // What it *does* do is fail the removal over unrelated lockfile entries that
    // happen to be too new (pnpm/pnpm#10071), which is a wall the user has no
    // way through from a panel with one button on it.
    //
    // The env var, specifically. `--config.minimumReleaseAge=0` is silently
    // dropped by pnpm 12's Rust CLI while the env overlay is honored
    // (pnpm/pnpm#13929), so the flag would look like a fix and change nothing.
    if outcome.code != 0 && outcome.flagged(RELEASE_AGE) {
        log(t!(
            "卸载被 pnpm 的 minimumReleaseAge 拦下了。卸载不会引入新代码，正在跳过这项检查重试…",
            "The removal was blocked by pnpm's minimumReleaseAge. Removing adds no code, so retrying with that check skipped…"
        ));

        let mut retry = removal();
        retry.env("PNPM_CONFIG_MINIMUM_RELEASE_AGE", "0");
        outcome = run(retry, log)?;
    }

    match outcome.code {
        0 => {
            log(t!("插件已卸载。", "Plugins removed."));
            Ok(())
        }
        127 => Err(t!(
            "dsh 找不到 pnpm。卸载插件同样需要它。",
            "dsh could not find pnpm. Removing a plugin needs it too."
        )
        .to_string()),
        _ => Err(diagnose(app, &outcome, true)),
    }
}

/// Hand the profile directory to the file manager, for the one thing this module
/// will not do on the user's behalf — see the module docs.
pub fn open_directory(app: &AppHandle) {
    use tauri_plugin_opener::OpenerExt;

    let directory = profile_dir(app);
    // It does not exist until dsh has initialized the profile, which the first
    // install does; opening a missing path is an error dialog from the file
    // manager rather than from us.
    if let Err(error) = std::fs::create_dir_all(&directory) {
        eprintln!("dsh-desktop: could not create {}: {error}", directory.display());
    }
    if let Err(error) = app.opener().open_path(directory.to_string_lossy(), None::<&str>) {
        eprintln!("dsh-desktop: could not open the profile directory: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::{
        dependencies_of, parse, pnpm_blamed, pnpm_codes, requested, spec_name, wanted_gone,
        Outcome, PRESETS, RELEASE_AGE,
    };
    use tauri::Url;

    fn codes(line: &str) -> Vec<String> {
        pnpm_codes(line).collect()
    }

    /// The exact shape pnpm prints under a release-age refusal, taken from a
    /// real failure. Only the name is wanted, and a scoped name must survive
    /// having its version cut off.
    #[test]
    fn reads_the_package_a_refusal_blames() {
        assert_eq!(
            pnpm_blamed(
                "  dshmarket@1.36.0 was published at 2026-08-28T15:38:44.000Z, within the minimumReleaseAge cutoff (2026-08-27T17:12:08.726Z)"
            )
            .as_deref(),
            Some("dshmarket")
        );
        assert_eq!(
            pnpm_blamed("  @scope/pkg@2.0.0 was published at 2026-01-01T00:00:00.000Z").as_deref(),
            Some("@scope/pkg")
        );
        // Every other line pnpm prints, including the headline that carries the
        // error code, is not one of these.
        assert_eq!(pnpm_blamed("Progress: resolved 42, reused 0"), None);
        assert_eq!(
            pnpm_blamed("[ERR_PNPM_MINIMUM_RELEASE_AGE_VIOLATION] 2 lockfile entries failed"),
            None
        );
    }

    /// What a spec installs under, so a refusal can be matched against what was
    /// asked for. An exotic spec resolves to a name only the fetched manifest
    /// knows, so it must not guess.
    #[test]
    fn reads_the_name_a_spec_installs_under() {
        assert_eq!(spec_name("dshmarket").as_deref(), Some("dshmarket"));
        assert_eq!(spec_name("dshmarket@1.36.0").as_deref(), Some("dshmarket"));
        assert_eq!(spec_name("@scope/pkg@^2").as_deref(), Some("@scope/pkg"));
        assert_eq!(spec_name("@scope/pkg").as_deref(), Some("@scope/pkg"));
        assert_eq!(spec_name("github:owner/repo"), None);
        assert_eq!(spec_name(""), None);
    }

    /// The distinction the install retry turns on: a refusal naming only
    /// packages nobody asked for is stale lockfile baggage, while one naming a
    /// package on the command line is the check doing its job.
    #[test]
    fn tells_stale_lockfile_entries_from_the_install() {
        let blamed: std::collections::HashSet<String> =
            ["dshmarket".to_string()].into_iter().collect();

        let installing_something_else: std::collections::HashSet<String> =
            [spec_name("dsh-web-search-free").unwrap()].into_iter().collect();
        assert!(blamed.iter().all(|n| !installing_something_else.contains(n)));

        let installing_the_blamed: std::collections::HashSet<String> =
            [spec_name("dshmarket@1.36.0").unwrap()].into_iter().collect();
        assert!(blamed.iter().all(|n| installing_the_blamed.contains(n)));
    }

    /// The line from the failure this was written for, verbatim: pnpm puts the
    /// code in brackets alongside prose, so the token has to come out of a line
    /// that is not just the token.
    #[test]
    fn a_release_age_failure_is_recognised() {
        let outcome = Outcome {
            code: 1,
            codes: codes("[ERR_PNPM_MINIMUM_RELEASE_AGE_VIOLATION] 2 lockfile entries failed verification:")
                .into_iter()
                .collect(),
            blamed: Default::default(),
        };

        assert!(outcome.flagged(RELEASE_AGE));
    }

    /// Only `ERR_PNPM_*` tokens are kept, and punctuation around them is not
    /// part of the token — otherwise the bracketed form above would never match
    /// the bare constant.
    #[test]
    fn only_pnpm_error_codes_are_kept() {
        assert_eq!(codes("Progress: resolved 42, reused 0"), Vec::<String>::new());
        assert_eq!(
            codes(" ERR_PNPM_FETCH_404  ERR_OTHER_THING error"),
            vec!["ERR_PNPM_FETCH_404"]
        );
    }

    /// The retry in [`super::remove`] lifts the cooldown through the
    /// environment, not through `--config.minimumReleaseAge=0`, because pnpm
    /// 12's Rust CLI silently ignores that flag while honoring the env overlay
    /// (pnpm/pnpm#13929). A flag would look like a fix and change nothing, so
    /// the spelling of the variable is worth pinning.
    #[test]
    fn the_cooldown_is_lifted_through_the_environment() {
        let source = include_str!("plugins.rs");

        assert!(source.contains(r#".env("PNPM_CONFIG_MINIMUM_RELEASE_AGE", "0")"#));

        // Built rather than written out, because a literal spelling the flag
        // out would appear in this file and match itself. The flag does appear
        // in prose above, explaining why it is not used; what must not appear
        // is the flag being passed as an argument.
        let flag = format!("{}{}", "--config.minimum", "-release-age");
        assert!(!source.contains(&format!("arg(\"{flag}")));
        assert!(!source.contains(&format!("\"{flag}=0\"")));
    }

    /// A run that named a different pnpm failure must not be reported as the
    /// release-age one — the whole point of switching on the code.
    #[test]
    fn an_unrelated_failure_is_not_release_age() {
        let outcome = Outcome {
            code: 1,
            codes: codes("[ERR_PNPM_NO_MATCHING_VERSION] no matching version")
                .into_iter()
                .collect(),
            blamed: Default::default(),
        };

        assert!(!outcome.flagged(RELEASE_AGE));
    }

    /// The list that actually ships, read the way the app reads it. Every entry
    /// has to survive [`parse`] — one that does not is silently missing from the
    /// panel — and only the entries that say so are ticked.
    #[test]
    fn the_shipped_list_parses() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join(PRESETS);
        let raw = std::fs::read_to_string(&path).expect("the shipped preset list");
        let entries: Vec<serde_json::Value> =
            serde_json::from_str(&raw).expect("the preset list is a JSON array");

        let presets = parse(&raw);
        assert_eq!(presets.len(), entries.len(), "an entry was dropped by parse");

        for (preset, entry) in presets.iter().zip(&entries) {
            assert_eq!(
                preset.checked,
                entry.get("checked").and_then(serde_json::Value::as_bool) == Some(true),
                "{} is ticked when the file does not say so",
                preset.id
            );
            assert!(!preset.spec.is_empty(), "{} has no spec", preset.id);
            // A section is never empty, so the panel always has a group to put
            // an entry in — including one written before sections existed.
            assert!(
                !preset.section.is_empty(),
                "{} has an empty section",
                preset.id
            );
        }
    }

    /// The two groups the panel draws, both actually present in the shipped
    /// list. A typo in one of these names is a heading the panel renders empty
    /// and a plugin that quietly falls to the end of the list.
    #[test]
    fn the_shipped_list_is_grouped() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join(PRESETS);
        let raw = std::fs::read_to_string(&path).expect("the shipped preset list");
        let presets = parse(&raw);

        for group in ["recommended", "authored"] {
            assert!(
                presets.iter().any(|preset| preset.section == group),
                "nothing is in the {group} group"
            );
        }
    }

    /// An entry from before sections existed still lands in a group.
    #[test]
    fn an_entry_without_a_section_is_recommended() {
        let presets = parse(
            r#"[{"id":"a","spec":"a","name":"A","description":"d"},
                {"id":"b","spec":"b","name":"B","description":"d","section":"  "}]"#,
        );

        assert_eq!(presets.len(), 2);
        assert!(presets.iter().all(|preset| preset.section == "recommended"));
    }

    #[test]
    fn reads_what_the_profile_holds() {
        let manifest = r#"{
            "dependencies": { "dshmarket": "^1.14.1", "dsh-better-sidebar": "^0.3.0" },
            "dsh": { "profile": { "bundles": ["@deepseek-ai/dsh-base", "dshmarket"] } }
        }"#;

        let held = dependencies_of(manifest);

        assert_eq!(held.len(), 2);
        assert!(held
            .iter()
            .any(|(name, range)| name == "dshmarket" && range == "^1.14.1"));
        // The layer stack is not a list of things to offer removing: the two
        // `@deepseek-ai` entries on it are the profile itself.
        assert!(!held.iter().any(|(name, _): &(String, String)| name.starts_with("@deepseek-ai")));
    }

    #[test]
    fn a_profile_holding_nothing_offers_nothing() {
        // dsh drops the key entirely when the last dependency goes.
        assert!(dependencies_of(r#"{"name":"dsh-profile-web"}"#).is_empty());
        assert!(dependencies_of("{}").is_empty());
        assert!(dependencies_of("not json at all").is_empty());
    }

    #[test]
    fn reads_a_list_of_names() {
        let url = Url::parse("dsh-window://plugins-remove?names=dshmarket,,%20dsh-better-sidebar%20")
            .expect("a URL");

        assert_eq!(
            wanted_gone(&url),
            vec!["dshmarket".to_string(), "dsh-better-sidebar".to_string()]
        );
    }

    /// A `plugins-install` navigation, as the panel builds one.
    fn asked(query: &str) -> (Vec<String>, Option<String>) {
        requested(&Url::parse(&format!("dsh-window://plugins-install?{query}")).expect("a URL"))
    }

    #[test]
    fn reads_a_list_of_ids() {
        let (ids, spec) = asked("ids=dshmarket,dsh-notification");

        assert_eq!(ids, ["dshmarket", "dsh-notification"]);
        assert!(spec.is_none());
    }

    #[test]
    fn reads_a_typed_spec() {
        let (ids, spec) = asked("ids=&spec=github%3Aowner%2Frepo");

        assert!(ids.is_empty());
        assert_eq!(spec.as_deref(), Some("github:owner/repo"));
    }

    /// An empty box is not a spec, and a trailing comma is not an id.
    #[test]
    fn drops_the_empty_parts() {
        let (ids, spec) = asked("ids=a,,b,&spec=%20%20");

        assert_eq!(ids, ["a", "b"]);
        assert!(spec.is_none());
    }
}
