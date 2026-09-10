//! Raising the toast, and hearing the click.
//!
//! [`crate::notify`] decides *whether* a notification is raised; this raises it.
//! That was `tauri-plugin-notification`'s job until now, and it was given up for
//! one reason. The plugin's desktop `show()` is, in full:
//!
//! ```ignore
//! tauri::async_runtime::spawn(async move { let _ = notification.show(); });
//! ```
//!
//! Both halves of what that call returns are dropped on the floor:
//!
//! - **The error.** So "this machine has no notification daemon" and "the toast
//!   is on screen" are the same `Ok(())`, and the one failure mode a user
//!   actually reports is the one nothing can be logged about.
//! - **The handle.** Which is the only thing an activation ever arrives on. A
//!   click on a toast could not reach this app because nothing was holding the
//!   object it would have arrived at.
//!
//! Neither is reachable from above the plugin, so the plugin is gone and its own
//! single dependency, `notify-rust`, is called here directly. Nothing else about
//! the path changed: the same crate raises the same toast through the same three
//! platform backends, under the same identity rules the plugin applied — see
//! [`identify`], which is a transcription of what it did.
//!
//! ## One call, three backends
//!
//! `notify_rust::Notification::show()` returns `Result<NotificationHandle>` on
//! all three desktop platforms, and every one of those handles has
//! `wait_for_response`, so the interesting part of this module is one code path
//! rather than three. Two platform facts do leak through:
//!
//! - **macOS shows nothing on `show()`.** Its `show()` only wraps the
//!   notification in a handle; the send happens either inside
//!   `wait_for_response` (synchronously, on `NSUserNotificationCenter`, which
//!   wants the main run loop) or in the handle's `Drop` (asynchronously,
//!   discarding the error). This app takes the second, which is exactly what the
//!   plugin's dropped handle was already doing — so macOS keeps today's
//!   behaviour, toast and all, and is the one platform where a click is still
//!   not heard. Waiting there would mean blocking a thread inside AppKit from
//!   off the main thread, untested, for a convenience.
//! - **Linux needs the body click declared.** Under XDG a click on the
//!   notification body is only delivered if the notification registered an
//!   action named `default`, so [`raise`] adds one there and nowhere else. On
//!   Windows the same call would draw a literal button next to a body that is
//!   already clickable.
//!
//! ## What a click reaches
//!
//! The window, through [`crate::reveal`] — the same function the tray icon and a
//! second launch of the app go through. And then, for a notification that named
//! a session, that session: [`crate::signal::open`] asks the plugin to select
//! it, so a click on "dsh has a question for you" lands on the session that
//! asked rather than on whichever one happened to be in front. A notification
//! the page raised names no session and stops at the window.
//!
//! The page's own `Notification.onclick` is still never fired; see
//! [`crate::notify`] for why the shim keeps it and leaves it alone.
//!
//! It is a click on the toast **while it is on screen**. Once the popup has
//! timed out, Windows and most Linux daemons keep a copy in a notification
//! centre, and a click on that copy is not delivered to a running process — on
//! Windows it wants a registered COM activator, which is an installer-level
//! thing this app has no other use for. So the wait below ends when the popup
//! does, and the thread ends with it.

use std::sync::atomic::{AtomicUsize, Ordering};
use tauri::AppHandle;

/// How many toasts may be waiting to be clicked at once.
///
/// Each one holds a thread — the wait is blocking on every backend — so this is
/// what stops a run of finished turns nobody is looking at from being a run of
/// threads. Past the limit the toast is still raised and simply not listened to,
/// which is the right way round: the notification is the part the user asked
/// for.
const LISTENERS: usize = 8;

/// How many of those threads are waiting right now.
static LISTENING: AtomicUsize = AtomicUsize::new(0);

/// Put one on screen. Returns immediately; the toast outlives the call.
///
/// On its own thread because two of the three backends do real work in `show()`
/// — a D-Bus round trip under XDG, COM activation on Windows — and because the
/// wait that follows is blocking by construction. `session` rides along to the
/// click; see the module docs.
pub fn raise(app: &AppHandle, title: String, body: String, session: Option<String>) {
    let app = app.clone();

    std::thread::spawn(move || {
        let mut notification = notify_rust::Notification::new();
        notification.summary(&title);
        if !body.is_empty() {
            notification.body(&body);
        }
        // The plugin called this whenever no icon was set, which was always.
        // Under XDG it is what makes the toast wear this app's icon instead of
        // the daemon's placeholder.
        notification.auto_icon();

        // See the module docs: the body is only clickable under XDG if it is
        // declared, and the label is what a daemon shows if it draws it as a
        // button anyway.
        #[cfg(all(unix, not(target_os = "macos")))]
        notification.action("default", t!("打开", "Open"));

        identify(&app, &mut notification);

        let handle = match notification.show() {
            Ok(handle) => handle,
            Err(error) => {
                // Unlike the plugin's, this one means something: the system
                // refused or could not be reached. The feature is a courtesy,
                // so it is written down rather than raised.
                eprintln!("dsh-desktop: the system would not show a notification: {error}");
                return;
            }
        };

        // macOS sends on drop; see the module docs. Nothing to wait for, and
        // so nothing for the session to be the answer to.
        #[cfg(target_os = "macos")]
        {
            let _ = &session;
            drop(handle);
        }

        #[cfg(not(target_os = "macos"))]
        {
            if LISTENING.fetch_add(1, Ordering::Relaxed) < LISTENERS {
                let _ = handle.wait_for_response(|response: &notify_rust::NotificationResponse| {
                    if !opens(response) {
                        return;
                    }
                    crate::reveal(&app);
                    if let Some(session) = &session {
                        crate::signal::open(&app, session);
                    }
                });
            }
            LISTENING.fetch_sub(1, Ordering::Relaxed);
        }
    });
}

/// Whether this is the user asking for the app.
///
/// `Default` is a click on the toast body, which every backend reports the same
/// way: Windows sends an activation with no arguments, and the XDG backend
/// normalises the `default` action key into it. `Action` is a named button,
/// which nothing here adds yet — when it does, the answer belongs to whatever
/// asked for the button rather than to this.
#[cfg(not(target_os = "macos"))]
fn opens(response: &notify_rust::NotificationResponse) -> bool {
    matches!(response, notify_rust::NotificationResponse::Default)
}

/// Whose name and icon the toast wears.
///
/// A transcription of what `tauri-plugin-notification` did, kept because it was
/// right and because getting it wrong is visible: a Windows toast raised under
/// an unresolvable AppUserModelID says *Windows PowerShell*, and a macOS one
/// raised under an unregistered bundle id does not appear at all.
/// [`crate::notify`]'s module docs explain both cases and what not to "fix".
#[allow(unused_variables)]
fn identify(app: &AppHandle, notification: &mut notify_rust::Notification) {
    #[cfg(windows)]
    {
        use std::path::MAIN_SEPARATOR as SEP;

        // The AUMID only resolves to a name and an icon through a Start Menu
        // shortcut, which a build running straight out of `target` has not
        // installed. Passing it anyway is what makes an uninstalled build's
        // toast blank rather than merely mislabelled.
        let Ok(exe) = tauri::utils::platform::current_exe() else {
            return;
        };
        let Some(directory) = exe.parent().map(|path| path.display().to_string()) else {
            return;
        };
        if !(directory.ends_with(&format!("{SEP}target{SEP}debug"))
            || directory.ends_with(&format!("{SEP}target{SEP}release")))
        {
            notification.app_id(&app.config().identifier);
        }
    }

    #[cfg(target_os = "macos")]
    {
        // A `tauri dev` run has no bundle of its own to be registered under, so
        // it borrows Terminal's identity — the plugin's own trick, and the
        // reason not to add a second `set_application` call anywhere else.
        let _ = notify_rust::set_application(if tauri::is_dev() {
            "com.apple.Terminal"
        } else {
            &app.config().identifier
        });
    }
}

#[cfg(test)]
mod tests {
    /// The one decision in this module that is not a platform call: which
    /// responses mean "the user wants the app".
    ///
    /// Worth a test because the obvious way to write the wait — `notify-rust`'s
    /// older `wait_for_action`, which hands back a `&str` — cannot express it.
    /// Its Windows arm folds a body click into the same `"__closed"` it reports
    /// a timeout as, so a toast dismissing itself would have revealed the
    /// window. `wait_for_response` keeps the two apart.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn opens_on_a_click_and_on_nothing_else() {
        use notify_rust::{CloseReason, NotificationResponse};

        assert!(super::opens(&NotificationResponse::Default));

        assert!(!super::opens(&NotificationResponse::Closed(
            CloseReason::Expired
        )));
        assert!(!super::opens(&NotificationResponse::Closed(
            CloseReason::Dismissed
        )));
        // Nothing adds a button yet, and one added later should have to say what
        // it does rather than inherit "open".
        assert!(!super::opens(&NotificationResponse::Action(
            "allow".to_string()
        )));
    }

    /// Raise one real toast and say what came back. Ignored, because it puts
    /// something on the screen: run it with
    /// `cargo test — —ignored —nocapture raises_one_real_toast`.
    ///
    /// A probe of the platform rather than of this module, which is why it
    /// builds its own notification instead of calling [`super::raise`] — that
    /// needs an `AppHandle`, and there is no app here. The question it answers
    /// is the one the rest of the file has to take on faith: whether an
    /// activation still reaches a process that has already returned from
    /// `show()`, since the object the handler was registered on is gone by
    /// then. Left alone it should print `Closed(..)` a few seconds later, when
    /// the popup times out — same delivery path, no click needed. Click the
    /// toast instead and it should print `Default`, which is what
    /// [`super::opens`] acts on.
    #[test]
    #[ignore]
    #[cfg(not(target_os = "macos"))]
    fn raises_one_real_toast() {
        let mut notification = notify_rust::Notification::new();
        notification.summary("dsh-desktop");
        notification.body("a probe, not a turn");

        #[cfg(all(unix, not(target_os = "macos")))]
        notification.action("default", "Open");

        let handle = notification.show().expect("the system should show a toast");
        handle
            .wait_for_response(|response: &notify_rust::NotificationResponse| {
                println!("the toast came back as {response:?}");
            })
            .expect("the response should arrive");
    }
}
