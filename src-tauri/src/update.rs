//! Self-update against the release feed named in `tauri.conf.json`.
//!
//! Everything here is optional and never blocks the app: a check that fails is
//! a check that did not happen. The user is asked before anything is downloaded
//! and again before the app restarts, because a restart drops a session the
//! agent may be in the middle of.

use tauri::AppHandle;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_updater::{Update, UpdaterExt};

/// Check on startup: silent unless there is something to install.
#[cfg_attr(debug_assertions, allow(dead_code))] // dev builds never auto-check
pub fn check_quietly(app: &AppHandle) {
    check(app.clone(), false);
}

/// Check from the tray menu: reports the outcome either way, because a menu
/// item that does nothing visible looks broken.
pub fn check_now(app: &AppHandle) {
    check(app.clone(), true);
}

fn check(app: AppHandle, verbose: bool) {
    tauri::async_runtime::spawn(async move {
        let found = match app.updater() {
            Ok(updater) => updater.check().await,
            Err(error) => Err(error),
        };

        match found {
            Ok(Some(update)) => offer(&app, update),
            Ok(None) if verbose => note(
                &app,
                "已是最新版本",
                &format!("当前版本 {} 已经是最新的。", app.package_info().version),
            ),
            Ok(None) => {}
            Err(error) if verbose => note(&app, "检查更新失败", &error.to_string()),
            Err(error) => eprintln!("dsh-desktop: 检查更新失败：{error}"),
        }
    });
}

/// Ask before spending the user's bandwidth.
fn offer(app: &AppHandle, update: Update) {
    let app = app.clone();
    let version = update.version.clone();
    let notes = update.body.clone().unwrap_or_default();

    app.clone()
        .dialog()
        .message(&if notes.is_empty() {
            format!("发现新版本 {version}，是否现在下载并安装？")
        } else {
            format!("发现新版本 {version}，是否现在下载并安装？\n\n{notes}")
        })
        .title("有可用更新")
        .buttons(MessageDialogButtons::OkCancelCustom(
            "现在更新".into(),
            "稍后".into(),
        ))
        .show(move |accepted| {
            if accepted {
                install(app.clone(), update);
            }
        });
}

fn install(app: AppHandle, update: Update) {
    tauri::async_runtime::spawn(async move {
        if let Err(error) = update.download_and_install(|_, _| {}, || {}).await {
            note(&app, "更新失败", &error.to_string());
            return;
        }

        let restarting = app.clone();
        app.dialog()
            .message("新版本已安装，重启后生效。正在进行的会话会被中断。")
            .title("更新就绪")
            .buttons(MessageDialogButtons::OkCancelCustom(
                "立即重启".into(),
                "稍后".into(),
            ))
            .show(move |now| {
                if now {
                    // Skips the ordinary shutdown path, so the dsh process tree
                    // is left to the job object backstop in `server`.
                    restarting.restart();
                }
            });
    });
}

fn note(app: &AppHandle, title: &str, detail: &str) {
    app.dialog().message(detail).title(title).show(|_| {});
}
