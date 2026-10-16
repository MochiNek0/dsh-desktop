//! The clipboard into dsh: a key pressed anywhere brings the window up with the
//! text on the clipboard added to the composer of the session on screen.
//!
//! Added, not sent, and after whatever was already being typed: an error copied
//! out of a terminal usually wants a sentence in front of it, and the user is
//! the one to write that. The composer is the plugin's to reach — see `draft`
//! in `plugin/lib/client.js` — so this module's part is reading the clipboard.
//!
//! Text only. An image on the clipboard would have to go through dsh's own
//! attachment intake, which is not something a plugin can call from outside;
//! one is left alone, and the window still comes up.
//!
//! Read by the platform's own means rather than through a clipboard crate,
//! which would be a dependency for one string: the Win32 clipboard on Windows,
//! `pbpaste` on macOS, and on Linux whichever of `wl-paste`, `xclip` and
//! `xsel` is installed — a desktop without any of them gets the window and no
//! text.

use tauri::AppHandle;

/// The longest clipboard text taken. A log pasted whole is still a paste the
/// user meant; this only bounds one that is not text anyone typed.
const LIMIT: usize = 100_000;

/// The key was pressed.
pub fn run(app: &AppHandle) {
    let text = read().map(|text| clamp(text.trim_end()));
    crate::reveal(app);
    match text {
        Some(text) if !text.is_empty() => crate::signal::draft(app, "", &text),
        _ => eprintln!("dsh-desktop: nothing on the clipboard to paste"),
    }
}

fn clamp(text: &str) -> String {
    match text.char_indices().nth(LIMIT) {
        Some((cut, _)) => text[..cut].to_string(),
        None => text.to_string(),
    }
}

#[cfg(windows)]
fn read() -> Option<String> {
    use windows::Win32::Foundation::{HANDLE, HGLOBAL};
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    use windows::Win32::System::Ole::CF_UNICODETEXT;

    // Another program can be holding the clipboard for a moment — the one
    // that just copied, most likely — so a few tries rather than one.
    let mut opened = false;
    for _ in 0..5 {
        if unsafe { OpenClipboard(None) }.is_ok() {
            opened = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    if !opened {
        return None;
    }

    let text = unsafe {
        GetClipboardData(CF_UNICODETEXT.0 as u32).ok().and_then(|data: HANDLE| {
            let memory = HGLOBAL(data.0);
            let start = GlobalLock(memory) as *const u16;
            if start.is_null() {
                return None;
            }
            // Bounded by the allocation as well as by the terminator, which a
            // well-behaved program writes and nothing enforces.
            let units = GlobalSize(memory) / 2;
            let slice = std::slice::from_raw_parts(start, units);
            let end = slice.iter().position(|&unit| unit == 0).unwrap_or(units);
            let text = String::from_utf16_lossy(&slice[..end]);
            let _ = GlobalUnlock(memory);
            Some(text)
        })
    };
    let _ = unsafe { CloseClipboard() };
    text
}

#[cfg(unix)]
fn read() -> Option<String> {
    use std::process::{Command, Stdio};

    #[cfg(target_os = "macos")]
    let tries: &[(&str, &[&str])] = &[("pbpaste", &[])];
    #[cfg(not(target_os = "macos"))]
    let tries: &[(&str, &[&str])] = &[
        ("wl-paste", &["--no-newline", "--type", "text"]),
        ("xclip", &["-selection", "clipboard", "-o"]),
        ("xsel", &["--clipboard", "--output"]),
    ];

    tries.iter().find_map(|(program, args)| {
        let output = Command::new(program)
            .args(*args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn clamps_on_a_character_boundary() {
        let long = "错".repeat(super::LIMIT + 10);
        assert_eq!(super::clamp(&long).chars().count(), super::LIMIT);
        assert_eq!(super::clamp("short"), "short");
    }

    /// Put text on the real clipboard and read it back. Ignored, because it
    /// replaces whatever the user had copied: run it with
    /// `cargo test -- --ignored reads_the_real_clipboard`.
    #[test]
    #[ignore]
    #[cfg(windows)]
    fn reads_the_real_clipboard() {
        let status = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", "Set-Clipboard -Value 'dsh 剪贴板 probe'"])
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(super::read().as_deref().map(str::trim_end), Some("dsh 剪贴板 probe"));
    }
}
