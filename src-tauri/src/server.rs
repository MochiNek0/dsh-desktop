//! The managed `dsh web` child process.
//!
//! `dsh web --no-open --port <port>` binds a loopback port — `0` for any free
//! one — and prints one line, `dsh web: <url>`, which is both the readiness
//! signal and the URL the window navigates to. `--no-open` suppresses the
//! default-browser handoff newer dsh does, since this window is the browser.
//!
//! Since dsh 0.1.2 that URL carries this dsh process's launch token: the window
//! navigates to `http://127.0.0.1:<port>/?token=<base64url>`, dsh answers 303
//! to a clean `/` and sets a signed browser cookie. The cookie is bound to the
//! authority it was minted for and signed with a secret that lives in dsh's
//! credential store rather than in the process — which is what makes a
//! same-port restart invisible to a page that is already loaded. See
//! [`crate::resume`], and [`crate::auth`] for the webviews the exchange has to
//! be spelled out to.

use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

/// Keeps a spawned process off the console it would otherwise pop up.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// How many output lines to keep around for the failure message.
const TAIL_LINES: usize = 30;

/// What the reader threads report back to the UI.
pub enum Event {
    /// The server is listening on this URL.
    Ready(String),
    /// The server exited before it ever printed one; carries its output tail.
    Failed(String),
    /// The server was serving and then stopped. Same tail, and the same EOF
    /// underneath it — what separates the two is whether a URL came first.
    ///
    /// This is the only warning anything gets that dsh is gone: the window is
    /// showing a page served by a process that no longer exists, and left alone
    /// it would sit there looking fine until the user clicked something. Told
    /// apart from a stop this app asked for by [`crate::Session::epoch`], not
    /// here — from inside the pipe the two are the same event.
    Exited(String),
}

pub struct Server {
    child: Child,
    /// Held for the lifetime of the app; see [`Job`].
    #[cfg(windows)]
    _job: Option<Job>,
    /// The pid file to delete on the way out; see [`leftover`].
    #[cfg(target_os = "macos")]
    record: Option<std::path::PathBuf>,
}

/// Spawn `dsh web` and return the handle plus a channel that yields exactly one
/// [`Event`].
///
/// `port` is a request rather than a claim: `None` lets the OS pick, which is
/// what a first start does, and a number asks for the one a server that has just
/// died was already on. See [`crate::resume`] for why that is worth asking for,
/// and note that a port already taken is a spawn that starts and then exits —
/// an [`Event::Failed`], not an error from here.
pub fn start(
    app: &tauri::AppHandle,
    port: Option<u16>,
) -> std::io::Result<(Server, Receiver<Event>)> {
    // Before the spawn rather than after it: the point of the sweep is that no
    // dsh of ours is running yet, so anything that answers to the recorded pid
    // belongs to a run that is over.
    #[cfg(target_os = "macos")]
    leftover::sweep(app);

    let mut prepared = command(app, port);
    prepared
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(working_dir());

    let mut child = tethered(prepared)?;

    #[cfg(windows)]
    let job = Job::hold(&child);
    #[cfg(target_os = "macos")]
    let record = leftover::remember(app, child.id());

    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");

    let (tx, rx) = channel();
    let tail = Arc::new(Mutex::new(Vec::new()));

    pump(stderr, tail.clone(), None);
    pump(stdout, tail, Some(tx));

    let server = Server {
        child,
        #[cfg(windows)]
        _job: job,
        #[cfg(target_os = "macos")]
        record,
    };
    Ok((server, rx))
}

impl Server {
    /// Stop the server.
    pub fn stop(&mut self) {
        kill_tree(&mut self.child);

        // This ran, so there is nothing for the next launch to clean up after —
        // and leaving the pid behind is what would have it looked up again long
        // after it has been handed to something else. See [`leftover`].
        #[cfg(target_os = "macos")]
        {
            if let Some(record) = self.record.take() {
                let _ = std::fs::remove_file(record);
            }
        }
    }
}

/// Kill a child process along with everything it spawned, and reap it.
///
/// The child is often a launcher rather than the process doing the work — on
/// Windows `dsh` on PATH is `cmd.exe` wrapping the `dsh.cmd` shim wrapping
/// node, and npm shells out to node of its own — so the whole tree has to go,
/// not just the parent.
pub fn kill_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    // The long-running children are put in a process group of their own by
    // `group_leader`, so the group is the tree and a negative pid takes all of
    // it. The short-lived ones — a `dsh --version`, an `npm view` — are not,
    // and would share ours: killing that group would kill the app, so the group
    // is only signalled once it has been confirmed to be the child's own.
    #[cfg(unix)]
    {
        let pid = child.id() as libc::pid_t;
        // SAFETY: both are reads and a signal against a pid we own and have not
        // reaped yet, so it cannot have been reused by another process.
        unsafe {
            if libc::getpgid(pid) == pid {
                libc::kill(-pid, libc::SIGKILL);
            } else {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }

    let _ = child.wait();
}

/// Put a child in a process group of its own, so that [`kill_tree`] can take
/// down everything it goes on to spawn.
///
/// This is the ordinary shutdown path only, and what covers an app that never
/// reaches it differs by platform: the job object below on Windows and
/// [`tethered`] on Linux, both of which have the kernel do it at the moment of
/// death. macOS has neither, so there it is the next launch that kills what the
/// last one left behind — see `leftover`.
#[cfg(unix)]
pub fn group_leader(command: &mut Command) {
    // SAFETY: `setpgid(0, 0)` is async-signal-safe and touches nothing but the
    // process group of the child that is about to exec. It cannot fail here:
    // the child is a fresh fork, so it is neither a session leader nor already
    // moved into another session.
    unsafe {
        command.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }
}

/// Spawn a child that the kernel will kill when this process dies, however it
/// dies. The Linux counterpart of the `Job` below, and used for the same
/// children.
///
/// The backstop matters most for `dsh web`: dsh takes an exclusive `flock` on a
/// session's `session.lock` for as long as it has that session open, and the
/// kernel only releases it when the holder dies. So a `dsh web` that outlives
/// the app it belonged to goes on owning the sessions it had open, and the next
/// launch — a new `dsh web`, reading the same `~/.dsh` — can list them and
/// cannot resume them, which dsh reports as `SessionAlreadyOwnedError`. The
/// sessions of one project sit in one directory, so what the user sees is one
/// folder that has stopped working while the rest are fine.
///
/// `PR_SET_PDEATHSIG` is what asks for it, and it fires when the *thread* that
/// forked the child exits rather than when the process does — which is why the
/// fork is handed to `spawner`, a thread that is started once and never
/// finishes. Setting it on a child forked from a boot thread or from the
/// restart watcher would have it killed the moment that thread was done, which
/// is the trap this indirection exists to stay out of.
///
/// Two limits, both deliberate. The signal goes to the child alone, not to the
/// group `group_leader` put it in: for `dsh web` that child is node itself —
/// npm links a POSIX shim as a symlink, so there is no shell in between — and
/// node is what holds the locks. And the child sets the flag after the fork, so
/// a parent that dies in that window leaves it unset; [`kill_tree`] is what
/// covers every path that runs at all.
pub fn tethered(mut command: Command) -> std::io::Result<Child> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `prctl(PR_SET_PDEATHSIG, ...)` is async-signal-safe and
        // touches nothing but the fresh child's own parent-death signal. A
        // failure would only lose the backstop, and there is nowhere to report
        // it from between fork and exec, so the result is dropped.
        unsafe {
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }

        let (tx, rx) = channel();
        spawner()
            .send((command, tx))
            .expect("the spawner thread runs for the life of the process");
        return rx
            .recv()
            .expect("the spawner thread answers every order it takes");
    }

    #[cfg(not(target_os = "linux"))]
    {
        command.spawn()
    }
}

/// The one thread children whose death should follow ours are forked from.
///
/// Started on first use and parked on the queue forever after: it has no exit
/// of its own, so the only thing that ends it is the process ending, which is
/// exactly the event [`tethered`] wants the children to hear about. Nothing but
/// a fork happens on it — the pipes are read by the threads [`pump`] starts,
/// off this one, so a child that never stops printing cannot hold up the next
/// spawn.
#[cfg(target_os = "linux")]
fn spawner() -> &'static Sender<(Command, Sender<std::io::Result<Child>>)> {
    static SPAWNER: std::sync::OnceLock<Sender<(Command, Sender<std::io::Result<Child>>)>> =
        std::sync::OnceLock::new();

    SPAWNER.get_or_init(|| {
        let (orders, queue) = channel::<(Command, Sender<std::io::Result<Child>>)>();
        std::thread::spawn(move || {
            for (mut command, answer) in queue {
                let _ = answer.send(command.spawn());
            }
        });
        orders
    })
}

/// The `dsh web` a previous run left behind.
///
/// macOS has neither the job object below nor `PR_SET_PDEATHSIG`, so a
/// force-killed app there leaves its `dsh web` running and that process goes on
/// owning every session it had open — see [`tethered`] for what the user sees
/// when it does. Nothing can be done at the moment of death, so it is done at
/// the next launch instead, which is exactly when it starts to matter: the pid
/// of each `dsh web` is written down beside the app's own state, and the next
/// launch kills whatever is still answering to it.
///
/// [`Server::stop`] removes the record, so a file that is still there is itself
/// the evidence that the app died without running any cleanup. Even then the
/// pid is not signalled on trust. Pids are recycled, and by the next launch a
/// stale one could belong to anything — so the process has to still look like a
/// `dsh web`, and its parent has to be launchd, which is what a child left
/// behind by a dead app is reparented to. A `dsh web` the user is running in a
/// terminal is the one thing here that must not be killed, and it has the
/// shell's pid as its parent, not 1.
#[cfg(target_os = "macos")]
mod leftover {
    use std::path::PathBuf;
    use std::process::{Command, Stdio};

    /// Where the pid is written. Beside `desktop.json` and the update stamp,
    /// which is where the rest of this app's small state lives.
    fn record(app: &tauri::AppHandle) -> Option<PathBuf> {
        Some(crate::dsh::app_dir(app)?.join("dsh-web.pid"))
    }

    /// Kill the `dsh web` of a run that never got to clean up after itself.
    ///
    /// Once per launch, and before the first spawn: a second call has nothing
    /// left to read, and every later `dsh web` is one this run is holding.
    pub fn sweep(app: &tauri::AppHandle) {
        static SWEPT: std::sync::Once = std::sync::Once::new();
        SWEPT.call_once(|| {
            let Some(record) = record(app) else { return };
            let read = std::fs::read_to_string(&record);
            // Whatever it said has now been acted on, and a record that cannot
            // be removed is one this would read again on every launch.
            let _ = std::fs::remove_file(&record);

            let Some(pid) = read.ok().and_then(|pid| pid.trim().parse::<i32>().ok()) else {
                return;
            };
            if !orphaned_dsh(pid) {
                return;
            }

            eprintln!("dsh-desktop: killing the dsh web the last run left behind (pid {pid})");
            // Negative: `group_leader` made it the leader of its own group, so
            // this takes the workers it spawned with it.
            //
            // SAFETY: a signal to a pid that has just been confirmed to be a
            // `dsh web` whose parent is launchd.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        });
    }

    /// Whether this pid is still a `dsh web`, and an abandoned one.
    ///
    /// `ps` rather than `libproc`, because the answer needs the arguments and
    /// not just the executable — every `dsh` is some `node`, and `node` on its
    /// own says nothing about whose process this is.
    fn orphaned_dsh(pid: i32) -> bool {
        let Ok(listed) = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "ppid=,command="])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
        else {
            return false;
        };

        let listed = String::from_utf8_lossy(&listed.stdout);
        let listed = listed.trim();
        let Some((parent, command)) = listed.split_once(char::is_whitespace) else {
            return false;
        };
        parent.trim() == "1" && command.contains("dsh") && command.contains(" web")
    }

    /// Write down the `dsh web` this run is starting.
    pub fn remember(app: &tauri::AppHandle, pid: u32) -> Option<PathBuf> {
        let record = record(app)?;

        // The directory is the app's own, and on a first launch nothing has
        // needed it yet.
        if let Some(parent) = record.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(error) = std::fs::write(&record, pid.to_string()) {
            eprintln!("dsh-desktop: could not record the dsh web pid: {error}");
            return None;
        }
        Some(record)
    }
}

/// A Windows job object holding a child's process tree, configured to kill
/// everything in it once the last handle to it closes. Used for the two
/// children that outlive a single call: `dsh web`, and the npm that installs a
/// dsh update.
///
/// [`kill_tree`] is the ordinary shutdown path and does the same job more
/// politely. This is the backstop for the paths that never reach it: the app is
/// force-killed (`taskkill /F` without `/T`), or it crashes. Handles are closed
/// by the kernel when a process dies however it dies, so closing ours is enough
/// to take the tree down with us — no cooperation from our own code required.
///
/// Best-effort: a failure here loses the backstop, not the app, so `hold`
/// returns `None` rather than propagating.
#[cfg(windows)]
pub struct Job(windows_sys::Win32::Foundation::HANDLE);

// SAFETY: a job object handle is process-wide, not owned by the thread that
// created it, so moving it into the `Server` another thread may drop is fine.
#[cfg(windows)]
unsafe impl Send for Job {}

#[cfg(windows)]
impl Job {
    /// Put `child` — and, by inheritance, everything it spawns — in a fresh job.
    ///
    /// There is a small race: the child is already running by the time it is
    /// assigned, so a grandchild spawned in that window would escape. In
    /// practice the child is a launcher that takes milliseconds to get to
    /// spawning node, and the ordinary shutdown path covers the tree anyway.
    pub fn hold(child: &Child) -> Option<Self> {
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };

        // SAFETY: every call below is a documented Win32 entry point given a
        // handle we just created (or std's live process handle) and a
        // correctly sized, fully initialized limit struct.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }

            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

            let configured = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            let assigned =
                configured != 0 && AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) != 0;

            if !assigned {
                CloseHandle(job);
                return None;
            }
            Some(Self(job))
        }
    }
}

#[cfg(windows)]
impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: `self.0` is the handle `hold` created and never handed out.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

/// The command that boots the browser UI.
///
/// [`crate::dsh::current`] resolves it to an absolute path — `DSH_BIN`, or the
/// npm shim in the global prefix — rather than leaving the lookup to
/// `Command::new`, which would search the PATH this process started with. On a
/// machine the installer has just put Node on, that PATH is already out of date.
///
/// Falling back to the bare name is for the case where nothing was found at
/// all: the error from a failed spawn is what the loading page reports, and
/// "dsh 不存在" is a better one than "找不到应用数据目录".
fn command(app: &tauri::AppHandle, port: Option<u16>) -> Command {
    let mut command = match crate::dsh::current(app) {
        Some(dsh) => Command::new(dsh.bin),
        None => Command::new(if cfg!(windows) { "dsh.cmd" } else { "dsh" }),
    };

    // `--port 0` is dsh's own way of saying "any free one"; a number is the one
    // a reconnect is trying to land back on.
    let port = port.map_or_else(|| "0".to_string(), |port| port.to_string());
    // `--no-open` keeps dsh from handing the URL to the system's default
    // browser: this app's own window navigates to it, so a second tab in the
    // user's browser is a leftover from running `dsh web` in a terminal, not
    // something the desktop client wants. Newer dsh defaults to opening it.
    command.args(["web", "--no-open", "--port", &port]);

    // dsh shells out to `node` for workers and plugin tooling, and the shim
    // itself needs one. See `dsh::apply_path`.
    crate::dsh::apply_path(app, &mut command);

    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    #[cfg(unix)]
    group_leader(&mut command);

    command
}

/// The agent's initial working directory. The UI's directory picker changes it
/// per session, so this only has to be somewhere predictable.
fn working_dir() -> std::path::PathBuf {
    #[allow(deprecated)]
    std::env::home_dir()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// Read one child stream to EOF, mirroring it to our own log and remembering the
/// tail. When `tx` is present, the first URL line resolves the channel and EOF
/// ends it — as a [`Event::Failed`] if no URL ever came, and as an
/// [`Event::Exited`] if one did.
fn pump<R: Read + Send + 'static>(
    stream: R,
    tail: Arc<Mutex<Vec<String>>>,
    tx: Option<Sender<Event>>,
) {
    std::thread::spawn(move || {
        let mut ready = false;

        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            eprintln!("[dsh] {line}");

            if let (false, Some(tx), Some(url)) = (ready, tx.as_ref(), parse_url(&line)) {
                ready = true;
                let _ = tx.send(Event::Ready(url));
            }

            let mut tail = tail.lock().unwrap();
            if tail.len() == TAIL_LINES {
                tail.remove(0);
            }
            tail.push(line);
        }

        // EOF on stdout: the process is gone, or close enough that it will never
        // print again. Which of the two events that is depends on whether it got
        // as far as serving.
        if let Some(tx) = tx {
            let output = tail.lock().unwrap().join("\n");
            let _ = tx.send(if ready {
                Event::Exited(output)
            } else {
                Event::Failed(output)
            });
        }
    });
}

/// Pull the served URL out of the `dsh web: <url>` line. The line also carries a
/// `(LAN: …)` suffix when the server is reachable off-box; the first URL is the
/// loopback one this client wants.
fn parse_url(line: &str) -> Option<String> {
    if !line.contains("dsh web:") {
        return None;
    }
    let start = line.find("http://").or_else(|| line.find("https://"))?;
    let url = line[start..].split_whitespace().next()?;
    Some(url.trim_end_matches(|c: char| !c.is_ascii_alphanumeric() && c != '/').to_string())
}

#[cfg(test)]
mod tests {
    use super::parse_url;

    #[test]
    fn reads_the_loopback_url() {
        assert_eq!(
            parse_url("dsh web: http://127.0.0.1:54775").as_deref(),
            Some("http://127.0.0.1:54775")
        );
    }

    #[test]
    fn ignores_the_lan_suffix() {
        assert_eq!(
            parse_url("dsh web: http://127.0.0.1:3080 (LAN: http://192.168.1.7:3080)").as_deref(),
            Some("http://127.0.0.1:3080")
        );
    }

    #[test]
    fn ignores_other_output() {
        assert_eq!(parse_url("listening on http://127.0.0.1:1"), None);
    }

    /// The shape dsh has printed since 0.1.2. The trailing trim exists to drop
    /// punctuation the line ends in, and the token is the one thing on the line
    /// it must not touch: base64url spends `-` and `_`, neither of which is
    /// alphanumeric.
    ///
    /// That holds today by arithmetic rather than by design. dsh's token is 32
    /// random bytes, which base64url encodes as 43 characters — 258 bits for
    /// 256 — so the last character carries two significant bits and can only be
    /// one of the sixteen whose alphabet index is a multiple of four. `-` and
    /// `_` are indices 62 and 63, so neither ever lands last. Change the token
    /// length upstream and that stops being true, which is what this is here to
    /// catch.
    #[test]
    fn keeps_the_launch_token() {
        let url = "http://127.0.0.1:63170/?token=DT8oXJ58ruxVOPtmFLiQDBLJ0M5oh22XI_C8TgecGj0";
        assert_eq!(parse_url(&format!("dsh web: {url}")).as_deref(), Some(url));
    }

    #[test]
    fn keeps_the_launch_token_past_the_lan_suffix() {
        assert_eq!(
            parse_url(
                "dsh web: http://127.0.0.1:3080/?token=ab_cd (LAN: http://192.168.1.7:3080/?token=ab_cd)"
            )
            .as_deref(),
            Some("http://127.0.0.1:3080/?token=ab_cd")
        );
    }
}
