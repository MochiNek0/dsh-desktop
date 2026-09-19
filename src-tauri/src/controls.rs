//! The window's own chrome, injected into whatever page the window is showing.
//!
//! The window has no frame, so minimise, maximise and close have to come from
//! inside it — and the page inside it is dsh's, not ours. So they are injected:
//! an initialization script runs on every document the window loads, the
//! loading page and the dsh UI alike, and puts three buttons in the top-left
//! corner plus a thin strip along the top edge to drag the window by.
//!
//! They sit on the left, in macOS order and shape, because dsh puts its own
//! controls in the top-right — the session log download among them — and a
//! Windows-style button row lands right on top of them. The top-left strip
//! above dsh's logo is the one piece of the header that is reliably empty.
//!
//! Nothing is reserved for them. They float over whatever the page draws rather
//! than pushing it down, because pushing dsh's layout down means guessing at how
//! it measures itself, and that guess would break on a dsh release we do not
//! control. The cost is that they sit on top of the page's own top-left corner,
//! which is why they are dimmed, and show their glyphs only once the pointer
//! comes near.
//!
//! ## The menu
//!
//! Beside them is the app's own menu — updating dsh, checking for a new app,
//! the login item, quitting. It was all in the tray, where it could not be
//! styled at all: a tray menu is drawn by the OS, and the app has no say over
//! anything but the words in it. Drawn here it is ours, and it is also where
//! the user is looking. The tray keeps the two items that are only ever wanted
//! when there is no window to look at — show, and quit.
//!
//! ## Talking back
//!
//! A click has to reach Rust, and the ordinary way — Tauri's IPC — would mean
//! granting IPC to `http://127.0.0.1:*`, which is to say to every line of
//! JavaScript dsh and its plugins load. That is a large door to open for a
//! handful of buttons.
//!
//! So the channel is a navigation instead: the page sets `location.href` to
//! `dsh-window://<action>`, [`action`] recognises it in the navigation handler,
//! and the navigation is cancelled before it goes anywhere. One-way, a fixed
//! list of verbs, no permissions. Dragging works the same way despite being
//! continuous, because [`WebviewWindow::start_dragging`] hands the whole drag to
//! the OS — the page only has to say when it starts.
//!
//! Everything the menu added to that list is something the user can already do
//! from the tray, and none of it reads anything back. What the page cannot do is
//! ask a question and get an answer, which is what the IPC door would have
//! opened.
//!
//! What it *can* do is press every button, and that is worth being plain about.
//! [`action`] recognises `dsh-window://` on any top-level navigation the window
//! makes and has no way to tell one this module's chrome sent from one a script
//! in the page sent. Nor can it be given one: the initialization scripts share a
//! JavaScript context with dsh's own code, so a token handed to them is a token
//! the page can read, and a button only they draw is a button the page can still
//! `click()`. So the verbs are not authenticated. The two that lead somewhere
//! sharp are narrowed at their payload instead — see [`is_web_link`] for the one
//! that hands a target to the system, and [`crate::plugins::is_package_spec`]
//! for the one that installs a package.
//!
//! Five things travel the other way, each pushed rather than asked for, because
//! the page has no way to see any of them: whether the window is maximised
//! ([`sync`]), whether the login item is on ([`sync_autostart`]), whether
//! notifications are ([`sync_notify`]), the menu's own labels when dsh changes
//! language under it ([`relabel`]), and whatever slow thing is running right
//! now ([`busy`]).

use tauri::{AppHandle, Manager, Url, WebviewWindow};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_opener::OpenerExt;

/// The scheme the injected buttons signal on. Not registered with anything — it
/// only has to be a scheme no real navigation would use, since the navigation
/// is cancelled the moment it is recognised.
pub const SCHEME: &str = "dsh-window";

/// The dedicated titlebar height (in px).
const TITLEBAR_HEIGHT: u32 = 36;

/// One dot, the space between two of them, and the padding around the row.
const DOT: u32 = 12;
const DOT_GAP: u32 = 8;
const ROW_PAD: u32 = 14;

/// The menu's typeface. Named rather than left to the webview, whose default for
/// a bare `<button>` is a serif face at a size of its own choosing.
pub(crate) const FONT: &str =
    "-apple-system,BlinkMacSystemFont,\"Segoe UI\",\"Microsoft YaHei\",system-ui,sans-serif";

/// What the page can ask the app to do. A fixed list, and every one of them is
/// a menu item the tray offered first.
pub enum Action {
    Minimize,
    Maximize,
    Close,
    Drag,
    UpdateDsh,
    CheckApp,
    Autostart,
    /// Turn this app's notifications on or off — every one of them, not just
    /// the finished-turn toast this verb is named after. The gate is in
    /// `notify::show`, which they all pass through; see [`crate::settings`].
    NotifyTurns,
    Quit,
    /// dsh switched language under a document that is not going to load
    /// again; see [`relabel`]. Carries the tag `<html lang>` moved to.
    Locale(String),
    /// Open the pairing card: a QR code for a phone to scan. See
    /// [`crate::remote`].
    Remote,
    /// Throw one paired device off. Carries the id the card drew it under.
    RemoteKick(String),
    /// Throw them all off and change the signing key.
    RemoteKickAll,
    /// Replace the nonce on the card with a fresh one.
    RemoteRefresh,
    /// Move the gateway to another channel. Carries the one the card asked
    /// for, already resolved to something this build implements.
    RemoteChannel(crate::remote::TunnelType),
    /// Start the channel that is already selected. The way back from a tunnel
    /// that fell over, and from the idle timer having taken one down.
    RemoteStart,
    /// Stop being reachable from the public internet, now. Signalled from the
    /// card and from the tray.
    RemoteStopPublic,
    /// The Cloudflare token and hostname, as typed into the card. Both halves
    /// or the signal is dropped; see [`mod@crate::remote::cloudflare`].
    RemoteCloudflare(String, String),
    /// The card was closed. The gateway stays up; the devices on it are still
    /// working.
    RemoteClose,
    /// The stylesheet-patch box on the card was ticked or unticked. Carries the
    /// state it is now in, not a request to flip.
    RemoteStyle(bool),
    /// The forget-on-exit box on the card was ticked or unticked. Same shape,
    /// and for the same reason.
    RemoteForget(bool),
    /// Open the plugin panel on the loading page.
    Plugins,
    /// Install what was ticked in it, and whatever was typed into its box.
    PluginsInstall(Vec<String>, Option<String>),
    /// Take the ticked ones back out again.
    PluginsRemove(Vec<String>),
    /// Bring the named ones up to their newest release. One card's button, so
    /// one name — but carried as a list because the payload is the same
    /// `?names=` field a removal uses, and narrowing it would be a second
    /// parser for no gain. See [`crate::plugins::update`].
    PluginsUpdate(Vec<String>),
    /// Leave the panel: back to dsh, starting it if the panel was shown before
    /// the boot ever got that far.
    PluginsDone,
    /// Show the profile directory in the file manager — the one step the panel
    /// does not take on the user's behalf. See [`crate::plugins`].
    PluginsDirectory,
    /// Change which registry an install of dsh is taken from, on a machine
    /// whose npm points somewhere of the user's own choosing; see
    /// [`crate::settings::RegistrySource`].
    Registry,
    /// A choice in the runtime chooser; see [`crate::setup`]. The index is the
    /// Node's place in the list the chooser was given.
    SetupUse(usize),
    SetupInstallDsh(usize),
    SetupInstallNode,
    /// Take dsh out of Node `i`.
    SetupUninstallDsh(usize),
    /// Delete the Node this app installed, and the dsh in it.
    SetupRemoveNode,
    /// Delete Node `i` itself, where a version manager installed it.
    SetupDeleteNode(usize),
    /// Look at the machine again, after a scan that could not.
    SetupRescan,
    /// Put the panel away. The menu's way out, where [`Action::SetupQuit`] is
    /// the boot's: there is a dsh running behind this one.
    SetupClose,
    SetupQuit,
    /// Open the panel from the menu.
    Runtime,
    /// A shell with dsh on its PATH.
    Terminal,
    /// Start `dsh web` again after it exited on its own.
    RestartDsh,
    /// Start it with every installed plugin off the profile's layer stack. The
    /// loading page's second button, offered on a dsh that would not come up;
    /// see [`crate::plugins::engage_safe`].
    SafeStart,
    /// Put them back and restart into them. The menu row that exists only while
    /// [`Action::SafeStart`] is in effect.
    SafeOff,
    /// A notification the page raised; see [`crate::notify`].
    Notify(crate::notify::Notice),
    /// A session transition the client plugin reported; see [`crate::signal`].
    Signal(crate::signal::Signal),
    /// A button in one of this app's own dialogs; see [`crate::dialog`]. Carries
    /// the whole URL because the token and the button id are read there.
    Answered(Url),
    /// Open an external URL in the system browser.
    OpenUrl(String),
}

/// Recognise a navigation as a button press. `None` for every ordinary URL,
/// which is all the navigation handler wants to know.
pub fn action(url: &Url) -> Option<Action> {
    if url.scheme() != SCHEME {
        return None;
    }

    // `dsh-window://close` puts the verb where a host would go.
    match url.host_str()? {
        "minimize" => Some(Action::Minimize),
        "maximize" => Some(Action::Maximize),
        "close" => Some(Action::Close),
        "drag" => Some(Action::Drag),
        "update-dsh" => Some(Action::UpdateDsh),
        "check-app" => Some(Action::CheckApp),
        "autostart" => Some(Action::Autostart),
        "notify-turns" => Some(Action::NotifyTurns),
        "quit" => Some(Action::Quit),
        "remote" => Some(Action::Remote),
        // The id is the store's own handle for a device, and it is checked by
        // being looked for: a forged one names no row and revokes nothing. See
        // [`crate::remote::session::SessionStore::revoke`].
        "remote-kick" => url
            .query_pairs()
            .find_map(|(key, value)| (key == "id").then(|| value.into_owned()))
            .filter(|id| !id.is_empty())
            .map(Action::RemoteKick),
        "remote-kick-all" => Some(Action::RemoteKickAll),
        "remote-refresh" => Some(Action::RemoteRefresh),
        // Parsed into the enum here rather than carried as a string: a name
        // this build does not implement is not a channel, and the place to
        // find that out is before anything acts on it.
        "remote-channel" => url
            .query_pairs()
            .find_map(|(key, value)| (key == "to").then(|| value.into_owned()))
            .and_then(|name| crate::remote::TunnelType::named(&name))
            .map(Action::RemoteChannel),
        "remote-start" => Some(Action::RemoteStart),
        "remote-public-off" => Some(Action::RemoteStopPublic),
        // The one verb on this channel carrying something that must not be
        // written down anywhere. It reaches Rust the way every other button
        // does — a navigation the handler cancels before the webview commits
        // it — and from here it goes straight into a file only this user can
        // read. Nothing on this path logs the URL, and this is the reason to
        // keep it that way: see the note in `perform`.
        "remote-cloudflare" => {
            let mut token = None;
            let mut host = None;
            for (key, value) in url.query_pairs() {
                match key.as_ref() {
                    "token" => token = Some(value.into_owned()),
                    "host" => host = Some(value.into_owned()),
                    _ => {}
                }
            }
            Some(Action::RemoteCloudflare(token?, host?))
        }
        "remote-close" => Some(Action::RemoteClose),
        // The state, not a flip: a signal that went missing would otherwise
        // leave the box and the flag disagreeing until the next click.
        "remote-style" => url
            .query_pairs()
            .find_map(|(key, value)| (key == "on").then(|| value == "1"))
            .map(Action::RemoteStyle),
        "remote-forget" => url
            .query_pairs()
            .find_map(|(key, value)| (key == "on").then(|| value == "1"))
            .map(Action::RemoteForget),
        "plugins" => Some(Action::Plugins),
        "plugins-install" => {
            let (ids, spec) = crate::plugins::requested(url);
            Some(Action::PluginsInstall(ids, spec))
        }
        "plugins-remove" => Some(Action::PluginsRemove(crate::plugins::wanted_gone(url))),
        "plugins-update" => Some(Action::PluginsUpdate(crate::plugins::wanted_gone(url))),
        "plugins-done" => Some(Action::PluginsDone),
        "plugins-directory" => Some(Action::PluginsDirectory),
        // The runtime chooser's verbs; see `setup`. The Node index travels as
        // `?i=` on the two that name one.
        "setup-use" => setup_index(url).map(Action::SetupUse),
        "setup-install-dsh" => setup_index(url).map(Action::SetupInstallDsh),
        "setup-install-node" => Some(Action::SetupInstallNode),
        "setup-uninstall-dsh" => setup_index(url).map(Action::SetupUninstallDsh),
        "setup-remove-node" => Some(Action::SetupRemoveNode),
        "setup-delete-node" => setup_index(url).map(Action::SetupDeleteNode),
        "setup-rescan" => Some(Action::SetupRescan),
        "setup-close" => Some(Action::SetupClose),
        "setup-quit" => Some(Action::SetupQuit),
        "runtime" => Some(Action::Runtime),
        "registry" => Some(Action::Registry),
        "terminal" => Some(Action::Terminal),
        "restart-dsh" => Some(Action::RestartDsh),
        "safe-start" => Some(Action::SafeStart),
        "safe-off" => Some(Action::SafeOff),
        // Not a request for anything: the page saying what it has already
        // done, so the chrome around it can catch up. See [`relabel`].
        "locale" => url
            .query_pairs()
            .find_map(|(key, value)| (key == "tag").then(|| value.into_owned()))
            .filter(|tag| !tag.is_empty())
            .map(Action::Locale),
        "open" => url
            .query_pairs()
            .find_map(|(key, value)| {
                if key == "url" {
                    Some(value.into_owned())
                } else {
                    None
                }
            })
            .filter(|target| is_web_link(target))
            .map(Action::OpenUrl),
        // The only one carrying a payload the app reads rather than acts on,
        // and the only one that can decline: an empty notification is dropped
        // here rather than raised as a blank toast.
        "notify" => crate::notify::received(url).map(Action::Notify),
        // Also a payload rather than a request: dsh's own view of what each
        // session is doing, from the client plugin in `plugin/`. See
        // `crate::signal`, which owns the parsing.
        "signal" => crate::signal::received(url).map(Action::Signal),
        // An answer to a question this app asked; see `crate::dialog`, which
        // owns the parsing because it owns the callback the answer runs.
        "ask" => Some(Action::Answered(url.clone())),
        other => {
            eprintln!("dsh-desktop: ignoring unknown window action {other}");
            None
        }
    }
}

/// Whether a target this app is being asked to hand to the system is a link
/// rather than something to run.
///
/// [`Action::OpenUrl`] ends in `opener::open_url`, which is `ShellExecute` on
/// Windows and `open`/`xdg-open` elsewhere. Given a path or a registered scheme
/// those launch a program; given `http` they open a browser. The verb exists for
/// the second — a link in the dsh page belongs in the user's browser rather than
/// in place of the session they are working in — and every such link is `http`,
/// `https` or `mailto`. So those three are what it accepts, and a `file:`, a
/// `ms-settings:` or a bare `C:\Windows\...` is declined here instead of
/// executed.
///
/// The payload is the only thing there is to be strict about, because this
/// channel takes no account of who asked: `dsh-window://` is recognised on any
/// top-level navigation the window makes, which is every document dsh loads as
/// well as this app's own pages. A page that can run script can reach every verb
/// on the list, so the ones that lead somewhere sharp are narrowed at the
/// payload — this, and the package specifier in [`crate::plugins::install`].
///
/// Nothing legitimate is turned away by this. The click handler below hands over
/// every scheme it does not recognise, but dsh renders its own links through a
/// protocol allowlist of `http`, `https` and `mailto` and leaves everything else
/// inert — so a scheme this declines is one no dsh page made clickable in the
/// first place. Relative targets fail to parse and are declined with the rest:
/// nothing that reaches here is relative to anything.
fn is_web_link(target: &str) -> bool {
    Url::parse(target).is_ok_and(|target| matches!(target.scheme(), "http" | "https" | "mailto"))
}

/// The `?i=` a chooser verb carries: which Node in the list the user picked.
/// `None` when it is missing or not a number, which leaves the verb to be
/// ignored rather than acted on with a nonsense index.
fn setup_index(url: &Url) -> Option<usize> {
    url.query_pairs()
        .find(|(key, _)| key == "i")
        .and_then(|(_, value)| value.parse::<usize>().ok())
}

/// Do what the button asked. Every call is best effort — a window that will not
/// minimise is not a reason to take the app down.
///
/// This runs inside the webview's navigation callback, on the main thread, with
/// the webview waiting on it. So nothing here blocks: the menu items that lead
/// to npm or to a shutdown hand themselves to a thread first.
pub fn perform(app: &AppHandle, action: Action) {
    match action {
        Action::UpdateDsh => return crate::update_dsh(app),
        Action::CheckApp => return crate::update::check_now(app),
        Action::Autostart => return crate::toggle_autostart(app),
        Action::NotifyTurns => return crate::toggle_notify_turns(app),
        Action::Quit => return crate::quit(app),
        Action::Remote => return crate::remote::open(app),
        Action::RemoteKick(id) => return crate::remote::kick(app, &id),
        Action::RemoteKickAll => return crate::remote::kick_all(app),
        Action::RemoteRefresh => return crate::remote::refresh(app),
        Action::RemoteChannel(kind) => return crate::remote::channel(app, kind),
        Action::RemoteStart => return crate::remote::start_channel(app),
        Action::RemoteStopPublic => return crate::remote::stop_public(app),
        // Not logged, and not traced. See the parser for this verb.
        Action::RemoteCloudflare(token, host) => {
            return crate::remote::cloudflare(app, &token, &host)
        }
        Action::RemoteClose => return crate::remote::close(app),
        Action::RemoteStyle(on) => return crate::remote::style(app, on),
        Action::RemoteForget(on) => return crate::remote::forget_on_exit(app, on),
        Action::Locale(tag) => return crate::switch_language(app, &tag),
        Action::Plugins => return crate::open_plugins(app),
        Action::PluginsInstall(ids, spec) => return crate::install_plugins(app, ids, spec),
        Action::PluginsRemove(names) => return crate::remove_plugins(app, names),
        Action::PluginsUpdate(names) => return crate::update_plugins(app, names),
        Action::PluginsDone => return crate::leave_plugins(app),
        Action::PluginsDirectory => return crate::plugins::open_directory(app),
        Action::SetupUse(i) => return crate::setup::answered(crate::setup::Choice::Use(i)),
        Action::SetupInstallDsh(i) => {
            return crate::setup::answered(crate::setup::Choice::InstallDsh(i))
        }
        Action::SetupInstallNode => {
            return crate::setup::answered(crate::setup::Choice::InstallNode)
        }
        Action::SetupUninstallDsh(i) => {
            return crate::setup::answered(crate::setup::Choice::UninstallDsh(i))
        }
        Action::SetupRemoveNode => return crate::setup::answered(crate::setup::Choice::RemoveNode),
        Action::SetupDeleteNode(i) => {
            return crate::setup::answered(crate::setup::Choice::DeleteNode(i))
        }
        Action::SetupRescan => return crate::setup::answered(crate::setup::Choice::Rescan),
        Action::SetupClose => return crate::setup::answered(crate::setup::Choice::Close),
        Action::SetupQuit => return crate::setup::answered(crate::setup::Choice::Quit),
        Action::Runtime => return crate::open_runtime(app),
        Action::Registry => return crate::open_registry(app),
        Action::RestartDsh => return crate::restart_dsh(app, false),
        Action::SafeStart => return crate::safe_start(app),
        Action::SafeOff => return crate::safe_off(app),
        Action::Notify(notice) => return crate::notify::show(app, notice),
        Action::Signal(signal) => return crate::signal::act(app, signal),
        Action::Answered(url) => return crate::dialog::answered(app, &url),
        Action::OpenUrl(target) => {
            let _ = app.opener().open_url(target, None::<&str>);
            return;
        }
        Action::Terminal => {
            if let Err(error) = crate::dsh::terminal(app) {
                crate::dsh::note(
                    app,
                    t!("打不开终端", "Could not open a terminal"),
                    &t!(
                        "没能启动终端程序：{}",
                        "The terminal could not be started: {}",
                        error
                    ),
                );
            }
            return;
        }
        _ => {}
    }

    let Some(window) = app.get_webview_window("main") else {
        return;
    };

    match action {
        Action::Minimize => {
            let _ = window.minimize();
        }
        Action::Maximize => {
            let _ = if window.is_maximized().unwrap_or(false) {
                window.unmaximize()
            } else {
                window.maximize()
            };
        }
        // The same thing the frame's close button did: park in the tray rather
        // than tear down an agent mid-task. Quitting is its own menu item.
        Action::Close => {
            let _ = window.hide();
            crate::memory::trim(&window);
        }
        Action::Drag => {
            let _ = window.start_dragging();
        }
        // Handled above, where they are the reason for the early return.
        _ => {}
    }
}

/// Tell the page whether the window is maximised, so the middle button shows the
/// right glyph. Pushed rather than guessed: the window can be maximised by ways
/// the page never sees — Win+Up, a snap, a double-click the OS handled itself.
pub fn sync(window: &WebviewWindow) {
    let maximized = window.is_maximized().unwrap_or(false);
    let _ = window.eval(format!(
        "window.__dshMaximized && window.__dshMaximized({maximized})"
    ));
}

/// Put the checkmark on the login item, or take it off. Pushed after every
/// toggle and on every page load, and it reports what the system actually ended
/// up with rather than what was asked for.
pub fn sync_autostart(app: &AppHandle) {
    let enabled = app.autolaunch().is_enabled().unwrap_or(false);
    eval(
        app,
        &format!("window.__dshAutostart && window.__dshAutostart({enabled})"),
    );
}

/// Put the checkmark on the notification item, or take it off — and say
/// whether the item can be used at all. Pushed on every page load and after
/// every toggle, like the login item above.
///
/// The second answer is [`crate::plugins::signalling`]: without the plugin
/// that reports what dsh is doing there is nothing to raise a notification
/// about, so the row is drawn dimmed and inert rather than as a switch that
/// can be turned on and will do nothing. The third is why, in the user's own
/// language, because a dimmed row with no explanation is a bug report.
///
/// Every page load is enough to keep it current: installing or removing a
/// plugin restarts dsh and reloads the document, which lands here.
pub fn sync_notify(app: &AppHandle) {
    // Two ways for the row to be unavailable, and they want different
    // sentences. Without the plugin the fix is to install it; on a safe launch
    // it is installed and deliberately not loaded, and telling that user to go
    // and install it would send them looking for something already on the list.
    let call = notify_call(
        crate::settings::notifications(app),
        crate::plugins::signalling(app),
        if crate::plugins::safe(app) {
            t!(
                "这次启动没有加载任何插件，「会话信号」也在内。用菜单里的「重新加载插件」装回来。",
                "This launch loaded no plugins, Session signals included. Use “Load plugins again” in the menu to bring them back."
            )
        } else {
            t!(
                "需要「会话信号」插件，在菜单的「插件」里安装。",
                "Needs the Session signals plugin — install it from Plugins in the menu."
            )
        },
    );
    eval(app, &call);
}

/// Say whether this launch is running on no plugins: the standing label in the
/// titlebar, and the row that ends it. Pushed on every page load beside the two
/// above, and for the same reason — a fresh document has no way of knowing
/// which kind of launch it is part of.
///
/// The label is there because the row on its own was not enough. Safe mode
/// outlives the click that started it: quitting and reopening the app comes
/// back into one, and a user who set a plugin aside yesterday would find an app
/// loading none of them today with nothing on screen saying so except a menu
/// item that reads as an action rather than a state.
pub fn sync_safe(app: &AppHandle) {
    let call = safe_call(
        crate::plugins::safe(app),
        t!("插件未加载", "No plugins loaded"),
        t!(
            "这次启动没有加载任何插件。点它把插件装回 profile，并重启 dsh。",
            "This launch loaded no plugins. This puts them back into the profile and restarts dsh."
        ),
    );
    eval(app, &call);
}

/// The call [`sync_safe`] makes, as a string so both halves of it can be read
/// in one test: three arguments here, three parameters in the function the
/// injected script hangs on `window`.
fn safe_call(on: bool, label: &str, hint: &str) -> String {
    let label = serde_json::to_string(label).expect("a string is always serializable");
    let hint = serde_json::to_string(hint).expect("a string is always serializable");
    format!("window.__dshSafeMode && window.__dshSafeMode({on}, {label}, {hint})")
}

/// The call [`sync_notify`] makes, as a string so both halves of it can be
/// read in one test: three arguments here, three parameters in the function
/// the injected script hangs on `window`.
fn notify_call(enabled: bool, available: bool, hint: &str) -> String {
    let hint = serde_json::to_string(hint).expect("a string is always serializable");
    format!("window.__dshNotifyTurns && window.__dshNotifyTurns({enabled}, {available}, {hint})")
}

/// Say what is running, or `""` when nothing is. Everything the menu starts
/// reaches the network before it has anything to show — `npm view` can sit there
/// for fifteen seconds — and a menu item that leads to nothing visible is a menu
/// item the user clicks again.
pub fn busy(app: &AppHandle, text: &str) {
    let text = serde_json::to_string(text).expect("a string is always serializable");
    eval(
        app,
        &format!("window.__dshBusy && window.__dshBusy({text})"),
    );
}

/// Both of the above are calls into the injected script, which may not be there:
/// the window can be gone, and a document that has not finished loading has no
/// `__dsh*` on it yet — hence the guard in each. Every one of them repaints
/// something the next page load pushes again, so a call that lands nowhere
/// costs nothing.
///
/// Shared with the two cards that carry labels of their own — see
/// [`crate::panel::relabel`] and [`crate::setup::relabel`] — because what they
/// push is the same kind of call under the same conditions.
pub(crate) fn eval(app: &AppHandle, call: &str) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.eval(call);
    }
}

/// Every menu label, against the verb its entry signals, as the object the
/// script indexes by verb.
///
/// One list rather than a name apiece, because the menu is written twice now:
/// into [`script`] when a document loads, and again by [`relabel`] when dsh
/// changes language under a document that is not going to load a second time.
/// Two copies of these strings would be exactly the drift [`crate::i18n`] is
/// arranged to prevent.
///
/// JSON rather than the bare text: this is pasted into a JavaScript literal,
/// and a label is one apostrophe away from being a syntax error that takes the
/// whole titlebar with it.
fn labels() -> String {
    let labels = [
        // Not a menu row: the one button beside the menu, whose label is its
        // tooltip. It is here because this is where the app's two languages
        // live, and a string drawn in the titlebar is no more exempt from that
        // than a row in the panel below it.
        ("remote", t!("手机连接…", "Connect a phone…")),
        ("plugins", t!("插件…", "Plugins…")),
        ("terminal", t!("打开终端", "Open a terminal")),
        ("restart-dsh", t!("重启 dsh", "Restart dsh")),
        // Plain words rather than "safe mode": the row exists for a user whose
        // app would not start, and what they need to read off it is what
        // clicking it does.
        ("safe-off", t!("重新加载插件", "Load plugins again")),
        ("update-dsh", t!("更新 dsh…", "Update dsh…")),
        // The one row that opens a card rather than doing something. Everything
        // behind it is a setting nobody changes twice, and the menu is what
        // they were making long; see [`SETTINGS`] in [`script`].
        ("settings", t!("设置…", "Settings…")),
        ("settings-done", t!("关闭", "Close")),
        ("runtime", t!("运行环境…", "Runtime…")),
        ("registry", t!("安装源…", "Install source…")),
        ("check-app", t!("检查应用更新…", "Check for app updates…")),
        ("autostart", t!("开机自启动", "Start at login")),
        // Not "Notify when a turn finishes": the switch behind it gates every
        // notification this app raises, a plugin's included. See
        // `crate::settings`.
        ("notify-turns", t!("通知", "Notifications")),
        ("quit", t!("退出 dsh", "Quit dsh")),
    ];

    let map: serde_json::Map<String, serde_json::Value> = labels
        .into_iter()
        .map(|(verb, label)| (verb.to_string(), label.into()))
        .collect();

    serde_json::to_string(&map).expect("a map of strings is always serializable")
}

/// The line under each row of the settings card, against the same verb its
/// label is under.
///
/// Separate from [`labels`] rather than `"runtime-note"` keys in it, because
/// that map is one string per verb and [`every_menu_entry_is_labelled`] is what
/// keeps it that way. These exist for the rows that moved off the menu: a menu
/// row has only its own words to explain itself and has to be short, and
/// "Runtime…" on its own never said which of node and dsh it was about.
fn notes() -> String {
    let notes = [
        (
            "runtime",
            t!(
                "选哪个 Node 跑 dsh，以及装上或卸掉 dsh 本身。",
                "Which Node runs dsh, and installing or removing dsh itself."
            ),
        ),
        (
            "registry",
            t!(
                "dsh 从哪个 npm 源安装和更新。",
                "The npm registry dsh is installed and updated from."
            ),
        ),
        (
            "autostart",
            t!(
                "登录后自动把 dsh desktop 启动起来。",
                "Start dsh desktop after you log in."
            ),
        ),
        (
            "notify-turns",
            t!(
                "一轮跑完、或者 dsh 要问你点什么的时候提醒你。",
                "Tell you when a turn finishes, or when dsh stops to ask something."
            ),
        ),
    ];

    let map: serde_json::Map<String, serde_json::Value> = notes
        .into_iter()
        .map(|(verb, note)| (verb.to_string(), note.into()))
        .collect();

    serde_json::to_string(&map).expect("a map of strings is always serializable")
}

/// Put the menu into the language dsh has just switched to.
///
/// Only what is already drawn needs this. Everything else in the app reads the
/// language at the moment it draws — see [`crate::i18n::switch`] — but the menu
/// was drawn when the document loaded, and a language switch inside dsh does
/// not load another one.
pub fn relabel(app: &AppHandle) {
    // First, so that anything the pages draw in response to the labels below is
    // already in the new language; see `dist/index.html`.
    eval(
        app,
        &format!("window.__DSH_LANG__ = {:?};", crate::i18n::tag()),
    );
    eval(
        app,
        &format!(
            "window.__dshRelabel && window.__dshRelabel({}, {})",
            labels(),
            notes()
        ),
    );
}

/// `function make(tag, className, parent)`, for the injected scripts that build
/// a card out of elements: the titlebar's siblings in [`crate::dialog`],
/// [`crate::panel`] and [`crate::setup`].
///
/// Five lines, and it was five lines written out three times. Not because
/// anything about it is subtle, but because three copies of a helper is three
/// places a fourth card would be tempted to copy it from again.
pub(crate) fn dom_make() -> &'static str {
    r#"  function make(tag, className, parent) {
    var node = document.createElement(tag);
    if (className) node.className = className;
    if (parent) parent.appendChild(node);
    return node;
  }"#
}

/// `var LUCIDE`, the shape of every icon the cards draw, keyed by its name in
/// the set it came from.
///
/// Lucide 1.46.0, under the ISC licence. Copied rather than depended on: there
/// is no frontend build here — these scripts are injected into a page that
/// belongs to dsh — so an icon has to arrive as the literal path data it is
/// drawn from, and a `<img src>` or a webfont would be a fetch into a page whose
/// network this app does not own.
///
/// One set rather than each card drawing its own. The shapes here were hand
/// written, a few strokes at a time, against whatever viewBox the card that
/// wanted them happened to use — 16 in [`crate::panel`], 14 and 12 here — so
/// the same tick existed twice at two weights and the chevron on a settings row
/// was not a shape at all but a `›` out of the page's own font. Every one of
/// them is on Lucide's 24-unit grid now, at Lucide's stroke width, so a card
/// picks the pixel size and gets the same weight the others have.
///
/// Pasted into both scripts by [`crate::panel::script`] and [`script`], the way
/// [`dom_make`] is, so adding an icon is an edit here rather than in each.
pub(crate) fn lucide() -> &'static str {
    r#"  // Lucide 1.46.0 icons, ISC licence. https://lucide.dev
  var LUCIDE = {
    menu: '<path d="M4 5h16"/><path d="M4 12h16"/><path d="M4 19h16"/>',
    check: '<path d="M20 6 9 17l-5-5"/>',
    chevronRight: '<path d="m9 18 6-6-6-6"/>',
    info: '<circle cx="12" cy="12" r="10"/><path d="M12 16v-4"/>' +
      '<path d="M12 8h.01"/>',
    externalLink: '<path d="M15 3h6v6"/><path d="M10 14 21 3"/>' +
      '<path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/>',
    // Filled, which the set does not ship: a star this size drawn as an
    // outline reads as a scribble. The fill is the only change to the path.
    star: '<path fill="currentColor" d="M11.525 2.295a.53.53 0 0 1 .95 0l2.31 ' +
      '4.679a2.123 2.123 0 0 0 1.595 1.16l5.166.756a.53.53 0 0 1 .294.904l-3.7' +
      '36 3.638a2.123 2.123 0 0 0-.611 1.878l.882 5.14a.53.53 0 0 1-.771.56l-4' +
      '.618-2.428a2.122 2.122 0 0 0-1.973 0L6.396 21.01a.53.53 0 0 1-.77-.56l.' +
      '881-5.139a2.122 2.122 0 0 0-.611-1.879L2.16 9.795a.53.53 0 0 1 .294-.90' +
      '6l5.165-.755a2.122 2.122 0 0 0 1.597-1.16z"/>',
    user: '<path d="M19 21v-2a4 4 0 0 0-4-4H9a4 4 0 0 0-4 4v2"/>' +
      '<circle cx="12" cy="7" r="4"/>',
    smartphone: '<rect width="14" height="20" x="5" y="2" rx="2" ry="2"/>' +
      '<path d="M12 18h.01"/>',
    package: '<path d="M11 21.73a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16V8a2 2 0 0 0' +
      '-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73z"/>' +
      '<path d="M12 22V12"/><polyline points="3.29 7 12 12 20.71 7"/>' +
      '<path d="m7.5 4.27 9 5.15"/>',
    // Round again: the same plugin at the next version. What this replaced was
    // an arrow rising off a line, which is the shape an upload wears -- and a
    // plugin card sends nothing anywhere.
    refreshCw: '<path d="M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8"/>' +
      '<path d="M21 3v5h-5"/>' +
      '<path d="M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16"/>' +
      '<path d="M8 16H3v5"/>'
  };

  /** One icon at `size` pixels, on Lucide's own grid and at its own weight. */
  function lucide(name, size, className) {
    return '<svg' + (className ? ' class="' + className + '"' : '') +
      ' width="' + size + '" height="' + size + '" viewBox="0 0 24 24"' +
      ' fill="none" stroke="currentColor" stroke-width="2"' +
      ' stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' +
      LUCIDE[name] + '</svg>';
  }"#
}

/// `corner-shape: round`, for every element and pseudo-element under `roots`.
///
/// Round corners, in a page that draws squircles. `corner-shape` decides what
/// a `border-radius` is actually drawn as, and dsh's page has moved it off its
/// initial `round`: every component in dsh's own stylesheet writes
/// `corner-shape:round` beside its own radius to opt back out, which is a
/// thing nobody writes six times unless the default around it is something
/// else. That default reaches whatever is drawn into the document, every card
/// in this app included — so a knob written as a 50% circle came out a rounded
/// square, and the capsule under it came out a box, however the radius was
/// written.
///
/// The property is not inherited, so the pseudo-elements are named here as
/// well as the elements: the switch knob is a `::after`.
///
/// One function because there is one answer, and because the four cards are
/// four stylesheets — the same drift [`theme_watcher`] exists to prevent.
/// `every_card_takes_its_corners_back` pins that they all come from here.
///
/// A browser that has not shipped `corner-shape` drops the declaration and
/// goes on drawing the ellipse corners it always drew, which is the same
/// picture.
pub(crate) fn corners(roots: &[&str]) -> String {
    let selector = roots
        .iter()
        .flat_map(|root| {
            [
                format!(".{root}"),
                format!(".{root} *"),
                format!(".{root} *::before"),
                format!(".{root} *::after"),
            ]
        })
        .collect::<Vec<_>>()
        .join(",");

    format!("{selector}{{corner-shape:round}}")
}

/// `function paint(node)`, which puts `dark_class` on `node` for as long as
/// dsh's page is dark and takes it off again when it is not.
///
/// dsh's theme is the *page's*, not the window's: it writes `color-scheme` on
/// the root element and `data-ds-dark-theme` on the body, and switching it
/// inside the UI changes both, and the page is the first thing to know. So
/// every card this app draws reads the page, and falls back to the media query
/// only where the page says nothing either way — which is dsh's boot, and is
/// the right answer there because the window answers that query with dsh's own
/// preference; see [`crate::theme`].
///
/// One function because there is one answer. This was written out four times —
/// once per card — and each copy carried a comment telling the next reader to
/// keep it in step with the others by hand. Nothing checked that they were, so
/// a correction to how dsh's theme is read would have been a correction to one
/// card and a silent divergence in three. `every_card_reads_the_theme_the_same_way`
/// now pins that they all come from here.
pub(crate) fn theme_watcher(dark_class: &str) -> String {
    format!(
        r#"  function paint(node) {{
    var media = window.matchMedia('(prefers-color-scheme:dark)');

    function dark() {{
      if (document.body.hasAttribute('data-ds-dark-theme')) return true;
      var declared = getComputedStyle(document.documentElement).colorScheme || '';
      var light = declared.indexOf('light') !== -1;
      var night = declared.indexOf('dark') !== -1;
      return night !== light ? night : media.matches;
    }}

    function repaint() {{
      node.classList.toggle({dark_class:?}, dark());
      // One hook, for the piece of this that is not a class on a card: the
      // strip the titlebar takes off the top of the page, whose colour has to
      // be worked out from what the page underneath has painted rather than
      // from a stylesheet. See `band` in [`script`]. Called from every card's
      // repaint, so it follows whichever of them notices a change first.
      if (window.__dshThemePainted) window.__dshThemePainted();
    }}

    repaint();
    var watch = new MutationObserver(repaint);
    watch.observe(document.documentElement, {{
      attributes: true, attributeFilter: ['style', 'class', 'data-theme']
    }});
    watch.observe(document.body, {{
      attributes: true, attributeFilter: ['style', 'class', 'data-ds-dark-theme']
    }});
    media.addEventListener('change', repaint);
  }}"#
    )
}

/// The script that draws all of it, injected into every document the window
/// loads.
///
/// The dots carry their own colour, the same three macOS uses, so there is
/// nothing about them that has to follow dsh's theme — they read the same
/// against a light page and a dark one. The menu does not have that luxury: it
/// is text on a panel, so it follows the theme of the page it is drawn over;
/// see [`theme_watcher`], which is where that reading lives for every card in
/// the app.
pub fn script() -> String {
    let titlebar_height = TITLEBAR_HEIGHT;
    let dot = DOT;
    let gap = DOT_GAP;
    let pad = ROW_PAD;
    let night = crate::theme::dark_css();

    let labels = labels();
    let notes = notes();
    let maker = dom_make();
    let icons = lucide();
    let watcher = theme_watcher("dsh-wc-dark");
    let corners = corners(&["dsh-wc", "dsh-wc-set", "dsh-wc-scrim"]);

    format!(
        r#"(function () {{
  // The page in the window, never a frame inside it. On Windows an
  // initialization script is registered through WebView2's
  // `AddScriptToExecuteOnDocumentCreated`, which runs it in every frame the
  // webview creates -- wry's `for_main_only` is a no-op there, it hands the
  // script straight over. dsh embeds pages it does not own in iframes, the
  // plugin market's comment dialog (giscus) among them, and each of them was
  // getting a titlebar of its own: the dots over its top-left corner, its body
  // pushed down by the padding rule below and then clipped to the frame by the
  // height and overflow ones, which cut the comment box off entirely.
  if (window.top !== window.self) return;
  if (window.__dshWindowControls) return;
  window.__dshWindowControls = true;

  // Drawn small and only shown on hover, as macOS does: idle, the dots are
  // just colour.
  var ICONS = {{
    minimize: '<path d="M2.4 5h5.2"/>',
    maximize: '<path fill="currentColor" stroke="none" d="M2.2 3.6v4.2h4.2z"/>' +
      '<path fill="currentColor" stroke="none" d="M7.8 6.4V2.2H3.6z"/>',
    // The same diagonal as `maximize`, with the right angles turned inward.
    // Smaller than that pair, because two triangles pointing at each other
    // this size would meet in the middle and read as one blob.
    restore: '<path fill="currentColor" stroke="none" d="M4.6 5.4H1L4.6 9z"/>' +
      '<path fill="currentColor" stroke="none" d="M5.4 4.6H9L5.4 1z"/>',
    close: '<path d="M2.9 2.9l4.2 4.2m0-4.2l-4.2 4.2"/>'
  }};

  // The labels come from Rust so there is one place the two languages live;
  // see `i18n`. Keyed by verb rather than laid out in order, because
  // `__dshRelabel` sends this same shape again when dsh changes language.
  var LABELS = {labels};
  // The second line each settings row carries; see `notes` in controls.rs.
  var NOTES = {notes};

  // The menu, top to bottom. `panel` marks the one row that opens a card of
  // this app's own instead of signalling Rust.
  //
  // What is here is what someone reaches for while using dsh. Five rows that
  // were not — which Node, which registry, the app updater, and the two
  // switches — are in the card below: the menu had grown to eleven rows and
  // four unlabelled rules, and picking the one you wanted out of it had become
  // reading rather than aiming.
  var ITEMS = [
    {{ verb: 'plugins' }},
    {{ verb: 'terminal' }},
    {{ separator: true }},
    {{ verb: 'restart-dsh' }},
    // Hidden until there is something to undo. See `__dshSafeMode`.
    {{ verb: 'safe-off', hidden: true }},
    {{ verb: 'update-dsh' }},
    // Beside the other update rather than on the card behind `settings`: the
    // two read as one pair — dsh, and the app dsh is running in — and neither
    // is a setting. The card is for what is configured once; this is a thing
    // done, and it was the one row on that card with nothing to remember.
    {{ verb: 'check-app' }},
    {{ separator: true }},
    {{ verb: 'settings', panel: true }},
    {{ separator: true }},
    {{ verb: 'quit' }}
  ];

  // The settings card, top to bottom. `check` marks a row that carries state
  // and stays put when it is clicked; the rest act and close the card.
  var SETTINGS = [
    // One row for the whole of which Node and which dsh, rather than one per
    // verb: the panel behind it can switch, install and uninstall. See `setup`.
    {{ verb: 'runtime' }},
    // Where dsh is fetched from, which is only ever a question on a machine
    // whose npm is pointed somewhere of the user's own; see `settings.rs`.
    {{ verb: 'registry' }},
    {{ verb: 'autostart', check: true }},
    {{ verb: 'notify-turns', check: true }},
    // The way out that is visible. Escape and the scrim close it too, and
    // neither is something a user finds by looking at the card.
    {{ verb: 'settings-done', close: true }}
  ];

{icons}

  // 12, which is the diameter of a dot. The three bars sit beside the three
  // dots and were drawn two pixels wider than them, so the one thing in the
  // bar that is not a window control was the largest thing in it.
  var MENU_GLYPH = lucide('menu', 12);

  // The one button in the bar that is not a window control and not the menu.
  // It sits to the menu's right because that is the only other place in this
  // strip that is reliably empty, and it is a button rather than a menu row
  // because what it opens is a thing you do — see `remote`.
  //
  // Written with a `verb:` the way the menu's rows are, so that the label check
  // in controls.rs's tests counts it like the rest of them.
  var PHONE = {{ verb: 'remote' }};
  var PHONE_GLYPH = lucide('smartphone', 13);


  // On every row of the settings card that opens something else. It was a `›`
  // until now -- a character, so it was set in whatever font dsh's page had
  // loaded, at whatever weight that font draws it, beside icons that are
  // strokes. See `LUCIDE`.
  var GO_GLYPH = lucide('chevronRight', 14);

  function svg(shape) {{
    return '<svg width="8" height="8" viewBox="0 0 10 10" fill="none" ' +
      'stroke="currentColor" stroke-width="1.3" stroke-linecap="round">' + shape + '</svg>';
  }}

  // The whole channel back to Rust; see controls.rs. The navigation is
  // cancelled there, so the page it is called from stays exactly where it is.
  function signal(verb) {{
    window.location.href = '{SCHEME}://' + verb;
  }}

  // --------------------------------------------------------- the elements --
  // Pasted in from `dom_make`, the one this app's cards are built with. The
  // titlebar used to have no card in it and built its row by hand; the
  // settings panel below is a card, so it uses the shared helper.
{maker}

  // ----------------------------------------------------------- the theme --
  // Pasted in from `controls::theme_watcher`, which every card in this app
  // draws its dark mode from; the rationale is there.
{watcher}

  // ------------------------------------------------------------- links --
  function isExternal(url) {{
    if (!url || typeof url !== 'string') return false;
    try {{
      var parsed = new URL(url, window.location.href);
      if (parsed.protocol === '{SCHEME}:' || parsed.protocol === 'javascript:' || parsed.protocol === 'about:' || parsed.protocol === 'blob:' || parsed.protocol === 'data:') {{
        return false;
      }}
      if (parsed.protocol === 'http:' || parsed.protocol === 'https:') {{
        if (parsed.origin === window.location.origin || parsed.hostname === 'tauri.localhost') {{
          return false;
        }}
        return true;
      }}
      return true;
    }} catch (e) {{
      return false;
    }}
  }}

  function openExternal(url) {{
    try {{
      var parsed = new URL(url, window.location.href);
      signal('open?url=' + encodeURIComponent(parsed.href));
    }} catch (e) {{
      signal('open?url=' + encodeURIComponent(url));
    }}
  }}

  document.addEventListener('click', function (event) {{
    if (event.defaultPrevented) return;
    if (event.button !== 0) return;
    var el = event.target;
    while (el && el !== document && el.tagName !== 'A') {{
      el = el.parentElement;
    }}
    if (!el || !el.href) return;
    if (isExternal(el.href)) {{
      event.preventDefault();
      event.stopPropagation();
      openExternal(el.href);
    }}
  }}, true);

  var origOpen = window.open;
  window.open = function (url, target, features) {{
    if (url && isExternal(url)) {{
      openExternal(url);
      return null;
    }}
    return origOpen ? origOpen.apply(this, arguments) : null;
  }};

  function start() {{
    var style = document.createElement('style');
    style.textContent =
      ':root{{--dsh-titlebar-height:{titlebar_height}px;}}' +
      // Everything the menu is drawn out of, in one place and in both themes.
      // On the two roots rather than on `:root`, so `dsh-wc-dark` -- put on by
      // `repaint` below out of what the page says, not out of the media query
      // -- is all it takes to swap the set.
      //
      // Two roots because the settings card is not inside the bar: the bar is
      // drawn at `opacity:.85`, which every descendant inherits, and a card
      // washed out to 85% is not a card. So it is a sibling on the body, with
      // its own copy of the set and its own `paint`.
      '.dsh-wc,.dsh-wc-set{{--dsh-wc-fg:rgba(0,0,0,.55);--dsh-wc-fg-hi:rgba(0,0,0,.85);' +
      '--dsh-wc-panel:rgba(255,255,255,.96);--dsh-wc-line:rgba(0,0,0,.09);' +
      '--dsh-wc-hover:rgba(0,0,0,.06);--dsh-wc-accent:#4d6bfe;' +
      // The switch track when it is off. Not `--dsh-wc-line`, which is the
      // hairline between rows: at 9% a track reads as absent rather than as
      // off, and the switch was the one control on the card you had to look
      // twice at to find.
      '--dsh-wc-sw-off:rgba(0,0,0,.22);' +
      '--dsh-wc-shadow:0 12px 32px rgba(0,0,0,.18),0 0 0 .5px rgba(0,0,0,.09);}}' +
      '.dsh-wc.dsh-wc-dark,.dsh-wc-set.dsh-wc-dark{{' +
      '--dsh-wc-fg:rgba(255,255,255,.62);--dsh-wc-fg-hi:rgba(255,255,255,.94);' +
      '--dsh-wc-panel:rgba(42,42,46,.96);--dsh-wc-line:rgba(255,255,255,.11);' +
      '--dsh-wc-hover:rgba(255,255,255,.09);--dsh-wc-sw-off:rgba(255,255,255,.26);' +
      '--dsh-wc-shadow:0 12px 32px rgba(0,0,0,.5),0 0 0 .5px rgba(255,255,255,.09);}}' +
      'html,body{{height:100%!important;margin:0!important;overflow:hidden!important;}}' +
      '#root{{height:calc(100% - var(--dsh-titlebar-height))!important;margin-top:var(--dsh-titlebar-height)!important;box-sizing:border-box!important;}}' +
      'body:not(:has(#root)){{padding-top:var(--dsh-titlebar-height)!important;box-sizing:border-box!important;}}' +
      // The corners this bar and its card draw, taken back from the page; see
      // `corners`. The two roots are separate because the card is not inside
      // the bar.
      '{corners}' +
      '.dsh-wc{{position:fixed;top:0;left:0;z-index:2147483647;display:flex;' +
      'align-items:center;height:{titlebar_height}px;padding:0 {pad}px;' +
      'opacity:.85;transition:opacity .2s ease;pointer-events:none}}' +
      '.dsh-wc-dots{{display:flex;align-items:center;gap:{gap}px}}' +
      // `dsh-wc-on` is put on by the magnification below whenever the pointer
      // is near the row, so the glyphs come up as it approaches rather than
      // one at a time as it crosses each dot.
      '.dsh-wc-on{{opacity:1}}' +
      // Only the dots take the pointer; the padding between and around them
      // lets clicks through to whatever dsh draws underneath.
      //
      // `will-change` is what keeps the motion smooth rather than crunchy: it
      // puts each dot on its own compositor layer, so resizing one does not
      // re-rasterize its ring and glyph a frame at a time.
      //
      // The transition is short because the transform is rewritten every frame
      // the pointer moves — it is there to smooth the steps between frames,
      // not to carry the animation. At rest it stretches out, so the row
      // settles back gently once the pointer leaves.
      '.dsh-wc button{{all:unset;pointer-events:auto;width:{dot}px;height:{dot}px;' +
      'border-radius:50%;display:grid;place-items:center;cursor:pointer;' +
      'color:rgba(0,0,0,.55);box-shadow:inset 0 0 0 .5px rgba(0,0,0,.12);' +
      'will-change:transform;transition:transform .13s cubic-bezier(.2,.9,.24,1),' +
      'filter .2s ease}}' +
      //
      // Down to the dots, these three, rather than to every button in the bar:
      // the menu's panel hangs off the bar too, and a rule that hides — or
      // reveals — the glyph in any button reaches the checkmark in a menu item
      // as well. `dsh-wc-on` comes on while the pointer is anywhere near the
      // row, which is where it is when the menu is opened, so an unchecked
      // login item wore a tick until the pointer moved off down the panel.
      '.dsh-wc:not(.dsh-wc-on) .dsh-wc-dots button{{transition-duration:.34s}}' +
      '.dsh-wc-dots button svg{{opacity:0;will-change:transform;' +
      'transition:opacity .2s ease,transform .13s cubic-bezier(.2,.9,.24,1)}}' +
      '.dsh-wc-on .dsh-wc-dots button svg{{opacity:1}}' +
      '.dsh-wc button:active{{filter:brightness(.85)}}' +
      // Qualified by `.dsh-wc button` so they outweigh its `all:unset`, which
      // would otherwise take the colour straight back off again.
      '.dsh-wc button.dsh-wc-close{{background:#ff5f57}}' +
      '.dsh-wc button.dsh-wc-min{{background:#febc2e}}' +
      '.dsh-wc button.dsh-wc-max{{background:#28c840}}' +
      // The menu button. Enough room from the dots that the magnification below
      // can push the green one sideways without the two touching.
      '.dsh-wc button.dsh-wc-menu{{width:24px;height:24px;margin-left:16px;' +
      'border-radius:7px;background:none;box-shadow:none;color:var(--dsh-wc-fg);' +
      'transition:background .15s ease,color .15s ease}}' +
      '.dsh-wc button.dsh-wc-menu:hover,.dsh-wc button.dsh-wc-menu.dsh-wc-shown{{' +
      'background:var(--dsh-wc-hover);color:var(--dsh-wc-fg-hi)}}' +
      '.dsh-wc button.dsh-wc-menu:active{{filter:none}}' +
      // Beside the menu rather than away from it: the gap before the menu is
      // there to keep the magnification off the green dot, and there is no dot
      // on this side to keep clear of.
      '.dsh-wc button.dsh-wc-phone{{margin-left:2px}}' +
      // The panel. `visibility` rather than `display` so the fade has something
      // to fade, with its own transition delayed until the opacity is done.
      '.dsh-wc-pop{{position:absolute;top:calc(100% - 3px);left:0;min-width:184px;' +
      'padding:6px;box-sizing:border-box;background:var(--dsh-wc-panel);' +
      'border-radius:12px;box-shadow:var(--dsh-wc-shadow);' +
      '-webkit-backdrop-filter:blur(24px) saturate(180%);' +
      'backdrop-filter:blur(24px) saturate(180%);' +
      'opacity:0;visibility:hidden;transform:translateY(-6px) scale(.97);' +
      'transform-origin:16px top;pointer-events:none;' +
      'transition:opacity .14s ease,transform .14s cubic-bezier(.2,.9,.24,1),' +
      'visibility 0s .14s}}' +
      '.dsh-wc-pop.dsh-wc-shown{{opacity:1;visibility:visible;transform:none;' +
      'pointer-events:auto;transition-delay:0s}}' +
      '.dsh-wc-pop button{{all:unset;box-sizing:border-box;pointer-events:auto;' +
      'display:flex;align-items:center;gap:10px;width:100%;height:30px;' +
      'padding:0 10px;border-radius:7px;cursor:pointer;white-space:nowrap;' +
      'color:var(--dsh-wc-fg-hi);font:13px/1 {FONT}}}' +
      // The row above sets `display`, and an author rule outranks the UA's
      // `[hidden]{{display:none}}` however the attribute is spelled — `all:unset`
      // would have taken that rule out even without the `display:flex`. So the
      // one row that comes and goes needs its own way to go; see
      // `__dshSafeMode`. More specific than the rule it is correcting, which is
      // what lets it win without `!important`.
      '.dsh-wc-pop button[hidden]{{display:none}}' +
      '.dsh-wc-pop button:hover{{background:var(--dsh-wc-hover)}}' +
      '.dsh-wc-pop hr{{border:0;height:1px;margin:5px 8px;' +
      'background:var(--dsh-wc-line)}}' +
      // A switch whose precondition is missing. Dimmed and inert rather than
      // hidden: a setting that vanishes is one the user cannot find again to
      // ask why. `title` says what is missing; see `sync_notify`. On the card
      // and nowhere else: the one row this is ever about lives there, and so
      // does every entry in `checks`, which is how `sync_notify` finds it.
      '.dsh-wc-set button.dsh-wc-unavailable{{opacity:.4;cursor:default}}' +
      '.dsh-wc-set button.dsh-wc-unavailable:hover{{background:none}}' +
      // ---------------------------------------------- the settings card --
      // Behind everything, and the click that closes the card without
      // choosing anything. Under the drag strip as well as under the bar, so
      // that the window can still be moved and closed while the card is up --
      // the same thing `dialog` is careful about, for the same reason: this is
      // modal to the page, not to the operating system.
      '.dsh-wc-scrim{{position:fixed;inset:0;z-index:2147483645;' +
      'background:rgba(0,0,0,.28);opacity:0;visibility:hidden;' +
      'transition:opacity .16s ease,visibility 0s .16s}}' +
      '.dsh-wc-scrim.dsh-wc-shown{{opacity:1;visibility:visible;' +
      'transition-delay:0s}}' +
      // Centred rather than hung off the menu button: it is a page of its own
      // now, not a longer menu, and the rows have two lines each.
      '.dsh-wc-set{{position:fixed;top:50%;left:50%;z-index:2147483647;' +
      'width:min(420px,calc(100vw - 48px));max-height:calc(100vh - 96px);' +
      'overflow:auto;box-sizing:border-box;padding:14px;' +
      'background:var(--dsh-wc-panel);border-radius:14px;' +
      'box-shadow:var(--dsh-wc-shadow);' +
      '-webkit-backdrop-filter:blur(24px) saturate(180%);' +
      'backdrop-filter:blur(24px) saturate(180%);' +
      'opacity:0;visibility:hidden;pointer-events:none;' +
      'transform:translate(-50%,-48%) scale(.98);' +
      'transition:opacity .16s ease,' +
      'transform .16s cubic-bezier(.2,.9,.24,1),visibility 0s .16s}}' +
      '.dsh-wc-set.dsh-wc-shown{{opacity:1;visibility:visible;' +
      'pointer-events:auto;transform:translate(-50%,-50%) scale(1);' +
      'transition-delay:0s}}' +
      '.dsh-wc-set-head{{display:flex;align-items:center;' +
      'margin:2px 4px 10px;color:var(--dsh-wc-fg-hi);' +
      'font:600 14px/1 {FONT}}}' +
      '.dsh-wc-set button{{all:unset;box-sizing:border-box;pointer-events:auto;' +
      'display:flex;align-items:center;gap:12px;width:100%;' +
      'padding:9px 10px;border-radius:9px;cursor:pointer;' +
      'color:var(--dsh-wc-fg-hi);font:13px/1 {FONT}}}' +
      '.dsh-wc-set button:hover{{background:var(--dsh-wc-hover)}}' +
      // `all: unset` above took the focus ring with it, and this card is
      // opened with the keyboard as readily as with the pointer.
      '.dsh-wc-set button:focus-visible{{outline:2px solid var(--dsh-wc-accent);' +
      'outline-offset:2px}}' +
      '.dsh-wc-set button.dsh-wc-set-shut{{justify-content:center;' +
      'margin-top:8px;color:var(--dsh-wc-fg);' +
      'box-shadow:inset 0 0 0 1px var(--dsh-wc-line)}}' +
      '.dsh-wc-set-text{{display:flex;flex-direction:column;gap:4px;' +
      'min-width:0;text-align:left}}' +
      '.dsh-wc-set-note{{color:var(--dsh-wc-fg);font:11px/1.45 {FONT};' +
      'white-space:normal}}' +
      // The one glyph that says "this opens something else". A flex box
      // rather than a font size, because what is in it is a shape now.
      '.dsh-wc-set-go{{display:flex;margin-left:auto;flex:none;' +
      'color:var(--dsh-wc-fg);opacity:.75}}' +
      // A switch rather than the menu's checkmark: a row two lines tall with a
      // tick floating beside it reads as a list item, not as something on or
      // off. Driven by the same `dsh-wc-checked` class `mark` already sets, so
      // nothing about how state arrives changed.
      //
      // 38 by 22 around a knob of 16, rather than 34 by 20 around one of 16.
      // The old pair left two pixels of track above and below the knob and
      // barely more at the ends, so the knob was the switch and the track was
      // a rim around it -- which is what made it read as flat however round it
      // actually was. Three pixels of clearance and a longer throw give the
      // knob somewhere to travel, which is the whole of what the control says.
      '.dsh-wc-sw{{position:relative;margin-left:auto;flex:none;' +
      'width:38px;height:22px;border-radius:999px;' +
      'background:var(--dsh-wc-sw-off);transition:background .16s ease}}' +
      // Double quotes inside a single-quoted string: this whole stylesheet is
      // JavaScript, and an apostrophe here would end the string it is in.
      '.dsh-wc-sw::after{{content:"";position:absolute;top:3px;left:3px;' +
      'width:16px;height:16px;border-radius:50%;background:#fff;' +
      'box-shadow:0 1px 2px rgba(0,0,0,.24),0 0 0 .5px rgba(0,0,0,.04);' +
      'transition:transform .16s cubic-bezier(.2,.9,.24,1)}}' +
      '.dsh-wc-set button.dsh-wc-checked .dsh-wc-sw{{' +
      'background:var(--dsh-wc-accent)}}' +
      '.dsh-wc-set button.dsh-wc-checked .dsh-wc-sw::after{{' +
      'transform:translateX(16px)}}' +
      // Standing, not passing: the launch is running on no plugins, and says so
      // for as long as that is true. Drawn as an outline rather than as text so
      // it does not read as another of the toast's transient messages, and left
      // to `pointer-events:none` from the bar so the window is still draggable
      // by it — the explanation lives on the menu row, which can be hovered.
      '.dsh-wc-safe{{display:flex;align-items:center;margin-left:12px;' +
      'height:19px;padding:0 8px;border:1px solid var(--dsh-wc-line);' +
      'border-radius:999px;color:var(--dsh-wc-fg);font:11px/1 {FONT};' +
      'white-space:nowrap}}' +
      // Sets its own `display`, so the UA's `[hidden]` rule loses here exactly
      // as it does on a menu row. See `.dsh-wc-pop button[hidden]`.
      '.dsh-wc-safe[hidden]{{display:none}}' +
      // What is running right now, beside the menu button that started it.
      '.dsh-wc-toast{{display:flex;align-items:center;gap:7px;margin-left:12px;' +
      'color:var(--dsh-wc-fg);font:12px/1 {FONT};white-space:nowrap;' +
      'opacity:0;transform:translateX(-4px);' +
      'transition:opacity .2s ease,transform .2s ease}}' +
      '.dsh-wc-toast.dsh-wc-shown{{opacity:1;transform:none}}' +
      '.dsh-wc-spin{{width:11px;height:11px;border-radius:50%;flex:none;' +
      'border:1.5px solid var(--dsh-wc-line);border-top-color:var(--dsh-wc-fg)}}' +
      // Only while the toast is up. The toast is hidden with `opacity`, which
      // leaves the spinner in the render tree, and an animation there never
      // stops ticking: a frame source that outlives what it is drawing keeps
      // the compositor awake for as long as the app runs. Measured at 1.6% of
      // a core with nothing on screen to show for it -- the same rule costs
      // 0.5% in a plain browser window, so the embedded compositing path makes
      // it three times worse than the mistake looks.
      '.dsh-wc-toast.dsh-wc-shown .dsh-wc-spin{{' +
      'animation:dsh-wc-spin .7s linear infinite}}' +
      '@keyframes dsh-wc-spin{{to{{transform:rotate(360deg)}}}}' +
      '.dsh-wc-drag{{position:fixed;top:0;left:0;right:0;' +
      'height:{titlebar_height}px;z-index:2147483646}}';
    document.head.appendChild(style);

    var drag = document.createElement('div');
    drag.className = 'dsh-wc-drag';
    // mousedown, not click: the OS takes the drag from here, and it will only
    // do that while the button is still down.
    drag.addEventListener('mousedown', function (event) {{
      if (event.button === 0) signal('drag');
    }});
    drag.addEventListener('dblclick', function () {{
      signal('maximize');
    }});

    var bar = document.createElement('div');
    bar.className = 'dsh-wc';

    var row = document.createElement('div');
    row.className = 'dsh-wc-dots';
    bar.appendChild(row);

    function add(verb, shape, extra) {{
      var button = document.createElement('button');
      button.type = 'button';
      button.className = extra || '';
      button.innerHTML = svg(shape);
      button.addEventListener('click', function () {{
        signal(verb);
      }});
      row.appendChild(button);
      return button;
    }}

    // macOS order, left to right.
    add('close', ICONS.close, 'dsh-wc-close');
    add('minimize', ICONS.minimize, 'dsh-wc-min');
    var zoom = add('maximize', ICONS.maximize, 'dsh-wc-max');

    // Called from Rust on every resize; see `sync`.
    window.__dshMaximized = function (maximized) {{
      zoom.innerHTML = svg(maximized ? ICONS.restore : ICONS.maximize);
    }};

    // ------------------------------------------------------------- the menu --

    var opener = document.createElement('button');
    opener.type = 'button';
    opener.className = 'dsh-wc-menu';
    opener.innerHTML = MENU_GLYPH;
    bar.appendChild(opener);

    // ------------------------------------------------------------ the phone --

    var phone = document.createElement('button');
    phone.type = 'button';
    phone.className = 'dsh-wc-menu dsh-wc-phone';
    phone.innerHTML = PHONE_GLYPH;
    phone.title = LABELS[PHONE.verb] || '';
    phone.setAttribute('aria-label', phone.title);
    phone.addEventListener('click', function () {{
      signal(PHONE.verb);
    }});
    bar.appendChild(phone);

    var safe = document.createElement('div');
    safe.className = 'dsh-wc-safe';
    safe.hidden = true;
    bar.appendChild(safe);

    var toast = document.createElement('div');
    toast.className = 'dsh-wc-toast';
    var spinner = document.createElement('div');
    spinner.className = 'dsh-wc-spin';
    var said = document.createElement('span');
    toast.appendChild(spinner);
    toast.appendChild(said);
    bar.appendChild(toast);

    var pop = document.createElement('div');
    pop.className = 'dsh-wc-pop';
    // The settings card's switches, by verb. Declared out here rather than
    // with the card because `mark` and `__dshNotifyTurns` reach it from the
    // same closure; nothing in the menu writes to it. The menu used to, for
    // the two switches that were rows on it, and drew a checkmark beside each
    // -- both the branch and the glyph went when the switches moved to the
    // card, since a row that is never `check` cannot fill either.
    var checks = {{}};
    // Kept for the same reason `checks` is: something out here changes them
    // after they are drawn. See `__dshRelabel`.
    //
    // A list per verb rather than one element, because a verb is drawn in two
    // places now: `settings` is a menu row and the heading of the card that
    // row opens, and a language switch has to move both.
    var spans = {{}};
    // The second line of a settings row, keyed the same way and relabelled
    // alongside the first; see `__dshRelabel`.
    var noteSpans = {{}};
    // And the rows themselves, for the one that comes and goes.
    var entries = {{}};

    ITEMS.forEach(function (item) {{
      if (item.separator) {{
        pop.appendChild(document.createElement('hr'));
        return;
      }}

      var entry = document.createElement('button');
      entry.type = 'button';
      entries[item.verb] = entry;
      if (item.hidden) entry.hidden = true;
      var label = document.createElement('span');
      label.textContent = LABELS[item.verb];
      (spans[item.verb] = spans[item.verb] || []).push(label);
      entry.appendChild(label);
      entry.addEventListener('click', function () {{
        // A row whose precondition is missing is inert. It is not a `disabled`
        // button, so that it can still be hovered for the reason why; this is
        // what makes it refuse. Keyboard included: Enter on a button arrives
        // here as a click.
        if (entry.getAttribute('aria-disabled') === 'true') return;
        // Closed first: the verb can end in a modal, and a menu still hanging
        // open behind it is a menu that is open again when the modal goes.
        shut();
        // The one row that goes nowhere near Rust: the card it opens is drawn
        // here, out of labels this script already holds, and everything on it
        // signals for itself.
        if (item.panel) showSettings();
        else signal(item.verb);
      }});
      pop.appendChild(entry);
    }});

    bar.appendChild(pop);

    var open = false;

    function shut() {{
      open = false;
      pop.classList.remove('dsh-wc-shown');
      opener.classList.remove('dsh-wc-shown');
    }}

    opener.addEventListener('click', function (event) {{
      event.stopPropagation();
      if (open) return shut();
      open = true;
      // Under the button wherever the row has put it, rather than at a measured
      // offset this would have to be kept in step with.
      pop.style.left = opener.offsetLeft + 'px';
      pop.classList.add('dsh-wc-shown');
      opener.classList.add('dsh-wc-shown');
    }});

    // Capturing, so a page that stops the event on its own elements cannot
    // leave the menu stuck open.
    document.addEventListener('mousedown', function (event) {{
      if (open && !pop.contains(event.target) && !opener.contains(event.target)) shut();
    }}, true);
    document.addEventListener('keydown', function (event) {{
      if (open && event.key === 'Escape') shut();
    }});
    // A dialog taking the focus is one of the ways a click here ends.
    window.addEventListener('blur', function () {{
      if (open) shut();
    }});

    // --------------------------------------------------------- the settings --
    //
    // Four rows that used to be menu items. None of them is reached mid-task —
    // which Node runs dsh, which registry it comes from, and the two switches —
    // and together they were more than half the menu.
    //
    // Everything on it is drawn from `LABELS` and `NOTES`, which this script
    // already holds, and every row signals the verb it always signalled. So
    // Rust gained no verb for this card: `settings` opens it here, the rows
    // reach the same handlers they reached from the menu, and `mark` finds the
    // switches where it always looked — under `checks`, by verb.

    var scrim = make('div', 'dsh-wc-scrim');
    var card = make('div', 'dsh-wc-set');
    card.setAttribute('role', 'dialog');
    card.setAttribute('aria-modal', 'true');
    card.setAttribute('aria-labelledby', 'dsh-wc-settings-title');

    var head = make('div', 'dsh-wc-set-head', card);
    head.id = 'dsh-wc-settings-title';
    spans['settings'].push(head);

    // The card's heading is the menu row's own words, minus the ellipsis: the
    // "…" is a promise that clicking opens something, and on the thing it
    // opened that promise has already been kept. One setter rather than one
    // strip here and another in `__dshRelabel`, which is the copy that would
    // have been forgotten.
    function word(node, text) {{
      node.textContent = node === head ? String(text).replace(/…$/, '') : text;
    }}

    word(head, LABELS['settings']);

    SETTINGS.forEach(function (item) {{
      var row = make('button', item.close ? 'dsh-wc-set-shut' : '', card);
      row.type = 'button';

      var text = make('span', 'dsh-wc-set-text', row);
      var name = make('span', '', text);
      word(name, LABELS[item.verb]);
      (spans[item.verb] = spans[item.verb] || []).push(name);

      if (NOTES[item.verb]) {{
        var note = make('span', 'dsh-wc-set-note', text);
        note.textContent = NOTES[item.verb];
        noteSpans[item.verb] = note;
      }}

      if (item.check) {{
        make('span', 'dsh-wc-sw', row);
        checks[item.verb] = row;
      }} else if (!item.close) {{
        make('span', 'dsh-wc-set-go', row).innerHTML = GO_GLYPH;
      }}

      row.addEventListener('click', function () {{
        // The same refusal a menu row makes; see the handler above.
        if (row.getAttribute('aria-disabled') === 'true') return;
        // A switch leaves the card where it is: the user came here to set it,
        // and Rust answers by marking this very row. Everything else either
        // leaves for a card of its own or is the way out.
        if (!item.check) hideSettings();
        if (!item.close) signal(item.verb);
      }});
    }});

    // Where the focus was when the card went up, so it can be handed back.
    var returnTo = null;

    function showSettings() {{
      returnTo = document.activeElement;
      scrim.classList.add('dsh-wc-shown');
      card.classList.add('dsh-wc-shown');
      var first = card.querySelector('button');
      if (first) first.focus();
    }}

    function hideSettings() {{
      if (!card.classList.contains('dsh-wc-shown')) return;
      scrim.classList.remove('dsh-wc-shown');
      card.classList.remove('dsh-wc-shown');
      if (returnTo && returnTo.focus) returnTo.focus();
      returnTo = null;
    }}

    scrim.addEventListener('mousedown', hideSettings);
    document.addEventListener('keydown', function (event) {{
      if (!card.classList.contains('dsh-wc-shown')) return;
      if (event.key === 'Escape') return hideSettings();
      // Kept inside the card, the way `dialog` keeps its own: without this,
      // Tab walks out of a modal and into dsh's page behind it.
      if (event.key !== 'Tab') return;
      var rows = card.querySelectorAll('button');
      if (!rows.length) return;
      var edge = rows[event.shiftKey ? 0 : rows.length - 1];
      if (document.activeElement !== edge) return;
      event.preventDefault();
      rows[event.shiftKey ? rows.length - 1 : 0].focus();
    }});

    // Called from Rust; see `sync_autostart`, `sync_notify` and `busy`.
    function mark(verb, on) {{
      var entry = checks[verb];
      if (entry) entry.classList.toggle('dsh-wc-checked', !!on);
    }}

    window.__dshAutostart = function (on) {{
      mark('autostart', on);
    }};
    // `on` is the preference, `available` whether anything can act on it, and
    // `hint` what is missing when it cannot. An unavailable row reads as off
    // whatever the stored preference says, because that is what it is: see
    // `notify::show`, which gates on the same answer.
    window.__dshNotifyTurns = function (on, available, hint) {{
      var usable = available === undefined || !!available;
      mark('notify-turns', usable && on);
      var entry = checks['notify-turns'];
      if (!entry) return;
      entry.classList.toggle('dsh-wc-unavailable', !usable);
      // Marked rather than `disabled`, deliberately. A disabled button takes
      // no pointer events, and a browser shows no `title` tooltip on one — so
      // the row would dim with no way left to say why. The class carries the
      // look, `aria-disabled` the meaning, and the click handler the refusal.
      entry.setAttribute('aria-disabled', usable ? 'false' : 'true');
      if (usable) entry.removeAttribute('title');
      else entry.title = hint || '';
    }};
    // Called from Rust; see `sync_safe`. Drawn from the answer every page load
    // asks for rather than remembered from the click that caused it, because a
    // safe launch outlives the click: quitting and reopening the app comes back
    // into one, and the label is the whole of what says so.
    //
    // `label` goes in the titlebar and `hint` on the menu row, where a pointer
    // can rest on it — the row reads as an action, and what a user coming back
    // to this tomorrow needs from it is which state it is going to leave.
    window.__dshSafeMode = function (on, label, hint) {{
      safe.textContent = label || '';
      safe.hidden = !on;

      var entry = entries['safe-off'];
      if (!entry) return;
      entry.hidden = !on;
      if (on) entry.title = hint || '';
      else entry.removeAttribute('title');
    }};
    window.__dshBusy = function (text) {{
      said.textContent = text || '';
      toast.classList.toggle('dsh-wc-shown', !!text);
    }};
    // Called from Rust after the language moved; see `relabel`. Two objects,
    // because a settings row has two lines and they come from two maps.
    window.__dshRelabel = function (next, notes) {{
      for (var verb in next) {{
        (spans[verb] || []).forEach(function (span) {{
          word(span, next[verb]);
        }});
      }}
      for (var noted in notes || {{}}) {{
        if (noteSpans[noted]) noteSpans[noted].textContent = notes[noted];
      }}
    }};

    // -------------------------------------------------------- the language --

    // dsh's language is the page's, the way its theme is: Settings writes the
    // choice through to `<html lang>` and swaps the copy live, without loading
    // the document again -- so the labels above, pasted in when this script was
    // injected, would keep the language they were pasted in. Report the move
    // and let Rust send them back.
    //
    // Only a move. What the document opened with is never reported: the loading
    // page declares a language in its markup, and taking that for an answer
    // would let it overrule the one dsh actually holds.
    var lang = document.documentElement.lang;
    new MutationObserver(function () {{
      var next = document.documentElement.lang;
      if (next === lang) return;
      lang = next;
      if (next) signal('locale?tag=' + encodeURIComponent(next));
    }}).observe(document.documentElement, {{
      attributes: true,
      attributeFilter: ['lang']
    }});

    // Before the bar is in the document, so it is never painted the wrong
    // colour first. `paint` starts watching as well as painting; see
    // `controls::theme_watcher`.
    // The strip is carved out of the page by pushing its content down, so what
    // shows through it is the page's own canvas -- and dsh leaves that at the
    // UA's white for the whole of its boot. A dark session therefore opens on
    // a white band across the top until "Loading plugins..." is done, measured
    // at exactly the 36px this app reserves.
    //
    // Covered only while the page has painted nothing of its own, and only
    // where that blank is the wrong colour. The moment dsh paints, the strip
    // goes back to transparent and the band is the page's own background
    // again -- which is the only way there is nothing to see at the 36px line,
    // since dsh's dark is its own colour (`#151517` in the build this was
    // measured against) and not one this app can hold a copy of.
    function band() {{
      var painted = getComputedStyle(document.body).backgroundColor || '';
      var parts = /^rgba?\(([^)]+)\)/.exec(painted);
      // Split on whatever separator the engine used: `rgb(21, 21, 23)` is the
      // comma form every current one returns, and the space form is legal.
      var channels = parts ? parts[1].split(/[\s,\/]+/).map(parseFloat) : [];
      var opaque = channels.length > 3 ? channels[3] !== 0 : channels.length === 3;
      var white = channels[0] === 255 && channels[1] === 255 && channels[2] === 255;
      var blank = !opaque || white;
      drag.style.backgroundColor =
        blank && bar.classList.contains('dsh-wc-dark') ? {night:?} : '';
    }}
    window.__dshThemePainted = band;

    paint(bar);
    // Its own watcher, because the card is not inside the bar; see the note on
    // the palette in the stylesheet above.
    paint(card);

    document.body.appendChild(drag);
    document.body.appendChild(bar);
    document.body.appendChild(scrim);
    document.body.appendChild(card);

    // Dock-style magnification. Hover states would make this three separate
    // on/off steps as the pointer crosses the row, which is what reads as
    // stiff no matter how the easing is tuned. Instead every dot's size is a
    // continuous function of how far the pointer is from its centre, so
    // sliding along the row moves all three at once and nothing ever snaps.
    var AMP = 0.2; // how much the dot under the pointer grows
    var SPREAD = 10; // how hard the others are pushed aside, in px
    var REACH = 34; // how far the influence carries sideways, in px
    // Shorter than REACH, and deliberately: the row sits at the very top of
    // the window, so the pointer nearly always arrives from below, across
    // whatever dsh is drawing. A tall reach would have the dots stirring
    // while the pointer is still busy somewhere else.
    var LIFT = 22; // and how far it carries vertically

    // Measured with `offsetLeft`, which is layout rather than paint and so is
    // not thrown off by the transforms this then writes. The bar is fixed at
    // the viewport's top-left corner with no border, so these come out in the
    // same coordinates as a mouse event's `clientX`.
    var dots = [].slice.call(row.children).map(function (el) {{
      return {{ el: el, x: 0 }};
    }});
    var mid = 0;
    var near = false;

    function measure() {{
      mid = bar.offsetHeight / 2;
      dots.forEach(function (dot) {{
        dot.x = dot.el.offsetLeft + dot.el.offsetWidth / 2;
      }});
    }}

    function place(px, py) {{
      var ny = (mid - py) / LIFT;

      // Distance is taken in two dimensions, not just along the row, so the
      // dots come up as the pointer rises towards them instead of switching
      // on the moment it crosses some line.
      var pull = dots.map(function (dot) {{
        var nx = (dot.x - px) / REACH;
        var d = Math.sqrt(nx * nx + ny * ny);
        // A raised cosine: 1 under the pointer, 0 at the edge of its reach,
        // and flat at both ends, so a dot neither pops in nor pops out.
        return d >= 1 ? 0 : (1 + Math.cos(d * Math.PI)) / 2;
      }});

      var any = pull.some(Boolean);
      if (!any && !near) return;
      if (any !== near) {{
        near = any;
        bar.classList.toggle('dsh-wc-on', any);
      }}

      dots.forEach(function (dot, i) {{
        var f = pull[i];
        var scale = 1 + AMP * f;
        var nx = (dot.x - px) / REACH;
        // Scale first, then shift, so SPREAD stays in real pixels. The shift
        // is signed by which side of the pointer the dot is on and vanishes
        // under it, which is what makes the row part rather than slide.
        var shift = SPREAD * f * (nx < -1 ? -1 : nx > 1 ? 1 : nx);
        dot.el.style.transform = f
          ? 'translateX(' + shift.toFixed(2) + 'px) scale(' + scale.toFixed(3) + ')'
          : '';
        // Held at its drawn size while the dot around it grows, as macOS
        // does. A glyph scaled off the pixel grid blurs, and a blurred glyph
        // is most of what reads as a rough animation. Looked up rather than
        // cached because `__dshMaximized` replaces the middle one's svg.
        var glyph = dot.el.firstChild;
        if (glyph) {{
          glyph.style.transform = f ? 'scale(' + (1 / scale).toFixed(3) + ')' : '';
        }}
      }});
    }}

    measure();
    window.addEventListener('resize', measure);

    // Coalesced to one update per frame: the listener is on the document, so
    // on dsh's page it sees every move of the pointer, not just moves over
    // the row.
    var queued = null;
    var frame = 0;
    function flush() {{
      frame = 0;
      place(queued.clientX, queued.clientY);
    }}
    document.addEventListener('mousemove', function (event) {{
      queued = event;
      // Almost every move is somewhere else on dsh's page entirely. `pull` is
      // zero for every dot once the pointer is further than LIFT from the
      // row's centre line -- the distance is two-dimensional, so it is at
      // least the vertical part -- and `place` would compute three square
      // roots to arrive at the early return below. Recorded either way, so a
      // frame already asked for still lands on the latest position; only the
      // asking is skipped. `near` is what keeps the pass that puts a raised
      // row back down.
      if (event.clientY > mid + LIFT && !near) return;
      if (!frame) frame = requestAnimationFrame(flush);
    }}, {{ capture: true, passive: true }});
    // The pointer can leave the window without ever passing the row.
    document.addEventListener('mouseleave', function () {{
      place(-1e4, -1e4);
    }});
  }}

  if (document.body) start();
  else document.addEventListener('DOMContentLoaded', start, {{ once: true }});
}})();"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri::Url;

    #[test]
    fn parses_open_action() {
        let url =
            Url::parse("dsh-window://open?url=https%3A%2F%2Fexample.com%2Fpath%3Fa%3D1").unwrap();
        match action(&url) {
            Some(Action::OpenUrl(target)) => assert_eq!(target, "https://example.com/path?a=1"),
            _ => panic!("expected Action::OpenUrl"),
        }
    }

    #[test]
    fn ignores_empty_open_action() {
        let url = Url::parse("dsh-window://open?url=").unwrap();
        assert!(action(&url).is_none());

        let url_no_param = Url::parse("dsh-window://open").unwrap();
        assert!(action(&url_no_param).is_none());
    }

    /// Every row this script draws — the menu's and the settings card's alike
    /// — has a label, and every label has a row. They are two lists — see
    /// [`super::labels`] — and a verb in one and not the other is a blank row,
    /// or a string nothing ever draws.
    #[test]
    fn every_menu_entry_is_labelled() {
        let script = super::script();
        let labels: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&super::labels()).unwrap();

        let verbs: Vec<&str> = script
            .split("verb: '")
            .skip(1)
            .filter_map(|rest| rest.split('\'').next())
            .collect();

        assert_eq!(verbs.len(), labels.len(), "{verbs:?} against {labels:?}");
        for verb in verbs {
            assert!(labels.contains_key(verb), "{verb} has no label");
        }
    }

    /// A menu row that hides itself needs a stylesheet that lets it. The rule
    /// these rows are drawn by sets `display` — and opens with `all: unset`,
    /// which takes the UA's own `[hidden]` rule out on its own — so the
    /// attribute alone does nothing and the row that leaves safe mode was
    /// visible on every launch. Pinned here because nothing about the markup
    /// looks wrong when this is missing.
    #[test]
    fn a_hidden_menu_row_is_actually_hidden() {
        let script = super::script();

        assert!(
            script.contains("'.dsh-wc-pop button[hidden]{display:none}'"),
            "the rows set their own display, so [hidden] needs a rule that outranks it"
        );
        assert!(
            script.contains("verb: 'safe-off', hidden: true"),
            "the row that leaves safe mode is the one that starts hidden"
        );
        // The titlebar's standing label sets its own display too, and is hidden
        // for the whole of an ordinary launch — so it walks into the same trap
        // from the other direction.
        assert!(
            script.contains("'.dsh-wc-safe[hidden]{display:none}'"),
            "the safe-mode label sets its own display, so [hidden] needs a rule too"
        );
    }

    /// What moved off the menu, and that it moved rather than being copied.
    ///
    /// Two rows drawn for one verb would each be `checks[verb]`, and the
    /// second assignment wins: the switch the user was looking at would stop
    /// following what Rust pushes. Which is silent, so it is pinned here.
    #[test]
    fn the_settings_rows_left_the_menu_for_the_card() {
        let script = super::script();
        let menu = script
            .split("var SETTINGS")
            .next()
            .expect("the script declares both lists");

        for verb in ["runtime", "registry", "autostart", "notify-turns"] {
            let row = format!("verb: '{verb}'");
            assert!(!menu.contains(&row), "{verb} is still drawn into the menu");
            assert!(script.contains(&row), "{verb} is on neither list");
        }

        // And the one row that opens the card rather than signalling. Without
        // the mark it would navigate to `dsh-window://settings`, which nothing
        // in `action` answers — a menu row that does nothing at all.
        assert!(
            script.contains("verb: 'settings', panel: true"),
            "the row that opens the card has to say that it does"
        );
        // Every string on the card, against the rows that carry one. A note
        // for a verb no row draws is a string nobody reads.
        let notes: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&super::notes()).unwrap();
        for verb in notes.keys() {
            assert!(
                script.contains(&format!("verb: '{verb}'")),
                "{verb} has a note and no row"
            );
        }
    }

    /// What a launch running on no plugins puts on screen, and that the script
    /// reads every part of it. The label and the hint are separate strings on
    /// purpose: one states the state in the titlebar, the other explains the
    /// row that ends it, and a row that reads as an action is the whole reason
    /// the label exists.
    #[test]
    fn a_safe_launch_says_so_in_two_places() {
        let call = super::safe_call(true, "No plugins loaded", "put them back");

        assert!(
            call.contains(r#"window.__dshSafeMode(true, "No plugins loaded", "put them back")"#),
            "{call}"
        );
        assert!(
            call.starts_with("window.__dshSafeMode &&"),
            "the page may not have the chrome yet: {call}"
        );

        assert!(
            script().contains("window.__dshSafeMode = function (on, label, hint)"),
            "the script must read every argument the call sends"
        );
    }

    /// The page reporting what dsh just did to it; see `relabel`.
    #[test]
    fn reads_the_language_the_page_moved_to() {
        let url = Url::parse("dsh-window://locale?tag=en").unwrap();
        match action(&url) {
            Some(Action::Locale(tag)) => assert_eq!(tag, "en"),
            _ => panic!("expected Action::Locale"),
        }
    }

    /// A tag is the whole of what this verb carries, so without one there is
    /// nothing to act on and the navigation is left alone.
    #[test]
    fn declines_a_locale_with_nothing_in_it() {
        for url in [
            "dsh-window://locale",
            "dsh-window://locale?tag=",
            "dsh-window://locale?lang=en",
        ] {
            assert!(action(&Url::parse(url).unwrap()).is_none(), "{url}");
        }
    }

    #[test]
    fn opens_mail_links_too() {
        let url = Url::parse("dsh-window://open?url=mailto%3Ahi%40example.com").unwrap();
        match action(&url) {
            Some(Action::OpenUrl(target)) => assert_eq!(target, "mailto:hi@example.com"),
            _ => panic!("expected Action::OpenUrl"),
        }
    }

    /// Everything `open` hands to the system that would run rather than browse.
    /// See [`is_web_link`].
    #[test]
    fn declines_open_targets_that_are_not_links() {
        for target in [
            "file:///C:/Windows/System32/cmd.exe",
            "file:///etc/passwd",
            "C:\\Windows\\System32\\cmd.exe",
            "\\\\server\\share\\payload.exe",
            "ms-settings:windowsupdate",
            "vscode://file/etc/passwd",
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "/usr/bin/open",
            "./payload.sh",
        ] {
            let mut url = Url::parse("dsh-window://open").unwrap();
            url.query_pairs_mut().append_pair("url", target);
            assert!(
                action(&url).is_none(),
                "{target} is not a link and must not be handed to the system"
            );
        }
    }

    #[test]
    fn parses_window_control_actions() {
        assert!(matches!(
            action(&Url::parse("dsh-window://minimize").unwrap()),
            Some(Action::Minimize)
        ));
        assert!(matches!(
            action(&Url::parse("dsh-window://maximize").unwrap()),
            Some(Action::Maximize)
        ));
        assert!(matches!(
            action(&Url::parse("dsh-window://close").unwrap()),
            Some(Action::Close)
        ));
    }

    /// The notification row is told three things, and the script it is told
    /// them through reads three.
    ///
    /// Two halves that are edited apart: `sync_notify` builds the call in
    /// Rust and the function it lands on is a string literal in the injected
    /// script. Adding an argument to one and not the other is silent — the
    /// extra is dropped, or the missing one reads `undefined` — and what it
    /// would silence is the row that says why notifications are unavailable.
    #[test]
    fn the_notification_row_is_told_whether_it_can_be_used() {
        let call = super::notify_call(true, false, "install the plugin");

        assert!(
            call.contains("window.__dshNotifyTurns(true, false, \"install the plugin\")"),
            "{call}"
        );
        assert!(
            call.starts_with("window.__dshNotifyTurns &&"),
            "the page may not have the chrome yet: {call}"
        );

        assert!(
            script().contains("window.__dshNotifyTurns = function (on, available, hint)"),
            "the script must read every argument the call sends"
        );
        // An older document, injected before availability existed, calls with
        // one argument. It has to keep reading as usable rather than as off.
        assert!(
            script().contains("available === undefined || !!available"),
            "a one-argument call must not read as unavailable"
        );
    }

    /// Every card this app draws over dsh's page reads dsh's theme from
    /// [`theme_watcher`], and none of them from a copy of it.
    ///
    /// Written across the four modules on purpose. This was four
    /// hand-maintained copies of the same twenty lines, each with a comment
    /// asking the next reader to keep it in step with the others; nothing
    /// checked that anyone had. A correction to how dsh's theme is read would
    /// have landed in one card and quietly missed three — the titlebar
    /// following a theme switch while the dialog over it stayed light.
    ///
    /// The assertion is deliberately the whole generated block rather than a
    /// phrase out of it: a copy that drifts by one line is exactly the failure
    /// this is here to catch, and matching on a fragment would let it through.
    #[test]
    fn every_card_takes_its_corners_back() {
        for (what, script, roots) in [
            (
                "the titlebar",
                script(),
                &["dsh-wc", "dsh-wc-set", "dsh-wc-scrim"][..],
            ),
            ("a dialog", crate::dialog::script(), &["dsh-ask"][..]),
            ("the plugin panel", crate::panel::script(), &["dsh-pp"][..]),
            (
                "the runtime chooser",
                crate::setup::script(),
                &["dsh-su"][..],
            ),
        ] {
            assert!(
                script.contains(&corners(roots)),
                "{what} is drawn over a page that makes every corner a \
                 squircle, so it has to ask for round ones from `corners`"
            );
        }
    }

    #[test]
    fn every_card_reads_the_theme_the_same_way() {
        for (what, script, class) in [
            ("the titlebar", script(), "dsh-wc-dark"),
            ("a dialog", crate::dialog::script(), "dsh-ask-dark"),
            ("the plugin panel", crate::panel::script(), "dsh-pp-dark"),
            ("the runtime chooser", crate::setup::script(), "dsh-su-dark"),
        ] {
            assert!(
                script.contains(&theme_watcher(class)),
                "{what} must paint itself from `theme_watcher`, not a copy of it"
            );
        }
    }

    /// Two of the cards carry their own labels, pasted into the injected
    /// script when the window is built rather than asked for when they draw —
    /// so a language switch has to send them again, and each has to answer.
    ///
    /// The call is guarded by `&&`, because a document that has not finished
    /// loading has no `__dsh*` on it. That guard is also what would swallow a
    /// hook this script no longer defines: the relabel would land nowhere and
    /// the card would stay in the language the app started in, which is the
    /// bug the two hooks were added for.
    #[test]
    fn every_card_with_labels_of_its_own_answers_a_relabel() {
        for (what, script, hook) in [
            (
                "the plugin panel",
                crate::panel::script(),
                crate::panel::RELABEL,
            ),
            (
                "the runtime chooser",
                crate::setup::script(),
                crate::setup::RELABEL,
            ),
        ] {
            assert!(
                script.contains(&format!("window.{hook} = function")),
                "{what} must answer on `{hook}`, which is what its `relabel` calls"
            );
        }
    }

    /// The cards built out of elements share one `make`, for the same reason:
    /// four copies is four places a fifth card copies from.
    ///
    /// The titlebar is in the list now. It used to build its row by hand and
    /// have no `make` to share; the settings card is a card, so it uses the
    /// same helper the other three do.
    /// And the icons they draw, for a third time: the shapes were hand written
    /// against whatever viewBox each card happened to use, so the same tick
    /// existed twice at two weights and a settings row's chevron was a `›` out
    /// of the page's own font rather than a shape at all. One set, on one grid.
    ///
    /// The titlebar's four window buttons are deliberately not in it. They are
    /// macOS's own symbols — the split square, the two arrows turned inward —
    /// drawn filled at 10 units to sit inside a 12-pixel dot, and no icon set
    /// has them because they belong to the platform rather than to a UI.
    #[test]
    fn the_cards_share_one_icon_set() {
        for (what, script) in [
            ("the plugin panel", crate::panel::script()),
            ("the settings card", super::script()),
        ] {
            assert!(
                script.contains(lucide()),
                "{what} must draw icons from `lucide`, not shapes of its own"
            );
        }

        // Nothing left in the panel is drawn by hand. The titlebar still has
        // its four, which is what the doc above is about.
        let panel = include_str!("panel.rs");
        assert!(
            !panel.contains("'<path"),
            "the panel draws every shape it has from `lucide` now"
        );
    }

    #[test]
    fn the_cards_share_one_element_helper() {
        for (what, script) in [
            ("a dialog", crate::dialog::script()),
            ("the plugin panel", crate::panel::script()),
            ("the runtime chooser", crate::setup::script()),
            ("the settings card", super::script()),
        ] {
            assert!(
                script.contains(dom_make()),
                "{what} must build elements with `dom_make`, not a copy of it"
            );
        }
    }
}
