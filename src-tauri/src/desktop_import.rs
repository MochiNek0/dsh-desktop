//! Bringing the official DeepSeek Harness Desktop's settings and plugins into
//! the profile this app runs.
//!
//! Both apps share one `$DSH_HOME`, and with it the sessions, credentials and
//! `.env`. What they do not share is the profile: Desktop boots
//! `profiles/desktop`, which dsh's CLI refuses to boot for anyone else, and this
//! app boots `profiles/web`. A profile holds the plugins (`package.json`) and
//! nearly every setting — custom model providers, the default model, theme,
//! language, each plugin's config (`cordis.patch.yml`). So someone coming over
//! from Desktop finds their sessions here and their models missing.
//!
//! Offered once, the first start that finds a Desktop profile with something
//! the web one lacks, and again whenever the tray item asks for it. Nothing
//! is synced afterwards: the two profiles are separate by dsh's design, and
//! keeping them in step both ways would mean deciding, on every change, which
//! app's edit wins.
//!
//! The merge only ever adds — see `desktop_import.mjs`, which does it with
//! dsh's own YAML library so comments and `!!js` expressions survive. A
//! setting the web profile already has keeps its value. Before the write the
//! patch file is copied aside, and after it dsh is asked to compose the
//! profile; a refusal puts the copy back.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use serde_json::Value;
use tauri::AppHandle;

use crate::dialog::{Ask, Choice};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const SCRIPT: &str = include_str!("desktop_import.mjs");

/// The offer has been answered, either way. A marker in the app's own
/// directory, like `plugins`' ones.
const OFFERED: &str = "desktop-import-offered";

/// Set by the tray item, read by the next start. See [`request`].
static REQUESTED: AtomicBool = AtomicBool::new(false);

/// How many lines of each kind the question lists before it says "and N more".
const LISTED: usize = 12;

fn desktop_dir() -> PathBuf {
    crate::plugins::dsh_home().join("profiles").join("desktop")
}

/// Whether this machine has a Desktop profile to import from.
pub fn available() -> bool {
    let dir = desktop_dir();
    dir.join("cordis.patch.yml").is_file() || dir.join("package.json").is_file()
}

/// Ask again on the next start, and say so even when there is nothing to bring.
/// The caller restarts dsh, which is where the question is asked.
pub fn request(app: &AppHandle) {
    REQUESTED.store(true, Ordering::SeqCst);
    if let Some(path) = marker(app) {
        let _ = std::fs::remove_file(path);
    }
}

/// What the script found.
#[derive(Default)]
struct Plan {
    added: Vec<String>,
    /// `(where, kept, not taken)`.
    conflicts: Vec<(String, String, String)>,
    skipped: Vec<String>,
    plugins: Vec<String>,
    unreachable: Vec<String>,
    missing_keys: Vec<String>,
}

impl Plan {
    fn parse(printed: &str) -> Result<Self, String> {
        let value: Value = serde_json::from_str(printed).map_err(|error| error.to_string())?;
        let strings = |key: &str| -> Vec<String> {
            value[key]
                .as_array()
                .map(|items| items.iter().filter_map(|item| item.as_str().map(String::from)).collect())
                .unwrap_or_default()
        };
        let paths = |items: &Value| -> Vec<String> {
            items
                .as_array()
                .map(|items| items.iter().map(shown_path).collect())
                .unwrap_or_default()
        };
        Ok(Self {
            added: paths(&value["added"]),
            conflicts: value["conflicts"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .map(|item| {
                            (
                                shown_path(&item["path"]),
                                item["kept"].as_str().unwrap_or_default().to_string(),
                                item["ignored"].as_str().unwrap_or_default().to_string(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
            skipped: strings("skipped"),
            plugins: strings("plugins"),
            unreachable: strings("unreachable"),
            missing_keys: strings("missingKeys"),
        })
    }

    fn nothing_to_do(&self) -> bool {
        self.added.is_empty() && self.plugins.is_empty()
    }
}

/// `["llm-pi-ai", "config", "providers", "sensenova"]` as
/// `llm-pi-ai › providers › sensenova`: every patch row's settings sit under
/// `config`, so saying it adds nothing.
fn shown_path(path: &Value) -> String {
    let parts: Vec<&str> = path
        .as_array()
        .map(|parts| parts.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    parts
        .iter()
        .enumerate()
        .filter(|(at, part)| !(*at == 1 && **part == "config"))
        .map(|(_, part)| *part)
        .collect::<Vec<_>>()
        .join(" › ")
}

/// Offer the import, and run it if accepted. Called by every start with dsh
/// down, so plugins can go in and the patch can be written with nothing
/// reading it. Blocks on the question; the caller is on a worker thread.
///
/// `visible` is whether anyone is looking: a login-item start in the tray
/// does not ask, and does not spend the offer either.
pub fn offer(app: &AppHandle, visible: bool, report: &crate::dsh::Report) {
    let asked = REQUESTED.swap(false, Ordering::SeqCst);
    if !(visible || asked) || !available() || (!asked && remembered(app)) {
        return;
    }
    let Some(dsh) = crate::dsh::current(app) else {
        return;
    };

    report(
        t!("正在检查官方桌面端的设置…", "Looking at DeepSeek Harness Desktop's settings…"),
        -1.0,
    );
    let plan = match run(app, &dsh, "plan") {
        Ok(plan) => plan,
        Err(why) => {
            eprintln!("dsh-desktop: could not read Desktop's profile: {why}");
            if asked {
                tell(app, t!("无法读取官方桌面端的设置", "Could not read Desktop's settings"), &why);
            }
            return;
        }
    };

    if plan.nothing_to_do() {
        // Not remembered: Desktop may yet grow something worth asking about.
        if asked {
            tell(
                app,
                t!("没有需要导入的内容", "Nothing to import"),
                t!(
                    "官方桌面端的设置和插件，当前配置里都已经有了。",
                    "Every setting and plugin from DeepSeek Harness Desktop is already here."
                ),
            );
        }
        return;
    }

    let answer = crate::dialog::choose(
        app,
        Ask {
            title: t!("导入官方桌面端的设置？", "Import DeepSeek Harness Desktop's settings?").to_string(),
            body: question(&plan),
            choices: vec![
                Choice::new("no", t!("不导入", "Don't import")),
                Choice::primary("import", t!("导入", "Import")),
            ],
            answered: Box::new(|_, _| {}),
        },
        "import",
    );
    // No answer is a question nobody saw: ask again next time.
    let Some(accepted) = answer else { return };
    remember(app);
    if accepted {
        import(app, &dsh, report);
    }
}

fn question(plan: &Plan) -> String {
    let mut body = t!(
        "在 {} 找到了官方桌面端的配置。本应用用的是另一份（web），所以那边的模型、插件和插件设置在这里看不到。\n\
         导入只会添加：当前已有的设置一项都不会被改。导入前会先备份。",
        "Found DeepSeek Harness Desktop's profile in {}. This app runs a different one (web), so \
         the models, plugins and plugin settings set up there are not visible here.\n\
         Importing only adds: nothing already set here changes. The current settings are backed up first.",
        desktop_dir().display()
    );

    section(&mut body, t!("会补进来的设置", "Settings to add"), &plan.added);
    section(&mut body, t!("会安装的插件", "Plugins to install"), &plan.plugins);
    let conflicts: Vec<String> = plan
        .conflicts
        .iter()
        .map(|(path, kept, ignored)| {
            t!("{}：保留 {}，不用 {}", "{}: keeping {}, not {}", path, kept, ignored)
        })
        .collect();
    section(
        &mut body,
        t!("两边不一样，保留当前值", "Different on each side; keeping this app's value"),
        &conflicts,
    );
    let mut left: Vec<String> = plan
        .skipped
        .iter()
        .map(|id| t!("{}（官方桌面端自己的组件）", "{} (part of Desktop itself)", id))
        .collect();
    left.extend(plan.unreachable.iter().map(|name| {
        t!(
            "插件 {}（找不到它的本地文件夹）",
            "plugin {} (its local folder is gone)",
            name
        )
    }));
    section(&mut body, t!("不会导入", "Not imported"), &left);
    body
}

fn section(body: &mut String, title: &str, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    body.push_str(&format!("\n\n{title}："));
    for line in lines.iter().take(LISTED) {
        body.push_str(&format!("\n  · {line}"));
    }
    if lines.len() > LISTED {
        body.push_str(&t!("\n  · 还有 {} 项", "\n  · and {} more", lines.len() - LISTED));
    }
}

fn import(app: &AppHandle, dsh: &crate::dsh::Install, report: &crate::dsh::Report) {
    report(
        t!("正在导入官方桌面端的设置…", "Importing DeepSeek Harness Desktop's settings…"),
        -1.0,
    );

    let patch = crate::plugins::profile_dir(app).join("cordis.patch.yml");
    let backup = match back_up(&patch) {
        Ok(backup) => backup,
        Err(why) => {
            tell(app, t!("导入没有进行", "Nothing was imported"), &why);
            return;
        }
    };

    let written = run(app, dsh, "apply").and_then(|plan| {
        composes(app, dsh)?;
        Ok(plan)
    });
    let plan = match written {
        Ok(plan) => plan,
        Err(why) => {
            let restored = restore(&patch, backup.as_deref());
            tell(
                app,
                t!("导入失败，已恢复原来的设置", "The import failed; the settings were put back"),
                &match restored {
                    Ok(()) => why,
                    Err(also) => format!("{why}\n\n{also}"),
                },
            );
            return;
        }
    };

    // After the settings, which are the part that cannot fail on the network.
    // A plugin that does not go in leaves its settings waiting for it: dsh
    // ignores config for an entry it does not have.
    let mut installed = Ok(());
    if !plan.plugins.is_empty() {
        report(
            t!("正在安装官方桌面端的插件…", "Installing DeepSeek Harness Desktop's plugins…"),
            -1.0,
        );
        let log = |line: &str| eprintln!("dsh-desktop: {line}");
        installed = crate::plugins::add(app, &plan.plugins, &log);
    }

    let mut body = t!(
        "补进了 {} 项设置。",
        "Added {} settings.",
        plan.added.len()
    );
    if let Some(backup) = &backup {
        body.push_str(&t!(
            "原来的设置备份在：\n{}",
            " The previous settings are backed up at:\n{}",
            backup.display()
        ));
    }
    match installed {
        Ok(()) if !plan.plugins.is_empty() => body.push_str(&t!(
            "\n\n装好了 {} 个插件。",
            "\n\nInstalled {} plugins.",
            plan.plugins.len()
        )),
        Ok(()) => {}
        Err(why) => body.push_str(&t!(
            "\n\n插件没有装上：{}\n可以稍后在插件面板里再装一次。",
            "\n\nThe plugins did not install: {}\nThey can be installed again from the plugin panel.",
            why
        )),
    }
    if !plan.missing_keys.is_empty() {
        body.push_str(&t!(
            "\n\n这些模型供应商还缺 API key，请到 dsh 设置里的模型页填写：{}",
            "\n\nThese model providers have no API key here yet; fill them in on dsh's Models settings page: {}",
            plan.missing_keys.join("、")
        ));
    }
    tell(app, t!("导入完成", "Import finished"), &body);
}

/// Run the merge script with `mode`, on the Node dsh runs on.
fn run(app: &AppHandle, dsh: &crate::dsh::Install, mode: &str) -> Result<Plan, String> {
    let node = crate::dsh::active_node(app)
        .ok_or_else(|| t!("找不到 Node", "Node could not be found").to_string())?;

    let mut command = Command::new(node);
    command
        .args(["--input-type=module", "-", mode])
        .arg(dsh.package_dir())
        .arg(crate::plugins::dsh_home())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::dsh::apply_path(app, &mut command);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let mut child = command.spawn().map_err(|error| error.to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(SCRIPT.as_bytes()).map_err(|error| error.to_string())?;
    }
    let output = child.wait_with_output().map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Plan::parse(&String::from_utf8_lossy(&output.stdout))
}

/// Whether dsh can still compose the web profile. The check the import is
/// undone on.
fn composes(app: &AppHandle, dsh: &crate::dsh::Install) -> Result<(), String> {
    let mut command = Command::new(&dsh.bin);
    command
        .args(["--profile", crate::plugins::PROFILE, "--dump-config"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    crate::dsh::apply_path(app, &mut command);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let output = command.output().map_err(|error| error.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    Err(t!(
        "导入后 dsh 读不了新的设置：\n{}",
        "dsh could not read the settings after the import:\n{}",
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

/// Copy the patch aside, as `wildcard` does. `None` when there was no file.
fn back_up(patch: &Path) -> Result<Option<PathBuf>, String> {
    if !patch.is_file() {
        return Ok(None);
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let backup = patch.with_extension(format!("yml.bak-{stamp}"));
    std::fs::copy(patch, &backup).map_err(|error| format!("{}: {error}", backup.display()))?;
    Ok(Some(backup))
}

fn restore(patch: &Path, backup: Option<&Path>) -> Result<(), String> {
    let restored = match backup {
        Some(backup) => std::fs::copy(backup, patch).map(|_| ()),
        // There was no file before; the import's is the one to take away.
        None => match std::fs::remove_file(patch) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        },
    };
    restored.map_err(|error| {
        t!(
            "恢复也失败了（{}）。原来的设置在：{}",
            "Putting them back failed too ({}). The previous settings are at: {}",
            error,
            backup.map_or_else(|| patch.display().to_string(), |backup| backup.display().to_string())
        )
    })
}

/// A note the start waits on: the loading page is about to give way to dsh's,
/// and a note that did not block would go with it unread.
fn tell(app: &AppHandle, title: &str, body: &str) {
    let _ = crate::dialog::choose(
        app,
        Ask {
            title: title.to_string(),
            body: body.to_string(),
            choices: vec![Choice::primary("ok", t!("继续", "Continue"))],
            answered: Box::new(|_, _| {}),
        },
        "ok",
    );
}

fn marker(app: &AppHandle) -> Option<PathBuf> {
    Some(crate::dsh::app_dir(app)?.join(OFFERED))
}

fn remembered(app: &AppHandle) -> bool {
    marker(app).is_some_and(|path| path.exists())
}

fn remember(app: &AppHandle) {
    let Some(path) = marker(app) else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(error) = std::fs::write(&path, b"") {
        eprintln!("dsh-desktop: could not record {OFFERED}: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_scripts_answer() {
        let plan = Plan::parse(
            r#"{"added":[["llm-pi-ai","config","providers","sensenova"],["web-search-free"]],
                "conflicts":[{"path":["ui-theme","config","theme"],"kept":"dark","ignored":"light"}],
                "skipped":["electron-shell"],"plugins":["dsh-agy@^0.4.6"],
                "unreachable":["gone"],"missingKeys":["SENSENOVA_API_KEY"]}"#,
        )
        .unwrap();
        assert_eq!(plan.added, ["llm-pi-ai › providers › sensenova", "web-search-free"]);
        assert_eq!(
            plan.conflicts,
            [("ui-theme › theme".to_string(), "dark".to_string(), "light".to_string())]
        );
        assert_eq!(plan.skipped, ["electron-shell"]);
        assert_eq!(plan.plugins, ["dsh-agy@^0.4.6"]);
        assert_eq!(plan.unreachable, ["gone"]);
        assert_eq!(plan.missing_keys, ["SENSENOVA_API_KEY"]);
        assert!(!plan.nothing_to_do());
    }

    #[test]
    fn conflicts_alone_are_nothing_to_do() {
        let plan = Plan::parse(
            r#"{"added":[],"conflicts":[{"path":["a"],"kept":"1","ignored":"2"}],
                "skipped":[],"plugins":[],"unreachable":[],"missingKeys":[]}"#,
        )
        .unwrap();
        assert!(plan.nothing_to_do());
    }

    #[test]
    fn a_long_list_is_cut_short() {
        let mut body = String::new();
        let lines: Vec<String> = (0..LISTED + 3).map(|at| at.to_string()).collect();
        section(&mut body, "x", &lines);
        assert_eq!(body.matches("\n  · ").count(), LISTED + 1);
        assert!(body.contains(" 3 "));
    }
}
