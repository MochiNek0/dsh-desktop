//! The environment the user's own terminal would have, for the children this app
//! starts.
//!
//! A `.app` opened from Finder or the Dock gets launchd's environment, and a
//! Linux one started from a desktop launcher gets the session manager's: neither
//! has read `~/.zprofile`, `~/.zshrc` or their equivalents. So `dsh web`, and
//! every agent shell and terminal it opens, had a PATH without
//! `/opt/homebrew/bin` and none of the `HTTPS_PROXY`s and API keys the user
//! exports there — a tool that works in their terminal was `command not found`
//! to the agent.
//!
//! The fix is to ask: run the account's login shell once as `<shell> -ilc` and
//! read `env -0` back out of it, between delimiters so whatever the rc files
//! print around it is ignored. Started from the Tauri setup hook on a thread of
//! its own, so it overlaps with the window coming up and the boot finding dsh;
//! the first child that needs it waits for whatever is left.
//!
//! What it does not do:
//!
//! - Windows. A GUI launch there already inherits the user's and the system's
//!   variables from the registry, so there is nothing to read.
//! - `DSH_*`. This app reads `DSH_HOME` and `DSH_BIN` off its own environment;
//!   handing the children different values from the shell would have the app
//!   and its dsh disagree about where the sessions are.
//! - Update. It is read once per launch, so an edit to `~/.zshrc` reaches dsh
//!   after the app is quit and opened again.
//!
//! A shell that fails to start, exits non-zero, prints no delimited output or
//! runs past [`TIMEOUT`] is logged and the next candidate tried; when all of
//! them fail the children get this process's own environment, which is what
//! they had before this module existed.

use std::process::Command;

/// What the dump is fenced with. Nothing an rc file prints will contain it.
#[cfg(any(unix, test))]
const DELIMITER: &[u8] = b"_DSH_SHELL_ENV_DELIMITER_";

/// Run by the shell after its startup files. POSIX syntax, so an account shell
/// such as fish or nushell fails it and the next candidate answers instead.
#[cfg(unix)]
const DUMP: &str = "printf '\\0%s\\0' _DSH_SHELL_ENV_DELIMITER_; command env -0 || exit; \
                    printf '\\0%s\\0' _DSH_SHELL_ENV_DELIMITER_; exit";

/// Set on the probe so that oh-my-zsh's update prompt and tmux-autostart
/// plugins do not wait on a terminal that is not there.
#[cfg(any(unix, test))]
const PROBE: &[(&str, &str)] = &[
    ("DISABLE_AUTO_UPDATE", "true"),
    ("ZSH_TMUX_AUTOSTARTED", "true"),
    ("ZSH_TMUX_AUTOSTART", "false"),
];

/// How long each candidate shell gets. A shell that is slower than this to
/// start is slower than anyone waits for a window.
#[cfg(unix)]
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// One variable as the shell printed it: name, then value.
#[cfg(any(unix, test))]
type Pair<'a> = (&'a [u8], &'a [u8]);

#[cfg(unix)]
type Vars = Vec<(std::ffi::OsString, std::ffi::OsString)>;

/// Start reading the login shell's environment in the background. Called once,
/// from the setup hook — after the single-instance check, so a second launch
/// that is about to be turned away does not run one.
pub fn start() {
    #[cfg(unix)]
    std::thread::spawn(|| {
        let _ = vars();
    });
}

/// The login shell's variables, waiting for the read if it is still running.
/// `None` on Windows, and when no shell answered.
#[cfg(unix)]
fn vars() -> Option<&'static Vars> {
    static READ: std::sync::OnceLock<Option<Vars>> = std::sync::OnceLock::new();
    READ.get_or_init(probe::read).as_ref()
}

/// Give a child the login shell's variables. They replace what it would have
/// inherited; a caller that sets something of its own — `apply_path`'s PATH —
/// does it afterwards.
pub fn apply(command: &mut Command) {
    #[cfg(unix)]
    if let Some(vars) = vars() {
        command.envs(vars.iter().map(|(name, value)| (name, value)));
    }
    #[cfg(not(unix))]
    let _ = command;
}

/// The PATH the user's terminal has: the login shell's, or this process's own
/// when there is none.
pub fn path() -> std::ffi::OsString {
    #[cfg(unix)]
    if let Some((_, value)) = vars()
        .into_iter()
        .flatten()
        .find(|(name, _)| name == "PATH")
    {
        return value.clone();
    }

    std::env::var_os("PATH").unwrap_or_default()
}

/// The variables printed between the two delimiters, minus the ones that
/// describe the probe rather than the user — see [`kept`]. `None` when both
/// delimiters are not there, which is a shell that never reached the dump.
#[cfg(any(unix, test))]
fn parse(stdout: &[u8]) -> Option<Vec<Pair<'_>>> {
    let parts: Vec<&[u8]> = stdout.split(|&byte| byte == 0).collect();
    let first = parts.iter().position(|part| *part == DELIMITER)?;
    let last = parts.iter().rposition(|part| *part == DELIMITER)?;
    if first == last {
        return None;
    }

    Some(
        parts[first + 1..last]
            .iter()
            .filter_map(|entry| {
                let equals = entry.iter().position(|&byte| byte == b'=')?;
                (equals > 0).then(|| (&entry[..equals], &entry[equals + 1..]))
            })
            .filter(|(name, _)| kept(name))
            .collect(),
    )
}

/// Whether a variable the shell printed is the user's to pass on. Not the
/// probe's own session (`PWD`, `SHLVL`, …), not what [`PROBE`] set, and not
/// `DSH_*`, which this app has already read from its own environment.
#[cfg(any(unix, test))]
fn kept(name: &[u8]) -> bool {
    !matches!(name, b"PWD" | b"OLDPWD" | b"SHLVL" | b"_")
        && !PROBE.iter().any(|(probe, _)| probe.as_bytes() == name)
        && !name.starts_with(b"DSH_")
}

/// How many complete delimiters `out` holds. Two is the end of the dump.
#[cfg(unix)]
fn delimiters(out: &[u8]) -> usize {
    let marker: Vec<u8> = [&[0][..], DELIMITER, &[0][..]].concat();
    out.windows(marker.len())
        .filter(|window| *window == marker.as_slice())
        .count()
}

#[cfg(unix)]
mod probe {
    use super::{Vars, DUMP, PROBE, TIMEOUT};
    use std::ffi::{CStr, OsString};
    use std::io::Read;
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;

    /// Try each candidate in turn; the first that answers wins.
    pub fn read() -> Option<Vars> {
        for shell in candidates() {
            match one(&shell) {
                Ok(vars) => {
                    eprintln!(
                        "[login shell] {} gave {} variables",
                        shell.display(),
                        vars.len()
                    );
                    return Some(vars);
                }
                Err(reason) => eprintln!("[login shell] {}: {reason}", shell.display()),
            }
        }

        eprintln!("[login shell] none answered; children keep this process's environment");
        None
    }

    /// The account's login shell from the user database, then the fixed system
    /// shells. Not `$SHELL`: that is whatever launched this process said, and
    /// the account record is what a terminal actually starts.
    fn candidates() -> Vec<PathBuf> {
        let mut shells: Vec<PathBuf> = account_shell().into_iter().collect();
        for fixed in ["/bin/zsh", "/bin/bash", "/bin/sh"] {
            let fixed = PathBuf::from(fixed);
            if !shells.contains(&fixed) {
                shells.push(fixed);
            }
        }
        shells
    }

    fn account_shell() -> Option<PathBuf> {
        let mut record: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        let mut buffer = vec![0 as libc::c_char; 16 * 1024];

        // SAFETY: `getpwuid_r` writes only into `record` and `buffer`, both of
        // which outlive the call and are sized as passed; `found` is either
        // null or points at `record`.
        let status = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                &mut record,
                buffer.as_mut_ptr(),
                buffer.len(),
                &mut found,
            )
        };
        if status != 0 || found.is_null() || record.pw_shell.is_null() {
            return None;
        }

        // SAFETY: non-null, and NUL-terminated inside `buffer` by `getpwuid_r`.
        let shell = unsafe { CStr::from_ptr(record.pw_shell) };
        let shell = std::ffi::OsStr::from_bytes(shell.to_bytes());
        (!shell.is_empty()).then(|| PathBuf::from(shell))
    }

    fn one(shell: &std::path::Path) -> Result<Vars, String> {
        let mut command = Command::new(shell);
        command
            .args(["-ilc", DUMP])
            .current_dir(std::env::var_os("HOME").unwrap_or_else(|| "/".into()))
            .envs(PROBE.iter().copied())
            // Closed stdin: a prompt in an rc file reads EOF instead of waiting.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // The shell is the host's, so it must not load the AppImage's libraries.
        crate::dsh::unbundle(&mut command);
        // A group of its own, so a timeout takes whatever the rc files started.
        crate::server::group_leader(&mut command);

        let mut child = command.spawn().map_err(|error| error.to_string())?;
        let mut stdout = child.stdout.take().expect("stdout was piped");

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut out = Vec::new();
            let mut chunk = [0u8; 8192];
            // Done at the closing delimiter rather than at EOF: something an rc
            // file started in the background inherits stdout and can hold it
            // open long after the shell itself has exited.
            while super::delimiters(&out) < 2 {
                match stdout.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => out.extend_from_slice(&chunk[..read]),
                }
            }
            let _ = tx.send(out);
            // Keep draining, so that background child does not die of EPIPE.
            let _ = std::io::copy(&mut stdout, &mut std::io::sink());
        });

        let Ok(out) = rx.recv_timeout(TIMEOUT) else {
            crate::server::kill_tree(&mut child);
            return Err(format!("no answer within {}s", TIMEOUT.as_secs()));
        };

        let Some(vars) = super::parse(&out) else {
            return Err(match child.try_wait() {
                Ok(Some(status)) => status.to_string(),
                _ => {
                    crate::server::kill_tree(&mut child);
                    "no environment in its output".to_string()
                }
            });
        };

        // Reaped off this thread: after the dump the shell only has `exit` left,
        // but a login shell runs its logout file on the way out.
        std::thread::spawn(move || {
            let _ = child.wait();
        });

        Ok(vars
            .into_iter()
            .map(|(name, value)| {
                (
                    OsString::from(std::ffi::OsStr::from_bytes(name)),
                    OsString::from(std::ffi::OsStr::from_bytes(value)),
                )
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dump(entries: &[&str]) -> Vec<u8> {
        let mut out = b"Welcome to zsh\n".to_vec();
        out.extend_from_slice(b"\0_DSH_SHELL_ENV_DELIMITER_\0");
        for entry in entries {
            out.extend_from_slice(entry.as_bytes());
            out.push(0);
        }
        out.extend_from_slice(b"\0_DSH_SHELL_ENV_DELIMITER_\0");
        out.extend_from_slice(b"bye\n");
        out
    }

    #[test]
    fn reads_what_is_between_the_delimiters() {
        let out = dump(&[
            "PATH=/opt/homebrew/bin:/usr/bin",
            "HTTPS_PROXY=http://x:1",
            "EQ=a=b",
        ]);
        let vars = parse(&out).unwrap();
        assert_eq!(
            vars,
            vec![
                (&b"PATH"[..], &b"/opt/homebrew/bin:/usr/bin"[..]),
                (&b"HTTPS_PROXY"[..], &b"http://x:1"[..]),
                (&b"EQ"[..], &b"a=b"[..]),
            ]
        );
    }

    #[test]
    fn keeps_newlines_inside_a_value() {
        let out = dump(&["MULTI=one\ntwo"]);
        assert_eq!(
            parse(&out).unwrap(),
            vec![(&b"MULTI"[..], &b"one\ntwo"[..])]
        );
    }

    #[test]
    fn drops_the_probes_own_session_and_dsh_variables() {
        let out = dump(&[
            "PWD=/Users/x",
            "OLDPWD=/",
            "SHLVL=1",
            "_=/usr/bin/env",
            "DISABLE_AUTO_UPDATE=true",
            "ZSH_TMUX_AUTOSTART=false",
            "DSH_HOME=/elsewhere",
            "LANG=zh_CN.UTF-8",
        ]);
        assert_eq!(
            parse(&out).unwrap(),
            vec![(&b"LANG"[..], &b"zh_CN.UTF-8"[..])]
        );
    }

    #[test]
    fn an_unfinished_dump_is_no_answer() {
        let mut out = b"\0_DSH_SHELL_ENV_DELIMITER_\0PATH=/usr/bin\0".to_vec();
        assert_eq!(parse(&out), None);
        out.clear();
        out.extend_from_slice(b"zsh: command not found: env\n");
        assert_eq!(parse(&out), None);
    }
}
