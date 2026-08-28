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
//! from the tray, and none of it reads anything back — the widest of them starts
//! an `npm install` of one hard-coded package, or quits. What the page cannot do
//! is ask a question and get an answer, which is what the IPC door would have
//! opened.
//!
//! Three things travel the other way, each pushed rather than asked for, because
//! the page has no way to see any of them: whether the window is maximised
//! ([`sync`]), whether the login item is on ([`sync_autostart`]), and whatever
//! slow thing is running right now ([`busy`]).

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
    /// Turn the finished-turn notification on or off; see [`crate::settings`].
    NotifyTurns,
    Quit,
    /// Open the plugin panel on the loading page.
    Plugins,
    /// Install what was ticked in it, and whatever was typed into its box.
    PluginsInstall(Vec<String>, Option<String>),
    /// Take the ticked ones back out again.
    PluginsRemove(Vec<String>),
    /// Leave the panel: back to dsh, starting it if the panel was shown before
    /// the boot ever got that far.
    PluginsDone,
    /// Show the profile directory in the file manager — the one step the panel
    /// does not take on the user's behalf. See [`crate::plugins`].
    PluginsDirectory,
    /// A shell with dsh on its PATH.
    Terminal,
    /// Start `dsh web` again after it exited on its own.
    RestartDsh,
    /// A notification the page raised; see [`crate::notify`].
    Notify(crate::notify::Notice),
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
        "plugins" => Some(Action::Plugins),
        "plugins-install" => {
            let (ids, spec) = crate::plugins::requested(url);
            Some(Action::PluginsInstall(ids, spec))
        }
        "plugins-remove" => Some(Action::PluginsRemove(crate::plugins::wanted_gone(url))),
        "plugins-done" => Some(Action::PluginsDone),
        "plugins-directory" => Some(Action::PluginsDirectory),
        "terminal" => Some(Action::Terminal),
        "restart-dsh" => Some(Action::RestartDsh),
        "open" => {
            url.query_pairs()
                .find_map(|(key, value)| if key == "url" { Some(value.into_owned()) } else { None })
                .filter(|target| !target.is_empty())
                .map(Action::OpenUrl)
        }
        // The only one carrying a payload the app reads rather than acts on,
        // and the only one that can decline: an empty notification is dropped
        // here rather than raised as a blank toast.
        "notify" => crate::notify::received(url).map(Action::Notify),
        // An answer to a question this app asked; see `crate::dialog`, which
        // owns the parsing because it owns the callback the answer runs.
        "ask" => Some(Action::Answered(url.clone())),
        other => {
            eprintln!("dsh-desktop: ignoring unknown window action {other}");
            None
        }
    }
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
        Action::Plugins => return crate::open_plugins(app),
        Action::PluginsInstall(ids, spec) => return crate::install_plugins(app, ids, spec),
        Action::PluginsRemove(names) => return crate::remove_plugins(app, names),
        Action::PluginsDone => return crate::leave_plugins(app),
        Action::PluginsDirectory => return crate::plugins::open_directory(app),
        Action::RestartDsh => return crate::restart_dsh(app),
        Action::Notify(notice) => return crate::notify::show(app, notice),
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
    let _ = window.eval(&format!(
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

/// Put the checkmark on the notification item, or take it off. Pushed on every
/// page load and after every toggle, like the login item above.
pub fn sync_notify(app: &AppHandle) {
    let enabled = crate::settings::notifications(app);
    eval(
        app,
        &format!("window.__dshNotifyTurns && window.__dshNotifyTurns({enabled})"),
    );
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
fn eval(app: &AppHandle, call: &str) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.eval(call);
    }
}

/// The script that draws all of it, injected into every document the window
/// loads.
///
/// The dots carry their own colour, the same three macOS uses, so there is
/// nothing about them that has to follow dsh's theme — they read the same
/// against a light page and a dark one. The menu does not have that luxury: it
/// is text on a panel, so it follows the theme of the page it is drawn over,
/// watching the two things dsh's own theme writes onto the document rather than
/// the webview's `prefers-color-scheme` — which is settled when the window is
/// built (see `theme`) and does not move when the theme is switched inside dsh.
pub fn script() -> String {
    let titlebar_height = TITLEBAR_HEIGHT;
    let dot = DOT;
    let gap = DOT_GAP;
    let pad = ROW_PAD;

    // JSON rather than the bare text: these are pasted into a JavaScript
    // literal, and a label is one apostrophe away from being a syntax error
    // that takes the whole titlebar with it.
    let label = |text: &str| serde_json::to_string(text).expect("a string is always serializable");
    let plugins = label(t!("插件…", "Plugins…"));
    let terminal = label(t!("打开终端", "Open a terminal"));
    let restart_dsh = label(t!("重启 dsh", "Restart dsh"));
    let update_dsh = label(t!("更新 dsh…", "Update dsh…"));
    let check_app = label(t!("检查应用更新…", "Check for app updates…"));
    let autostart = label(t!("开机自启动", "Start at login"));
    // Not "Notify when a turn finishes": the switch behind it gates every
    // notification this app raises, a plugin's included. See `crate::settings`.
    let notify = label(t!("通知", "Notifications"));
    let quit = label(t!("退出 dsh", "Quit dsh"));

    format!(
        r#"(function () {{
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

  // The menu, top to bottom. `check` marks the one item that carries state.
  // The labels come from Rust so there is one place the two languages live;
  // see `i18n`.
  var ITEMS = [
    {{ verb: 'plugins', label: {plugins} }},
    {{ verb: 'terminal', label: {terminal} }},
    {{ separator: true }},
    {{ verb: 'restart-dsh', label: {restart_dsh} }},
    {{ verb: 'update-dsh', label: {update_dsh} }},
    {{ verb: 'check-app', label: {check_app} }},
    {{ separator: true }},
    {{ verb: 'autostart', label: {autostart}, check: true }},
    {{ verb: 'notify-turns', label: {notify}, check: true }},
    {{ separator: true }},
    {{ verb: 'quit', label: {quit} }}
  ];

  var MENU_GLYPH = '<svg width="14" height="14" viewBox="0 0 14 14" fill="none" ' +
    'stroke="currentColor" stroke-width="1.4" stroke-linecap="round">' +
    '<path d="M3 4.5h8M3 7h8M3 9.5h8"/></svg>';

  var TICK = '<svg class="dsh-wc-tick" width="11" height="11" viewBox="0 0 12 12" ' +
    'fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" ' +
    'stroke-linejoin="round"><path d="M2.4 6.4l2.3 2.3 4.9-5.1"/></svg>';

  function svg(shape) {{
    return '<svg width="8" height="8" viewBox="0 0 10 10" fill="none" ' +
      'stroke="currentColor" stroke-width="1.3" stroke-linecap="round">' + shape + '</svg>';
  }}

  // The whole channel back to Rust; see controls.rs. The navigation is
  // cancelled there, so the page it is called from stays exactly where it is.
  function signal(verb) {{
    window.location.href = '{SCHEME}://' + verb;
  }}

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
      // On the bar rather than on `:root`, so `dsh-wc-dark` -- put on by
      // `repaint` below out of what the page says, not out of the media query
      // -- is all it takes to swap the set.
      '.dsh-wc{{--dsh-wc-fg:rgba(0,0,0,.55);--dsh-wc-fg-hi:rgba(0,0,0,.85);' +
      '--dsh-wc-panel:rgba(255,255,255,.96);--dsh-wc-line:rgba(0,0,0,.09);' +
      '--dsh-wc-hover:rgba(0,0,0,.06);' +
      '--dsh-wc-shadow:0 12px 32px rgba(0,0,0,.18),0 0 0 .5px rgba(0,0,0,.09);}}' +
      '.dsh-wc.dsh-wc-dark{{' +
      '--dsh-wc-fg:rgba(255,255,255,.62);--dsh-wc-fg-hi:rgba(255,255,255,.94);' +
      '--dsh-wc-panel:rgba(42,42,46,.96);--dsh-wc-line:rgba(255,255,255,.11);' +
      '--dsh-wc-hover:rgba(255,255,255,.09);' +
      '--dsh-wc-shadow:0 12px 32px rgba(0,0,0,.5),0 0 0 .5px rgba(255,255,255,.09);}}' +
      'html,body{{height:100%!important;margin:0!important;overflow:hidden!important;}}' +
      '#root{{height:calc(100% - var(--dsh-titlebar-height))!important;margin-top:var(--dsh-titlebar-height)!important;box-sizing:border-box!important;}}' +
      'body:not(:has(#root)){{padding-top:var(--dsh-titlebar-height)!important;box-sizing:border-box!important;}}' +
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
      '.dsh-wc-pop button:hover{{background:var(--dsh-wc-hover)}}' +
      '.dsh-wc-pop hr{{border:0;height:1px;margin:5px 8px;' +
      'background:var(--dsh-wc-line)}}' +
      '.dsh-wc-tick{{margin-left:auto;opacity:0;transition:opacity .12s ease}}' +
      '.dsh-wc-pop button.dsh-wc-checked .dsh-wc-tick{{opacity:1}}' +
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
    var checks = {{}};

    ITEMS.forEach(function (item) {{
      if (item.separator) {{
        pop.appendChild(document.createElement('hr'));
        return;
      }}

      var entry = document.createElement('button');
      entry.type = 'button';
      var label = document.createElement('span');
      label.textContent = item.label;
      entry.appendChild(label);
      if (item.check) {{
        entry.insertAdjacentHTML('beforeend', TICK);
        checks[item.verb] = entry;
      }}
      entry.addEventListener('click', function () {{
        // Closed first: the verb can end in a modal, and a menu still hanging
        // open behind it is a menu that is open again when the modal goes.
        shut();
        signal(item.verb);
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

    // Called from Rust; see `sync_autostart`, `sync_notify` and `busy`.
    function mark(verb, on) {{
      var entry = checks[verb];
      if (entry) entry.classList.toggle('dsh-wc-checked', !!on);
    }}

    window.__dshAutostart = function (on) {{
      mark('autostart', on);
    }};
    window.__dshNotifyTurns = function (on) {{
      mark('notify-turns', on);
    }};
    window.__dshBusy = function (text) {{
      said.textContent = text || '';
      toast.classList.toggle('dsh-wc-shown', !!text);
    }};

    // ----------------------------------------------------------- the theme --

    // dsh's theme is the page's, not the window's: it writes `color-scheme` on
    // the root element and `data-ds-dark-theme` on the body, and switching it
    // inside the UI changes both without anything reaching the webview's own
    // `prefers-color-scheme` -- which is fixed when the window is built and has
    // nothing to move it afterwards. So the menu reads the page, and falls back
    // to the media query only where the page says nothing either way: the
    // loading page, whose `color-scheme` is the bare `light dark`.
    var media = window.matchMedia('(prefers-color-scheme:dark)');

    function dark() {{
      if (document.body.hasAttribute('data-ds-dark-theme')) return true;
      var declared = getComputedStyle(document.documentElement).colorScheme || '';
      var light = declared.indexOf('light') !== -1;
      var night = declared.indexOf('dark') !== -1;
      return night !== light ? night : media.matches;
    }}

    function repaint() {{
      bar.classList.toggle('dsh-wc-dark', dark());
    }}

    // Before the bar is in the document, so it is never painted the wrong
    // colour first.
    repaint();
    var watch = new MutationObserver(repaint);
    watch.observe(document.documentElement, {{
      attributes: true, attributeFilter: ['style', 'class', 'data-theme']
    }});
    watch.observe(document.body, {{
      attributes: true, attributeFilter: ['style', 'class', 'data-ds-dark-theme']
    }});
    media.addEventListener('change', repaint);

    document.body.appendChild(drag);
    document.body.appendChild(bar);

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
        let url = Url::parse("dsh-window://open?url=https%3A%2F%2Fexample.com%2Fpath%3Fa%3D1").unwrap();
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
}

