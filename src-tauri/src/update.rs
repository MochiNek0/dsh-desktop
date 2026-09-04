//! Self-update against the release feed named in `tauri.conf.json`.
//!
//! Everything here is optional and never blocks the app: a check that fails is
//! a check that did not happen. The user is asked before anything is downloaded,
//! and — everywhere but Windows, where the installer takes the decision out of
//! our hands — again before the app restarts, because a restart drops a session
//! the agent may be in the middle of.
//!
//! The download itself reports into the titlebar's status line rather than onto
//! the loading page: the user is on dsh's own page when they ask for this, and
//! navigating away from it to draw a progress bar would drop the session the
//! restart is being so careful about.

use std::time::{Duration, Instant};

use tauri::AppHandle;
use tauri_plugin_updater::{Update, UpdaterExt};

/// How often the download may repaint the status line. Every chunk would be a
/// `window.eval` every few milliseconds.
const REPORT_EVERY: Duration = Duration::from_millis(250);

/// How long the check may wait on the release feed before giving up. Without one
/// a network that is refusing connections slowly rather than quickly leaves the
/// status line saying "checking" for as long as the app is open. It caps the
/// check alone: the plugin hands the `Update` its own timeout of `None`, so a
/// download that takes longer than this is not cut off.
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);

/// Check on startup: silent unless there is something to install.
#[cfg_attr(debug_assertions, allow(dead_code))] // dev builds never auto-check
pub fn check_quietly(app: &AppHandle) {
    check(app.clone(), false);
}

/// Check from the menu: reports the outcome either way, because a menu item that
/// does nothing visible looks broken — including while it is still working, which
/// is a request to a release feed and takes as long as that takes.
pub fn check_now(app: &AppHandle) {
    crate::controls::busy(app, t!("正在检查应用更新…", "Checking for app updates…"));
    check(app.clone(), true);
}

fn check(app: AppHandle, verbose: bool) {
    tauri::async_runtime::spawn(async move {
        let found = match app.updater_builder().timeout(CHECK_TIMEOUT).build() {
            Ok(updater) => updater.check().await,
            Err(error) => Err(error),
        };
        // Only the one `check_now` put up: the quiet check runs behind a boot,
        // and taking the line down would take down whatever *that* is saying.
        if verbose {
            crate::controls::busy(&app, "");
        }

        match found {
            Ok(Some(update)) => offer(&app, update),
            Ok(None) if verbose => note(
                &app,
                t!("已是最新版本", "Up to date"),
                &t!(
                    "当前版本 {} 已经是最新的。",
                    "Version {} is the latest there is.",
                    app.package_info().version
                ),
            ),
            Ok(None) => {}
            Err(error) if verbose => note(
                &app,
                t!("检查更新失败", "Update check failed"),
                &explain(&error),
            ),
            Err(error) => eprintln!("dsh-desktop: the update check failed: {error}"),
        }
    });
}

/// Ask before spending the user's bandwidth.
fn offer(app: &AppHandle, update: Update) {
    let version = update.version.clone();
    let notes = update.body.clone().unwrap_or_default();

    let body = if notes.is_empty() {
        t!(
            "发现新版本 {}，是否现在下载并安装？",
            "Version {} is available. Download and install it now?",
            version
        )
    } else {
        t!(
            "发现新版本 {}，是否现在下载并安装？\n\n{}",
            "Version {} is available. Download and install it now?\n\n{}",
            version,
            notes
        )
    };

    // The update is not `Clone` and the callback is a `Box<dyn FnOnce>` that
    // has to be `Send`; a mutex is how it is carried into one and taken out
    // again exactly once.
    let carried = std::sync::Mutex::new(Some(update));

    crate::dialog::ask(
        app,
        crate::dialog::Ask {
            title: t!("有可用更新", "Update available").to_string(),
            body,
            choices: vec![
                crate::dialog::Choice::new("later", t!("稍后", "Later")),
                crate::dialog::Choice::primary("now", t!("现在更新", "Update now")),
            ],
            answered: Box::new(move |app, id| {
                if id != "now" {
                    return;
                }
                let Some(update) = carried.lock().ok().and_then(|mut held| held.take()) else {
                    return;
                };
                install(app.clone(), update);
            }),
        },
    );
}

fn install(app: AppHandle, update: Update) {
    tauri::async_runtime::spawn(async move {
        // Before the first chunk, which is a request to a release feed away: the
        // dialog has just closed, and until something arrives there is nothing on
        // screen to say the answer was heard.
        crate::controls::busy(&app, t!("正在下载新版本…", "Downloading the update…"));

        let downloading = app.clone();
        let installing = app.clone();

        // How much has come down, and when it was last said out loud. Both live
        // in the closure, which the updater calls once per chunk.
        let mut done: u64 = 0;
        let mut said: Option<Instant> = None;

        let finished = update
            .download_and_install(
                move |chunk, total| {
                    done += chunk as u64;
                    if said.is_some_and(|at| at.elapsed() < REPORT_EVERY) {
                        return;
                    }
                    said = Some(Instant::now());
                    crate::controls::busy(&downloading, &downloaded(done, total));
                },
                move || {
                    // On Windows this is the last thing the app says: `install`
                    // hands the installer to ShellExecute and ends the process,
                    // and the installer draws its own progress from there.
                    crate::controls::busy(
                        &installing,
                        t!(
                            "正在安装新版本，应用即将重启…",
                            "Installing the update; the app is about to restart…"
                        ),
                    );
                },
            )
            .await;

        // An `else` rather than an early return: on Windows the block below is
        // compiled out, which would leave the `return` as the last statement in
        // the function — redundant, and clippy is right to say so.
        if let Err(error) = finished {
            crate::controls::busy(&app, "");
            note(&app, t!("更新失败", "Update failed"), &explain(&error));
        } else {
            // Windows never reaches this: `Update::install` there ends in
            // `std::process::exit(0)` after starting the NSIS installer, which
            // is passed `/P /R` and so restarts the app itself. Everywhere else
            // the install returns and the restart is ours to ask about.
            #[cfg(not(windows))]
            {
                crate::controls::busy(&app, "");

                crate::dialog::ask(
                    &app,
                    crate::dialog::Ask {
                        title: t!("更新就绪", "Update ready").to_string(),
                        body: t!(
                            "新版本已安装，重启后生效。正在进行的会话会被中断。",
                            "The update is installed and takes effect on restart. \
                             A session in progress will be interrupted."
                        )
                        .to_string(),
                        choices: vec![
                            crate::dialog::Choice::new("later", t!("稍后", "Later")),
                            crate::dialog::Choice::primary("now", t!("立即重启", "Restart now")),
                        ],
                        answered: Box::new(|app, id| {
                            if id == "now" {
                                // Skips the ordinary shutdown path, so the dsh
                                // process tree is left to the job object
                                // backstop in `server`.
                                app.restart();
                            }
                        }),
                    },
                );
            }
        }
    });
}

/// What the status line says while the new version comes down. The total is what
/// the server declared, which it need not have.
fn downloaded(done: u64, total: Option<u64>) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let megabytes = done as f64 / MB;

    match total {
        Some(total) if total > 0 => t!(
            "正在下载新版本 {:.1} MB / {:.1} MB（{:.0}%）",
            "Downloading the update: {:.1} MB / {:.1} MB ({:.0}%)",
            megabytes,
            total as f64 / MB,
            done as f64 / total as f64 * 100.0
        ),
        _ => t!(
            "正在下载新版本 {:.1} MB",
            "Downloading the update: {:.1} MB",
            megabytes
        ),
    }
}

/// What to say when the network is the thing that failed.
///
/// `reqwest`'s own words are "error sending request for url (…)" with the
/// endpoint spelled out: true, and no help to read. The endpoint is not
/// something the user picked, and what broke is between them and it rather than
/// in it. `ReleaseNotFound` means the same thing from the outside — it is what
/// the plugin returns once every endpoint has answered with something that was
/// not a feed. Anything else is a fault in the release itself, an unreadable
/// signature or a platform missing from the feed, and goes through verbatim:
/// that is the kind of thing a bug report needs quoted.
fn explain(error: &tauri_plugin_updater::Error) -> String {
    use tauri_plugin_updater::Error;

    match error {
        Error::Reqwest(_) | Error::Network(_) | Error::ReleaseNotFound => t!(
            "无法连接到更新服务器。请检查网络后重试，或前往 dsh-desktop.cc.cd 下载最新版本。",
            "Could not reach the update server. Check your connection and try again, \
             or download the latest version from dsh-desktop.cc.cd."
        )
        .to_string(),
        other => other.to_string(),
    }
}

/// Say something that needs no answer. One button, because the only thing to do
/// about "up to date" or "the check failed" is to stop reading it.
fn note(app: &AppHandle, title: &str, detail: &str) {
    crate::dialog::ask(
        app,
        crate::dialog::Ask {
            title: title.to_string(),
            body: detail.to_string(),
            choices: vec![crate::dialog::Choice::primary("ok", t!("知道了", "OK"))],
            answered: Box::new(|_, _| {}),
        },
    );
}
