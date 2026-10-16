//! Keeping the machine awake while a turn runs.
//!
//! A turn can take the better part of an hour, and the reason this app lives in
//! the tray is so that it can run while nobody is looking. Nobody looking is
//! also exactly when the system decides it is idle and goes to sleep, which
//! stops `dsh web`, the model request it was waiting on, and the turn. So while
//! any session is mid-turn this asks the system not to sleep on its own, and
//! stops asking the moment none is.
//!
//! What it holds off is *idle* sleep and nothing else. The screen still turns
//! off, a closed lid or the Start menu's "Sleep" still sleeps, and so does a
//! battery running out: each of those is the user's own decision, or the
//! hardware's, and overriding one is not what a convenience gets to do.
//!
//! The count is the plugin's — the same `running` the tray's dot is drawn from,
//! see [`crate::status`]. A forged count from the page is a machine that stays
//! awake for as long as the page keeps saying so, which the page could already
//! do by playing a video.
//!
//! ## Three platforms, three mechanisms
//!
//! - **Windows**: `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED)`.
//!   The request belongs to the thread that made it and lapses when that thread
//!   ends, so it is made and withdrawn on the main thread, which outlives
//!   everything here. A process that dies loses it with the thread.
//! - **macOS**: `caffeinate -i -w <our pid>`, a child that holds the assertion
//!   for as long as it runs. `-w` makes it end with this process however this
//!   process ends.
//! - **Linux**: `systemd-inhibit --what=idle:sleep … sleep infinity`, the same
//!   kind of child, tied to this process by [`crate::server::tethered`]. A
//!   machine without systemd has no `systemd-inhibit`, and then the spawn fails
//!   and nothing is held; said on stderr.

use std::sync::Mutex;

use tauri::AppHandle;

/// Whether a hold is in place, and on the two platforms that need one, the
/// child that is the hold.
static HELD: Mutex<Option<Hold>> = Mutex::new(None);

/// What the plugin last said: is anything mid-turn. Kept so that turning the
/// setting on in the middle of a turn takes effect at once rather than at the
/// next count.
static RUNNING: Mutex<bool> = Mutex::new(false);

/// The latest count from the plugin.
pub fn running(app: &AppHandle, count: u32) {
    if let Ok(mut running) = RUNNING.lock() {
        *running = count > 0;
    }
    apply(app);
}

/// The setting moved; hold or let go to match.
pub fn apply(app: &AppHandle) {
    let wanted = RUNNING.lock().map(|running| *running).unwrap_or(false)
        && crate::settings::keep_awake(app);

    let Ok(mut held) = HELD.lock() else { return };
    if held.is_some() == wanted {
        return;
    }
    *held = if wanted { hold(app) } else { None };
}

#[cfg(windows)]
struct Hold {
    app: AppHandle,
}

#[cfg(windows)]
fn hold(app: &AppHandle) -> Option<Hold> {
    use windows_sys::Win32::System::Power::{
        SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED,
    };

    // On the main thread whatever thread this is called from: the request is
    // that thread's, and the main thread is the one that lives as long as the
    // process does.
    let _ = app.run_on_main_thread(|| unsafe {
        if SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) == 0 {
            eprintln!("dsh-desktop: the system would not stay awake for the turn");
        }
    });
    Some(Hold { app: app.clone() })
}

#[cfg(windows)]
impl Drop for Hold {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS};

        // `ES_CONTINUOUS` alone clears what the thread asked for before.
        let _ = self.app.run_on_main_thread(|| unsafe {
            SetThreadExecutionState(ES_CONTINUOUS);
        });
    }
}

#[cfg(unix)]
struct Hold {
    child: std::process::Child,
}

#[cfg(unix)]
fn hold(_app: &AppHandle) -> Option<Hold> {
    use std::process::{Command, Stdio};

    #[cfg(target_os = "macos")]
    let command = {
        let mut command = Command::new("/usr/bin/caffeinate");
        command.args(["-i", "-w", &std::process::id().to_string()]);
        command
    };

    #[cfg(not(target_os = "macos"))]
    let command = {
        let mut command = Command::new("systemd-inhibit");
        command.args([
            "--what=idle:sleep",
            "--who=dsh desktop",
            "--why=A dsh turn is running",
            "--mode=block",
            "sleep",
            "infinity",
        ]);
        command
    };

    let mut command = command;
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());

    match crate::server::tethered(command) {
        Ok(child) => Some(Hold { child }),
        Err(error) => {
            eprintln!("dsh-desktop: could not keep the system awake for the turn: {error}");
            None
        }
    }
}

#[cfg(unix)]
impl Drop for Hold {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
