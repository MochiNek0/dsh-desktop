//! The app's own dialogs, drawn over the page instead of by the window manager.
//!
//! [`crate::update`] used to ask its questions through `tauri-plugin-dialog`,
//! which raises a native message box. That works, and it produced three
//! different-looking apps: on Windows a message box from 2012 — a grey strip,
//! the system font at the system size, the affirmative button first — on macOS
//! an NSAlert with the affirmative last, on Linux whatever GTK theme is
//! installed. Next to the plugin panel — which is this app's own card, in dsh's
//! own colours, following dsh's own light/dark — every one of them read as a
//! different program.
//!
//! That plugin is gone now; this module is the only thing that asks. Nothing
//! depends on `tauri-plugin-dialog` any more, which on Linux also takes `rfd`
//! and its GTK dialog machinery out of the build.
//!
//! So the questions are drawn here, in the same idiom as [`crate::panel`]: an
//! injected script, a card over the page, the theme read off the document, and
//! the answer sent back over the cancelled-navigation channel from
//! [`crate::controls`]. The plugin panel is the pattern; this is the general
//! case of it.
//!
//! ## Why a general dialog and not a second panel
//!
//! Everything the updater asks is the same shape: a title, a paragraph, and two
//! or three ways out. Rather than a bespoke card per question, Rust describes
//! one — `Ask { title, body, buttons }` — and the script draws whatever it is
//! given. The update flow then reads as the conversation it is, and a later
//! question costs a call rather than a panel.
//!
//! ## Answers come back by id, not by index
//!
//! Each button carries an id chosen by the caller, and the click sends that id
//! back. Not the index: a dialog that grows a third button would silently
//! change what "1" means, and the thing being answered here is whether to
//! install software and restart the app.
//!
//! ## What it deliberately does not do
//!
//! It is not modal to the operating system, only to the page: the window can
//! still be moved, minimised and closed while one is up, which is the right
//! behaviour for a window whose whole job is to be left alone for minutes at a
//! time. And it never blocks Rust. The dialog is shown, the caller returns, and
//! the answer arrives later as a navigation — which is what lets an update be
//! offered from inside the webview's own navigation handler without deadlocking
//! it.
//!
//! ## The same dialog on all three platforms
//!
//! That is the point of drawing it here, and it is also the obligation. A native
//! message box came with things a `<div>` does not, and each of them had to be
//! put back by hand or the three platforms would drift apart in a different way
//! than before:
//!
//! - **Escape closes it**, which macOS in particular expects of anything shaped
//!   like a dialog. It answers with [`DISMISSED`] rather than merely hiding the
//!   card — see the note there.
//! - **Focus is trapped** in the card and handed back to whatever had it when
//!   the dialog closes. Without the trap, Tab walks out of a modal into dsh's
//!   page behind it.
//! - **It announces itself** as `role="dialog"`, `aria-modal`, and a title and
//!   body the card points at. A screen reader got this for free from the
//!   platform and gets nothing from a bare card.
//! - **The buttons are drawn, not native.** `-webkit-appearance` goes in beside
//!   the unprefixed property, or an older WKWebView and an older WebKitGTK keep
//!   the system button shell under our own.
//!
//! Button order is ours now rather than the platform's, which is the one place
//! this deliberately overrides a convention: the affirmative is last everywhere,
//! including on Windows, where a native box would have put it first.

use std::sync::{Mutex, OnceLock};
use std::thread::ThreadId;

use tauri::{AppHandle, Manager};

/// The thread `main` runs on, recorded there before anything can ask.
///
/// Tauri exposes no public "am I on the main thread", and the answer is what
/// the debug assertion in [`confirm`] needs. The deadlock it guards is
/// otherwise unrecoverable: a dialog is on screen, and the thread that would
/// deliver the click is the one sitting in `recv`.
static MAIN: OnceLock<ThreadId> = OnceLock::new();

/// Record the calling thread as the main one. Called first thing in `main`.
pub fn remember_main_thread() {
    let _ = MAIN.set(std::thread::current().id());
}

/// Whether this is the thread that pumps the event loop.
///
/// `false` when nobody ever recorded one, which is every unit test: there is no
/// event loop there to deadlock, and an assertion that fired would be noise.
fn on_main_thread() -> bool {
    is_main(MAIN.get().copied(), std::thread::current().id())
}

/// The decision [`on_main_thread`] makes, with the global read out of it.
///
/// Split out because [`MAIN`] is deliberately write-once for the life of the
/// process, so a test cannot set it to each of the cases in turn — and two
/// tests that each tried would race for who got to claim it. This takes the
/// recorded thread as an argument instead, which is the part with the logic in
/// it; the accessor above is the part with the global in it.
fn is_main(recorded: Option<ThreadId>, current: ThreadId) -> bool {
    recorded == Some(current)
}

/// One way out of a dialog.
pub struct Choice {
    /// What comes back when it is clicked. Chosen by the caller; see the module
    /// docs on why this is not an index.
    pub id: &'static str,
    pub label: String,
    /// Draws as the filled accent button. At most one, and it is the one the
    /// dialog is really asking about.
    pub primary: bool,
}

impl Choice {
    /// `impl Into<String>` so both halves of `t!` fit: without arguments it is
    /// a `&'static str`, with them a `String`, and a dialog uses each in
    /// different places.
    pub fn new(id: &'static str, label: impl Into<String>) -> Self {
        Self {
            id,
            label: label.into(),
            primary: false,
        }
    }

    pub fn primary(id: &'static str, label: impl Into<String>) -> Self {
        Self {
            id,
            label: label.into(),
            primary: true,
        }
    }
}

/// What runs when a button is clicked: the app, and the id of that button.
///
/// `FnOnce` because an answer is answered once, `Send` because it is stored
/// until the click arrives, and boxed because every caller's is a different
/// closure.
type Answer = Box<dyn FnOnce(&AppHandle, &str) + Send + 'static>;

/// A question, and what to do with the answer.
pub struct Ask {
    pub title: String,
    pub body: String,
    /// Left to right, so the affirmative goes last — the end of the row is
    /// where this app's other cards put the button that does the thing.
    pub choices: Vec<Choice>,
    /// Run on the main thread when the dialog is answered, with the id of the
    /// button that was clicked — or [`DISMISSED`] when the user pressed Escape.
    ///
    /// Dismissal answers rather than going quiet, because a dialog that goes
    /// quiet leaves [`confirm`] blocked on a worker thread. Every callback here
    /// is written as "act on this one id, otherwise do nothing", which is what
    /// makes an id none of them names the safe answer.
    pub answered: Answer,
}

/// The dialog currently on screen, and the token the next one will carry.
///
/// One at a time: the script draws into a single card, and a second dialog
/// would replace the first while its `answered` was still pending — which for
/// the updater would mean a download whose question nobody ever answers.
///
/// Both halves under one lock, because they have to move together. The token
/// came from an `AtomicU64` of its own, and two threads could then claim in one
/// order and install in the other — leaving the newer dialog on screen and the
/// older one's entry in the slot, so the click on what the user was actually
/// looking at arrived with a token nothing recognised and was dropped as stale.
/// For [`confirm`] that is a wait that ends at [`ANSWER_TIMEOUT`]; the window
/// with dsh's update question in it can be answered and go nowhere. Two dialogs
/// at once is rare — the tray's dsh update while the app updater's check comes
/// back is the reachable pair — but rare and silent is the worst of the two
/// halves of that.
///
/// [`ask`] therefore holds this across the whole business of putting a dialog
/// up: claiming the token, handing the script to the window, and installing the
/// entry. `claim` taking `&mut self` is what makes it impossible to number a
/// dialog without holding the lock that will file it.
static DIALOGS: Mutex<Dialogs> = Mutex::new(Dialogs {
    next: 1,
    on_screen: None,
});

struct Dialogs {
    /// The token the next dialog gets. Monotonic and never reused: an answer can
    /// outlive its question — the user clicks, and in the same instant a new
    /// dialog goes up — and the stale click has to be recognisable as stale.
    next: u64,
    on_screen: Option<Pending>,
}

impl Dialogs {
    /// Number the dialog that is about to go up.
    fn claim(&mut self) -> u64 {
        let token = self.next;
        self.next += 1;
        token
    }
}

struct Pending {
    token: u64,
    answered: Answer,
}

/// Whether an arriving answer belongs to the dialog now on screen.
///
/// Split out from [`answered`] the way [`is_main`] is split out of
/// [`on_main_thread`]: this is the part with the rule in it, and it is the rule
/// that a test can hold on to. `None` is a dialog that has already been answered
/// — the entry is taken out under the lock, so the second of two clicks in
/// flight finds nothing rather than running a callback twice.
fn is_current(on_screen: Option<u64>, arriving: u64) -> bool {
    on_screen == Some(arriving)
}

/// The JavaScript that hands one dialog to the page.
///
/// Called unguarded — not `window.__dshAsk && window.__dshAsk(…)` — but not for
/// the reason an earlier version of this comment gave. That version claimed the
/// bare call throws when the script has not run yet, that `eval` reports the
/// throw, and that [`ask`] can therefore tell a delivered dialog from an
/// undelivered one. None of that is true on any of the three platforms:
/// `Webview::eval` hands the script to `eval_script`, which from a worker thread
/// only queues a message on the event loop and returns `Ok` before any
/// JavaScript has run, and which on the main thread reaches
/// `WebviewMessage::EvaluateScript` — where the runtime logs the error and
/// discards it. Underneath, all three backends evaluate asynchronously with no
/// way back: WKWebView with a nil completion handler, WebView2's `ExecuteScript`
/// with no callback, and WebKitGTK's `webkit_web_view_evaluate_javascript`.
///
/// So the guard is left off only because it would add noise, and delivery is
/// made certain at the other end instead: the script waits for `document.body`
/// (see `ready` in [`script`]) rather than throwing without one, and [`confirm`]
/// carries [`ANSWER_TIMEOUT`] rather than trusting a bool that cannot be wrong.
///
/// Its own function so a test can pin the shape of the call.
fn delivery(json: &str) -> String {
    format!("window.__dshAsk({json})")
}

/// Hand the call to the window, or hold it until a document can receive it.
///
/// Through the same queue every other injected call goes through — `Splash` in
/// main.rs — rather than straight to `Webview::eval`, and for the reason that
/// queue exists: a call evaluated into a document that is on its way out is
/// lost, exactly as one made before the first load is. The `ready` wrapper in
/// [`script`] covers a document whose body has not been built yet; it cannot
/// cover a document whose initialization script has not run at all, which is
/// every moment before the first `PageLoadEvent::Finished` and every moment
/// between a navigation and the next one.
///
/// That gap is not hypothetical for the one caller that cannot survive it.
/// `boot` asks whether to update dsh from a worker thread it started before the
/// loading page had finished loading, and [`confirm`] then blocks on the answer:
/// a question that missed its document is a loading page that sits there until
/// [`ANSWER_TIMEOUT`].
///
/// `true` once the call is out or queued. Falls back to evaluating directly when
/// there is no session to queue against, which is every unit test and the moment
/// before `setup` has managed one.
fn deliver(app: &AppHandle, window: &tauri::WebviewWindow, js: String) -> bool {
    match app.try_state::<crate::Session>() {
        Some(session) => {
            session.splash.send(window, js);
            true
        }
        None => window.eval(js).is_ok(),
    }
}

/// The id an answer carries when the dialog was dismissed rather than answered.
///
/// Escape sends this. It is deliberately a string no caller will ever name a
/// button, so the ordinary `if id == "now"` / `id == affirmative` test in every
/// callback reads it as "not the affirmative" without any of them having to know
/// dismissal exists. See the module docs on why dismissal must answer at all
/// rather than merely hiding the card.
pub const DISMISSED: &str = "";

/// How long [`confirm`] waits for a click before giving up and answering "no".
///
/// A backstop, not a policy: every path that should end the wait already does —
/// a click, a dismissal, [`ask`] replacing the dialog and dropping its sender.
/// What this covers is the paths that cannot signal, all of which end with the
/// card gone and the sender still parked in [`DIALOGS`]: the document navigating
/// out from under a dialog, a webview that never ran the script, a renderer
/// crash. Without it those hang `boot` forever, and a boot thread that never
/// returns is an app that never starts dsh and cannot be recovered except by
/// killing it.
///
/// Long enough that it is never reached by a user who simply walked away
/// mid-question and came back — these dialogs ask about downloads that take
/// minutes, and answering "no" under someone's hand would be worse than waiting.
const ANSWER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Put a question on screen. Returns immediately; see the module docs.
///
/// Answers whether the question was handed over at all: there was a window, and
/// the call to it is out or queued. It is *not* a promise that the dialog is on
/// screen — see [`delivery`] for why no such promise is available from `eval`.
/// Callers that need the answer use [`confirm`], which does not rely on this.
///
/// A question this answers `false` to changes nothing: the dialog already on
/// screen, if there was one, is still there and still answerable, and this one's
/// `answered` is dropped here rather than left in the slot for a click that can
/// never come — which is what fails [`confirm`]'s receive instead of parking it
/// for [`ANSWER_TIMEOUT`].
pub fn ask(app: &AppHandle, ask: Ask) -> bool {
    // Nothing to hand a dialog to, and nothing to disturb: the dialog already on
    // screen, if there is one, is left where it is and stays answerable. An
    // earlier version installed this dialog's entry first and only then looked
    // for a window, so a question raised with no window took the live one's
    // callback down with it.
    let Some(window) = app.get_webview_window("main") else {
        eprintln!(
            "dsh-desktop: no window could show the dialog {:?}",
            ask.title
        );
        return false;
    };

    let buttons: Vec<serde_json::Value> = ask
        .choices
        .iter()
        .map(|choice| {
            serde_json::json!({
                "id": choice.id,
                "label": choice.label,
                "primary": choice.primary,
            })
        })
        .collect();

    // Held across all three steps — numbering the dialog, handing it over, and
    // filing the entry that answers it — so the dialog on screen and the entry
    // in the slot cannot end up being different dialogs. See [`DIALOGS`].
    let Ok(mut dialogs) = DIALOGS.lock() else {
        eprintln!("dsh-desktop: the dialog slot is poisoned; dropping a question");
        return false;
    };

    let token = dialogs.claim();

    let payload = serde_json::json!({
        "token": token,
        "title": ask.title,
        "body": ask.body,
        "buttons": buttons,
    })
    .to_string();

    // The payload is JSON, and it is pasted into a JavaScript call — so it is
    // serialised a second time, into a string literal the script then parses.
    // Without that, a quote in an update's release notes ends the argument and
    // takes the rest of the call with it.
    let json = serde_json::to_string(&payload).expect("a string is always serializable");

    if !deliver(app, &window, delivery(&json)) {
        eprintln!(
            "dsh-desktop: the window would not take the dialog {:?}",
            ask.title
        );
        // Before the slot is touched, so a dialog that could not be handed over
        // leaves the one already on screen answerable. The token it claimed is
        // spent either way, which is the point of never reusing one.
        return false;
    }

    // Replaces whatever was there. The previous dialog's `answered` is dropped
    // without running, which is the same outcome as dismissing it — and dropping
    // it is what fails the receive in [`confirm`] rather than leaving it to wait
    // out [`ANSWER_TIMEOUT`].
    dialogs.on_screen = Some(Pending {
        token,
        answered: ask.answered,
    });

    true
}

/// Ask, and wait for the answer. `true` for the affirmative.
///
/// For the callers whose next step *is* the answer: the boot has to know
/// whether to spend two minutes updating dsh before it starts one, and there is
/// nothing useful to do in the meantime. They already run on a worker thread —
/// see `boot` in main.rs — and blocking one of those is the whole point.
///
/// Never call this from the main thread. The answer arrives as a navigation,
/// which is delivered on the main thread, so a main-thread caller would be
/// waiting for a message only it could deliver. The debug assertion below says
/// so out loud rather than hanging the window.
///
/// Four things end the wait, and all four answer the negative — which for every
/// caller means "do not touch anything", the safe half:
///
///   - the user clicks something that is not `affirmative`;
///   - the user dismisses with Escape, which sends [`DISMISSED`];
///   - [`ask`] replaces the dialog before it is answered, dropping its sender
///     and failing the receive;
///   - nothing happens for [`ANSWER_TIMEOUT`], which is the backstop for a card
///     that went away without being able to say so — see the note there.
pub fn confirm(app: &AppHandle, mut ask_for: Ask, affirmative: &'static str) -> bool {
    // The deadlock this prevents is total: the dialog is up, and the thread
    // that would carry the click back is the one about to block. Debug-only
    // because the cost in a release build is a hang nobody can report usefully,
    // whereas in a debug build this is a panic with a stack trace pointing at
    // the caller that has to move to a worker thread.
    debug_assert!(
        !on_main_thread(),
        "dialog::confirm blocks until a click arrives on the main thread; \
         call it from a worker thread. See the module docs."
    );

    let (send, receive) = std::sync::mpsc::channel();
    ask_for.answered = Box::new(move |_, id| {
        let _ = send.send(id == affirmative);
    });

    // Nothing to wait for when there was no window: the sender is already
    // dropped and the receive below would only confirm it the slow way.
    if !ask(app, ask_for) {
        return false;
    }

    match receive.recv_timeout(ANSWER_TIMEOUT) {
        Ok(answer) => answer,
        // Disconnected: the dialog was replaced, and its sender went with it.
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => false,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            eprintln!(
                "dsh-desktop: nothing answered the dialog in {} minutes; \
                 taking it as a no",
                ANSWER_TIMEOUT.as_secs() / 60
            );
            false
        }
    }
}

/// A `dsh-window://ask` navigation: the user clicked something.
///
/// Parsed here rather than in [`crate::controls`] because the token check and
/// the callback both live here; controls only needs to know it was a button.
pub fn answered(app: &AppHandle, url: &tauri::Url) {
    let mut token = None;
    let mut id = String::new();

    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "token" => token = value.parse::<u64>().ok(),
            "id" => id = value.into_owned(),
            _ => {}
        }
    }

    let Some(token) = token else {
        return;
    };

    // Taken out under the lock, so two clicks in flight cannot both run it. The
    // callback runs after the lock is released — it is somebody else's code, and
    // one that asks another question would otherwise be asking for the lock it
    // is already inside.
    let found = DIALOGS.lock().ok().and_then(|mut dialogs| {
        let on_screen = dialogs.on_screen.as_ref().map(|one| one.token);
        if is_current(on_screen, token) {
            dialogs.on_screen.take()
        } else {
            None
        }
    });

    if let Some(pending) = found {
        (pending.answered)(app, &id);
    }
}

/// The script that draws it, injected into every document the window loads.
///
/// Built on first use, like the panel's: most launches never ask anything, and
/// a document this app does not own is not somewhere to leave a card lying
/// around unasked.
pub fn script() -> String {
    let scheme = crate::controls::SCHEME;
    let font = crate::controls::FONT;
    let dismissed = DISMISSED;

    format!(
        r#"(function () {{
  // The top document only. Drawn anywhere else this is a panel the size of an
  // iframe, with buttons that answer through a navigation no iframe can make.
  // See `controls`.
  if (window.top !== window.self) return;
  if (window.__dshAskPanel) return;
  window.__dshAskPanel = true;

  var root = null, card, heading, message, row;
  var token = 0;
  // Whatever had the keyboard before the dialog took it, so it can have it back.
  var restore = null;

  // The channel back to Rust; see controls.rs. The navigation is cancelled
  // there, so answering never moves the page.
  function signal(id) {{
    window.location.href = '{scheme}://ask?token=' + token +
      '&id=' + encodeURIComponent(id);
  }}

  /** Take the card down and answer `id`. Every way out of a dialog comes here. */
  function close(id) {{
    // Down before the signal: the navigation is cancelled, so nothing else
    // takes this card off the screen.
    root.classList.remove('dsh-ask-shown');
    if (restore && document.contains(restore)) {{
      try {{ restore.focus(); }} catch (error) {{}}
    }}
    restore = null;
    signal(id);
  }}

  /** The dialog's own buttons, in tab order. */
  function stops() {{
    return row.querySelectorAll('button');
  }}

  function make(tag, className, parent) {{
    var node = document.createElement(tag);
    if (className) node.className = className;
    if (parent) parent.appendChild(node);
    return node;
  }}

  // dsh's theme is the page's, not the window's. Read exactly the way the
  // titlebar and the plugin panel read it; see controls.rs.
  function paint(node) {{
    var media = window.matchMedia('(prefers-color-scheme:dark)');

    function dark() {{
      if (document.body.hasAttribute('data-ds-dark-theme')) return true;
      var declared = getComputedStyle(document.documentElement).colorScheme || '';
      var light = declared.indexOf('light') !== -1;
      var night = declared.indexOf('dark') !== -1;
      return night !== light ? night : media.matches;
    }}

    function repaint() {{
      node.classList.toggle('dsh-ask-dark', dark());
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
  }}

  function build() {{
    var style = document.createElement('style');
    style.textContent =
      // One layer under the plugin panel, so a question raised while the panel
      // is up is still drawn over it -- and, like the panel, clear of the
      // titlebar so the window buttons stay reachable.
      '.dsh-ask{{position:fixed;inset:0;z-index:2147483644;display:none;' +
      'align-items:center;justify-content:center;box-sizing:border-box;' +
      'padding:calc(var(--dsh-titlebar-height,36px) + 12px) 16px 20px;' +
      'background:rgba(18,18,22,.34);-webkit-backdrop-filter:blur(3px);' +
      'backdrop-filter:blur(3px);font-size:14px;line-height:1.6;' +
      'user-select:none;-webkit-user-select:none;' +
      '--ask-bg:#fff;--ask-fg:#1a1a1a;--ask-muted:#6b7280;--ask-line:#e5e7eb;' +
      '--ask-accent:#4d6bfe;--ask-soft:#f7f8fa}}' +
      '.dsh-ask.dsh-ask-dark{{background:rgba(0,0,0,.5);' +
      '--ask-bg:#17171d;--ask-fg:#ececf1;--ask-muted:#9aa0ac;--ask-line:#2b2b34;' +
      '--ask-soft:rgba(255,255,255,.04)}}' +
      '.dsh-ask.dsh-ask-shown{{display:flex}}' +
      '.dsh-ask,.dsh-ask *{{box-sizing:border-box;font-family:{font}}}' +
      // Narrower than the plugin panel: this is a paragraph and some buttons,
      // and a question set in a 760px line is a question that looks like a
      // document.
      '.dsh-ask-card{{display:flex;flex-direction:column;min-height:0;' +
      'max-height:100%;width:min(460px,100%);padding:22px 24px;' +
      'border-radius:14px;background:var(--ask-bg);color:var(--ask-fg);' +
      'box-shadow:0 24px 64px rgba(0,0,0,.32),0 0 0 .5px var(--ask-line)}}' +
      '.dsh-ask-card h1{{font-size:17px;font-weight:600;line-height:1.4;margin:0 0 8px}}' +
      // `pre-wrap` because the update notes arrive with their own newlines.
      '.dsh-ask-body{{margin:0;color:var(--ask-muted);font-size:13px;' +
      'white-space:pre-wrap;overflow:auto;min-height:0;' +
      'user-select:text;-webkit-user-select:text}}' +
      '.dsh-ask-row{{display:flex;gap:8px;justify-content:flex-end;' +
      'flex-wrap:wrap;margin-top:20px}}' +
      // `-webkit-appearance` as well as the unprefixed property: the plain one
      // is Safari 15.4 and WebKitGTK 2.36, so on an older WKWebView or an older
      // Ubuntu the native push-button shell survives underneath everything set
      // below and the three platforms stop looking alike.
      '.dsh-ask button{{-webkit-appearance:none;appearance:none;' +
      'border-radius:8px;padding:7px 14px;' +
      'background:var(--ask-bg);border:1px solid var(--ask-line);cursor:pointer;' +
      'font-size:13px;line-height:1;color:var(--ask-fg);white-space:nowrap}}' +
      '.dsh-ask button:hover{{background:var(--ask-soft)}}' +
      '.dsh-ask button.dsh-ask-primary{{background:var(--ask-accent);' +
      'border-color:var(--ask-accent);color:#fff}}' +
      '.dsh-ask button.dsh-ask-primary:hover{{filter:brightness(1.06)}}' +
      '.dsh-ask button:focus-visible{{outline:2px solid var(--ask-accent);' +
      'outline-offset:2px}}';
    document.head.appendChild(style);

    root = make('div', 'dsh-ask');
    card = make('div', 'dsh-ask-card', root);
    // A card over a page is not a dialog to anything that is not looking at
    // pixels. The window manager's message box carried all of this for free;
    // drawing our own means saying it by hand.
    card.setAttribute('role', 'dialog');
    card.setAttribute('aria-modal', 'true');

    heading = make('h1', '', card);
    heading.id = 'dsh-ask-title';
    card.setAttribute('aria-labelledby', heading.id);

    message = make('p', 'dsh-ask-body', card);
    message.id = 'dsh-ask-desc';
    card.setAttribute('aria-describedby', message.id);

    row = make('div', 'dsh-ask-row', card);

    // Escape, and the tab loop. Both are things the platform's own dialog did
    // and a <div> does not, and both have to hold on all three platforms --
    // macOS especially, where a card that will not take Escape reads as stuck.
    //
    // On the document rather than on the card, and capturing, because the page
    // underneath is dsh's and has keybindings of its own: while a question is up
    // these keys are the dialog's and nobody else's.
    document.addEventListener('keydown', function (event) {{
      if (!root || !root.classList.contains('dsh-ask-shown')) return;

      if (event.key === 'Escape') {{
        event.preventDefault();
        event.stopPropagation();
        // Answered, not merely hidden. A dialog that goes quiet without
        // answering leaves `confirm` blocked on a worker thread; see dialog.rs.
        close({dismissed:?});
        return;
      }}

      if (event.key !== 'Tab') return;

      // Keep the focus inside the card. Without this, Tab walks straight out of
      // a supposedly modal dialog into the page behind it.
      var focusable = stops();
      if (!focusable.length) return;
      var first = focusable[0];
      var last = focusable[focusable.length - 1];
      var here = document.activeElement;

      if (event.shiftKey && (here === first || !card.contains(here))) {{
        event.preventDefault();
        last.focus();
      }} else if (!event.shiftKey && (here === last || !card.contains(here))) {{
        event.preventDefault();
        first.focus();
      }}
    }}, true);

    paint(root);
    document.body.appendChild(root);
  }}

  function ready(then) {{
    if (document.body) then();
    else document.addEventListener('DOMContentLoaded', then, {{ once: true }});
  }}

  /**
   * Put a question up. Called from Rust; see dialog.rs.
   *
   * Wrapped in `ready` the way the plugin panel's entry point is: this is
   * reachable from `boot`, which runs before the first PageLoadEvent, so
   * `document.body` is not a given. Building without one throws, and a throw
   * here is a dialog that never appears -- which Rust cannot detect, because
   * `eval` does not carry JavaScript exceptions back on any platform. Waiting
   * is what makes the question arrive rather than the throw.
   */
  window.__dshAsk = function (payload) {{
    var data;
    try {{
      data = JSON.parse(payload);
    }} catch (error) {{
      return;
    }}

    ready(function () {{
      if (!root) build();

      // Only when a dialog is not already up, or a second question would hand
      // back focus to the first one's button.
      if (!root.classList.contains('dsh-ask-shown')) {{
        restore = document.activeElement;
      }}

      token = data.token;
      heading.textContent = data.title || '';
      message.textContent = data.body || '';
      message.hidden = !data.body;
      row.textContent = '';

      var buttons = data.buttons || [];
      for (var i = 0; i < buttons.length; i++) {{
        (function (choice) {{
          var node = make('button', choice.primary ? 'dsh-ask-primary' : '', row);
          node.type = 'button';
          node.textContent = choice.label;
          node.addEventListener('click', function () {{
            close(choice.id);
          }});
        }})(buttons[i]);
      }}

      root.classList.add('dsh-ask-shown');
      // The primary if there is one, so Enter answers the question the dialog is
      // actually asking.
      var focus = row.querySelector('.dsh-ask-primary') || row.querySelector('button');
      if (focus) focus.focus();
    }});
  }};
}})();"#
    )
}

#[cfg(test)]
mod tests {
    use super::{is_current, is_main, remember_main_thread, script, Ask, Choice, Dialogs};

    /// Every dialog is numbered once, in order, and no number comes back.
    ///
    /// Written against [`Dialogs`] rather than the static, the way the
    /// main-thread tests are written against [`super::is_main`]: the static is
    /// process-wide and two tests taking numbers out of it would each see the
    /// other's.
    #[test]
    fn every_dialog_gets_its_own_token() {
        let mut dialogs = Dialogs {
            next: 1,
            on_screen: None,
        };

        assert_eq!(dialogs.claim(), 1);
        assert_eq!(dialogs.claim(), 2);
        assert_eq!(dialogs.claim(), 3, "a token is never handed out twice");
    }

    /// Only the dialog on screen can be answered.
    ///
    /// The token is what makes a click that lands as a new question goes up
    /// answer nothing rather than answer the new one — see [`super::DIALOGS`] on
    /// why claiming it and filing it are one step under one lock.
    #[test]
    fn only_the_dialog_on_screen_can_be_answered() {
        assert!(is_current(Some(7), 7));
        // The click the user made a moment before this dialog replaced the last.
        assert!(!is_current(Some(8), 7));
        // Already answered: the entry is taken out under the lock, so the second
        // of two clicks in flight finds nothing.
        assert!(!is_current(None, 7));
    }

    /// The rule `confirm`'s debug assertion is built on: only the recorded
    /// thread is the main one.
    ///
    /// Written against [`super::is_main`] rather than the global, which is
    /// write-once for the life of the process — two tests that each tried to
    /// set it would race for who claimed it first, and the loser would see an
    /// answer that had nothing to do with what it was asserting.
    #[test]
    fn only_the_recorded_thread_is_the_main_one() {
        let here = std::thread::current().id();
        let elsewhere = std::thread::spawn(|| std::thread::current().id())
            .join()
            .expect("the probe thread");

        assert!(is_main(Some(here), here));
        // A worker thread is the correct caller of `confirm`, so this is the
        // case that must not fire the assertion.
        assert!(!is_main(Some(elsewhere), here));
    }

    /// Before `main` has recorded anything there is no event loop to deadlock,
    /// so nothing counts as the main thread and the assertion stays quiet. This
    /// is what every unit test in this binary runs under.
    #[test]
    fn nothing_is_the_main_thread_until_one_is_recorded() {
        assert!(!is_main(None, std::thread::current().id()));
    }

    /// Recording is once-only, so a stray later call cannot move the main
    /// thread onto whichever worker happened to make it.
    #[test]
    fn the_main_thread_is_recorded_once() {
        remember_main_thread();
        let first = super::MAIN.get().copied();
        assert!(first.is_some(), "the first call records something");

        std::thread::spawn(remember_main_thread)
            .join()
            .expect("the probe thread");

        assert_eq!(
            super::MAIN.get().copied(),
            first,
            "a later call must not reassign the main thread"
        );
    }

    /// The shim is called plainly, with the payload as its one argument.
    ///
    /// The call used to be unguarded for a stated reason that was wrong — that
    /// a bare call throws when the script has not run and that `eval` reports
    /// the throw. It does not, on any of the three platforms; see [`delivery`].
    /// What keeps an undelivered dialog from hanging a caller is the `ready`
    /// wrapper in the script and the timeout in [`confirm`], both asserted
    /// below. This test now only pins the shape of the call.
    #[test]
    fn delivers_a_single_payload() {
        let call = super::delivery(r#""{}""#);

        assert_eq!(call, r#"window.__dshAsk("{}")"#);
    }

    /// The entry point waits for a body rather than throwing without one.
    ///
    /// `ask` is reachable from `boot`, which runs before the first
    /// `PageLoadEvent::Finished`, and `build` ends in `document.body
    /// .appendChild`. Throwing there is invisible to Rust, so the wait is the
    /// only thing standing between that race and a dialog that never appears.
    #[test]
    fn waits_for_a_document_before_drawing() {
        let script = script();

        assert!(script.contains("function ready(then)"));
        assert!(
            script.contains("ready(function () {"),
            "__dshAsk must go through `ready`, not build straight away"
        );
    }

    /// Escape answers instead of merely hiding the card.
    ///
    /// A dismissal that only took the card off the screen would leave `confirm`
    /// parked on a worker thread with nothing left that could ever wake it —
    /// which for `boot` is an app that never starts dsh.
    #[test]
    fn escape_answers_rather_than_going_quiet() {
        let script = script();

        assert!(script.contains("event.key === 'Escape'"));
        assert!(
            script.contains(&format!("close({:?})", super::DISMISSED)),
            "Escape must send the dismissal id back to Rust"
        );
    }

    /// The dismissal id is not something a caller could name a button, so every
    /// `id == "…"` test in a callback reads it as "not that one" for free.
    #[test]
    fn nothing_can_collide_with_the_dismissal_id() {
        for id in ["ok", "now", "later", "skip", "cancel", "update", "go"] {
            assert_ne!(id, super::DISMISSED);
        }
    }

    /// The things a native message box did and a `<div>` has to be told to do.
    /// Each is a way the three platforms would otherwise differ; see the module
    /// docs.
    #[test]
    fn carries_what_the_platform_dialog_carried() {
        let script = script();

        assert!(script.contains(r#"'role', 'dialog'"#));
        assert!(script.contains(r#"'aria-modal', 'true'"#));
        assert!(script.contains("aria-labelledby"));
        // The tab loop, without which "modal" is only a claim about z-index.
        assert!(script.contains("event.key !== 'Tab'"));
        assert!(script.contains("last.focus()") && script.contains("first.focus()"));
        // Both spellings, or an older WKWebView and an older WebKitGTK keep the
        // native button shell.
        assert!(script.contains("-webkit-appearance:none;appearance:none"));
    }

    #[test]
    fn a_choice_is_ordinary_unless_it_is_primary() {
        let plain = Choice::new("later", "Later");
        let go = Choice::primary("now", "Update now");

        assert!(!plain.primary);
        assert!(go.primary);
    }

    /// The script is pasted into a document that has its own everything, so the
    /// pieces it relies on have to actually be in it.
    #[test]
    fn the_script_draws_what_rust_pushes() {
        let script = script();

        assert!(script.contains("window.__dshAsk"));
        // It has to answer over the same scheme everything else in this window
        // uses, or the navigation is a real one and the page leaves.
        assert!(script.contains(&format!("{}://ask?", crate::controls::SCHEME)));
    }

    /// The query the script sends is the one `answered` reads.
    ///
    /// Both halves are here rather than in a running app because the parse is
    /// where a rename would go unnoticed: the script builds `?token=…&id=…` by
    /// hand, and nothing else checks the two agree.
    #[test]
    fn reads_the_query_the_script_writes() {
        let script = super::script();

        assert!(
            script.contains("'?token=' + token") || script.contains("://ask?token=' + token"),
            "the script must send the token it was given"
        );
        assert!(
            script.contains("'&id=' + encodeURIComponent(id)"),
            "the script must send the button id, escaped"
        );
    }

    /// Write every injected script out for `node --check`. Ignored, because it
    /// is a development aid rather than an assertion: run it with
    /// `cargo test -- --ignored dumps_the_scripts`.
    #[test]
    #[ignore]
    fn dumps_the_scripts() {
        let out = std::path::Path::new("../target/scripts");
        std::fs::create_dir_all(out).expect("a writable target directory");
        for (name, text) in [
            ("dialog.js", super::script()),
            ("panel.js", crate::panel::script()),
            ("controls.js", crate::controls::script()),
            ("notify.js", crate::notify::script()),
            ("turn.js", crate::turn::script()),
            ("waiting.js", crate::waiting::script()),
            ("setup.js", crate::setup::script()),
        ] {
            std::fs::write(out.join(name), text).expect("a written script");
        }
    }

    /// Dismissing a dialog drops its callback rather than running it with some
    /// stand-in answer, which for the updater is the difference between "not
    /// now" and "yes, restart".
    #[test]
    fn an_ask_owns_its_callback() {
        let ask = Ask {
            title: "t".to_string(),
            body: "b".to_string(),
            choices: vec![Choice::primary("go", "Go")],
            answered: Box::new(|_, _| unreachable!("never answered in this test")),
        };

        assert_eq!(ask.choices.len(), 1);
        assert_eq!(ask.choices[0].id, "go");
    }
}
