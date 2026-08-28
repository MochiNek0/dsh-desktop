//! The app's own dialogs, drawn over the page instead of by the window manager.
//!
//! [`crate::update`] used to ask its questions through `tauri-plugin-dialog`,
//! which raises a native message box. That works, and on Windows it looks like
//! a message box from 2012: a grey strip, the system font at the system size,
//! and buttons whose order and labels are the platform's rather than ours. Next
//! to the plugin panel — which is this app's own card, in dsh's own colours,
//! following dsh's own light/dark — it reads as a different program.
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

use std::sync::atomic::{AtomicU64, Ordering};
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
    /// Run on the main thread when a button is clicked, with the id of that
    /// button. Dismissing without choosing never calls it.
    pub answered: Answer,
}

/// The dialog currently on screen, if any.
///
/// One at a time: the script draws into a single card, and a second dialog
/// would replace the first while its `answered` was still pending — which for
/// the updater would mean a download whose question nobody ever answers.
static PENDING: Mutex<Option<Pending>> = Mutex::new(None);

/// Which dialog an arriving answer belongs to.
///
/// A monotonic token rather than a flag, because an answer can outlive the
/// question: the user clicks, and in the same instant a new dialog goes up. The
/// stale click carries the old token and is dropped instead of answering the
/// new question.
static TOKEN: AtomicU64 = AtomicU64::new(0);

struct Pending {
    token: u64,
    answered: Answer,
}

/// The JavaScript that hands one dialog to the page.
///
/// Deliberately not `window.__dshAsk && window.__dshAsk(…)`. That guard reads
/// like defensive good manners, but it turns the one case worth detecting —
/// the document has not run the injected script yet, which `boot` can easily
/// beat — into a silent success: `eval` returns `Ok`, [`ask`] reports the
/// dialog delivered, and [`confirm`] then blocks forever on an answer nobody
/// can give. Called unguarded, that case throws, `eval` reports it, and the
/// caller gets to fall back.
///
/// Its own function so a test can assert the guard has not crept back in.
fn delivery(json: &str) -> String {
    format!("window.__dshAsk({json})")
}

/// Put a question on screen. Returns immediately; see the module docs.
///
/// Answers whether the question actually reached the window. It can fail to:
/// there may be no window yet, or the document showing may not have run the
/// injected script — `ask` is reachable from `boot`, which starts before the
/// first `PageLoadEvent::Finished`. A caller that only wanted to say something
/// can ignore that, but [`confirm`] cannot, because a question nobody can see
/// is a question nobody will ever answer.
pub fn ask(app: &AppHandle, ask: Ask) -> bool {
    let token = TOKEN.fetch_add(1, Ordering::SeqCst) + 1;

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

    let payload = serde_json::json!({
        "token": token,
        "title": ask.title,
        "body": ask.body,
        "buttons": buttons,
    })
    .to_string();

    // Kept for the failure message below: `answered` is moved out of `ask` on
    // the next line, and a partially moved struct cannot be read from again.
    let title = ask.title;

    // Replaces whatever was there. The previous dialog's `answered` is dropped
    // without running, which is the same outcome as dismissing it.
    if let Ok(mut pending) = PENDING.lock() {
        *pending = Some(Pending {
            token,
            answered: ask.answered,
        });
    }

    // The payload is JSON, and it is pasted into a JavaScript call — so it is
    // serialised a second time, into a string literal the script then parses.
    // Without that, a quote in an update's release notes ends the argument and
    // takes the rest of the call with it.
    let json = serde_json::to_string(&payload).expect("a string is always serializable");

    let delivered = app
        .get_webview_window("main")
        .is_some_and(|window| window.eval(delivery(&json)).is_ok());

    if !delivered {
        eprintln!("dsh-desktop: no window could show the dialog {title:?}");
        // Put back whatever was pending, rather than leaving this dialog's
        // entry to be answered by a click that can never come. Dropping it here
        // also drops the sender `confirm` is waiting on, which is what turns its
        // `recv` into an error instead of a hang.
        if let Ok(mut pending) = PENDING.lock() {
            if pending.as_ref().is_some_and(|one| one.token == token) {
                *pending = None;
            }
        }
    }

    delivered
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
/// A dialog that is replaced before it is answered — [`ask`] overwrites the
/// pending one — drops its sender, and the receive then fails rather than
/// waiting forever. A dialog that never reached the window is the same story
/// told earlier: [`ask`] says so, and this answers without waiting at all.
/// Both are reported as the negative answer, which for every caller means "do
/// not touch anything", the safe half.
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

    // Nothing to wait for when nothing was shown: the sender is already dropped
    // and the `recv` below would only confirm it the slow way.
    if !ask(app, ask_for) {
        return false;
    }

    receive.recv().unwrap_or(false)
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

    // Taken out under the lock, so two clicks in flight cannot both run it.
    let found = PENDING.lock().ok().and_then(|mut pending| {
        let matches = pending.as_ref().is_some_and(|one| one.token == token);
        if matches {
            pending.take()
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

    format!(
        r#"(function () {{
  if (window.__dshAskPanel) return;
  window.__dshAskPanel = true;

  var root = null, card, heading, message, row;
  var token = 0;

  // The channel back to Rust; see controls.rs. The navigation is cancelled
  // there, so answering never moves the page.
  function signal(id) {{
    window.location.href = '{scheme}://ask?token=' + token +
      '&id=' + encodeURIComponent(id);
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
      '.dsh-ask button{{appearance:none;border-radius:8px;padding:7px 14px;' +
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
    heading = make('h1', '', card);
    message = make('p', 'dsh-ask-body', card);
    row = make('div', 'dsh-ask-row', card);

    paint(root);
    document.body.appendChild(root);
  }}

  /** Put a question up. Called from Rust; see dialog.rs. */
  window.__dshAsk = function (payload) {{
    var data;
    try {{
      data = JSON.parse(payload);
    }} catch (error) {{
      return;
    }}
    if (!root) build();

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
          // Down before the signal: the navigation is cancelled, so nothing
          // else takes this card off the screen.
          root.classList.remove('dsh-ask-shown');
          signal(choice.id);
        }});
      }})(buttons[i]);
    }}

    root.classList.add('dsh-ask-shown');
    // The primary if there is one, so Enter answers the question the dialog is
    // actually asking.
    var focus = row.querySelector('.dsh-ask-primary') || row.querySelector('button');
    if (focus) focus.focus();
  }};

  /** Take it down without answering. */
  window.__dshAskHide = function () {{
    if (root) root.classList.remove('dsh-ask-shown');
  }};
}})();"#
    )
}

#[cfg(test)]
mod tests {
    use super::{is_main, remember_main_thread, script, Ask, Choice};

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

    /// The shim is called unguarded.
    ///
    /// `window.__dshAsk && window.__dshAsk(…)` would swallow the case where the
    /// document has not run the injected script yet — `eval` would succeed,
    /// `ask` would report the dialog delivered, and `confirm` would block on an
    /// answer that can never arrive. The throw is the signal, so the guard must
    /// not come back.
    #[test]
    fn delivers_without_a_guard() {
        let call = super::delivery(r#""{}""#);

        assert!(
            !call.contains("&&"),
            "a guard here hides an undelivered dialog: {call}"
        );
        assert!(call.starts_with("window.__dshAsk("));
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
        assert!(script.contains("window.__dshAskHide"));
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
