//! System notifications, from the notifications the page already raises.
//!
//! dsh's plugins announce a finished turn through the browser's `Notification`
//! API. In a webview that goes nowhere: there is no notification permission
//! prompt to grant and nothing behind the API to draw a toast, so a plugin that
//! calls it succeeds silently and the user — who closed the window into the
//! tray precisely because the turn was going to take a while — is told nothing.
//!
//! So the API is replaced. An initialization script puts a shim in front of
//! `window.Notification` on every document the window loads, and the shim hands
//! each notification to Rust, which raises a real one. Any plugin using the
//! standard API is covered without knowing this app exists; there is no bridge
//! for it to speak and no bridge to break when dsh changes.
//!
//! ## Over the same channel as everything else
//!
//! The payload travels as a navigation to `dsh-window://notify?title=…`, the
//! channel [`crate::controls`] already opened, rather than over Tauri's IPC —
//! for the reason set out there: granting IPC to `http://127.0.0.1:*` grants it
//! to every line of JavaScript dsh and its plugins load, and this is a toast,
//! not a reason to open that door. The query is one-way and carries three
//! strings, all of which end up as text in a notification.
//!
//! ## What a click does
//!
//! Nothing. Routing a click on a native toast back into the page is not
//! portable — the three platforms disagree about whether an activation is even
//! delivered to a running process — and a handler that fires on one of them is
//! worse than none, because the plugin author cannot tell which. So the shim
//! keeps the `Notification` object and its `onclick` intact and simply never
//! fires it, which is exactly what happens today, and the notification itself is
//! the part that was missing.
//!
//! This has been asked about, so the state of the ground underneath it: the
//! Windows half is in fact reachable — `notify-rust` calls `Toast::on_activated`
//! and hands back a `NotificationHandle` carrying the activation — but
//! `tauri-plugin-notification`'s desktop `show()` drops that handle on the
//! floor, so nothing above it can see a click. Wiring one up means going around
//! the plugin to `notify-rust` directly, on Windows only, and owning a second
//! notification path for the one platform. That is a real cost for a
//! convenience, and the decision here is deliberately to leave the toast
//! inert and put what the user needs into its text instead. Revisit if the
//! plugin ever surfaces the handle.
//!
//! Because a click leads nowhere, the body is written to stand on its own: it
//! says what finished, not "click to return".
//!
//! ## Whose name and icon a toast carries
//!
//! The installed app's. This has been reported as a bug more than once, so:
//! a Windows toast is drawn with the name and icon of the AppUserModelID it was
//! raised under, and `tauri-plugin-notification` passes the bundle identifier as
//! that AUMID — but only when the running exe is not under `target\debug` or
//! `target\release`. For an uninstalled build it passes nothing, notify-rust
//! substitutes its `POWERSHELL_APP_ID`, and the toast says *Windows PowerShell*
//! and wears PowerShell's icon.
//!
//! That is a property of running the build output directly, not a defect to fix
//! here: the AUMID only resolves to a name and an icon because a Start Menu
//! shortcut declares it, and an uninstalled build has no shortcut. The NSIS
//! template already stamps `${BUNDLEID}` onto both shortcuts it creates (its
//! `SetLnkAppUserModelId`), which is the same string the plugin sends, so an
//! installed app is correct with nothing added. Do not "fix" this by stamping
//! the AUMID again from `installer-hooks.nsh` — it is already done, and a second
//! copy is one more thing to keep in step.
//!
//! ## The other two platforms
//!
//! Everything above is Windows. The same call behaves like this elsewhere, and
//! it is worth writing down because the failures look identical from here — the
//! toast simply does not appear:
//!
//! - **macOS** goes through `mac-notification-sys`, which needs a bundle
//!   identifier registered with LaunchServices. The plugin handles the awkward
//!   case itself: `set_application(if tauri::is_dev() { "com.apple.Terminal" }
//!   else { identifier })`, so a `tauri dev` run borrows Terminal's identity and
//!   an installed `.app` uses its own. Nothing to do here, and in particular do
//!   not add a `cfg(target_os = "macos")` branch that sets it again.
//! - **Linux** goes over D-Bus to `org.freedesktop.Notifications`. That is
//!   present under GNOME, KDE and anything else with a notification daemon, and
//!   absent under a bare window manager or in a container — where the toast is
//!   dropped by the session, not by this app.
//!
//! In all three cases the failure is silent by design: see the note in [`show`]
//! on why the `Result` cannot tell you which happened.

use tauri::{AppHandle, Manager, Url};
use tauri_plugin_notification::NotificationExt;

/// How much of each string survives the trip. A notification is two lines on
/// screen wherever it is drawn; the rest would only make the URL longer.
const LIMIT: usize = 200;

/// One notification, as it arrived from the page.
pub struct Notice {
    title: String,
    body: String,
}

/// Read a `dsh-window://notify` navigation. `None` for a query with nothing in
/// it to show — a notification with neither title nor body is a toast the user
/// cannot act on and cannot dismiss the cause of.
pub fn received(url: &Url) -> Option<Notice> {
    let mut title = String::new();
    let mut body = String::new();

    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "title" => title = clamp(&value),
            "body" => body = clamp(&value),
            _ => {}
        }
    }

    if title.is_empty() && body.is_empty() {
        return None;
    }
    // A notification with a body and no title reads as an orphan on Windows,
    // where the title is the bold first line; the app's own name is what would
    // have been there had the page passed one.
    if title.is_empty() {
        title = "dsh".to_string();
    }

    Some(Notice { title, body })
}

/// Raise it, unless the user turned notifications off or is already looking at
/// the thing it is about.
///
/// A toast over the window it is announcing something in is noise — the user can
/// see the turn finish. Suppressed here rather than in the shim because the page
/// cannot tell: a webview's `document.hidden` follows the tab, and this window
/// has no tabs, so it reads visible while the window is buried behind three
/// others or sitting in the tray.
///
/// The preference is checked here too, for the same reason it is the last gate
/// rather than the first: this is the one place every notification passes
/// through, whoever raised it. Turning the setting off silences a plugin's
/// notifications as well as this app's own — which is why the setting is called
/// "Notifications" rather than naming the finished-turn toast. It was named for
/// the toast once, and a switch whose label promises less than it does is a
/// switch that surprises the person who used it. See [`crate::settings`].
pub fn show(app: &AppHandle, notice: Notice) {
    if !crate::settings::notifications(app) || watching(app) {
        return;
    }

    let mut builder = app.notification().builder().title(notice.title);
    if !notice.body.is_empty() {
        builder = builder.body(notice.body);
    }

    // The `Result` is about building the request, not about raising the toast.
    // `tauri-plugin-notification`'s desktop `show()` hands the notification to
    // `tauri::async_runtime::spawn` and discards what comes back, so it answers
    // `Ok` on every platform whether or not anything was ever displayed. This
    // arm therefore catches almost nothing — kept because it costs a line, but
    // do not read silence here as a toast that appeared.
    if let Err(error) = builder.show() {
        // The whole feature is a courtesy; a platform that will not raise one is
        // not a reason to interrupt anybody.
        eprintln!("dsh-desktop: could not raise a notification: {error}");
    }
}

/// Whether the window is on screen and has the user's attention. Anything less —
/// minimised, hidden in the tray, behind another app — counts as away.
fn watching(app: &AppHandle) -> bool {
    let Some(window) = app.get_webview_window("main") else {
        return false;
    };

    window.is_visible().unwrap_or(false)
        && !window.is_minimized().unwrap_or(false)
        && window.is_focused().unwrap_or(false)
}

/// One string, trimmed to something a toast can hold, on a character boundary.
fn clamp(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(LIMIT) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_string(),
    }
}

/// The shim, injected into every document the window loads.
///
/// It is deliberately a small class rather than a wrapper around the real
/// `Notification`: there is no real one to wrap. What it has to get right is the
/// shape callers check before they commit to using it — `permission`, which
/// gates most call sites, and `requestPermission`, which the careful ones await
/// first.
pub fn script() -> String {
    let scheme = crate::controls::SCHEME;
    let limit = LIMIT;

    format!(
        r#"(function () {{
  // The top document only. This shim answers by navigating, and a navigation
  // started inside an iframe never reaches the handler that cancels it: wry
  // hooks WebView2's main-frame `NavigationStarting` and nothing else. See
  // `controls`.
  if (window.top !== window.self) return;
  if (window.__dshNotify) return;
  window.__dshNotify = true;

  // Bumped for every notification and pasted into the URL below, which is what
  // makes each one different from the last.
  //
  // Assigning `location.href` only navigates when the value actually changes,
  // and this navigation is cancelled in Rust (see main.rs, which answers
  // `false` to it) so the address bar never moves. Two identical notifications
  // would therefore build the same string twice, and the second assignment
  // would be a no-op: the toast simply would not appear. That is not
  // hypothetical -- it is what made a repeated notification fire exactly once.
  var nonce = 0;

  // The channel back to Rust; see controls.rs. The navigation is cancelled
  // there, so raising a notification never moves the page.
  function signal(title, body) {{
    var query = 'title=' + encodeURIComponent(title) + '&body=' + encodeURIComponent(body) +
      '&n=' + (++nonce) + '.' + Date.now();
    window.location.href = '{scheme}://notify?' + query;
  }}

  function text(value) {{
    if (value === undefined || value === null) return '';
    return String(value).slice(0, {limit});
  }}

  // An EventTarget, so `addEventListener('close', …)` and the `onclose` the
  // shim fires below both behave. No event is ever dispatched for a click —
  // see the module docs in notify.rs.
  function DshNotification(title, options) {{
    if (!(this instanceof DshNotification)) {{
      throw new TypeError("Failed to construct 'Notification': please use the 'new' operator");
    }}
    var settings = options || {{}};
    var target = new EventTarget();

    this.title = text(title);
    this.body = text(settings.body);
    this.tag = text(settings.tag);
    this.data = settings.data;
    this.icon = text(settings.icon);
    this.onclick = null;
    this.onclose = null;
    this.onerror = null;
    this.onshow = null;
    this.addEventListener = target.addEventListener.bind(target);
    this.removeEventListener = target.removeEventListener.bind(target);
    this.dispatchEvent = target.dispatchEvent.bind(target);

    signal(this.title, this.body);

    // The OS owns it from here, so there is nothing left to close. `close()` is
    // called by plugins that clear their own notifications on a timer, and one
    // that throws would take the timer's callback down with it.
    var self = this;
    this.close = function () {{
      var event = new Event('close');
      if (typeof self.onclose === 'function') self.onclose(event);
      target.dispatchEvent(event);
    }};
  }}

  DshNotification.permission = 'granted';
  DshNotification.maxActions = 0;
  DshNotification.requestPermission = function (callback) {{
    if (typeof callback === 'function') callback('granted');
    return Promise.resolve('granted');
  }};

  try {{
    Object.defineProperty(window, 'Notification', {{
      value: DshNotification,
      writable: true,
      configurable: true
    }});
  }} catch (error) {{
    window.Notification = DshNotification;
  }}
}})();"#
    )
}

#[cfg(test)]
mod tests {
    use super::{clamp, received, LIMIT};
    use tauri::Url;

    fn parse(query: &str) -> Option<super::Notice> {
        received(&Url::parse(&format!("dsh-window://notify?{query}")).expect("a valid test URL"))
    }

    #[test]
    fn reads_both_halves() {
        let notice = parse("title=Turn%20finished&body=all%20done").expect("a notice");

        assert_eq!(notice.title, "Turn finished");
        assert_eq!(notice.body, "all done");
    }

    /// Windows draws the title as the first line; a toast without one is an
    /// orphaned sentence.
    #[test]
    fn stands_in_for_a_missing_title() {
        let notice = parse("body=all%20done").expect("a notice");

        assert_eq!(notice.title, "dsh");
    }

    #[test]
    fn refuses_a_notification_with_nothing_in_it() {
        assert!(parse("title=&body=%20").is_none());
        assert!(parse("").is_none());
    }

    /// The cut lands on a character boundary, which for the scripts this app is
    /// read in is every third byte.
    #[test]
    fn clamps_without_splitting_a_character() {
        let long = "会话完成".repeat(LIMIT);
        let clamped = clamp(&long);

        assert_eq!(clamped.chars().count(), LIMIT + 1);
        assert!(clamped.ends_with('…'));
    }

    #[test]
    fn leaves_a_short_string_alone() {
        assert_eq!(clamp("  done  "), "done");
    }

    /// The cache-buster the shim adds is not part of the notification.
    ///
    /// It exists because `location.href` only navigates when the string
    /// changes, and this navigation is cancelled — so two identical
    /// notifications would produce one toast. Reading it as a field would put
    /// it on screen.
    #[test]
    fn ignores_the_cache_buster() {
        let notice = parse("title=Turn%20finished&body=all%20done&n=7.1700000000000")
            .expect("a notice");

        assert_eq!(notice.title, "Turn finished");
        assert_eq!(notice.body, "all done");
    }

    /// The shim has to put something different in the URL every time, or the
    /// second of two identical notifications never navigates and so is never
    /// raised. Asserted against the generated script because that is where the
    /// mistake would be reintroduced.
    #[test]
    fn the_shim_varies_every_url() {
        let script = super::script();

        assert!(
            script.contains("++nonce"),
            "the shim must make each notification URL unique"
        );
    }
}
