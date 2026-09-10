//! Profile plugins: listing what dsh can be extended with, and installing one
//! for a user who has no way to do it themselves.
//!
//! `dsh plugin --profile web <args…>` is a thin pnpm forwarder — it initializes
//! `$DSH_HOME/profiles/web`, runs `pnpm <args…>` in it, and reconciles the
//! `dsh.profile.bundles` list against what pnpm actually installed. Which means
//! installing a plugin needs three things on the machine: dsh, the Node under
//! it, and pnpm. This app already goes to some trouble to get the first two onto
//! a machine that has neither — and then leaves them where only it can reach
//! them, because it does not write to the user's PATH (see `dsh`). So a user who
//! arrived with no Node has a working dsh, no `dsh` command in their terminal,
//! and no way to type the command that would add a plugin to it.
//!
//! Hence this module. It runs that command with the paths the app resolved,
//! puts pnpm beside them when it is missing, and prints the whole thing onto a
//! panel drawn over whatever the window is showing (see `panel`). There is
//! nothing here the user could not have done in a terminal; the point is that
//! this user cannot open one that has dsh in it.
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
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::channel;
use std::sync::Mutex;

use tauri::AppHandle;

/// The shim names and the three states one can be in, from the module that owns
/// finding them; see [`crate::dsh::DSH`]. Imported rather than requalified at
/// each of the five call sites below, so that the names this module asks about
/// are visibly the same ones the app resolves dsh with.
use crate::dsh::{Tool, DSH, PNPM};

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

/// The spec prefix of a plugin that ships inside this app.
///
/// Not a scheme pnpm has ever heard of — it is resolved here, before pnpm sees
/// it, into the absolute path of the staged directory. It has to be resolved
/// rather than written down because that path is only known at runtime: the
/// user chose where to install the app. See [`bundled`] and [`local_spec`].
const BUNDLED: &str = "bundled:";

/// What pnpm is handed for one preset, which for all but [`BUNDLED`] is what
/// the file already says.
fn spec_for(app: &AppHandle, spec: &str) -> Result<String, String> {
    let Some(name) = spec.strip_prefix(BUNDLED) else {
        return Ok(spec.to_string());
    };

    let dir = bundled(app, name).ok_or_else(|| {
        t!(
            "{} 应该随这个应用一起装上的，但它不在安装目录里。重新安装一次应用能把它带回来。",
            "{} ships inside this app, and it is not in the installation directory.              Reinstalling the app puts it back.",
            name
        )
    })?;

    Ok(local_spec(&dir))
}

/// Where a directory that ships with the app is: the staged resource, or the
/// one in the source tree when this is a `tauri dev` build that has never been
/// bundled. Same two places and same order as [`preset_file`].
///
/// The two names line up on purpose: `scripts/bundle-runtime.mjs` stages the
/// repository's `plugin/` as `resources/plugin`, so one preset spec addresses
/// both. A `tauri dev` run has the staged copy too — it is restaged on every
/// launch — so what a dev build links is that copy rather than the working
/// tree. Editing the plugin against a running dsh is `dsh plugin add -w
/// ./plugin`, which is a link to the working tree; see `docs/notifications.md`.
fn bundled(app: &AppHandle, name: &str) -> Option<PathBuf> {
    if let Some(staged) = crate::dsh::resources(app).map(|dir| dir.join(name)) {
        if staged.is_dir() {
            return Some(staged);
        }
    }

    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .join(name);
    source.is_dir().then_some(source)
}

/// A local directory as pnpm has to receive it.
///
/// The quotes on Windows are load-bearing. `dsh plugin` forwards its arguments
/// to pnpm through Node's `spawnSync(…, { shell: true })`, and Node does not
/// quote when it does that — it joins the arguments with spaces and hands the
/// one string to `cmd.exe`. The app installs to `C:\Program Files\…` by
/// default, so an unquoted spec arrives at pnpm as *two* arguments, and what
/// pnpm does with them is not fail: it installs two dependencies named after
/// the halves (`Program`, `plugin`), warns that neither declares a bundle, and
/// exits 0. Measured, both halves — and Rust's own escaping on the way out
/// carries the quotes through intact rather than eating them.
///
/// Nothing to quote anywhere else: off Windows there is no shell in the path,
/// and a quote would become part of the filename.
fn local_spec(dir: &Path) -> String {
    let path = dir.display().to_string();
    if cfg!(windows) {
        format!("\"{path}\"")
    } else {
        path
    }
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
    // One read and one parse for both questions below. They used to be a call
    // apiece, each opening the profile manifest for itself — and pnpm is free
    // to rewrite that directory between two reads of it, which would build the
    // list of what can go in and the list of what is in out of two different
    // files.
    let manifest = profile_manifest(app);
    let installed = installed_in(&manifest);

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
    let held: Vec<serde_json::Value> = dependencies_in(&manifest)
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

/// The profile manifest, read and parsed once.
///
/// `Value::Null` for a profile that is not there yet, or a manifest that is not
/// JSON — which both readers below answer as an empty profile, the same as the
/// separate reads they replaced did.
fn profile_manifest(app: &AppHandle) -> serde_json::Value {
    std::fs::read_to_string(profile_dir(app).join("package.json"))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(serde_json::Value::Null)
}

/// What pnpm was asked to install: every dependency, with the range recorded.
///
/// `dsh.profile.bundles` is deliberately not read here, though [`installed_in`]
/// reads both: `@deepseek-ai/dsh-base` and `@deepseek-ai/dsh-web-app` are on
/// that list and are not plugins. Offering to remove the profile's own
/// foundation would be offering to break it.
fn dependencies(app: &AppHandle) -> Vec<(String, String)> {
    dependencies_in(&profile_manifest(app))
}

fn dependencies_in(manifest: &serde_json::Value) -> Vec<(String, String)> {
    manifest
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
fn installed_in(manifest: &serde_json::Value) -> HashSet<String> {
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

/// The plugin that tells this app what dsh is doing, by the name it installs
/// under. See [`crate::signal`] for what it reports and `plugin/` for the
/// thing itself.
///
/// Written down rather than read out of the preset list, because what depends
/// on it is one specific plugin and not "whatever the list happens to ship".
/// Both spellings are checked against the plugin's own manifest by a test, so
/// this and the list cannot drift apart from it or from each other.
pub const SIGNAL: &str = "dsh-desktop-signal";

/// Whether dsh has any way to tell this app what it is doing.
///
/// Which is the same question as "is [`SIGNAL`] installed", because it is the
/// only answer left. This app used to sniff dsh's DOM for a finished turn and
/// for a question waiting to be answered; both of those are gone, and every
/// notification now starts as a signal from that plugin. So without it there
/// is nothing to notify about, and the switch that turns notifications on is
/// drawn unavailable rather than on-and-silent. See [`crate::notify::show`],
/// which is the gate, and [`crate::controls::sync_notify`], which is the way
/// the menu is told.
///
/// Read from the profile manifest, and read the same way the panel decides
/// what is "already installed" — those two answers agreeing is the whole
/// point, since the panel is where a user goes to change this one.
pub fn signalling(app: &AppHandle) -> bool {
    installed_in(&profile_manifest(app)).contains(SIGNAL)
}

/// Put [`SIGNAL`] in, once, on the first launch that finds it missing.
///
/// The plugin ships inside this app and used to wait in the panel for someone
/// to notice it, which made every notification wait on a step the user had no
/// reason to know about: the menu item said what was missing, and that is a
/// worse place to learn it than never having to.
///
/// One offer and no more. A user who takes the plugin back out has decided
/// something, and a launch that reinstalled it would be arguing — so this asks
/// [`remembered`] first and answers to nothing else. That also settles what
/// happens on the launch after a failed one: nothing. The panel still lists
/// it, and installing it there is the same install this would have run.
///
/// Recorded before the install rather than after, for the reason
/// [`mark_guided`] is: an install that takes this launch down with it should
/// not take the next one down too. So what is spent is the attempt, not the
/// success.
///
/// Which is why the one attempt has to be *seen* when it fails, and why this
/// answers whether it did. Borrowing [`mark_guided`]'s rule without borrowing
/// a way to say so would not be borrowing the same trade: what a spent marker
/// costs there is a panel the user can open whenever they like, and what it
/// costs here is every notification, silently, until somebody notices a menu
/// item is grey. The caller turns a `true` into the plugin panel — the one
/// place that lists this plugin, installs it on a click, and prints why if it
/// fails again. Retrying on the next launch instead would put an npm timeout
/// in front of every launch a machine spends offline.
///
/// Blocking, for as long as an install takes. The caller runs it on the boot
/// thread with no server up yet — which is the point of running it there
/// rather than later, since a plugin going in is a reason to stop `dsh web`
/// and there is nothing yet to stop.
///
/// @returns Whether this launch tried to install the plugin and could not.
/// False covers every other outcome, the three that do nothing included: the
/// offer was already spent, the plugin was already there, or it went in.
pub fn adopt(app: &AppHandle, report: &crate::dsh::Report) -> bool {
    if remembered(app, ADOPTED) {
        return false;
    }

    // Already here — put in from the panel by hand, or by a build that shipped
    // before this did. Nothing to install, and the offer is spent either way,
    // so that taking it out later stays taken out.
    if signalling(app) {
        remember(app, ADOPTED);
        return false;
    }

    remember(app, ADOPTED);
    report(
        t!(
            "正在装上「会话信号」插件…",
            "Installing the session signal plugin…"
        ),
        -1.0,
    );

    // To the terminal and nowhere else. This install is the app's own idea,
    // running before the window belongs to dsh and with no panel to print
    // into; a wall of pnpm output on the loading page would be answering a
    // question nobody asked. What the user sees is the line above.
    let log = |line: &str| eprintln!("dsh-desktop: {line}");
    match install(app, &[SIGNAL.to_string()], None, &log) {
        Ok(()) => false,
        Err(why) => {
            // The terminal gets the reason, because the panel the caller is
            // about to raise cannot hold it: this install ran before the panel
            // existed and its log started empty. What the panel offers is the
            // same install again, this time with somewhere to print.
            eprintln!("dsh-desktop: could not install {SIGNAL} on this launch: {why}");
            true
        }
    }
}

/// `$DSH_HOME/profiles/web`, where a plugin ends up.
///
/// `DSH_HOME` is dsh's own variable and this app passes it through untouched, so
/// the answer has to be worked out the way dsh works it out — not read off
/// anything of ours.
pub fn profile_dir(app: &AppHandle) -> PathBuf {
    // Taken and not used, and the one place in this module that is true. The
    // answer is `$DSH_HOME`, which is dsh's and is read straight off the
    // environment — nothing about it comes from this app's own state. The
    // handle stays in the signature because every other path here is asked
    // for one and a dozen call sites would otherwise split into two shapes for
    // no gain; `dsh_home` below, which has one caller, does without it.
    let _ = app;
    dsh_home().join("profiles").join(PROFILE)
}

fn dsh_home() -> PathBuf {
    if let Some(home) = std::env::var_os("DSH_HOME") {
        return PathBuf::from(home);
    }

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
        specs.push(spec_for(app, &preset.spec)?);
    }
    if let Some(extra) = extra.map(str::trim).filter(|extra| !extra.is_empty()) {
        if !is_package_spec(extra) {
            return Err(t!(
                "{} 不是可以装的插件。这里接受 npm 上的包（@scope/name、name@1.2.3）\
                 和 github:owner/repo；本地路径、tarball 地址和其它协议都不行。",
                "{} is not something this can install. This field takes a package on npm — \
                 `@scope/name`, `name@1.2.3` — or `github:owner/repo`. Not a local path, \
                 a tarball URL, or another scheme.",
                extra
            ));
        }
        specs.push(extra.to_string());
    }

    if specs.is_empty() {
        return Err(t!("没有选择任何插件", "nothing was selected").to_string());
    }

    let dsh = crate::dsh::current(app).ok_or_else(|| {
        // `current` answers `None` both for a machine with no dsh and for one
        // whose `dsh` shim is present but broken — a dangling symlink left by a
        // Node version switch. Those need different things done about them, and
        // telling the second user there is "no dsh" contradicts the working
        // `dsh` in their terminal.
        match crate::dsh::tool(app, DSH) {
            Tool::Broken(why) => t!(
                "找到了 dsh，但它无法运行。{}请重新安装 dsh 之后再装插件：npm install -g @deepseek-ai/dsh",
                "dsh is here but cannot run. {}Reinstall dsh before installing plugins: npm install -g @deepseek-ai/dsh",
                why
            ),
            _ => t!(
                "这台机器上还没有装好的 dsh，插件没有可以装进去的地方。",
                "there is no working dsh on this machine for a plugin to go into."
            )
            .to_string(),
        }
    })?;

    ensure_pnpm(app, log)?;
    repair(app, log);

    log(&t!("正在安装：{}", "Installing: {}", specs.join(" ")));

    let mut command = Command::new(&dsh.bin);
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

    // Marked for as long as pnpm is in the profile. The `?` on this run and on
    // the retry below are deliberately not places that clear it: an install the
    // app killed on its way out is exactly what the next run needs to know
    // about, and it is the only thing that gets to leave the file behind.
    mark(app);
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

            let mut retry = Command::new(&dsh.bin);
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

    // pnpm exited on its own, whatever it exited with, so the directory is in a
    // state it chose rather than one it was interrupted in.
    unmark(app);

    match outcome.code {
        0 => {
            log(t!("插件安装完成。", "Plugins installed."));
            Ok(())
        }
        // 127 is what dsh answers with when pnpm is not on the PATH it was
        // given. Reaching it means `ensure_pnpm` said yes to something that then
        // could not be executed, so the shim it accepted is re-examined: a
        // broken one has a specific cause worth printing, and blaming the
        // install step for a pnpm that was already there and already broken sent
        // the user after the wrong thing.
        127 => Err(match crate::dsh::tool(app, PNPM) {
            Tool::Broken(why) => {
                t!("dsh 无法运行 pnpm。{}", "dsh could not run pnpm. {}", why)
            }
            _ => t!(
                "dsh 找不到 pnpm。插件安装需要 pnpm，自动安装它这一步没有成功。",
                "dsh could not find pnpm. Installing plugins needs it, and installing pnpm did not work."
            )
            .to_string(),
        }),
        _ => Err(diagnose(app, &outcome, false)),
    }
}

/// The pnpm error code for a package younger than the `minimumReleaseAge`
/// cooldown the machine is configured with.
const RELEASE_AGE: &str = "ERR_PNPM_MINIMUM_RELEASE_AGE_VIOLATION";

/// Clear what a plugin run that did not finish left behind, before pnpm walks
/// back into it.
///
/// The profile links its plugins into `node_modules` as junctions — dsh sets
/// `nodeLinker: hoisted`, under which pnpm wants a real directory at that path
/// and a junction is what it has to replace. Kill pnpm while it is working and
/// those junctions outlive the virtual store entries they point at, and a
/// dangling one is a wall pnpm cannot get past on its own: with its target gone
/// it is no longer a directory, so pnpm clears it the way it clears a file, and
/// the way to clear a file is `DeleteFileW` — which refuses a *directory* link
/// with `ERROR_ACCESS_DENIED` however free of locks the path is. Every later
/// install then fails on that entry, reporting a permission problem that no
/// amount of closing things fixes.
///
/// Reproduced from nothing but a dangling junction, with no dsh running and
/// nothing holding the path: `pnpm add` under a hoisted linker answers
/// `failed to clear non-directory dirent at "…": (os error 5)`, and answers it
/// again forever. See [`unlink`] for the call that does work, and
/// [`pnpm_stuck`] for the failure as it arrives.
///
/// Announced when it finds something, and when the run before this one is known
/// not to have finished — see [`UNFINISHED`] — because a repair nobody is told
/// about is indistinguishable from an install that inexplicably worked this
/// time.
fn repair(app: &AppHandle, log: &Log) {
    let unfinished = unfinished(app);
    if unfinished {
        log(t!(
            "上次的插件操作没有正常收尾，先看看 node_modules 里留下了什么…",
            "The last plugin run did not finish; checking what it left in node_modules…"
        ));
    }

    let cleared = sweep(&profile_dir(app).join("node_modules"));
    if !cleared.is_empty() {
        log(&t!(
            "清掉了装到一半留下的断链：{}",
            "Cleared dangling links from an unfinished install: {}",
            cleared.join(" ")
        ));
    } else if unfinished {
        log(t!("没有残留需要清理。", "Nothing was left behind."));
    }
}

/// Delete every link under `root` whose target is gone, and answer with what
/// was deleted.
///
/// The top level, and one level into each `@scope` directory: that is where a
/// package is linked in. `.pnpm` — the virtual store below it, thousands of
/// entries deep — is deliberately not walked. The failure this clears is at the
/// level pnpm is adding a package to, and a sweep of the store would be a long
/// walk for a problem nothing has reported.
fn sweep(root: &Path) -> Vec<String> {
    let mut cleared = Vec::new();

    for entry in read(root) {
        let name = entry.file_name().to_string_lossy().to_string();

        // A scope is a real directory with the links inside it.
        if name.starts_with('@') && entry.path().is_dir() {
            for scoped in read(&entry.path()) {
                if clear(&scoped.path()) {
                    cleared.push(format!("{name}/{}", scoped.file_name().to_string_lossy()));
                }
            }
            continue;
        }

        if clear(&entry.path()) {
            cleared.push(name);
        }
    }

    cleared
}

/// One entry: deleted if it is a link with nothing at the other end, left alone
/// otherwise. Answers whether it went.
fn clear(path: &Path) -> bool {
    let Ok(link) = std::fs::symlink_metadata(path) else {
        return false;
    };

    // `exists` follows the link, so one that still resolves answers `true` here
    // and is none of this function's business: pnpm replaces those itself, and
    // deleting a working one would take a plugin out from under the user.
    if !link.file_type().is_symlink() || path.exists() {
        return false;
    }

    match unlink(path) {
        Ok(()) => true,
        // Reported and not raised: pnpm is about to hit the same path and say
        // so in its own words, and [`diagnose`] is what turns that into
        // something the user can act on.
        Err(error) => {
            eprintln!(
                "dsh-desktop: could not clear the dangling link {}: {error}",
                path.display()
            );
            false
        }
    }
}

/// Remove a link of either kind.
///
/// A directory link has to go through `RemoveDirectoryW`, which is
/// `remove_dir`; `remove_file` is `DeleteFileW` and refuses one. A file link is
/// the other way round. Both are tried rather than asked about, because asking
/// needs a platform-specific answer — `FileTypeExt::is_symlink_dir` — and the
/// fallback does not.
fn unlink(path: &Path) -> std::io::Result<()> {
    std::fs::remove_dir(path).or_else(|_| std::fs::remove_file(path))
}

/// The entries of a directory that may not be there at all. A `node_modules`
/// that cannot be read is a profile nothing has installed into yet, which is
/// not a failure of the sweep.
fn read(dir: &Path) -> Vec<std::fs::DirEntry> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .collect()
}

/// The file that says a pnpm run in this profile did not get to finish.
///
/// Written before pnpm starts and removed once it has exited on its own.
/// What leaves it behind is the one path nothing can clean up from: [`stop`]
/// kills pnpm because the app is quitting, and the thread that would remove
/// this is racing the process exit. A file still here on the next run is that
/// having happened, and the next run's [`repair`] is what it is for.
///
/// A spawn that never got pnpm started leaves it too, which is why neither the
/// file nor the line the panel prints about it claims more than it knows: the
/// last run did not finish, and the directory is worth a look.
const UNFINISHED: &str = ".dsh-desktop-unfinished";

fn unfinished(app: &AppHandle) -> bool {
    profile_dir(app).join(UNFINISHED).is_file()
}

/// Best effort, and in both directions: a marker that could not be written
/// costs the next run its announcement, and one that could not be removed costs
/// it a sweep that finds nothing. Neither is worth failing a plugin run over.
fn mark(app: &AppHandle) {
    let dir = profile_dir(app);
    // No profile yet — dsh creates it on the run that is about to start, and
    // there is nothing in it for a killed pnpm to leave half-written.
    if !dir.is_dir() {
        return;
    }

    let path = dir.join(UNFINISHED);
    if let Err(error) = std::fs::write(&path, b"") {
        eprintln!("dsh-desktop: could not write {}: {error}", path.display());
    }
}

fn unmark(app: &AppHandle) {
    let path = profile_dir(app).join(UNFINISHED);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => eprintln!("dsh-desktop: could not remove {}: {error}", path.display()),
    }
}

/// Whether a hand-typed spec is something this app is willing to install.
///
/// The panel's other field is a list of ids looked up in the preset manifest, so
/// a name that is not on it is refused before anything runs. This one is free
/// text and goes straight into the argument list of `dsh plugin add`, where pnpm
/// takes a great deal more than a package name: a tarball URL, a `file:` path,
/// `git+ssh://`, a local directory. Each of those installs code from somewhere
/// that never passed through a registry, and each is reachable from any script
/// running in the window — `dsh-window://` is recognised on every top-level
/// navigation, whoever made it, so the payload is the only thing there is to be
/// strict about. See [`crate::controls::is_web_link`], which is the same
/// argument about the other sharp verb on that channel.
///
/// What is left through is what the panel actually offers: a registry spec, and
/// the `github:owner/repo` its placeholder shows. That is a deliberate hole —
/// a repository is not a registry — and it is here because installing a plugin
/// straight from GitHub is a feature of this box rather than an oversight in it.
/// What it does buy is the two forms that are only ever an attack: a payload
/// already sitting on the disk, which needs no network and no publishing at all,
/// and any other scheme's idea of where to fetch from.
///
/// The leading `-` goes with them for a second reason: these are `args`, so
/// `--registry=…` would be read as a flag to pnpm rather than as a package.
fn is_package_spec(spec: &str) -> bool {
    if spec.is_empty() || spec.starts_with('-') || spec.contains(['\\', ' ', '\t']) {
        return false;
    }

    match spec.split_once(':') {
        // A scheme, and only one of them is allowed. This is also what turns
        // away `C:/…`, whose drive letter parses as one.
        Some((scheme, rest)) => scheme == "github" && is_repository(rest),
        None => is_registry_spec(spec),
    }
}

/// `[@scope/]name[@range]`.
fn is_registry_spec(spec: &str) -> bool {
    // The range separator, if one was typed. A scoped name opens with `@`, which
    // is why it is the *last* one and why position zero is not it.
    let (name, range) = match spec.rfind('@') {
        Some(at) if at > 0 => (&spec[..at], Some(&spec[at + 1..])),
        _ => (spec, None),
    };

    let ranged = match range {
        None => true,
        // `1.2.3`, `latest`, `^1.0.0`, `~2.1`, `1.x`, `*`. Not a `/`, which is
        // what every path and URL form needs to say where it points.
        Some(range) => {
            !range.is_empty()
                && range.chars().all(|c| {
                    c.is_ascii_alphanumeric()
                        || matches!(c, '.' | '-' | '+' | '^' | '~' | '>' | '<' | '=' | '*' | '|')
                })
        }
    };

    ranged
        && match name.strip_prefix('@') {
            // The one `/` a package name may contain, and only after a scope.
            Some(scoped) => match scoped.split_once('/') {
                Some((scope, bare)) => is_name_part(scope) && is_name_part(bare),
                None => false,
            },
            None => is_name_part(name),
        }
}

/// What follows `github:` — `owner/repo`, optionally pinned with `#<ref>`.
fn is_repository(rest: &str) -> bool {
    let (path, reference) = match rest.split_once('#') {
        Some((path, reference)) => (path, Some(reference)),
        None => (rest, None),
    };

    let Some((owner, repo)) = path.split_once('/') else {
        return false;
    };

    is_name_part(owner)
        && is_name_part(repo)
        && match reference {
            None => true,
            // A branch, a tag or a commit. Slashes are ordinary in branch names.
            Some(reference) => {
                !reference.is_empty()
                    && reference
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '/'))
            }
        }
}

/// One segment of a name — a scope, a package, a repository owner. npm's own
/// rule, minus anything that could be read as a path or, in the leading
/// position, as a flag.
fn is_name_part(part: &str) -> bool {
    !part.is_empty()
        && !part.starts_with(['.', '_', '-'])
        && part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

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
    without_range(spec)
}

/// A `name@version` with the version range cut off, or `None` when what is
/// left is not usable as a name.
///
/// A scoped name keeps its leading `@`, so the separator is the *last* one and
/// only when something precedes it. Shared by [`spec_name`] and
/// [`pnpm_blamed`]: one reads a spec the user typed and the other a spec pnpm
/// printed, but `@scope/pkg@1.2.3` has to come apart the same way in both, and
/// it came apart in two places here before it came apart in one.
fn without_range(spec: &str) -> Option<String> {
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

    // Not split by `removing`, unlike the refusal above: what this says is
    // about the state of a directory, and that is the same state whichever verb
    // walked into it.
    if outcome.stuck {
        return t!(
            "pnpm 清不掉 {} 里的一个路径——装到一半留下的断链会这样，这次已经替你清过一遍了。如果还是失败：完全退出 dsh（含托盘图标）后删掉整个 node_modules 再装一次，里面的插件都能从 package.json 装回来。",
            "pnpm could not clear a path in {} — dangling links from an install that did not finish do this, and this run already swept them once. If it keeps failing: quit dsh completely, tray icon included, then delete the whole node_modules and install again. Everything in it comes back from package.json.",
            profile_dir(app).join("node_modules").display()
        );
    }

    t!(
        "dsh plugin 退出码 {}。上面是它的完整输出。",
        "dsh plugin exited with code {}. Its full output is above.",
        outcome.code
    )
}

/// Put pnpm where the dsh we are about to run will find it.
///
/// `npm install -g pnpm`, into the same global prefix the app's dsh lives in, so
/// that [`crate::dsh::apply_path`] puts it on the child's PATH along with
/// everything else. Downloading a standalone pnpm would mean a second installer
/// in this app — a fetch, a checksum, an archive to unpack, a mirror list to
/// walk — to arrive at a binary npm can place in one command.
fn ensure_pnpm(app: &AppHandle, log: &Log) -> Result<(), String> {
    // A pnpm that is there and runs is the whole check. A pnpm that is there and
    // does *not* run has to be named: reinstalling over a dangling symlink is
    // what npm is about to be asked to do, and if that fails the user needs to
    // know it was already broken rather than merely absent.
    //
    // Both arms are built into a `String` before logging. `t!` hands back a
    // `&'static str` with no arguments and a `String` with them, so taking the
    // owned form in both keeps this a single `log` call instead of two that
    // differ only by an `&`.
    let announce = match crate::dsh::tool(app, PNPM) {
        Tool::Ready => return Ok(()),
        Tool::Missing => t!(
            "没有找到 pnpm，先安装它（dsh 的插件安装是转发给 pnpm 的）…",
            "No pnpm found; installing it first (dsh forwards plugin installs to pnpm)…"
        )
        .to_string(),
        Tool::Broken(why) => t!(
            "已有的 pnpm 无法运行。{}正在重新安装…",
            "The pnpm already here cannot run. {}Reinstalling it now…",
            why
        ),
    };
    log(&announce);

    let mut npm = crate::dsh::npm(app).ok_or_else(|| {
        t!(
            "找不到 npm，无法安装 pnpm。",
            "no npm to install pnpm with."
        )
        .to_string()
    })?;
    npm.args(["install", "-g", "pnpm"]);

    // Where it goes is chosen rather than left to npm; see `tool_prefix`. Two
    // failures come from letting npm decide: an `EACCES` on a prefix under
    // `/usr` or `/usr/local` that the user cannot write to, and a success into
    // some prefix that is not the one `search_path` looks in — a pnpm that
    // exists and cannot be found.
    //
    // `--prefix` rather than a `PATH` trick, because it is npm's own answer to
    // this question and it is what `install-deps.sh` already passes for dsh.
    let prefix = crate::dsh::tool_prefix(app);
    if let Some(prefix) = prefix.as_deref() {
        npm.arg("--prefix").arg(prefix);
        log(&t!(
            "把 pnpm 装到 {}",
            "Installing pnpm into {}",
            prefix.display()
        ));
    }

    // Only the exit code matters here: this runs npm, not pnpm, so there are no
    // `ERR_PNPM_*` codes to switch on.
    match run(npm, log)?.code {
        // npm exiting 0 is not the same as pnpm being runnable: it will report
        // success having written a shim whose target the next step cannot
        // execute, or into a prefix nothing on the search path looks at. The
        // claim this function makes is that pnpm runs, so it is checked.
        0 => match crate::dsh::tool(app, PNPM) {
            Tool::Ready => {
                if let Some(prefix) = prefix.as_deref() {
                    claim_pnpm(prefix);
                }
                log(t!("pnpm 安装完成。", "pnpm installed."));
                Ok(())
            }
            Tool::Broken(why) => Err(t!(
                "npm 报告 pnpm 安装成功，但它仍然无法运行。{}",
                "npm reported pnpm installed, but it still cannot run. {}",
                why
            )),
            Tool::Missing => Err(t!(
                "npm 报告 pnpm 安装成功，但在 dsh 会搜索的目录里找不到它。",
                "npm reported pnpm installed, but it is not in any directory dsh searches."
            )
            .to_string()),
        },
        // A prefix was chosen above precisely so that the permission failure
        // cannot happen, so this is no longer assumed to be one. It is still
        // *checked*, and both halves of the answer matter: which directory npm
        // was aimed at, and whether the machine's own global prefix is one the
        // user cannot write to.
        //
        // The unwritable prefix is reported whenever there is one, including
        // when a `--prefix` of our own was passed. Those coexist: `tool_prefix`
        // falls back to the app's own directory exactly because the machine's
        // prefix was unwritable, so on the `/usr` Node this is all about, the
        // install is aimed somewhere writable *and* the standing problem is
        // still worth naming — it is why dsh and pnpm are not on the user's own
        // PATH, and moving npm's prefix is what fixes that for good.
        code => Err(match (prefix, crate::dsh::unwritable_prefix(app)) {
            // Aimed at a directory of ours, and the machine's global prefix is
            // also unwritable: say both, so neither the target nor the advice
            // has to be guessed at.
            (Some(target), Some(blocked)) => t!(
                "安装 pnpm 失败（npm 退出码 {}），目标目录是 {}。另外，npm 的全局目录 {} 不可写——把它换到你自己拥有的位置可以一并解决终端里找不到 dsh 的问题：npm config set prefix ~/.npm-global（并把 ~/.npm-global/bin 加进 PATH）。上面是 npm 的完整输出。",
                "Installing pnpm failed (npm exit code {}) with {} as the target. Separately, npm's global directory {} is not writable — pointing it at a prefix you own also fixes dsh not being found in your terminal: npm config set prefix ~/.npm-global (and add ~/.npm-global/bin to your PATH). npm's full output is above.",
                code,
                target.display(),
                blocked.display()
            ),
            // Installed into a directory this app picked and can write to, and
            // nothing else is known to be blocked — so permissions are not it.
            (Some(target), None) => t!(
                "安装 pnpm 失败（npm 退出码 {}），目标目录是 {}。这不是权限问题——上面是 npm 的完整输出，通常是网络或注册表访问失败。",
                "Installing pnpm failed (npm exit code {}) with {} as the target. This is not a permission problem — npm's full output is above, and it is usually a network or registry failure.",
                code,
                target.display()
            ),
            // No prefix of our own to aim at, so npm chose, and it chose one the
            // user cannot write to: the plain permission case.
            (None, Some(blocked)) => t!(
                "安装 pnpm 失败（npm 退出码 {}）：全局目录 {} 不可写，需要管理员权限。可以把 npm 的全局目录换到你自己拥有的位置再重试：npm config set prefix ~/.npm-global（并把 ~/.npm-global/bin 加进 PATH）。",
                "Installing pnpm failed (npm exit code {}): the global directory {} is not writable and needs administrator rights. Point npm at a prefix you own and retry: npm config set prefix ~/.npm-global (and add ~/.npm-global/bin to your PATH).",
                code,
                blocked.display()
            ),
            (None, None) => t!(
                "安装 pnpm 失败，npm 退出码 {}。上面是它的完整输出。",
                "installing pnpm failed; npm exited with code {}. Its full output is above.",
                code
            ),
        }),
    }
}

/// The file [`ensure_pnpm`] drops in a prefix to say that the pnpm in it is
/// ours.
///
/// Nothing else writes one, which is the whole point: a pnpm without it was
/// installed by the user, and `uninstall-dsh` leaves it alone. Kept as a file
/// beside the install rather than as a field in the bootstrap marker because
/// pnpm is per-prefix — a machine with three Nodes can end up with three, and
/// the marker records one of everything — and because the scripts are what read
/// it, in a mode that is handed a Node rather than the marker's.
///
/// The name is spelled again in `install-deps.sh` and `install-deps.ps1`, which
/// are the readers. Changing it here means changing it there.
const PNPM_MARK: &str = ".dsh-owns-pnpm";

/// Record that the pnpm now in `prefix` is one this app installed.
///
/// Best effort. A prefix that cannot be written to is not a failure of the
/// install that just succeeded — npm had to write there for it to succeed at all
/// — and the only cost of a missing file is a pnpm that outlives the dsh it came
/// for, which is exactly where this app was before the file existed.
fn claim_pnpm(prefix: &std::path::Path) {
    let mark = prefix.join(PNPM_MARK);
    if let Err(error) = std::fs::write(&mark, b"") {
        eprintln!(
            "dsh-desktop: could not record that {} is ours: {error}",
            mark.display()
        );
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
    /// Whether the run died on a path it could not delete or replace. Not an
    /// `ERR_PNPM_*` code, which is why it is a field of its own rather than one
    /// more entry in `codes`: this failure comes out of pnpm's Rust half, which
    /// reports an OS error and no code at all. See [`pnpm_stuck`].
    stuck: bool,
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

    without_range(spec)
}

/// Whether this line is pnpm failing to delete or replace a path.
///
/// The one failure in this module that pnpm reports with no error code to
/// switch on, and the one the panel used to hand over as a bare exit code — a
/// user who clicked a button being shown `os error 5` and left to work out that
/// the fix is a directory this app put there.
///
/// The first mark is the exact wording of the Windows failure [`repair`]
/// exists for, and it is the one that carries the case: pnpm wraps the failing
/// path across several lines, so `os error 5` can arrive split down the middle
/// — `(os error` on one line and `5)` on the next — while the phrase itself
/// stays whole. The rest are the same event in the words other parts of pnpm
/// use: a path that will not go away, whether because it is a directory link on
/// Windows, because something holds it open, or because the account cannot
/// write there. All four end at the same advice, which is what makes them one
/// case rather than four.
///
/// `EACCES` is deliberately not among them. It is the store or the npm prefix
/// being unwritable — a different directory, with a different fix, and
/// [`crate::dsh::unwritable_prefix`] already names it where it happens.
fn pnpm_stuck(line: &str) -> bool {
    const MARKS: [&str; 4] = [
        "failed to clear non-directory dirent",
        "os error 5",
        "EPERM",
        "EBUSY",
    ];

    MARKS.iter().any(|mark| line.contains(mark))
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
    let mut stuck = false;
    for line in rx {
        eprintln!("[plugin] {line}");
        codes.extend(pnpm_codes(&line));
        blamed.extend(pnpm_blamed(&line));
        stuck |= pnpm_stuck(&line);
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
        stuck,
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
    remembered(app, GUIDED)
}

pub fn mark_guided(app: &AppHandle) {
    remember(app, GUIDED);
}

/// The panel has been shown. See [`guided`].
const GUIDED: &str = "plugins-guided";

/// [`SIGNAL`] has had its one offer. See [`adopt`].
const ADOPTED: &str = "signal-adopted";

/// Whether a launch has already done the thing `name` stands for.
///
/// Both markers are empty files. What either one records is that something
/// happened once, and a file either is there or is not — there is nothing to
/// put inside it that its own presence does not already say.
fn remembered(app: &AppHandle, name: &str) -> bool {
    marker(app, name).is_some_and(|path| path.exists())
}

/// Record that it happened. Failing to is not worth stopping for: the cost is
/// the same launch doing the same thing once more next time.
fn remember(app: &AppHandle, name: &str) {
    let Some(path) = marker(app, name) else { return };

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(error) = std::fs::write(&path, b"") {
        eprintln!("dsh-desktop: could not record {name}: {error}");
    }
}

fn marker(app: &AppHandle, name: &str) -> Option<PathBuf> {
    Some(crate::dsh::app_dir(app)?.join(name))
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
        // The same two cases as in `install`; see the note there.
        match crate::dsh::tool(app, DSH) {
            Tool::Broken(why) => t!(
                "找到了 dsh，但它无法运行。{}请先重新安装 dsh：npm install -g @deepseek-ai/dsh",
                "dsh is here but cannot run. {}Reinstall dsh first: npm install -g @deepseek-ai/dsh",
                why
            ),
            _ => t!(
                "这台机器上还没有装好的 dsh。",
                "there is no working dsh on this machine."
            )
            .to_string(),
        }
    })?;

    ensure_pnpm(app, log)?;
    // A removal rewrites `node_modules` exactly as an install does, so it walks
    // into the same residue and is stopped by it the same way.
    repair(app, log);

    log(&t!("正在卸载：{}", "Removing: {}", names.join(" ")));

    // Built twice, because the retry below needs a `Command` of its own — they
    // are not reusable once run.
    let removal = || {
        let mut command = Command::new(&dsh.bin);
        command.args(["plugin", "--profile", PROFILE, "remove"]);
        command.args(&names);
        crate::dsh::apply_path(app, &mut command);
        command
    };

    // See `install`: held across both runs, cleared only once pnpm has exited.
    mark(app);
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

    unmark(app);

    match outcome.code {
        0 => {
            log(t!("插件已卸载。", "Plugins removed."));
            Ok(())
        }
        // As in `install`: a broken shim is a different problem from a missing
        // one, and only one of them is fixed by installing pnpm again.
        127 => Err(match crate::dsh::tool(app, PNPM) {
            Tool::Broken(why) => {
                t!("dsh 无法运行 pnpm。{}", "dsh could not run pnpm. {}", why)
            }
            _ => t!(
                "dsh 找不到 pnpm。卸载插件同样需要它。",
                "dsh could not find pnpm. Removing a plugin needs it too."
            )
            .to_string(),
        }),
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
        dependencies_in, is_package_spec, local_spec, parse, pnpm_blamed, pnpm_codes, pnpm_stuck,
        requested, spec_name, sweep, wanted_gone, Outcome, BUNDLED, PRESETS, RELEASE_AGE, SIGNAL,
    };
    use std::path::{Path, PathBuf};
    use tauri::Url;

    /// `dependencies_in` reads a parsed manifest now, because `listing` parses
    /// the file once and asks it two questions. What a manifest that will not
    /// parse at all comes out as is `profile_manifest`'s answer — `Value::Null`
    /// — which is what this hands over for the unparseable cases below.
    fn dependencies_of(manifest: &str) -> Vec<(String, String)> {
        dependencies_in(&serde_json::from_str(manifest).unwrap_or(serde_json::Value::Null))
    }

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
            stuck: false,
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
            stuck: false,
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

    /// The plugin this app ships inside itself, checked against the plugin
    /// itself.
    ///
    /// Three things have to line up and none is checked anywhere else. The
    /// spec has to name a directory that is really there — `bundled:` is
    /// resolved at install time, so a rename shows up as an install that
    /// cannot find itself. And the preset's `package` has to be the name that
    /// directory installs under, because that name is what "already
    /// installed" is decided against: get it wrong and the panel offers the
    /// plugin forever, to a user who already has it. And the preset's `id` has
    /// to be that name too, because `adopt` asks for this one preset by id and
    /// `install` looks ids up in the list — a drift there breaks only the
    /// automatic install, which is the one nobody is watching.
    #[test]
    fn the_plugin_that_ships_inside_the_app_is_the_one_the_list_names() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the repository root");
        let raw = std::fs::read_to_string(root.join("src-tauri/resources").join(PRESETS))
            .expect("the shipped preset list");

        let mut found = 0;
        for preset in parse(&raw) {
            let Some(name) = preset.spec.strip_prefix(BUNDLED) else {
                continue;
            };
            found += 1;

            let manifest = root.join(name).join("package.json");
            let manifest = std::fs::read_to_string(&manifest)
                .unwrap_or_else(|_| panic!("{} names {}, which is not here", preset.id, name));
            let manifest: serde_json::Value =
                serde_json::from_str(&manifest).expect("the plugin manifest is JSON");

            assert_eq!(
                manifest.get("name").and_then(serde_json::Value::as_str),
                Some(preset.package.as_str()),
                "{} would install under a different name than the list expects",
                preset.id
            );
            assert_eq!(
                preset.package, SIGNAL,
                "the gate in `signalling` names a different plugin than the list ships"
            );
            // And the id, because `adopt` installs this preset by naming it —
            // `install` looks its `ids` up in the list, so an id that drifted
            // off the package name would leave the first-launch install
            // failing with "no preset called dsh-desktop-signal" while
            // everything a user can reach by hand kept working.
            assert_eq!(
                preset.id, SIGNAL,
                "`adopt` asks for this preset by id and would not find it"
            );
        }

        assert_eq!(found, 1, "the signal plugin should be on the shipped list");
    }

    /// The gate every notification passes: the plugin is there, or it is not.
    ///
    /// Read out of the same set the panel calls "already installed", so the
    /// two cannot disagree — a panel saying the plugin is in while the menu
    /// says notifications are unavailable would be unanswerable.
    #[test]
    fn notifications_wait_on_the_plugin_that_feeds_them() {
        let holding = |manifest: &str| {
            let manifest: serde_json::Value = serde_json::from_str(manifest).expect("a manifest");
            super::installed_in(&manifest).contains(SIGNAL)
        };

        assert!(holding(
            r#"{"dependencies":{"dsh-desktop-signal":"link:/somewhere"}}"#
        ));
        // Listed as a profile layer but not as a dependency, which is what a
        // hand-edited profile looks like. Still installed.
        assert!(holding(
            r#"{"dsh":{"profile":{"bundles":["@deepseek-ai/dsh-base","dsh-desktop-signal"]}}}"#
        ));

        assert!(!holding(r#"{"dependencies":{"dshmarket":"^1.40.0"}}"#));
        assert!(!holding("{}"), "an empty profile signals nothing");
        assert!(!holding("null"), "no profile at all signals nothing");
    }

    /// A path handed to pnpm on Windows arrives quoted, and nowhere else does.
    ///
    /// The reasoning is in [`super::local_spec`]; this is the part that would
    /// silently stop being true. An unquoted path through Program Files does
    /// not fail the install — it installs two dependencies named after the halves
    /// of the path and exits 0.
    #[test]
    fn a_local_path_survives_the_shell_dsh_forwards_through() {
        let path = ["C:", "Program Files", "dsh-desktop", "resources", "plugin"]
            .join(std::path::MAIN_SEPARATOR_STR);
        let quoted = local_spec(Path::new(&path));

        if cfg!(windows) {
            assert!(quoted.starts_with('"') && quoted.ends_with('"'), "{quoted}");
            assert!(quoted.contains("Program Files"));
        } else {
            assert!(!quoted.contains('"'), "{quoted}");
        }
    }

    /// Everything that is not [`BUNDLED`] is passed through untouched, quotes
    /// included — a registry spec never goes near a filesystem path.
    #[test]
    fn an_ordinary_preset_spec_is_left_alone() {
        // `spec_for` needs an `AppHandle` for the bundled arm only, so the
        // pass-through is what is readable here: the prefix is the whole test.
        assert!(!"dshmarket".starts_with(BUNDLED));
        assert!("bundled:plugin".starts_with(BUNDLED));
        assert_eq!("bundled:plugin".strip_prefix(BUNDLED), Some("plugin"));
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
    /// What a user actually types into the panel's box.
    #[test]
    fn takes_the_package_names_a_plugin_is_published_under() {
        for spec in [
            "dsh-plugin-thing",
            "@dsh/plugin-thing",
            "@dsh/plugin-thing@1.2.3",
            "@dsh/plugin-thing@latest",
            "dsh-plugin-thing@^1.0.0",
            "dsh-plugin-thing@~2.1",
            "dsh-plugin-thing@1.x",
            "dsh-plugin-thing@*",
            "a.b_c-d",
            "github:owner/repo",
            "github:owner/repo#main",
            "github:owner/repo#feature/x",
            "github:owner/repo#0b1f2e3",
        ] {
            assert!(is_package_spec(spec), "{spec} is a package name");
        }
    }

    /// Every shape pnpm would also accept, each of which fetches code from
    /// somewhere no registry ever saw. See [`is_package_spec`].
    #[test]
    fn refuses_everything_that_is_not_a_package_name() {
        for spec in [
            "",
            "https://example.com/payload.tgz",
            "http://example.com/payload.tgz",
            "file:../payload",
            "file:///C:/payload",
            "git+ssh://git@example.com/o/r.git",
            "gitlab:owner/repo",
            "github:owner",
            "github:owner/repo#",
            "github:../../evil",
            "owner/repo",
            "../payload",
            "./payload",
            "/abs/payload",
            "C:\\payload",
            ".hidden",
            "-g",
            "--registry=http://example.com",
            "two words",
            "@dsh",
            "@dsh/",
            "@/name",
            "name@",
            "name@1.2.3/../..",
            "name@file:../payload",
        ] {
            assert!(!is_package_spec(spec), "{spec} is not a package name");
        }
    }

    /// The failure this module could not previously say anything about, in the
    /// words it arrived in — from a Windows machine whose profile still held
    /// the links of an install that had been killed partway through.
    #[test]
    fn a_path_that_will_not_go_away_is_recognised() {
        assert!(pnpm_stuck(
            "  ╰─▶ failed to clear non-directory dirent at \"C:\\Users\\me\\.dsh\\profiles\\web\\node_modules\\dsh-web-search-free\": 拒绝访问。 (os error 5)"
        ));
        assert!(pnpm_stuck("EPERM: operation not permitted, unlink"));
        assert!(pnpm_stuck("EBUSY: resource busy or locked, rmdir"));

        // The ordinary output of a run that is going fine, and the one failure
        // that is explicitly somebody else's: see the note on `pnpm_stuck`.
        assert!(!pnpm_stuck("Progress: resolved 8, reused 1, downloaded 0, added 0"));
        assert!(!pnpm_stuck(
            "Packages are hard linked from the content-addressable store to the virtual store."
        ));
        assert!(!pnpm_stuck("EACCES: permission denied, mkdir '/usr/lib/node_modules'"));
    }

    /// What the sweep is allowed to touch. The dangling link is the residue an
    /// interrupted install leaves; everything else in a `node_modules` is
    /// either working or none of its business, and deleting a working link
    /// would take a plugin out from under the user.
    #[test]
    fn clears_only_the_links_whose_target_is_gone() {
        let scratch = Scratch::new("sweep");
        let modules = scratch.dir("node_modules");

        // A package linked in the way pnpm links one, with its store entry
        // still where the link says it is.
        link_dir(&scratch.dir("store/live"), &modules.join("live"));

        // The same, with the store entry gone: what a killed pnpm leaves.
        let gone = scratch.dir("store/gone");
        link_dir(&gone, &modules.join("dangling"));
        std::fs::remove_dir_all(&gone).expect("the store entry goes");

        // And the scoped shape, which is one level further down.
        let scoped = scratch.dir("store/scoped");
        scratch.dir("node_modules/@scope");
        link_dir(&scoped, &modules.join("@scope").join("dangling"));
        std::fs::remove_dir_all(&scoped).expect("the store entry goes");

        // Real directories and files, which are not links at all.
        scratch.dir("node_modules/.pnpm");
        std::fs::write(modules.join(".modules.yaml"), b"").expect("the state file");

        let mut cleared = sweep(&modules);
        cleared.sort();
        assert_eq!(cleared, vec!["@scope/dangling", "dangling"]);

        assert!(std::fs::symlink_metadata(modules.join("dangling")).is_err());
        assert!(
            std::fs::symlink_metadata(modules.join("@scope").join("dangling")).is_err(),
            "the scoped link is still there"
        );
        assert!(modules.join("live").is_dir(), "a working link was deleted");
        assert!(modules.join(".pnpm").is_dir());
        assert!(modules.join(".modules.yaml").is_file());
    }

    /// Link a directory the way pnpm links a package into `node_modules`.
    ///
    /// A junction on Windows, and not by preference: `symlink_dir` there needs
    /// a privilege a test runner does not have, `mklink /J` needs none — and a
    /// junction is what pnpm actually creates, so it is also the case worth
    /// testing. It is the one `remove_file` cannot delete.
    fn link_dir(target: &Path, link: &Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).expect("the link");

        #[cfg(windows)]
        {
            let made = std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .expect("mklink runs");
            assert!(
                made.status.success(),
                "mklink /J {} {}: {}{}",
                link.display(),
                target.display(),
                String::from_utf8_lossy(&made.stdout),
                String::from_utf8_lossy(&made.stderr)
            );
        }
    }

    /// A directory of this test's own, gone again when it ends. Built on disk,
    /// because what [`sweep`] answers is a question about links, and a link
    /// only exists on one.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let unique = format!("dsh-plugins-{name}-{}", std::process::id());
            let dir = std::env::temp_dir().join(unique);
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("a scratch directory under the temp dir");
            Self(dir)
        }

        /// One component at a time, because these paths are handed to `mklink`
        /// below and cmd refuses a forward slash in one — which `join` would
        /// otherwise leave in the middle of the path on Windows.
        fn dir(&self, path: &str) -> PathBuf {
            let dir = path
                .split('/')
                .fold(self.0.clone(), |dir, part| dir.join(part));
            std::fs::create_dir_all(&dir).expect("the directory");
            dir
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
