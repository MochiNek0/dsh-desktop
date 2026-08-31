//! The chooser the app shows when a launch has nothing it can run.
//!
//! Two states reach it, and [`crate::dsh::needs_setup`] is where they are named:
//! no dsh on the machine at all, and a dsh sitting behind a Node older than
//! [`crate::dsh::NODE_MINIMUM`], which will not start whatever the marker says.
//! Before this module both were settled without asking — see the module docs in
//! [`crate::dsh`] for what that decided and what it got wrong.
//!
//! [`present`] enumerates every Node on the machine (the script's `list` mode
//! does the walking; one implementation per platform, the same way the rest of
//! the bootstrap is), hands the list to an injected panel, and blocks on the
//! user's answer the way [`crate::dialog`] blocks on one: a channel a worker
//! thread waits on, answered by a navigation the main thread delivers. The
//! answers are
//!
//!   - **use** a Node that already has dsh — the script's `switch` mode records
//!     the marker and nothing else: nothing is downloaded and nothing is
//!     installed;
//!   - **install dsh** into a Node the user picked — `install-dsh` mode;
//!   - **install a fresh Node** — `install-node` mode, for a machine with no
//!     Node the floor would accept;
//!   - **scan again**, offered only when the machine could not be looked at;
//!   - **quit**, which takes the app down, because there is nothing behind this
//!     panel to go back to.
//!
//! Only Nodes at or above the floor are offered for the first two, dsh or no
//! dsh: adopting a Node too old to run dsh is the state this panel exists to get
//! a machine out of. The scripts check the same floor again before they act.
//!
//! Whichever runs, the marker it writes is what [`crate::dsh::current`] falls
//! back to on the next step, so the boot carries straight on into starting that
//! dsh.
//!
//! None of them touches the user's PATH, and the marker they write does not
//! override it: if the Node chosen here is the one the user's version manager
//! currently points at, later launches find its dsh on PATH and never read the
//! marker at all. That is deliberate — see [`crate::dsh::search_path`] — and it
//! is why this panel comes back when the user switches to a Node with no dsh in
//! it, rather than quietly going on running a dsh their terminal cannot see.
//!
//! The panel is injected the way [`crate::panel`] and [`crate::dialog`] are, and
//! answers over the same cancelled-navigation channel everything else in this
//! window uses; see [`crate::controls`].

use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::json;
use tauri::AppHandle;

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

use crate::dsh::Report;

/// How long [`present`] waits for a choice before giving up. A backstop for a
/// panel that went away without being able to say so — a renderer crash, a
/// document navigated out from under it — rather than a policy, the way the same
/// constant in [`crate::dialog`] is. A user who walked away and came back is
/// never timed out by it.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// How long the machine gets to be looked at. The scripts ask a login shell for
/// its PATH — bounded at five seconds of its own — and then run every `node`
/// they find, so this is not a formality on a machine with a version manager or
/// two. Running over is reported as a failed scan, not as a machine with no
/// Node.
const SCAN_TIMEOUT: Duration = Duration::from_secs(60);

/// The Node version a fresh install brings down, shown on the button that asks
/// for one. Pinned here to match `install-deps.ps1` / `install-deps.sh`; it is
/// display only, and the script is the authority on what it actually installs.
const NODE_VERSION: &str = "24.19.0";

/// One Node the `list` mode found, in the shape the chooser needs.
///
/// Built by hand from [`serde_json::Value`] rather than derived, the way
/// [`crate::dsh::marker`] is: the rest of this crate pulls no `serde` derive
/// feature, and one more struct is not the reason to start.
struct NodeInfo {
    /// The `node` binary, as the script found it. Passed back to `switch` or
    /// `install-dsh` as `-NodeExe`.
    path: PathBuf,
    version: String,
    meets_minimum: bool,
    has_dsh: bool,
    dsh_version: Option<String>,
    /// Where the Node came from — `nvm`, `fnm`, `path`, `managed`… — for the
    /// panel's label.
    source: String,
}

/// What the user picked in the panel. Carried back across the channel by id, the
/// way a dialog's button id is; see [`crate::dialog`].
pub enum Choice {
    /// Use the dsh already in Node `i` (`switch`).
    Use(usize),
    /// Install dsh into Node `i` (`install-dsh`).
    InstallDsh(usize),
    /// Download a fresh Node and dsh (`install-node`).
    InstallNode,
    /// Look again. The button only appears when the scan failed, which is the
    /// one state a user can do something about without restarting the app.
    Rescan,
    /// Walk away.
    Quit,
}

/// The panel currently on screen, and the sender its answer travels back on.
/// One at a time, for the same reason [`crate::dialog::DIALOGS`] is: a second
/// panel would replace the first while its answer was still pending.
static PENDING: Mutex<Option<Pending>> = Mutex::new(None);

struct Pending {
    answer: mpsc::Sender<Choice>,
}

/// Run the script's `list` mode and parse what it prints into the chooser's
/// payload.
///
/// `None` means the machine was not looked at: no script to run, a run that
/// failed or timed out, output that would not parse. That is kept apart from
/// `Some(vec![])` — looked, found nothing — because the two want opposite
/// answers from the user. An empty list means installing a Node is the only way
/// forward; a failed scan means the one thing not to do is tell someone with
/// three Nodes that they have none and offer to download a fourth.
fn enumerate(app: &AppHandle) -> Option<Vec<NodeInfo>> {
    let Some(path) = crate::dsh::script(app) else {
        eprintln!("dsh-desktop: no bootstrap script to list Nodes with");
        return None;
    };

    let mut command = crate::dsh::interpreter(&path);
    command
        .args([OsStr::new("-Mode"), OsStr::new("list")])
        .stdin(Stdio::null());

    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    // `list` runs other Node binaries to read their versions; a crash in one of
    // those should not outlive this process.
    #[cfg(unix)]
    crate::server::group_leader(&mut command);

    let Some(output) = crate::dsh::printed(command, SCAN_TIMEOUT) else {
        eprintln!("dsh-desktop: listing the machine's Nodes failed or timed out");
        return None;
    };
    parse_nodes(&output)
}

/// What [`enumerate`] makes of one JSON line. Split from the run so the parse —
/// the part that can go wrong quietly — can be read on its own.
fn parse_nodes(json: &str) -> Option<Vec<NodeInfo>> {
    let Ok(array) = serde_json::from_str::<serde_json::Value>(json) else {
        return None;
    };
    let array = array.as_array()?;

    Some(array
        .iter()
        .filter_map(|node| {
            let path = PathBuf::from(node.get("path")?.as_str()?);
            let version = node.get("version")?.as_str()?.to_string();
            let meets_minimum = node
                .get("meetsMinimum")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let has_dsh = node
                .get("hasDsh")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let dsh_version = node
                .get("dshVersion")
                .and_then(|value| value.as_str())
                .map(String::from);
            let source = node
                .get("source")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();

            Some(NodeInfo {
                path,
                version,
                meets_minimum,
                has_dsh,
                dsh_version,
                source,
            })
        })
        .collect())
}

/// Settle a machine that has no dsh, by asking.
///
/// Returns `true` to go on and boot — which is every outcome except the user
/// quitting or the panel timing out. Runs on the boot's worker thread, like
/// [`crate::dsh::gate`], and blocks on the user the way [`crate::dialog::confirm`]
/// does.
pub fn present(app: &AppHandle, report: &Report) -> bool {
    // The window has to be visible: a chooser over a hidden window is one the
    // user cannot answer, and the autostart path reaches this with it hidden.
    reveal(app);

    let mut nodes = scan(app, report);
    let mut error = None::<String>;
    loop {
        // A fresh channel each time round: a failed action re-shows the panel,
        // and the answer to the second showing is not the answer to the first.
        let (send, receive) = mpsc::channel();
        if PENDING.lock().unwrap().replace(Pending { answer: send }).is_some() {
            // A previous setup never answered. Its sender is dropped here, which
            // fails its receive as "disconnected" — the safe half, same as a
            // replaced dialog.
        }

        crate::deliver_setup(app, &payload(nodes.as_deref(), error.as_deref()));

        let choice = receive.recv_timeout(ANSWER_TIMEOUT);
        // The panel is no longer the one on screen either way; take the slot so a
        // late click arrives at nothing rather than at a second showing.
        let _ = PENDING.lock().unwrap().take();

        match choice {
            Ok(Choice::Quit) => {
                quit(app);
                return false;
            }
            Ok(Choice::Rescan) => {
                crate::hide_setup(app);
                error = None;
                nodes = scan(app, report);
                continue;
            }
            Ok(choice) => {
                crate::hide_setup(app);
                match act(app, choice, nodes.as_deref().unwrap_or_default(), report) {
                    Outcome::Done => return true,
                    Outcome::Aborted => return false,
                    Outcome::Failed(message) => {
                        // Looked at again rather than reused: what just failed
                        // may be why — a Node uninstalled since the scan, an
                        // `install-node` that got as far as unpacking one — and
                        // the list a second choice is made from should be the
                        // machine as it is now.
                        error = Some(message);
                        nodes = scan(app, report);
                        reveal(app);
                        continue;
                    }
                }
            }
            // Nobody is here. Quitting rather than leaving a loading page that
            // will never move: there is no dsh to start, nothing else for the
            // boot thread to do, and reopening the app is what asks again.
            Err(RecvTimeoutError::Timeout) => {
                eprintln!(
                    "dsh-desktop: nothing answered the setup panel in {} minutes; quitting",
                    ANSWER_TIMEOUT.as_secs() / 60
                );
                quit(app);
                return false;
            }
            Err(RecvTimeoutError::Disconnected) => return false,
        }
    }
}

/// Look at the machine, with the loading page saying so. Slow enough to need the
/// status line: the scripts ask a login shell for its PATH and then run every
/// `node` they find.
fn scan(app: &AppHandle, report: &Report) -> Option<Vec<NodeInfo>> {
    report(t!("正在检测运行环境…", "Scanning for a runtime…"), -1.0);
    enumerate(app)
}

/// Take the panel down, and the app with it.
fn quit(app: &AppHandle) {
    crate::hide_setup(app);
    let app = app.clone();
    let _ = app.clone().run_on_main_thread(move || app.exit(0));
}

/// Run the script mode the user's choice asks for, reporting its progress onto
/// the loading page the way the rest of the bootstrap does.
fn act(app: &AppHandle, choice: Choice, nodes: &[NodeInfo], report: &Report) -> Outcome {
    let result = match choice {
        Choice::Use(index) => {
            let Some(node) = nodes.get(index) else {
                return Outcome::Failed(t!("找不到所选的 Node。", "The chosen Node is gone.").into());
            };
            report(t!("正在切换到所选的 Node…", "Switching to the chosen Node…"), -1.0);
            crate::dsh::run(
                app,
                &[
                    OsStr::new("-Mode"),
                    OsStr::new("switch"),
                    OsStr::new("-NodeExe"),
                    node.path.as_os_str(),
                ],
                report,
            )
        }
        Choice::InstallDsh(index) => {
            let Some(node) = nodes.get(index) else {
                return Outcome::Failed(t!("找不到所选的 Node。", "The chosen Node is gone.").into());
            };
            crate::dsh::run(
                app,
                &[
                    OsStr::new("-Mode"),
                    OsStr::new("install-dsh"),
                    OsStr::new("-NodeExe"),
                    node.path.as_os_str(),
                ],
                report,
            )
        }
        Choice::InstallNode => {
            crate::dsh::run(app, &[OsStr::new("-Mode"), OsStr::new("install-node")], report)
        }
        // Both handled in [`present`] before the action runs.
        Choice::Quit | Choice::Rescan => return Outcome::Aborted,
    };

    match result {
        Ok(true) => Outcome::Done,
        Ok(false) => Outcome::Aborted,
        Err(message) => Outcome::Failed(message),
    }
}

/// What [`act`] came back with.
enum Outcome {
    /// The script finished; the marker is written, and the boot can start dsh.
    Done,
    /// The app is quitting and took the running script down with it.
    Aborted,
    /// The script failed. The panel is re-shown with this so the user can choose
    /// again rather than be left on a dead loading page.
    Failed(String),
}

/// The JSON the panel takes: the nodes, whether the machine could be looked at
/// at all, the version a fresh install would bring, and the error from a
/// previous attempt when there was one.
fn payload(nodes: Option<&[NodeInfo]>, error: Option<&str>) -> String {
    let list: Vec<serde_json::Value> = nodes
        .unwrap_or_default()
        .iter()
        .map(|node| {
            json!({
                "path": node.path.display().to_string(),
                "version": node.version,
                "meetsMinimum": node.meets_minimum,
                "hasDsh": node.has_dsh,
                "dshVersion": node.dsh_version.clone().unwrap_or_default(),
                "source": node.source,
            })
        })
        .collect();

    json!({
        "nodes": list,
        "scanned": nodes.is_some(),
        "nodeVersion": NODE_VERSION,
        "error": error,
    })
    .to_string()
}

/// Make the window visible. A wrapper around the one in [`crate::main`] because
/// that one runs on the main thread and everything here runs on the boot's.
fn reveal(app: &AppHandle) {
    let app = app.clone();
    let _ = app
        .clone()
        .run_on_main_thread(move || crate::reveal(&app));
}

/// A click in the panel, delivered as a navigation. Called from
/// [`crate::controls::perform`] on the main thread; the send is instant, and the
/// boot thread waiting in [`present`] is what does the work.
pub fn answered(choice: Choice) {
    if let Some(pending) = PENDING.lock().unwrap().take() {
        let _ = pending.answer.send(choice);
    }
}

/// The script that draws the chooser, injected into every document the window
/// loads. Built on first use, like the plugin panel's and the dialog's.
pub fn script() -> String {
    let scheme = crate::controls::SCHEME;
    let font = crate::controls::FONT;

    let labels = json!({
        "title": t!("选择运行环境", "Choose a runtime"),
        "ledeNodes": t!(
            "这台机器上有不止一个 Node。选一个来运行 dsh —— 已经装好 dsh 的可以直接用，没装的也能在这里装上。",
            "This machine has more than one Node. Pick one to run dsh — use one that already has it, or install dsh into one here."
        ),
        "ledeNone": t!(
            "这台机器上没有检测到 Node。可以在这里装一个，dsh 会跟着一起装好。",
            "No Node was found on this machine. Install one here and dsh comes with it."
        ),
        "ledeFailed": t!(
            "没能列出这台机器上的 Node —— 检测脚本没有跑起来，或者跑得太久。如果你确定机器上装过 Node，先点“重新检测”；也可以直接装一个新的 Node。",
            "The machine's Nodes could not be listed — the scan would not run, or took too long. If you know this machine has a Node, scan again; installing a fresh one is the other way out."
        ),
        "use": t!("使用这个", "Use this one"),
        "installDsh": t!("在此安装 dsh", "Install dsh here"),
        "tooOld": t!("版本过低", "Too old"),
        "hasDsh": t!("已装 dsh", "Has dsh"),
        "canInstall": t!("可安装 dsh", "Can install dsh"),
        "installNode": t!("安装新的 Node", "Install a fresh Node"),
        "rescan": t!("重新检测", "Scan again"),
        "quit": t!("退出", "Quit"),
        "sources": {
            "managed": t!("应用管理", "App-managed"),
            "path": "PATH",
            "shell": "PATH",
            "nvm": "nvm",
            "fnm": "fnm",
            "volta": "Volta",
            "asdf": "asdf",
            "homebrew": "Homebrew",
            "scoop": "Scoop",
            "installer": "Node.js",
            "system": t!("系统", "System"),
            "snap": "Snap",
            "": t!("Node", "Node")
        }
    })
    .to_string();

    format!(
        r#"(function () {{
  // The top document only; see `controls`.
  if (window.top !== window.self) return;
  if (window.__dshSetupPanel) return;
  window.__dshSetupPanel = true;

  var TEXT = {labels};

  var root = null, card, lede, list, errBox, errText;
  var installNode, rescan, quit;
  var sent = false;

  // One answer per showing. The panel stays up until the app takes it down, and
  // a second click in that gap would be read against a list the next payload is
  // about to replace — `?i=2` answering a question about a different machine.
  function signal(verb) {{
    if (sent) return;
    sent = true;
    if (root) root.classList.add('dsh-su-busy');
    window.location.href = '{scheme}://' + verb;
  }}

  function make(tag, className, parent) {{
    var node = document.createElement(tag);
    if (className) node.className = className;
    if (parent) parent.appendChild(node);
    return node;
  }}

  function button(parent, text, onclick, primary) {{
    var node = make('button', primary ? 'dsh-su-primary' : '', parent);
    node.type = 'button';
    node.textContent = text;
    node.addEventListener('click', onclick);
    return node;
  }}

  function sourceLabel(source) {{
    return TEXT.sources[source] || TEXT.sources[''] || source;
  }}

  function chip(kind, text) {{
    var tag = make('span', 'dsh-su-chip dsh-su-' + kind);
    tag.textContent = text;
    return tag;
  }}

  // One Node, as a row the user can act on — or, if it is below the floor, only
  // read.
  //
  // The floor is asked first, before whether dsh is there. A dsh already
  // installed into a Node too old to run it is not a way out of anything: it is
  // the state this panel exists to get a machine out of. So both chips can show
  // on one row — "has dsh 0.1.8" and "too old" — which is the honest description
  // of it, and the button is the disabled one.
  function row(node, index) {{
    var line = make('div', 'dsh-su-row');

    var head = make('div', 'dsh-su-head', line);
    var title = make('span', 'dsh-su-ver', head);
    title.textContent = 'Node ' + node.version;
    if (node.hasDsh) {{
      head.appendChild(chip('have',
        node.dshVersion ? TEXT.hasDsh + ' ' + node.dshVersion : TEXT.hasDsh));
    }}
    if (!node.meetsMinimum) {{
      head.appendChild(chip('bad', TEXT.tooOld));
    }} else if (!node.hasDsh) {{
      head.appendChild(chip('ok', TEXT.canInstall));
    }}
    make('span', 'dsh-su-src', head).textContent = sourceLabel(node.source);

    make('div', 'dsh-su-path', line).textContent = node.path;

    var actions = make('div', 'dsh-su-actions', line);
    if (!node.meetsMinimum) {{
      var off = button(actions, TEXT.tooOld, function () {{}});
      off.disabled = true;
    }} else if (node.hasDsh) {{
      button(actions, TEXT.use, function () {{
        signal('setup-use?i=' + index);
      }}, true);
    }} else {{
      button(actions, TEXT.installDsh, function () {{
        signal('setup-install-dsh?i=' + index);
      }});
    }}

    return line;
  }}

  // dsh's theme is the page's, read the way the titlebar and the panels read it.
  function paint(node) {{
    var media = window.matchMedia('(prefers-color-scheme:dark)');

    function dark() {{
      if (document.body.hasAttribute('data-ds-dark-theme')) return true;
      var declared = getComputedStyle(document.documentElement).colorScheme || '';
      var light = declared.indexOf('light') !== -1;
      var night = declared.indexOf('dark') !== -1;
      return night !== light ? night : media.matches;
    }}

    function repaint() {{
      node.classList.toggle('dsh-su-dark', dark());
    }}

    repaint();
    var watch = new MutationObserver(repaint);
    watch.observe(document.documentElement, {{
      attributes: true, attributeFilter: ['style', 'class', 'data-theme']
    }});
    watch.observe(document.body, {{
      attributes: true, attributeFilter: ['style', 'class', 'data-ds-dark-theme']
    }});
    media.addEventListener('change', repaint);
  }}

  function build() {{
    var style = document.createElement('style');
    style.textContent =
      '.dsh-su{{position:fixed;inset:0;z-index:2147483643;display:none;' +
      'align-items:center;justify-content:center;box-sizing:border-box;' +
      'padding:calc(var(--dsh-titlebar-height,36px) + 12px) 16px 20px;' +
      'background:rgba(18,18,22,.34);-webkit-backdrop-filter:blur(3px);' +
      'backdrop-filter:blur(3px);font-size:14px;line-height:1.6;' +
      'user-select:none;-webkit-user-select:none;' +
      '--su-bg:#fff;--su-fg:#1a1a1a;--su-muted:#6b7280;--su-line:#e5e7eb;' +
      '--su-accent:#4d6bfe;--su-soft:#f7f8fa;--su-ok:#12805c;--su-bad:#b42318}}' +
      '.dsh-su.dsh-su-dark{{background:rgba(0,0,0,.5);' +
      '--su-bg:#17171d;--su-fg:#ececf1;--su-muted:#9aa0ac;--su-line:#2b2b34;' +
      '--su-soft:rgba(255,255,255,.04);--su-ok:#3ccb9a;--su-bad:#f97066}}' +
      '.dsh-su.dsh-su-shown{{display:flex}}' +
      '.dsh-su,.dsh-su *{{box-sizing:border-box;font-family:{font}}}' +
      '.dsh-su-card{{display:flex;flex-direction:column;min-height:0;' +
      'max-height:100%;width:min(620px,100%);padding:22px 24px;' +
      'border-radius:14px;background:var(--su-bg);color:var(--su-fg);' +
      'box-shadow:0 24px 64px rgba(0,0,0,.32),0 0 0 .5px var(--su-line)}}' +
      '.dsh-su-card h1{{font-size:17px;font-weight:600;line-height:1.4;margin:0 0 6px}}' +
      '.dsh-su-lede{{margin:0 0 16px;color:var(--su-muted);font-size:13px}}' +
      '.dsh-su-list{{flex:1 1 auto;min-height:0;overflow:auto;margin:0 -4px;padding:0 4px}}' +
      '.dsh-su-row{{border:1px solid var(--su-line);border-radius:12px;' +
      'padding:13px 15px;margin-bottom:9px;background:var(--su-bg)}}' +
      '.dsh-su-head{{display:flex;align-items:center;gap:8px;flex-wrap:wrap}}' +
      '.dsh-su-ver{{font-weight:600}}' +
      '.dsh-su-src{{margin-left:auto;font-size:11px;color:var(--su-muted);' +
      'text-transform:uppercase;letter-spacing:.04em}}' +
      '.dsh-su-path{{margin-top:5px;font:12px ui-monospace,Consolas,monospace;' +
      'color:var(--su-muted);word-break:break-all}}' +
      '.dsh-su-actions{{display:flex;gap:8px;margin-top:11px}}' +
      '.dsh-su-chip{{font-size:11px;font-weight:500;line-height:1.5;padding:0 7px;' +
      'border-radius:999px;border:1px solid currentColor}}' +
      '.dsh-su-chip.dsh-su-have{{color:var(--su-ok)}}' +
      '.dsh-su-chip.dsh-su-ok{{color:var(--su-accent)}}' +
      '.dsh-su-chip.dsh-su-bad{{color:var(--su-bad)}}' +
      '.dsh-su-err{{display:none;margin-bottom:14px;padding:11px 13px;' +
      'border:1px solid var(--su-bad);border-radius:10px;' +
      'background:rgba(178,35,24,.06);font-size:13px;color:var(--su-bad);' +
      'white-space:pre-wrap;user-select:text;-webkit-user-select:text}}' +
      '.dsh-su-err.dsh-su-err-shown{{display:block}}' +
      '.dsh-su-foot{{display:flex;flex-wrap:wrap;align-items:center;' +
      'justify-content:flex-end;gap:10px;margin-top:14px}}' +
      '.dsh-su-foot .dsh-su-spacer{{flex:1 1 auto}}' +
      '.dsh-su button{{-webkit-appearance:none;appearance:none;' +
      'display:inline-flex;align-items:center;justify-content:center;' +
      'height:32px;padding:0 15px;border-radius:8px;border:1px solid var(--su-line);' +
      'cursor:pointer;font:13px/1 inherit;color:var(--su-fg);white-space:nowrap}}' +
      '.dsh-su button:hover{{background:var(--su-soft)}}' +
      '.dsh-su button.dsh-su-primary{{background:var(--su-accent);' +
      'border-color:var(--su-accent);color:#fff}}' +
      '.dsh-su button.dsh-su-primary:hover{{filter:brightness(1.08)}}' +
      '.dsh-su button[disabled]{{opacity:.45;cursor:default;pointer-events:none}}' +
      '.dsh-su button[hidden]{{display:none}}' +
      // Answered, and waiting for the app to take the panel away.
      '.dsh-su.dsh-su-busy .dsh-su-card{{opacity:.6;pointer-events:none}}';
    document.head.appendChild(style);

    root = make('div', 'dsh-su');
    card = make('div', 'dsh-su-card', root);
    make('h1', '', card).textContent = TEXT.title;
    lede = make('p', 'dsh-su-lede', card);
    errBox = make('div', 'dsh-su-err', card);
    errText = make('div', '', errBox);
    list = make('div', 'dsh-su-list', card);

    var foot = make('div', 'dsh-su-foot', card);
    quit = button(foot, TEXT.quit, function () {{ signal('setup-quit'); }});
    make('span', 'dsh-su-spacer', foot);
    rescan = button(foot, TEXT.rescan, function () {{ signal('setup-rescan'); }});
    installNode = button(foot, TEXT.installNode, function () {{
      signal('setup-install-node');
    }}, true);

    // No Escape-to-quit. There is nothing here to cancel back to — the app is
    // waiting on this answer to decide whether it starts at all — and the key
    // people press to dismiss things should not be the one that closes the app.

    paint(root);
    document.body.appendChild(root);
  }}

  function ready(then) {{
    if (document.body) then();
    else document.addEventListener('DOMContentLoaded', then, {{ once: true }});
  }}

  window.__dshSetup = function (payload) {{
    var data;
    try {{
      data = JSON.parse(payload);
    }} catch (error) {{
      return;
    }}

    ready(function () {{
      if (!root) build();

      sent = false;
      root.classList.remove('dsh-su-busy');

      var nodes = data.nodes || [];
      // `scanned: false` is a machine that could not be looked at, which is not
      // the same as a machine with no Node — see `enumerate`.
      var scanned = data.scanned !== false;
      lede.textContent = !scanned
        ? TEXT.ledeFailed
        : (nodes.length ? TEXT.ledeNodes : TEXT.ledeNone);
      rescan.hidden = scanned;

      var shown = data.error && typeof data.error === 'string';
      errBox.classList.toggle('dsh-su-err-shown', shown);
      errText.textContent = shown ? data.error : '';

      list.textContent = '';
      nodes.forEach(function (node, index) {{
        list.appendChild(row(node, index));
      }});

      // The label carries the version a fresh install would bring, so the user
      // knows what they are agreeing to download.
      installNode.textContent = TEXT.installNode +
        (data.nodeVersion ? ' (Node ' + data.nodeVersion + ')' : '');

      root.classList.add('dsh-su-shown');
    }});
  }};

  window.__dshSetupHide = function () {{
    if (root) root.classList.remove('dsh-su-shown');
  }};
}})();"#
    )
}

#[cfg(test)]
mod tests {
    use super::parse_nodes;

    /// The shape the scripts print, parsed into the chooser's payload.
    #[test]
    fn parses_what_the_scripts_print() {
        let json = r#"[
          {"path":"/usr/local/bin/node","version":"24.9.0","meetsMinimum":true,"prefix":"/usr/local","hasDsh":true,"dshVersion":"0.1.8","source":"homebrew"},
          {"path":"/usr/bin/node","version":"18.4.0","meetsMinimum":false,"prefix":"/usr","hasDsh":false,"dshVersion":null,"source":"system"}
        ]"#;

        let nodes = parse_nodes(json).expect("a scan, not a failure");

        assert_eq!(nodes.len(), 2);
        assert!(nodes[0].has_dsh);
        assert_eq!(nodes[0].dsh_version.as_deref(), Some("0.1.8"));
        assert!(nodes[0].meets_minimum);
        assert!(!nodes[1].has_dsh);
        assert!(nodes[1].dsh_version.is_none());
        assert!(!nodes[1].meets_minimum);
    }

    /// A null `dshVersion` is the common case — most Nodes have no dsh — and a
    /// null `prefix` happens for a Node whose npm would not answer.
    #[test]
    fn handles_null_fields() {
        let json = r#"[{"path":"/n/node","version":"22.0.0","meetsMinimum":true,"prefix":null,"hasDsh":false,"dshVersion":null,"source":"nvm"}]"#;

        let nodes = parse_nodes(json).expect("a scan, not a failure");

        assert_eq!(nodes.len(), 1);
        assert!(nodes[0].dsh_version.is_none());
    }

    /// Anything that is not the array the scripts print is a scan that did not
    /// happen, not a machine with no Node — the panel says something different
    /// for each, and offering to download a Node is only right for one of them.
    #[test]
    fn anything_that_is_not_an_array_is_a_failed_scan() {
        assert!(parse_nodes("").is_none());
        assert!(parse_nodes("not json").is_none());
        assert!(parse_nodes("{}").is_none());
    }

    /// And the empty array the scripts print for a machine with no Node is a
    /// scan, not a failure.
    #[test]
    fn an_empty_array_is_a_scan_that_found_nothing() {
        let nodes = parse_nodes("[]").expect("a scan, not a failure");
        assert!(nodes.is_empty());
    }

    /// Both states reach the panel, and it has to be able to tell them apart.
    #[test]
    fn the_payload_says_whether_the_machine_was_looked_at() {
        assert!(super::payload(Some(&[]), None).contains("\"scanned\":true"));
        assert!(super::payload(None, None).contains("\"scanned\":false"));
    }

    /// The version on the install-a-Node button is this module's copy of a
    /// number the scripts own. Wrong, it offers a download of something else.
    #[test]
    fn the_offered_node_version_matches_the_scripts() {
        for (path, literal) in [
            (
                concat!(env!("CARGO_MANIFEST_DIR"), "/../scripts/install-deps.ps1"),
                format!("$NodeVersion = '{}'", super::NODE_VERSION),
            ),
            (
                concat!(env!("CARGO_MANIFEST_DIR"), "/../scripts/install-deps.sh"),
                format!("NODE_VERSION='{}'", super::NODE_VERSION),
            ),
        ] {
            let script = std::fs::read(path).expect("a readable bootstrap script");
            let script = String::from_utf8_lossy(&script);
            assert!(script.contains(&literal), "{path} does not say {literal}");
        }
    }

    /// Every button in the panel navigates to a verb, and `controls` is what
    /// turns one back into an action. The two are written far apart and nothing
    /// else checks they agree: a renamed verb is a button that silently does
    /// nothing, on the one screen with no other way forward.
    #[test]
    fn every_verb_the_panel_signals_is_one_the_app_answers() {
        let script = super::script();

        let mut checked = 0;
        for piece in script.split("signal('").skip(1) {
            let verb = piece.split('\'').next().expect("a closed string literal");
            // `setup-use?i=` and friends have the index appended at click time.
            let target = format!("{}://{verb}", crate::controls::SCHEME);
            let target = if target.ends_with("?i=") {
                format!("{target}0")
            } else {
                target
            };

            let url = tauri::Url::parse(&target).expect("a parseable control url");
            assert!(
                crate::controls::action(&url).is_some(),
                "the panel signals {verb}, which `controls::action` does not answer"
            );
            checked += 1;
        }

        assert_eq!(checked, 5, "expected five verbs, found {checked}");
    }
}
