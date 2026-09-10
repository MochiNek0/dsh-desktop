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
//! ## What a button reaches
//!
//! dsh, without the window coming forward at all. [`crate::signal::buttons`]
//! decides which buttons a wait gets and what each one means; this module only
//! draws them and hands the id of the one that was pressed back to
//! [`crate::signal::answer`]. That is the whole point of them — a press that
//! raised the window would be a click on the body with extra steps.
//!
//! The two are kept apart at the response: `Default` is the body and reveals,
//! `Action(id)` is a button and answers. Under XDG the body click arrives as
//! the action named `default`, and `notify-rust` folds it into `Default`
//! before this sees it, so there is no id here that is not a button.
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

/// How long a toast that can be answered stays up, in milliseconds. See where
/// it is applied for why this number and not another.
const ANSWERABLE: u32 = 25_000;

/// Put one on screen. Returns immediately; the toast outlives the call.
///
/// On its own thread because two of the three backends do real work in `show()`
/// — a D-Bus round trip under XDG, COM activation on Windows — and because the
/// wait that follows is blocking by construction. `click` rides along to the
/// press; see the module docs.
pub fn raise(app: &AppHandle, title: String, body: String, click: Option<crate::notify::Click>) {
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
        // button anyway. First, so that a daemon drawing all of them puts the
        // way out where the way out goes.
        #[cfg(all(unix, not(target_os = "macos")))]
        notification.action("default", t!("打开", "Open"));

        // Not on macOS, where nothing is listening: its `show()` sends on
        // drop and the response never comes back (see the module docs), so a
        // button there would be one that visibly does nothing. Better no
        // button and a body click that opens the app.
        #[cfg(not(target_os = "macos"))]
        if let Some(click) = &click {
            for button in &click.buttons {
                notification.action(&button.id, &button.label);
            }

            // A toast with something to decide on is given the long life the
            // platform has, because the short one is not enough to decide in:
            // Windows shows a toast for about five seconds by default, and a
            // press after that reaches the notification centre, which reaches
            // nothing. Twenty-five seconds is the threshold `notify-rust` maps
            // to `Duration::Long` there, and a plain expiry everywhere else —
            // as opposed to `Timeout::Never`, which under XDG would leave the
            // toast, and the thread waiting on it, up until someone swept it
            // away. A toast with nothing to press keeps the default: it is a
            // sentence, and the notification centre can hold it.
            if !click.buttons.is_empty() {
                notification.timeout(notify_rust::Timeout::Milliseconds(ANSWERABLE));
            }
        }

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
        // so nothing for the buttons to be the answer to.
        #[cfg(target_os = "macos")]
        {
            let _ = &click;
            drop(handle);
        }

        #[cfg(not(target_os = "macos"))]
        {
            if LISTENING.fetch_add(1, Ordering::Relaxed) < LISTENERS {
                let _ = handle.wait_for_response(|response: &notify_rust::NotificationResponse| {
                    let Some(click) = &click else {
                        // Raised by the page, which named nothing to go to.
                        return;
                    };

                    match press(response) {
                        Some(Press::Open) => {
                            crate::reveal(&app);
                            crate::signal::open(&app, &click.session);
                        }
                        Some(Press::Answer(id)) => {
                            crate::signal::answer(&app, &click.session, &click.key, id);
                        }
                        None => {}
                    }
                });
            }
            LISTENING.fetch_sub(1, Ordering::Relaxed);
        }
    });
}

/// What the user did to the toast, out of the things worth doing anything
/// about. `None` for a toast that expired or was swept away, neither of which
/// is an answer or a request for the window.
#[cfg(not(target_os = "macos"))]
#[derive(Debug, PartialEq, Eq)]
enum Press<'a> {
    /// The body was clicked: the user is asking for the app.
    Open,
    /// A button was pressed, by the id [`crate::signal::buttons`] gave it.
    Answer(&'a str),
}

/// Read one response.
///
/// The one decision in this module that is not a platform call, and worth
/// naming because the obvious way to write the wait — `notify-rust`'s older
/// `wait_for_action`, which hands back a bare `&str` — cannot express it. Its
/// Windows arm folds a body click into the same `"__closed"` it reports a
/// timeout as, so a toast dismissing itself would have revealed the window.
///
/// `Default` is the body on every backend: Windows sends an activation with no
/// arguments, and the XDG backend normalises the `default` action key into it
/// before this sees it. So an `Action` is always one of the buttons above.
#[cfg(not(target_os = "macos"))]
fn press(response: &notify_rust::NotificationResponse) -> Option<Press<'_>> {
    match response {
        notify_rust::NotificationResponse::Default => Some(Press::Open),
        notify_rust::NotificationResponse::Action(id) => Some(Press::Answer(id)),
        _ => None,
    }
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
    /// The body opens the app, a button answers dsh, and a toast that closed
    /// on its own does neither.
    ///
    /// The last of those is the one worth pinning: `wait_for_action`, the call
    /// this module does not use, reports a timeout and a body click as the
    /// same `"__closed"`, so written that way a toast expiring would have
    /// pulled the window in front of whatever the user had moved on to.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn tells_the_body_the_buttons_and_a_toast_going_away_apart() {
        use super::Press;
        use notify_rust::{CloseReason, NotificationResponse};

        assert_eq!(
            super::press(&NotificationResponse::Default),
            Some(Press::Open)
        );
        assert_eq!(
            super::press(&NotificationResponse::Action("allow".to_string())),
            Some(Press::Answer("allow")),
            "a button means what the id says, not \"open\""
        );

        assert_eq!(
            super::press(&NotificationResponse::Closed(CloseReason::Expired)),
            None
        );
        assert_eq!(
            super::press(&NotificationResponse::Closed(CloseReason::Dismissed)),
            None
        );
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
    /// then. Left alone it should print `Closed(..)` half a minute later, when
    /// the popup times out — same delivery path, no click needed. Click the
    /// body instead and it should print `Default`; press one of the two
    /// buttons and it should print `Action("allow")` or `Action("reject")`,
    /// which is the pair [`super::press`] tells apart.
    #[test]
    #[ignore]
    #[cfg(not(target_os = "macos"))]
    fn raises_one_real_toast() {
        let mut notification = notify_rust::Notification::new();
        notification.summary("dsh-desktop");
        notification.body("a probe, not a turn");
        notification.timeout(notify_rust::Timeout::Milliseconds(super::ANSWERABLE));

        #[cfg(all(unix, not(target_os = "macos")))]
        notification.action("default", "Open");
        notification.action("allow", "Allow");
        notification.action("reject", "Refuse");

        let handle = notification.show().expect("the system should show a toast");
        handle
            .wait_for_response(|response: &notify_rust::NotificationResponse| {
                println!("the toast came back as {response:?}");
            })
            .expect("the response should arrive");
    }
}
