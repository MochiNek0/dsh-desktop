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

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
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

    match run(command, log)? {
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
        code => Err(t!(
            "dsh plugin 退出码 {}。上面是它的完整输出。",
            "dsh plugin exited with code {}. Its full output is above.",
            code
        )),
    }
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

    match run(npm, log)? {
        // npm exiting 0 is not the same as pnpm being runnable: it will report
        // success having written a shim whose target the next step cannot
        // execute, or into a prefix nothing on the search path looks at. The
        // claim this function makes is that pnpm runs, so it is checked.
        0 => match crate::dsh::tool(app, PNPM) {
            Tool::Ready => {
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

/// Run a child to completion, putting every line either stream produces through
/// `log`, and answer with its exit code.
///
/// Both streams, interleaved as they arrive: pnpm writes its progress to one and
/// its warnings — including the `allowBuilds` instruction this module
/// deliberately does not act on — to the other, and a user reading a failure
/// needs them in the order they happened.
fn run(mut command: Command, log: &Log) -> Result<i32, String> {
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

    for line in rx {
        eprintln!("[plugin] {line}");
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

    Ok(status?.code().unwrap_or(-1))
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

    log(&t!("正在卸载：{}", "Removing: {}", names.join(" ")));

    let mut command = Command::new(&dsh.bin);
    command.args(["plugin", "--profile", PROFILE, "remove"]);
    command.args(&names);
    crate::dsh::apply_path(app, &mut command);

    match run(command, log)? {
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
        code => Err(t!(
            "dsh plugin 退出码 {}。上面是它的完整输出。",
            "dsh plugin exited with code {}. Its full output is above.",
            code
        )),
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
    use super::{dependencies_of, parse, requested, wanted_gone, PRESETS};
    use tauri::Url;

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
