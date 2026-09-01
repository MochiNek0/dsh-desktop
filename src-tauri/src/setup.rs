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

/// Why the panel is up, which is the whole of what differs between its two
/// callers.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// The boot cannot go on without an answer. Nothing is running behind the
    /// panel, so the way out of it is quitting the app, and an answer is not
    /// confirmed — there is no state to lose and nothing to interrupt.
    Required,
    /// Opened from the menu, over a dsh that is already serving. The way out is
    /// closing it; every action is confirmed first, because every action ends in
    /// a restart and two of them delete something.
    Manage,
}

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
    /// The directory `delete-node` would remove, for the Nodes it will remove at
    /// all: one version under one version manager's root. `None` for every other
    /// Node the scan lists, which is most of them — the official installer's, a
    /// distro package's, a PATH entry of unknown provenance — and the panel draws
    /// no delete button for those.
    ///
    /// Decided by the script rather than here, off the `source` string: which
    /// directory holds a version is a fact about how each manager lays its
    /// versions out, and that knowledge already lives in the two scripts. This
    /// module only shows what they answered and passes the Node back.
    removable: Option<PathBuf>,
    /// Whether this is the Node the app is running dsh with right now. Filled in
    /// by [`mark_current`] rather than by the script: which Node is in use is a
    /// question about [`crate::dsh::search_path`], not about the machine.
    current: bool,
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
    /// Take dsh out of Node `i` (`uninstall-dsh`). Offered in [`Mode::Manage`]
    /// only: it does not get a blocked boot any closer to starting.
    UninstallDsh(usize),
    /// Delete the Node this app installed, and the dsh in it (`remove-node`).
    /// [`Mode::Manage`] only, and only when there is one.
    RemoveNode,
    /// Delete Node `i` itself, where a version manager installed it and the app
    /// is not running on it (`delete-node`). [`Mode::Manage`] only.
    ///
    /// The neighbour above is about the one Node this app unpacked, and clears
    /// the marker and PATH entry that came with it; this one is about somebody
    /// else's Node, and the script will only touch the narrow set its
    /// `removable` field named. The two are kept apart rather than merged
    /// because what they may delete, and what they have to clean up after, are
    /// not the same.
    DeleteNode(usize),
    /// Look again. The button only appears when the scan failed, which is the
    /// one state a user can do something about without restarting the app.
    Rescan,
    /// Put the panel away, leaving the running dsh alone. [`Mode::Manage`] only.
    Close,
    /// Walk away, and take the app with it.
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
    let mut nodes = parse_nodes(&output)?;
    mark_current(app, &mut nodes);
    Some(nodes)
}

/// What [`enumerate`] makes of one JSON line. Split from the run so the parse —
/// the part that can go wrong quietly — can be read on its own.
/// Mark the row the app is actually running out of.
///
/// Compared by the Node's path rather than by the marker, because the marker is
/// no longer the whole answer: a dsh on the user's own PATH outranks it. See
/// [`crate::dsh::search_path`].
fn mark_current(app: &AppHandle, nodes: &mut [NodeInfo]) {
    let Some(active) = crate::dsh::active_node(app) else {
        return;
    };
    for node in nodes.iter_mut() {
        node.current = same_path(&node.path, &active);
    }
}

/// Whether two paths name the same file, for the comparison above.
///
/// Case-folded on Windows, where `D:\app\nvm` and `d:\App\nvm` are one
/// directory and the script and [`crate::dsh::look_up`] build their strings
/// independently. Compared as written everywhere else, where they are two.
fn same_path(one: &std::path::Path, other: &std::path::Path) -> bool {
    if cfg!(windows) {
        one.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&other.as_os_str().to_string_lossy())
    } else {
        one == other
    }
}

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
            let removable = node
                .get("removable")
                .and_then(|value| value.as_str())
                .map(PathBuf::from);

            Some(NodeInfo {
                path,
                version,
                meets_minimum,
                has_dsh,
                dsh_version,
                source,
                removable,
                current: false,
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
    show(app, report, Mode::Required)
}

/// The same panel, opened from the titlebar menu over a dsh that is already
/// running: switch which Node runs it, install dsh into another one, take one
/// out, or delete the Node this app installed.
///
/// One panel rather than a menu item each. Everything here is a different answer
/// to the same question — which Node, and which dsh — and they are only legible
/// next to each other and next to the list of what is actually on the machine.
///
/// Blocks for as long as the panel is up, so it belongs on a worker thread; see
/// [`crate::open_runtime`]. Progress goes into the panel's own status line
/// rather than onto the loading page, which is not the page underneath this
/// time.
pub fn manage(app: &AppHandle) {
    // One panel at a time. The boot's is not reachable behind itself, but a
    // second click on the menu item while the first is still up would replace
    // the pending answer and leave the first loop waiting on a dead channel.
    if PENDING.lock().unwrap().is_some() {
        return;
    }

    let status = |text: &str, percent: f64| crate::setup_status(app, text, percent);
    show(app, &status, Mode::Manage);
}

/// The loop both callers run; [`Mode`] is what differs.
fn show(app: &AppHandle, report: &Report, mode: Mode) -> bool {
    // The window has to be visible: a chooser over a hidden window is one the
    // user cannot answer, and the autostart path reaches this with it hidden.
    reveal(app);

    let mut nodes = scan(app, report, mode);
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

        crate::deliver_setup(app, &payload(nodes.as_deref(), error.as_deref(), mode));

        let choice = receive.recv_timeout(ANSWER_TIMEOUT);
        // The panel is no longer the one on screen either way; take the slot so a
        // late click arrives at nothing rather than at a second showing.
        let _ = PENDING.lock().unwrap().take();

        match choice {
            // Only the boot's panel can take the app down. The menu's has a dsh
            // running behind it, and the button there says "close".
            Ok(Choice::Quit) if mode == Mode::Required => {
                quit(app);
                return false;
            }
            Ok(Choice::Quit | Choice::Close) => {
                crate::hide_setup(app);
                return true;
            }
            Ok(Choice::Rescan) => {
                // Same as everywhere else in this loop: the menu's panel stays
                // up and reports onto itself, and taking it down first would be
                // a blink of the page behind it on the way to putting it
                // straight back.
                if mode == Mode::Required {
                    crate::hide_setup(app);
                }
                error = None;
                nodes = scan(app, report, mode);
                continue;
            }
            Ok(choice) => {
                let listed = nodes.as_deref().unwrap_or_default();

                // Asked before the panel comes down, so the dialog is read
                // against the row that raised it. Nothing to confirm on the
                // boot's panel: no dsh is running to be interrupted, and the
                // destructive verbs are not offered there.
                if mode == Mode::Manage && !confirm(app, &choice, listed) {
                    error = None;
                    continue;
                }

                // Deleting somebody else's Node is the one action here that
                // changes nothing about the dsh the window behind is running:
                // no marker moves, nothing is installed, and the panel never
                // offers it for the Node in use. So it goes back to the list
                // with the row gone, rather than through a restart that would
                // take a session with it for a directory the app was not using.
                let listed_again = matches!(choice, Choice::DeleteNode(_));

                // The menu's panel stays up while the action runs. Its own
                // status line is the only thing on screen that can report one —
                // see [`crate::setup_status`] — and hidden, an `install-dsh`
                // was several minutes of a window that had simply gone quiet.
                // `signal` has already put the card in its busy state, which is
                // what that state is for. The boot's comes down onto the
                // loading page, which has a status line of its own.
                if mode == Mode::Required {
                    crate::hide_setup(app);
                }
                match act(app, choice, listed, report) {
                    Outcome::Done if mode == Mode::Manage && listed_again => {
                        error = None;
                        nodes = scan(app, report, mode);
                        reveal(app);
                        continue;
                    }
                    Outcome::Done if mode == Mode::Manage => {
                        // Which dsh runs is settled once, at boot, by
                        // `dsh::current` — and the server, the plugin list and
                        // the update check all hang off that one answer. Rather
                        // than unpick them, the app comes up again on the new
                        // one. Never returns.
                        report(t!("正在重启应用…", "Restarting…"), -1.0);
                        app.restart();
                    }
                    Outcome::Done => return true,
                    Outcome::Aborted => return false,
                    Outcome::Failed(message) => {
                        // Looked at again rather than reused: what just failed
                        // may be why — a Node uninstalled since the scan, an
                        // `install-node` that got as far as unpacking one — and
                        // the list a second choice is made from should be the
                        // machine as it is now.
                        error = Some(message);
                        nodes = scan(app, report, mode);
                        reveal(app);
                        continue;
                    }
                }
            }
            // Nobody is here. The boot's panel quits rather than leaving a
            // loading page that will never move: there is no dsh to start and
            // nothing else for that thread to do, and reopening the app is what
            // asks again. The menu's just goes away — there is a dsh behind it.
            Err(RecvTimeoutError::Timeout) => {
                eprintln!(
                    "dsh-desktop: nothing answered the setup panel in {} minutes",
                    ANSWER_TIMEOUT.as_secs() / 60
                );
                if mode == Mode::Required {
                    quit(app);
                    return false;
                }
                crate::hide_setup(app);
                return true;
            }
            Err(RecvTimeoutError::Disconnected) => return false,
        }
    }
}

/// Look at the machine, with something on screen saying so. Slow enough to need
/// the status line: the scripts ask a login shell for its PATH and then run
/// every `node` they find, which is a second on a quiet machine and several on
/// one with a version manager or two.
///
/// The menu's panel is put up empty first. Its status line is drawn on the card
/// itself — see [`crate::setup_status`] — so until the card exists the line
/// below lands nowhere, and the whole of the scan was a menu item that looked
/// like it had missed. The boot's panel is already over the loading page, which
/// has a status line of its own.
fn scan(app: &AppHandle, report: &Report, mode: Mode) -> Option<Vec<NodeInfo>> {
    if mode == Mode::Manage {
        crate::deliver_setup(app, &scanning());
    }
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
/// Put the action to the user before it runs, in [`Mode::Manage`] only.
///
/// Every one of these ends by restarting the app, and two of them delete
/// something, so none of them should happen on one click of a row. The dialog is
/// the one the rest of the app uses, and it draws above this panel — see the
/// z-index note in [`crate::dialog`].
fn confirm(app: &AppHandle, choice: &Choice, nodes: &[NodeInfo]) -> bool {
    use crate::dialog::{Ask, Choice as Button};

    let (title, body, affirmative) = match choice {
        Choice::Use(index) => {
            let Some(node) = nodes.get(*index) else {
                return false;
            };
            (
                t!("切换到 Node {}？", "Switch to Node {}?", node.version),
                t!(
                    "应用会重启，改用这里的 dsh：\n{}",
                    "The app restarts and runs the dsh in:\n{}",
                    node.path.display()
                ),
                t!("切换并重启", "Switch and restart"),
            )
        }
        Choice::InstallDsh(index) => {
            let Some(node) = nodes.get(*index) else {
                return false;
            };
            (
                t!(
                    "在 Node {} 里安装 dsh？",
                    "Install dsh into Node {}?",
                    node.version
                ),
                t!(
                    "会下载约 185 MB，装到 {}。装好之后应用会重启并改用它。",
                    "About 185 MB will be downloaded into {}. The app restarts onto it when that is done.",
                    node.path.display()
                ),
                t!("安装并重启", "Install and restart"),
            )
        }
        Choice::InstallNode => (
            t!("安装 Node {}？", "Install Node {}?", NODE_VERSION),
            t!(
                "会把 Node 和 dsh（约 185 MB）装到应用自己的目录，不动你已有的任何一个 Node。装好之后应用会重启。",
                "Node and dsh (about 185 MB) go into the app's own directory, leaving every Node you already have alone. The app restarts when that is done."
            )
            .to_string(),
            t!("安装并重启", "Install and restart"),
        ),
        Choice::UninstallDsh(index) => {
            let Some(node) = nodes.get(*index) else {
                return false;
            };
            let mut body = t!(
                "会把 dsh 从这个 Node 里卸载：\n{}\n\n你的会话、凭证和配置（$DSH_HOME）不受影响。",
                "dsh will be uninstalled from:\n{}\n\nYour sessions, credentials and settings ($DSH_HOME) are left alone.",
                node.path.display()
            );
            if node.current {
                body.push_str(t!(
                    "\n\n这正是应用现在用的那一份。卸载并重启之后，应用会再问你选哪一个。",
                    "\n\nThis is the one the app is running. After the restart it will ask you to pick another."
                ));
            }
            (
                t!(
                    "从 Node {} 里卸载 dsh？",
                    "Uninstall dsh from Node {}?",
                    node.version
                ),
                body,
                t!("卸载并重启", "Uninstall and restart"),
            )
        }
        Choice::DeleteNode(index) => {
            let Some(node) = nodes.get(*index) else {
                return false;
            };
            // The directory, not the `node` binary inside it: what goes is the
            // whole version, and the path the script answered with is the one
            // thing that says so exactly.
            let Some(dir) = node.removable.as_deref() else {
                return false;
            };
            let mut body = t!(
                "会删掉这个目录，以及里面的一切：\n{}\n\n你的其他 Node 一个都不会动，版本管理器本身也不会。",
                "This directory goes, and everything in it:\n{}\n\nEvery other Node is left alone, and so is the version manager itself.",
                dir.display()
            );
            if node.has_dsh {
                body.push_str(t!(
                    "\n\n装在这个 Node 里的 dsh 会跟着一起没了。应用现在用的不是它，所以不受影响。",
                    "\n\nThe dsh installed into this Node goes with it. It is not the one the app is running, so nothing in use is affected."
                ));
            }
            (
                t!("删除 Node {}？", "Delete Node {}?", node.version),
                body,
                t!("删除", "Delete"),
            )
        }
        Choice::RemoveNode => (
            t!("删除应用装的 Node？", "Delete the Node the app installed?").to_string(),
            t!(
                "会删掉本应用自己装的那个 Node，以及装在它里面的 dsh。你自己装的 Node 一个都不会动。",
                "The Node this app unpacked goes, and the dsh inside it. None of the Nodes you installed yourself are touched."
            )
            .to_string(),
            t!("删除并重启", "Delete and restart"),
        ),
        // Never reach here; [`show`] answers them before the action runs.
        Choice::Rescan | Choice::Close | Choice::Quit => return true,
    };

    crate::dialog::confirm(
        app,
        Ask {
            title,
            body,
            choices: vec![
                Button::new("cancel", t!("取消", "Cancel")),
                Button::primary("go", affirmative),
            ],
            answered: Box::new(|_, _| {}),
        },
        "go",
    )
}

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
            // Said here rather than left to the script's first `::status`.
            // Between the confirm dialog closing and that line there is a
            // PowerShell to start, a 1500-line script to parse and an
            // `npm prefix -g` to run for the Node's global directory — seconds
            // in which the card has greyed itself out and said nothing about
            // why. Every other action below announces itself the same way.
            report(t!("正在准备安装 dsh…", "Getting ready to install dsh…"), -1.0);
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
            // Same gap as `InstallDsh` above: the script's own first line comes
            // after the mirror speed test has been set up.
            report(t!("正在准备安装 Node…", "Getting ready to install Node…"), -1.0);
            crate::dsh::run(app, &[OsStr::new("-Mode"), OsStr::new("install-node")], report)
        }
        Choice::UninstallDsh(index) => {
            let Some(node) = nodes.get(index) else {
                return Outcome::Failed(t!("找不到所选的 Node。", "The chosen Node is gone.").into());
            };
            report(t!("正在卸载 dsh…", "Uninstalling dsh…"), -1.0);
            crate::dsh::run(
                app,
                &[
                    OsStr::new("-Mode"),
                    OsStr::new("uninstall-dsh"),
                    OsStr::new("-NodeExe"),
                    node.path.as_os_str(),
                ],
                report,
            )
        }
        Choice::DeleteNode(index) => {
            let Some(node) = nodes.get(index) else {
                return Outcome::Failed(t!("找不到所选的 Node。", "The chosen Node is gone.").into());
            };
            report(t!("正在删除这个 Node…", "Deleting that Node…"), -1.0);
            crate::dsh::run(
                app,
                &[
                    OsStr::new("-Mode"),
                    OsStr::new("delete-node"),
                    OsStr::new("-NodeExe"),
                    node.path.as_os_str(),
                ],
                report,
            )
        }
        Choice::RemoveNode => {
            report(
                t!("正在删除应用安装的 Node…", "Deleting the app's Node…"),
                -1.0,
            );
            crate::dsh::run(app, &[OsStr::new("-Mode"), OsStr::new("remove-node")], report)
        }
        // All handled in [`show`] before the action runs.
        Choice::Quit | Choice::Close | Choice::Rescan => return Outcome::Aborted,
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

/// The same panel with its list not filled in yet, delivered by [`scan`] before
/// the script runs so there is a card on screen for the scan to report onto.
///
/// [`Mode::Manage`] only, so `manage` is asserted rather than passed. Nothing in
/// it can be clicked: no answer is pending yet — [`show`] opens that channel
/// after the scan — and a button that swallows the click and freezes the card is
/// worse than no button, so the panel puts them all away until the real payload
/// arrives.
fn scanning() -> String {
    json!({
        "nodes": [],
        "scanned": true,
        "scanning": true,
        "nodeVersion": NODE_VERSION,
        "error": null,
        "manage": true,
    })
    .to_string()
}

/// The JSON the panel takes: the nodes, whether the machine could be looked at
/// at all, the version a fresh install would bring, and the error from a
/// previous attempt when there was one.
fn payload(nodes: Option<&[NodeInfo]>, error: Option<&str>, mode: Mode) -> String {
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
                "removable": node.removable.as_ref().map(|dir| dir.display().to_string()),
                "current": node.current,
            })
        })
        .collect();

    json!({
        "nodes": list,
        "scanned": nodes.is_some(),
        "nodeVersion": NODE_VERSION,
        "error": error,
        // What the panel keys its two shapes off: which way out it offers, and
        // whether the verbs that remove things are drawn at all.
        "manage": mode == Mode::Manage,
        // Whether this launch is following the dsh on the user's PATH rather
        // than the marker. While it is, switching Node here settles nothing —
        // `switch` writes the marker, and the marker is what that PATH outranks
        // on the next launch — so the panel says so and draws those buttons
        // dead. Read from the process rather than passed in: it is a fact about
        // the PATH this app inherited, and [`crate::dsh::usable_dsh_on_path`]
        // answers it once for the whole run.
        "followsPath": crate::dsh::usable_dsh_on_path(),
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
        "ledeManage": t!(
            "这台机器上的 Node，以及每个里面的 dsh。可以换用哪一个，也可以在这里装上或卸掉。改动之后应用会重启。",
            "The Nodes on this machine, and the dsh in each. Switch which one runs, or install and uninstall from here. The app restarts after a change."
        ),
        "ledeFailed": t!(
            "没能列出这台机器上的 Node —— 检测脚本没有跑起来，或者跑得太久。如果你确定机器上装过 Node，先点“重新检测”；也可以直接装一个新的 Node。",
            "The machine's Nodes could not be listed — the scan would not run, or took too long. If you know this machine has a Node, scan again; installing a fresh one is the other way out."
        ),
        // The same words `scan` reports, so the line does not change under the
        // user when the report lands a moment after the card.
        "scanning": t!("正在检测运行环境…", "Scanning for a runtime…"),
        "use": t!("使用这个", "Use this one"),
        "installDsh": t!("在此安装 dsh", "Install dsh here"),
        "tooOld": t!("版本过低", "Too old"),
        "hasDsh": t!("已装 dsh", "Has dsh"),
        "canInstall": t!("可安装 dsh", "Can install dsh"),
        "installNode": t!("安装新的 Node", "Install a fresh Node"),
        "uninstallDsh": t!("卸载 dsh", "Uninstall dsh"),
        "deleteNode": t!("删除这个 Node", "Delete this Node"),
        "removeNode": t!("删除应用装的 Node", "Delete the app's Node"),
        "inUse": t!("使用中", "In use"),
        // Added to the manage lede when the app is following the PATH's dsh.
        // Without it the dead buttons are a panel that has stopped working for
        // no stated reason.
        "ledeFollows": t!(
            "应用现在跟着终端走 —— 用的是 PATH 上的那个 dsh，所以在这里换 Node、装 dsh 都改不了应用用哪一个。要换，先在终端里把 Node 切过去（nvm use 之类），或者把 PATH 上的那个 dsh 卸掉。",
            "The app follows your terminal: it runs the dsh on your PATH, so switching Node or installing one here would not change which dsh it runs. Point your terminal at the Node you want (nvm use, and so on), or uninstall the dsh that is on your PATH."
        ),
        "rescan": t!("重新检测", "Scan again"),
        "close": t!("关闭", "Close"),
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
  var statusBox, statusText, statusFill;
  var installNode, removeNode, rescan, quit;
  var sent = false;
  // Set from every payload; see the `manage` field in `payload`.
  var managing = false;
  // And the `followsPath` field beside it: the app is running the PATH's dsh,
  // which no answer given here can change.
  var followsPath = false;

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
    var line = make('div', 'dsh-su-row' + (node.current ? ' dsh-su-now' : ''));

    var head = make('div', 'dsh-su-head', line);
    var title = make('span', 'dsh-su-ver', head);
    title.textContent = 'Node ' + node.version;
    if (node.current) head.appendChild(chip('now', TEXT.inUse));
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
    }} else if (!node.hasDsh) {{
      // Before `current`, and that ordering is the whole of this branch.
      // `current` is `dsh::active_node` — the first `node` on the search path —
      // and it says nothing about dsh. So the Node a first launch would run dsh
      // with, on a machine that has one on PATH and no dsh anywhere, is marked
      // current *and* has nothing to run: asking `current` first drew it the
      // dead "in use" button and no way to install, on the one panel whose only
      // job is to get dsh onto the machine. The chips never had this bug —
      // theirs is `!meetsMinimum` then `!hasDsh` — so the row said "can install
      // dsh" beside a button that would not.
      var add = button(actions, TEXT.installDsh, function () {{
        signal('setup-install-dsh?i=' + index);
      }});
      // Dead while the app follows the PATH: an install ends in a marker, and
      // the marker is what that PATH outranks. The dsh would land in that Node
      // and the app would go on running the terminal's.
      add.disabled = managing && followsPath;
    }} else if (node.current) {{
      // Nothing to switch to; this is where the app already is, and it has a
      // dsh — the branch above is the case where it has not.
      var here = button(actions, TEXT.inUse, function () {{}});
      here.disabled = true;
    }} else {{
      var pick = button(actions, TEXT.use, function () {{
        signal('setup-use?i=' + index);
      }}, true);
      // Dead for the same reason as the install above: `switch` writes the
      // marker too, so the button is a restart that lands back on this same
      // list. The lede is where the reason is written — a disabled button takes
      // no pointer events, so a `title` on it would never be read.
      pick.disabled = managing && followsPath;
    }}

    // Taking dsh out is offered wherever there is one, floor or no floor: a dsh
    // in a Node too old to run it is exactly the thing worth clearing away. Not
    // on the boot's panel though — it gets a blocked launch no closer to
    // starting, and that panel has one job.
    if (managing && node.hasDsh) {{
      var drop = button(actions, TEXT.uninstallDsh, function () {{
        signal('setup-uninstall-dsh?i=' + index);
      }});
      drop.className = 'dsh-su-danger';
    }}

    // And taking the Node itself away, for the ones a version manager installed
    // — `removable` is the script's answer for which those are, and it is null
    // for every Node that belongs to something else. Never for the row the app
    // is running out of: deleting that from under a serving dsh is not an undo,
    // it is a machine with no dsh and a window still open on one.
    if (managing && node.removable && !node.current) {{
      var wipe = button(actions, TEXT.deleteNode, function () {{
        signal('setup-delete-node?i=' + index);
      }});
      wipe.className = 'dsh-su-danger';
      wipe.title = node.removable;
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
      '.dsh-su button.dsh-su-danger{{color:var(--su-bad);' +
      'border-color:color-mix(in srgb,var(--su-bad) 40%,transparent)}}' +
      '.dsh-su button.dsh-su-danger:hover{{background:var(--su-bad);color:#fff;' +
      'border-color:var(--su-bad)}}' +
      '.dsh-su-row.dsh-su-now{{border-color:var(--su-accent)}}' +
      // The status line an action reports onto. Only the menu's panel needs it
      // — the boot's has the loading page underneath — but it is drawn either
      // way and simply stays empty.
      '.dsh-su-status{{display:none;margin-top:12px;font-size:13px;' +
      'color:var(--su-muted)}}' +
      '.dsh-su-status.dsh-su-status-on{{display:block}}' +
      '.dsh-su-bar{{height:4px;margin-top:8px;border-radius:999px;' +
      'background:var(--su-soft);overflow:hidden;display:none}}' +
      '.dsh-su-bar.dsh-su-bar-on{{display:block}}' +
      '.dsh-su-bar i{{display:block;height:100%;width:0;border-radius:999px;' +
      'background:var(--su-accent);transition:width .25s ease}}' +
      // Answered, and waiting for the app to take the panel away. The list goes
      // quiet; the status line does not, because reading it is the whole of what
      // there is to do while an install runs.
      '.dsh-su.dsh-su-busy .dsh-su-card{{pointer-events:none}}' +
      '.dsh-su.dsh-su-busy .dsh-su-list{{opacity:.45}}';
    document.head.appendChild(style);

    root = make('div', 'dsh-su');
    card = make('div', 'dsh-su-card', root);
    make('h1', '', card).textContent = TEXT.title;
    lede = make('p', 'dsh-su-lede', card);
    errBox = make('div', 'dsh-su-err', card);
    errText = make('div', '', errBox);
    list = make('div', 'dsh-su-list', card);

    statusBox = make('div', 'dsh-su-status', card);
    statusText = make('span', '', statusBox);
    var bar = make('div', 'dsh-su-bar', statusBox);
    statusFill = make('i', '', bar);

    var foot = make('div', 'dsh-su-foot', card);
    quit = button(foot, TEXT.quit, function () {{
      // Written out rather than as a ternary on the verb: the test below reads
      // the verb literals out of this script, and a computed one is a verb
      // nothing checks.
      if (managing) signal('setup-close');
      else signal('setup-quit');
    }});
    make('span', 'dsh-su-spacer', foot);
    rescan = button(foot, TEXT.rescan, function () {{ signal('setup-rescan'); }});
    removeNode = button(foot, TEXT.removeNode, function () {{
      signal('setup-remove-node');
    }});
    removeNode.className = 'dsh-su-danger';
    installNode = button(foot, TEXT.installNode, function () {{
      signal('setup-install-node');
    }}, true);

    // Escape closes the panel opened from the menu, and does nothing on the
    // one the boot is waiting on: there the app has not decided whether it
    // starts at all, and the key people press to dismiss things should not be
    // the one that quits.
    document.addEventListener('keydown', function (event) {{
      if (event.key === 'Escape' && managing && root.classList.contains('dsh-su-shown')) {{
        signal('setup-close');
      }}
    }});

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

      // Read before the rows are built: `row` keys the destructive verbs off
      // the first and the switch button off the second.
      managing = data.manage === true;
      followsPath = data.followsPath === true;
      quit.textContent = managing ? TEXT.close : TEXT.quit;

      // The card before the list: `scan` puts this up while the script walks the
      // machine, which is a second or more of a menu item that had otherwise
      // done nothing visible. Every button goes away rather than being drawn
      // dead — there is no answer to give yet — and `busy` is the state that
      // already means exactly this: the list quiet, the status line not.
      if (data.scanning === true) {{
        root.classList.add('dsh-su-busy');
        // Escape is still wired, and it is the one way a click could still be
        // made from here. Latched, so it is ignored rather than swallowed.
        sent = true;
        lede.textContent = TEXT.ledeManage;
        errBox.classList.remove('dsh-su-err-shown');
        errText.textContent = '';
        list.textContent = '';
        statusText.textContent = TEXT.scanning;
        statusBox.classList.add('dsh-su-status-on');
        statusFill.parentNode.classList.remove('dsh-su-bar-on');
        quit.hidden = true;
        rescan.hidden = true;
        removeNode.hidden = true;
        installNode.hidden = true;
        root.classList.add('dsh-su-shown');
        return;
      }}
      quit.hidden = false;

      var nodes = data.nodes || [];
      // `scanned: false` is a machine that could not be looked at, which is not
      // the same as a machine with no Node — see `enumerate`.
      var scanned = data.scanned !== false;
      lede.textContent = !scanned
        ? TEXT.ledeFailed
        : (managing ? TEXT.ledeManage
          : (nodes.length ? TEXT.ledeNodes : TEXT.ledeNone));
      // Only on the menu's panel, and only where it is true. The boot's panel
      // is a launch with no dsh to follow, and it is the one screen that has to
      // stay answerable.
      if (managing && followsPath && scanned) {{
        lede.textContent += ' ' + TEXT.ledeFollows;
      }}
      rescan.hidden = scanned;

      // Only where there is one to delete, and not while the app is running out
      // of it — the same rule the rows follow, and the app's own Node is only
      // ever deletable from here, so this is where that rule has to hold. A
      // Node cannot be pulled out from under the dsh serving the window behind:
      // on Windows the running `node.exe` will not delete at all, and the mode
      // clears the marker and the PATH entry either way, so what is left is a
      // half-deleted directory the app no longer knows it installed. Switching
      // to another Node first brings the button back.
      var ours = nodes.some(function (node) {{
        return node.source === 'managed' && !node.current;
      }});
      removeNode.hidden = !(managing && ours);

      // A fresh Node is only worth offering when it would bring one this
      // machine has not got. The app unpacks exactly one version — the one this
      // button names — so an app-managed row already at that version makes the
      // click 185 MB for a second copy of what is already there. Worse than
      // wasteful, over the Node the app is running from: `install-node`
      // replaces the directory outright, Windows will not delete a running
      // `node.exe`, and the mirror loop reports that as "无法下载 Node" for a
      // download that in fact worked.
      //
      // An app-managed Node at another version — an earlier release bundled an
      // earlier Node — is a real upgrade, and keeps the button.
      installNode.hidden = nodes.some(function (node) {{
        return node.source === 'managed' && node.version === data.nodeVersion;
      }});
      // And dead where it would change nothing: a fresh Node arrives with its
      // own dsh and a marker naming it, and while the app follows the PATH that
      // marker is the loser of every lookup. Live again the moment the PATH has
      // no dsh to follow.
      installNode.disabled = managing && followsPath;

      statusBox.classList.remove('dsh-su-status-on');
      statusText.textContent = '';

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

  /** Progress for an action started from the menu; see `setup_status`. */
  window.__dshSetupStatus = function (text, percent) {{
    if (!root || !statusBox) return;
    statusText.textContent = text || '';
    statusBox.classList.toggle('dsh-su-status-on', !!text);

    var value = parseFloat(percent);
    var known = !isNaN(value) && value >= 0;
    statusFill.parentNode.classList.toggle('dsh-su-bar-on', known);
    if (known) statusFill.style.width = Math.min(100, value) + '%';
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
          {"path":"/home/a/.nvm/versions/node/v24.9.0/bin/node","version":"24.9.0","meetsMinimum":true,"prefix":"/usr/local","hasDsh":true,"dshVersion":"0.1.8","source":"nvm","removable":"/home/a/.nvm/versions/node/v24.9.0"},
          {"path":"/usr/bin/node","version":"18.4.0","meetsMinimum":false,"prefix":"/usr","hasDsh":false,"dshVersion":null,"source":"system","removable":null}
        ]"#;

        let nodes = parse_nodes(json).expect("a scan, not a failure");

        assert_eq!(nodes.len(), 2);
        assert!(nodes[0].has_dsh);
        assert_eq!(nodes[0].dsh_version.as_deref(), Some("0.1.8"));
        assert!(nodes[0].meets_minimum);
        assert!(!nodes[1].has_dsh);
        assert!(nodes[1].dsh_version.is_none());
        assert!(!nodes[1].meets_minimum);
        // The field the delete button hangs off: a directory for the version a
        // manager unpacked, nothing for the Node the distro owns.
        assert_eq!(
            nodes[0].removable.as_deref(),
            Some(std::path::Path::new("/home/a/.nvm/versions/node/v24.9.0"))
        );
        assert!(nodes[1].removable.is_none());
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
        let scanned = super::payload(Some(&[]), None, super::Mode::Required);
        let failed = super::payload(None, None, super::Mode::Required);

        assert!(scanned.contains("\"scanned\":true"));
        assert!(failed.contains("\"scanned\":false"));
    }

    /// The card `scan` puts up before the script runs says it is a scan in
    /// progress, and says it is the menu's panel — the branch that draws it
    /// hides every button, so a payload that reached the boot's chooser with
    /// this flag would be a screen with no way forward at all.
    #[test]
    fn the_scanning_payload_is_the_menu_panel_with_no_list_yet() {
        let scanning = super::scanning();

        assert!(scanning.contains("\"scanning\":true"));
        assert!(scanning.contains("\"manage\":true"));
        assert!(scanning.contains("\"nodes\":[]"));
        // And the finished one is not mistaken for it.
        assert!(!super::payload(Some(&[]), None, super::Mode::Manage).contains("\"scanning\":true"));
    }

    /// The other flag the panel keys off. Wrong, and either the boot's chooser
    /// grows a button that quits to nowhere, or the menu's loses the only way
    /// out it has.
    #[test]
    fn the_payload_says_which_panel_this_is() {
        assert!(super::payload(Some(&[]), None, super::Mode::Manage).contains("\"manage\":true"));
        assert!(super::payload(Some(&[]), None, super::Mode::Required).contains("\"manage\":false"));
    }

    /// The field the panel greys its switch buttons off, and the panel's read
    /// of it. Written in two places with nothing else making them agree: a
    /// rename on either side is an `undefined` on the other, which reads as
    /// false — every "use this one" live again, and each one a restart that
    /// lands back on the same list.
    #[test]
    fn the_panel_reads_the_follows_path_field_the_payload_sends() {
        let sent = super::payload(Some(&[]), None, super::Mode::Manage);

        assert!(sent.contains("\"followsPath\":"));
        assert!(super::script().contains("data.followsPath"));
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

        let mut checked = std::collections::BTreeSet::new();
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
            checked.insert(verb);
        }

        // A set, not a count: two of the verbs are signalled from more than one
        // place, and what matters is that every distinct one resolves.
        assert_eq!(
            checked,
            std::collections::BTreeSet::from([
                "setup-use?i=",
                "setup-install-dsh?i=",
                "setup-uninstall-dsh?i=",
                "setup-delete-node?i=",
                "setup-install-node",
                "setup-remove-node",
                "setup-rescan",
                "setup-close",
                "setup-quit",
            ]),
            "the panel's verbs are not the ones expected"
        );
    }

    /// Every `-Mode` this module runs has to be one both scripts accept. They
    /// are three files edited separately, and a mode only one of them knows is a
    /// button that fails with a parameter error the moment it is pressed —
    /// after the confirm, and on the panel that has no other way forward.
    #[test]
    fn every_mode_this_module_runs_is_one_the_scripts_take() {
        let here = include_str!("setup.rs");
        let read = |path: &str| {
            let bytes = std::fs::read(path).expect("a readable bootstrap script");
            String::from_utf8_lossy(&bytes).into_owned()
        };
        let ps1 = read(concat!(env!("CARGO_MANIFEST_DIR"), "/../scripts/install-deps.ps1"));
        let sh = read(concat!(env!("CARGO_MANIFEST_DIR"), "/../scripts/install-deps.sh"));

        for mode in [
            "switch",
            "install-dsh",
            "install-node",
            "uninstall-dsh",
            "remove-node",
            "delete-node",
        ] {
            // The list above is only worth checking if it is still the list this
            // module actually runs.
            assert!(
                here.contains(&format!("OsStr::new(\"{mode}\")")),
                "{mode} is not run from this module any more"
            );
            assert!(
                ps1.contains(&format!("'{mode}'")),
                "install-deps.ps1 does not accept -Mode {mode}"
            );
            assert!(
                sh.contains(&format!("\n    {mode}) ")),
                "install-deps.sh does not accept -Mode {mode}"
            );
        }
    }
}
