//! The profile that asks dsh to listen on every interface, and the one-click
//! way back.
//!
//! Newer dsh's webserver refuses wildcard binds (0.2.1-rc.2 does, 0.1.7-rc.2
//! did not) — `0.0.0.0` would
//! put an agent with a shell on the whole network, with no authentication of
//! its own in front of it. What still writes one is a third-party "LAN mode"
//! plugin or script: a `- id: webserver` row in `profiles/web/cordis.patch.yml`
//! with `host: '0.0.0.0'`, which a dsh older than that obeyed quietly and a
//! newer one answers by not starting at all.
//!
//! Nothing here runs unless that failure is on screen. The app does not police
//! the profile at launch: the file is the user's, and the plugin that wrote the
//! block may well rewrite it. The fix is offered as a button on the failure, it
//! touches the one `host:` line that caused it, and the file it replaces is
//! kept beside it. A phone that wants in goes through [`crate::remote`]'s
//! gateway, which is what that block was standing in for.
//!
//! ## Why a text edit rather than a YAML round trip
//!
//! The file is the user's and is full of things a parse and reserialize would
//! lose: comments, the third-party block's `managed - do not edit` markers,
//! `!!js` tags no YAML crate here knows. So the edit is the one value on the one
//! line, quotes and comment left where they were, and anything not shaped like
//! the common case is left for the user with the path to go to.

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use tauri::AppHandle;

/// Whether dsh's output is this failure. Both halves of the webserver's own
/// message, so a `--host` usage error — which this app never passes — and an
/// unrelated mention of either phrase do not count.
pub fn spotted(output: &str) -> bool {
    output.contains("webserver: host ") && output.contains("is an unspecified (wildcard) address")
}

/// What to say above dsh's own output when [`spotted`].
pub fn explain(app: &AppHandle) -> String {
    t!(
        "dsh 的配置要求监听所有网卡（0.0.0.0），新版 dsh 出于安全考虑拒绝这样启动。\
         这段配置通常是第三方“局域网访问”插件写进去的，在：\n{}\n\n\
         点“改为仅本机并重启”会把其中的 host 改成 127.0.0.1（原文件会先备份）。\
         手机访问请使用桌面端自带的远程访问。",
        "dsh's settings ask it to listen on every interface (0.0.0.0), which newer dsh \
         refuses to do for safety. A third-party \"LAN access\" plugin usually writes \
         that setting, in:\n{}\n\n\
         \"Bind to this machine and restart\" changes that host to 127.0.0.1, after \
         backing up the file. To reach dsh from a phone, use the desktop's remote access.",
        path(app).display()
    )
}

/// The profile's own patch layer, which is where such a block lives.
fn path(app: &AppHandle) -> PathBuf {
    crate::plugins::profile_dir(app).join("cordis.patch.yml")
}

/// Rewrite the wildcard host and say where the original went.
pub fn repair(app: &AppHandle) -> Result<PathBuf, String> {
    repair_at(&path(app))
}

fn repair_at(path: &Path) -> Result<PathBuf, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("{}: {error}", path.display()))?;

    let Some(fixed) = rewrite(&text) else {
        // A bundle's own layer, a flow-style mapping, an alias: dsh is still
        // right about the cause, this just cannot point at the line for sure.
        return Err(t!(
            "没在 {} 里找到 webserver 的通配 host。请手动把 webserver 的 host 改成 127.0.0.1；\
             如果这个文件里没有，可能是某个插件自带的配置，试试“不加载插件启动”。",
            "No wildcard webserver host was found in {}. Change the webserver's host to \
             127.0.0.1 by hand; if it is not in that file, a plugin's own layer may set it — \
             try \"Start without plugins\".",
            path.display()
        ));
    };

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let backup = path.with_extension(format!("yml.bak-{stamp}"));
    std::fs::copy(path, &backup).map_err(|error| format!("{}: {error}", backup.display()))?;

    // Through a sibling and a rename, as everywhere else a file dsh reads is
    // written: half a profile layer is a dsh that will not start for a new
    // reason.
    let sibling = path.with_extension("yml.new");
    std::fs::write(&sibling, fixed).map_err(|error| format!("{}: {error}", sibling.display()))?;
    std::fs::rename(&sibling, path).map_err(|error| {
        let _ = std::fs::remove_file(&sibling);
        format!("{}: {error}", path.display())
    })?;
    Ok(backup)
}

/// The same text with every wildcard `host:` in a webserver row turned into
/// loopback of the same family. `None` when there was nothing to change.
///
/// A row is a top-level list item: from a line that starts with `-` in the
/// first column to the next one. It is the webserver's when one of its lines
/// is `id: webserver` or names the package, which are the two ways a patch
/// entry can aim at it.
fn rewrite(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();

    let mut starts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with('-'))
        .map(|(at, _)| at)
        .collect();
    starts.push(lines.len());

    let mut out: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
    let mut changed = false;

    for row in starts.windows(2) {
        let range = row[0]..row[1];
        if !lines[range.clone()].iter().any(|line| aims_at_webserver(line)) {
            continue;
        }
        for at in range {
            if let Some(fixed) = loopback_host(lines[at]) {
                out[at] = fixed;
                changed = true;
            }
        }
    }

    changed.then(|| out.concat())
}

fn aims_at_webserver(line: &str) -> bool {
    let line = line.trim_end();
    let line = line.trim_start().trim_start_matches("- ").trim_start();
    let Some((key, value)) = line.split_once(':') else {
        return false;
    };
    let value = value.split(" #").next().unwrap_or("").trim().trim_matches(['\'', '"']);
    match key.trim() {
        "id" => value == "webserver",
        "name" => value == "@deepseek-ai/dsh-host-webserver",
        _ => false,
    }
}

/// `host: <wildcard>` with the address swapped for loopback, everything else
/// on the line — indent, quotes, a trailing comment, the line ending — kept.
fn loopback_host(line: &str) -> Option<String> {
    let indent = line.len() - line.trim_start().len();
    let rest = line[indent..].strip_prefix("host:")?;

    let start = indent + "host:".len() + (rest.len() - rest.trim_start().len());
    let value = &line[start..];
    let quote = value.chars().next().filter(|c| *c == '\'' || *c == '"');
    let body = &value[quote.map_or(0, char::len_utf8)..];
    let end = match quote {
        Some(quote) => body.find(quote)?,
        None => body
            .find(|c: char| c.is_whitespace() || c == '#')
            .unwrap_or(body.len()),
    };
    let address = &body[..end];

    let bracketed = address.starts_with('[') && address.ends_with(']');
    let bare = if bracketed { &address[1..address.len() - 1] } else { address };
    let loopback = match bare.parse::<IpAddr>().ok()? {
        IpAddr::V4(v4) if v4.is_unspecified() => "127.0.0.1",
        IpAddr::V6(v6) if v6.is_unspecified() => "::1",
        _ => return None,
    };
    let loopback = if bracketed {
        format!("[{loopback}]")
    } else {
        loopback.to_string()
    };

    let from = start + quote.map_or(0, char::len_utf8);
    Some(format!("{}{loopback}{}", &line[..from], &line[from + end..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The block the report came with, verbatim, CRLF and all: the editor in
    /// the screenshot said Unix (LF), but a Windows tool is as likely to write
    /// the other. `concat!` rather than `\` continuations, which would eat the
    /// indentation that makes it YAML.
    const REPORTED: &str = concat!(
        "# Your patch layer for this dsh profile, applied after every bundle layer:\r\n",
        "# --- remote-web-ui lan-bind block (managed - do not edit) ---\r\n",
        "- id: webserver\r\n",
        "  name: '@deepseek-ai/dsh-host-webserver'\r\n",
        "  config:\r\n",
        "    host: '0.0.0.0'\r\n",
        "    port: 3080\r\n",
        "    compression: gzip\r\n",
        "# --- end remote-web-ui lan-bind block ---\r\n",
    );

    #[test]
    fn recognises_the_webservers_refusal() {
        let output = "dsh: startup failed: 2 required plugins did not activate\n\
            Error: webserver: host \"0.0.0.0\" is an unspecified (wildcard) address, \
            which is not supported: binding every interface would expose remote code \
            execution to the network";
        assert!(spotted(output));
        assert!(!spotted("dsh: startup failed: 2 required plugins did not activate"));
        // The CLI's own usage error. The app never passes `--host`, and a
        // profile edit would not fix it.
        assert!(!spotted(
            "error: --host 0.0.0.0 is an unspecified (wildcard) address, which is not supported"
        ));
    }

    #[test]
    fn changes_only_the_host_of_the_reported_block() {
        let fixed = rewrite(REPORTED).unwrap();
        assert_eq!(fixed, REPORTED.replace("'0.0.0.0'", "'127.0.0.1'"));
    }

    #[test]
    fn keeps_quotes_comments_and_family() {
        let row = |host: &str| format!("- id: webserver\n  config:\n    host: {host}\n    port: 3080\n");
        let cases = [
            ("0.0.0.0", "127.0.0.1"),
            ("\"0.0.0.0\"  # LAN", "\"127.0.0.1\"  # LAN"),
            ("'::'", "'::1'"),
            ("'[::]'", "'[::1]'"),
            ("0:0:0:0:0:0:0:0", "::1"),
        ];
        for (from, to) in cases {
            assert_eq!(rewrite(&row(from)), Some(row(to)), "{from}");
        }
    }

    #[test]
    fn leaves_concrete_addresses_and_other_rows_alone() {
        // Not wildcards: dsh starts on these, and a LAN address there is a
        // choice the user made.
        assert_eq!(rewrite("- id: webserver\n  config:\n    host: '192.168.1.5'\n"), None);
        assert_eq!(rewrite("- id: webserver\n  config:\n    host: !!js ctx.webStartup.host\n"), None);
        // Some other plugin's `host`, before and after the webserver's.
        let text = "- id: proxy\n  config:\n    host: 0.0.0.0\n\
                    - name: '@deepseek-ai/dsh-host-webserver'\n  id: webserver\n  config:\n    host: 0.0.0.0\n\
                    - id: webserver-extra\n  config:\n    host: 0.0.0.0\n";
        let fixed = rewrite(text).unwrap();
        assert_eq!(fixed.matches("host: 0.0.0.0").count(), 2);
        assert_eq!(fixed.matches("host: 127.0.0.1").count(), 1);
        assert!(fixed.contains("id: webserver\n  config:\n    host: 127.0.0.1\n"));
    }

    #[test]
    fn an_empty_layer_has_nothing_to_fix() {
        assert_eq!(rewrite("# comment\n[]\n"), None);
    }

    /// The whole round against the `dsh` on the PATH, in a `DSH_HOME` of its
    /// own: the reported block stops it, [`spotted`] knows the output, the
    /// repair lets it serve. Slow and needs a dsh new enough to refuse
    /// wildcards: `cargo test -- --ignored against_a_real_dsh`.
    #[test]
    #[ignore]
    fn against_a_real_dsh() {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};

        let home = std::env::temp_dir().join(format!("dsh-desktop-real-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let dsh = if cfg!(windows) { "dsh.cmd" } else { "dsh" };

        // Lets dsh lay the profile out before the block goes into it.
        let laid = Command::new(dsh)
            .args(["--profile", "web", "--dump-config"])
            .env("DSH_HOME", &home)
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(laid.success());
        let layer = home.join("profiles").join("web").join("cordis.patch.yml");
        std::fs::write(&layer, REPORTED).unwrap();

        // Every line until it serves or exits; killed once it serves.
        let run = || {
            let mut child = Command::new(dsh)
                .args(["web", "--no-open", "--port", "0"])
                .env("DSH_HOME", &home)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let stderr = child.stderr.take().unwrap();
            let errors = std::thread::spawn(move || {
                std::io::read_to_string(stderr).unwrap_or_default()
            });
            let mut served = false;
            let mut out = String::new();
            for line in BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
                out.push_str(&line);
                out.push('\n');
                if line.contains("dsh web:") {
                    served = true;
                    break;
                }
            }
            if served {
                #[cfg(windows)]
                let _ = Command::new("taskkill")
                    .args(["/T", "/F", "/PID", &child.id().to_string()])
                    .status();
                let _ = child.kill();
            }
            let _ = child.wait();
            if !served {
                out.push_str(&errors.join().unwrap_or_default());
            }
            (served, out)
        };

        let (served, out) = run();
        assert!(!served, "dsh served on a wildcard host:\n{out}");
        assert!(spotted(&out), "not recognised:\n{out}");

        let backup = repair_at(&layer).unwrap();
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), REPORTED);

        let (served, out) = run();
        assert!(served, "still not serving after the repair:\n{out}");
        assert!(out.contains("http://127.0.0.1:"), "{out}");

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn repair_backs_up_then_rewrites() {
        let dir = std::env::temp_dir().join(format!("dsh-desktop-wildcard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cordis.patch.yml");
        std::fs::write(&path, REPORTED).unwrap();

        let backup = repair_at(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), REPORTED);
        assert!(std::fs::read_to_string(&path).unwrap().contains("host: '127.0.0.1'"));
        assert!(!path.with_extension("yml.new").exists());

        // Already fixed: nothing to do, and nothing written.
        assert!(repair_at(&path).is_err());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
