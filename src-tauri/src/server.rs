//! The managed `dsh web` child process.
//!
//! `dsh web --port 0` binds a free loopback port and prints one line —
//! `dsh web: http://127.0.0.1:<port>` — which is both the readiness signal and
//! the URL the window navigates to.

use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;

/// Keeps a spawned process off the console it would otherwise pop up.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// How many output lines to keep around for the failure message.
const TAIL_LINES: usize = 30;

/// What the reader threads report back to the UI.
pub enum Event {
    /// The server is listening on this URL.
    Ready(String),
    /// The server exited (or never printed a URL); carries its output tail.
    Failed(String),
}

pub struct Server {
    child: Child,
    /// Held for the lifetime of the app; see [`Job`].
    #[cfg(windows)]
    _job: Option<Job>,
}

/// Spawn `dsh web` and return the handle plus a channel that yields exactly one
/// [`Event`].
pub fn start(app: &tauri::AppHandle) -> std::io::Result<(Server, Receiver<Event>)> {
    let mut child = command(app)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(working_dir())
        .spawn()?;

    #[cfg(windows)]
    let job = Job::hold(&child);

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
    };
    Ok((server, rx))
}

impl Server {
    /// Stop the server.
    pub fn stop(&mut self) {
        kill_tree(&mut self.child);
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

    #[cfg(not(windows))]
    {
        let _ = child.kill();
    }

    let _ = child.wait();
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

/// The command that boots the browser UI, from the first of three sources that
/// this machine actually has:
///
/// 1. `DSH_BIN` — an explicit choice, so it wins outright.
/// 2. The dsh the app manages, in app data. Absent under `tauri dev`, and on an
///    install whose download failed.
/// 3. `dsh` on PATH, for a user who installed one themselves.
///
/// [`crate::dsh::installed`] walks the same three in the same order, so that the
/// update check is about the copy this starts.
fn command(app: &tauri::AppHandle) -> Command {
    let mut command = if let Some(bin) = std::env::var_os("DSH_BIN") {
        Command::new(bin)
    } else if let Some(dsh) = crate::dsh::current(app) {
        dsh.command()
    } else {
        Command::new(default_bin())
    };

    command.args(["web", "--port", "0"]);

    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    command
}

/// `dsh` ships as an npm shim: `dsh.cmd` on Windows, a shell script elsewhere.
/// Naming the extension is what lets std route the batch file through cmd.exe
/// with the correct argument quoting.
pub fn default_bin() -> &'static str {
    if cfg!(windows) {
        "dsh.cmd"
    } else {
        "dsh"
    }
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
/// without one means the server died before it could serve.
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

        if let (false, Some(tx)) = (ready, tx) {
            let _ = tx.send(Event::Failed(tail.lock().unwrap().join("\n")));
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
}
