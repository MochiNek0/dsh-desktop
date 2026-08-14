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
}

/// Spawn `dsh web` and return the handle plus a channel that yields exactly one
/// [`Event`].
pub fn start() -> std::io::Result<(Server, Receiver<Event>)> {
    let mut child = command()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(working_dir())
        .spawn()?;

    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");

    let (tx, rx) = channel();
    let tail = Arc::new(Mutex::new(Vec::new()));

    pump(stderr, tail.clone(), None);
    pump(stdout, tail, Some(tx));

    Ok((Server { child }, rx))
}

impl Server {
    /// Stop the server. On Windows the child is `cmd.exe` wrapping the `dsh.cmd`
    /// shim wrapping node, so the whole tree has to go, not just the parent.
    pub fn stop(&mut self) {
        let pid = self.child.id();

        #[cfg(windows)]
        {
            let _ = Command::new("taskkill")
                .args(["/T", "/F", "/PID", &pid.to_string()])
                .creation_flags(CREATE_NO_WINDOW)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }

        #[cfg(not(windows))]
        {
            let _ = pid;
            let _ = self.child.kill();
        }

        let _ = self.child.wait();
    }
}

/// The command that boots the browser UI. `DSH_BIN` overrides the executable
/// for installs where `dsh` is not on the GUI session's PATH.
fn command() -> Command {
    let bin = std::env::var("DSH_BIN").unwrap_or_else(|_| default_bin().to_string());
    let mut command = Command::new(bin);
    command.args(["web", "--port", "0"]);

    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    command
}

/// `dsh` ships as an npm shim: `dsh.cmd` on Windows, a shell script elsewhere.
/// Naming the extension is what lets std route the batch file through cmd.exe
/// with the correct argument quoting.
fn default_bin() -> &'static str {
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
