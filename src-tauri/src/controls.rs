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
//! which is why they are dimmed, and show their glyphs only when pointed at.
//!
//! ## Talking back
//!
//! A click has to reach Rust, and the ordinary way — Tauri's IPC — would mean
//! granting IPC to `http://127.0.0.1:*`, which is to say to every line of
//! JavaScript dsh and its plugins load. That is a large door to open for three
//! buttons.
//!
//! So the channel is a navigation instead: the page sets `location.href` to
//! `dsh-window://<action>`, [`action`] recognises it in the navigation handler,
//! and the navigation is cancelled before it goes anywhere. One-way, four verbs,
//! no permissions. Dragging works the same way despite being continuous, because
//! [`WebviewWindow::start_dragging`] hands the whole drag to the OS — the page
//! only has to say when it starts.
//!
//! The one thing that travels the other way is whether the window is maximised,
//! which is pushed with [`sync`]: the page cannot see a Win+Up or a snap, so it
//! is told rather than left to guess.

use tauri::{AppHandle, Manager, Url, WebviewWindow};

/// The scheme the injected buttons signal on. Not registered with anything — it
/// only has to be a scheme no real navigation would use, since the navigation
/// is cancelled the moment it is recognised.
const SCHEME: &str = "dsh-window";

/// The strip's height, and how far below the top edge it starts — the first few
/// pixels belong to the window's own resize border, and taking them would cost
/// the user the ability to resize from the top.
const DRAG_HEIGHT: u32 = 10;
const DRAG_INSET: u32 = 4;

/// One dot, the space between two of them, and the padding around the row. The
/// row's total width is where the drag strip picks up.
const DOT: u32 = 12;
const DOT_GAP: u32 = 8;
const ROW_PAD: u32 = 12;
const BUTTONS: u32 = 3;

/// How far the drag strip starts from the left edge: past the whole row, so a
/// press near a dot never turns into a window drag.
const fn row_width() -> u32 {
    ROW_PAD * 2 + DOT * BUTTONS + DOT_GAP * (BUTTONS - 1)
}

/// What the page can ask the window to do. Deliberately short: the page is
/// dsh's, and this is the whole of what it is trusted with.
pub enum Action {
    Minimize,
    Maximize,
    Close,
    Drag,
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
        other => {
            eprintln!("dsh-desktop: 忽略未知的窗口操作 {other}");
            None
        }
    }
}

/// Do what the button asked. Every call is best effort — a window that will not
/// minimise is not a reason to take the app down.
pub fn perform(app: &AppHandle, action: Action) {
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
        // than tear down an agent mid-task. Quitting goes through the tray menu.
        Action::Close => {
            let _ = window.hide();
        }
        Action::Drag => {
            let _ = window.start_dragging();
        }
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

/// The script that draws all of it, injected into every document the window
/// loads.
///
/// The dots carry their own colour, the same three macOS uses, so unlike the
/// rest of the window there is nothing here that has to follow dsh's theme —
/// they read the same against a light page and a dark one.
pub fn script() -> String {
    let strip = DRAG_INSET;
    let height = DRAG_HEIGHT;
    let clear = row_width();
    let row = DOT + ROW_PAD + DRAG_INSET;
    let dot = DOT;
    let gap = DOT_GAP;
    let pad = ROW_PAD;

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

  function svg(shape) {{
    return '<svg width="8" height="8" viewBox="0 0 10 10" fill="none" ' +
      'stroke="currentColor" stroke-width="1.3" stroke-linecap="round">' + shape + '</svg>';
  }}

  // The whole channel back to Rust; see controls.rs. The navigation is
  // cancelled there, so the page it is called from stays exactly where it is.
  function signal(verb) {{
    window.location.href = '{SCHEME}://' + verb;
  }}

  function start() {{
    var style = document.createElement('style');
    style.textContent =
      '.dsh-wc{{position:fixed;top:0;left:0;z-index:2147483647;display:flex;' +
      'align-items:center;gap:{gap}px;height:{row}px;padding:0 {pad}px;' +
      'opacity:.6;transition:opacity .12s ease;pointer-events:none}}' +
      '.dsh-wc:hover{{opacity:1}}' +
      // Only the dots take the pointer; the padding between and around them
      // lets clicks through to whatever dsh draws underneath.
      '.dsh-wc button{{all:unset;pointer-events:auto;width:{dot}px;height:{dot}px;' +
      'border-radius:50%;display:grid;place-items:center;cursor:default;' +
      'color:rgba(0,0,0,.55);box-shadow:inset 0 0 0 .5px rgba(0,0,0,.12)}}' +
      '.dsh-wc button svg{{opacity:0;transition:opacity .12s ease}}' +
      '.dsh-wc:hover button svg{{opacity:1}}' +
      '.dsh-wc button:active{{filter:brightness(.85)}}' +
      // Qualified by `.dsh-wc button` so they outweigh its `all:unset`, which
      // would otherwise take the colour straight back off again.
      '.dsh-wc button.dsh-wc-close{{background:#ff5f57}}' +
      '.dsh-wc button.dsh-wc-min{{background:#febc2e}}' +
      '.dsh-wc button.dsh-wc-max{{background:#28c840}}' +
      '.dsh-wc-drag{{position:fixed;top:{strip}px;left:{clear}px;right:{strip}px;' +
      'height:{height}px;z-index:2147483646}}';
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

    function add(verb, shape, extra) {{
      var button = document.createElement('button');
      button.type = 'button';
      button.className = extra || '';
      button.innerHTML = svg(shape);
      button.addEventListener('click', function () {{
        signal(verb);
      }});
      bar.appendChild(button);
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

    document.body.appendChild(drag);
    document.body.appendChild(bar);
  }}

  if (document.body) start();
  else document.addEventListener('DOMContentLoaded', start, {{ once: true }});
}})();"#
    )
}
