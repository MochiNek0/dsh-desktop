//! Telling the user a turn finished while they were looking somewhere else.
//!
//! [`crate::notify`] already turns a page's `Notification` call into a real
//! toast, and suppresses it when the window is in front. What it cannot do is
//! decide that a turn ended: nothing in the page raises a notification unless a
//! plugin was installed to do it, so the machinery sits there with nobody
//! calling it. This module is the caller. It watches dsh's own UI for the moment
//! a run stops and raises exactly one ordinary `Notification` when it does,
//! which then travels the path notify.rs already built.
//!
//! Deliberately over the standard API rather than over a private channel of its
//! own: a user who later installs a plugin that announces turns should get one
//! notification, not two. Going through `window.Notification` means both paths
//! land in the same suppression check, and it keeps this module to the one
//! question it is actually qualified to answer — has the run stopped.
//!
//! ## What it watches
//!
//! The primary button at the right of the input bar. dsh renders it from a
//! single `running` flag: while a turn is going it is a stop button whose only
//! child is `<svg><rect/></svg>`, and the rest of the time it is a send button
//! whose child is `<svg><path/></svg>`. So "a turn is running" is "the primary
//! button's svg holds a rect", which is a shape rather than a word.
//!
//! That matters, because the obvious signals are all worse:
//!
//! - **The `aria-label`** is the translated string for stop or send. Reading it
//!   would mean shipping dsh's translations in this app and keeping them in step
//!   with a package this app does not version.
//! - **`data-phase`** on the textarea covers submission (`submitting`,
//!   `adjudicating`), not the streaming that follows it, so it clears while the
//!   model is still working.
//! - **CSS class names** are hashed per build by CSS modules.
//!
//! The class attribute is still used, but only as a hint for *finding* the
//! button among the others in the bar — never for deciding the state. If the
//! hint stops matching, the search falls back to the last button in the bar.
//!
//! ## Why it can be wrong without being harmful
//!
//! Everything here is a guess about someone else's DOM, so the failure that
//! matters is the noisy one. Three things keep it quiet: the notification is
//! raised only on a true edge — running, then not running — so a redraw that
//! does not change the state raises nothing; the run has to have lasted past
//! [`SETTLE`] before it counts, so clicking send and immediately stopping is not
//! an event; and if the button is never found, the observer simply never sees an
//! edge and the feature is absent rather than wrong.
//!
//! Suppression when the window is focused is not repeated here. It belongs to
//! notify.rs, which can ask the OS; the page cannot — see the note on
//! `document.hidden` there.

/// How long a run has to last before finishing it is worth a toast, in
/// milliseconds. Below this the user is still sitting at the window that just
/// answered them.
const SETTLE: u32 = 4_000;

/// How long the button has to hold its new state before the change is believed,
/// in milliseconds. React swaps the two arms of the button in one commit, but a
/// re-render can momentarily drop it out of the tree, and a stop-start flicker
/// inside one turn should not read as two turns.
const DEBOUNCE: u32 = 700;

/// The watcher, injected into every document the window loads.
pub fn script() -> String {
    let settle = SETTLE;
    let debounce = DEBOUNCE;
    let title = t!("对话已完成", "Turn finished");
    let body = t!(
        "dsh 已经处理完这一轮，可以回来看看了。",
        "dsh has finished this turn and is waiting for you."
    );

    format!(
        r#"(function () {{
  if (window.__dshTurnWatch) return;
  window.__dshTurnWatch = true;

  var TITLE = {title:?};
  var BODY = {body:?};

  // Null until a turn has actually been seen to start; the timestamp it started
  // at once one has. Kept as a time rather than a flag so the settle test below
  // is a subtraction.
  var startedAt = null;
  var pending = null;

  /** The primary button of the input bar, or null when the bar is not up. */
  function primary() {{
    // The class is hashed per build -- it reads `uV2eYG_primary` in the build
    // this was confirmed against, where `uV2eYG` is the hash and `primary` the
    // name from the stylesheet. So this matches the readable half and never the
    // whole, and it is only ever a hint. See the module docs.
    //
    // The last match rather than the first: `primary` also appears on buttons
    // elsewhere in dsh's chrome, and the input bar is the deepest thing on the
    // page that has one.
    var buttons = document.querySelectorAll('button[class*="primary"]');
    if (buttons.length) return buttons[buttons.length - 1];

    // No hint matched. The primary button is the last one in the bar that owns
    // the textarea, which is the one piece of this that dsh is not free to
    // rename. The bar is a plain <div>, not a <form>, so this walks up from
    // the textarea rather than asking `closest('form')` for one.
    var input = document.querySelector('textarea[data-phase]');
    if (!input) return null;

    var bar = input.parentElement;
    for (var depth = 0; bar && depth < 6; depth++) {{
      var found = bar.querySelectorAll('button');
      if (found.length) return found[found.length - 1];
      bar = bar.parentElement;
    }}
    return null;
  }}

  /**
   * Whether a turn is running: the primary button is showing the stop glyph,
   * which is the one <rect> dsh draws it with. A send button is a <path>.
   */
  function running() {{
    var button = primary();
    if (!button) return null;
    if (button.querySelector('svg rect')) return true;
    if (button.querySelector('svg path')) return false;
    // A button with neither is a button mid-render; saying nothing leaves the
    // last known state alone rather than inventing an edge.
    return null;
  }}

  function announce() {{
    try {{
      // Straight down the standard API, so this lands in the same place a
      // plugin's own notification would -- including notify.rs's check for
      // whether the user is already looking at the window.
      new Notification(TITLE, {{ body: BODY, tag: 'dsh-turn' }});
    }} catch (error) {{
      // A toast is a courtesy; it is not worth throwing inside an observer
      // callback that dsh's own rendering is driving.
    }}
  }}

  /** Read the button and act only on a settled change. */
  function sample() {{
    var now = running();
    if (now === null) return;

    if (now) {{
      // Already counted; a re-render during a run is not a new one.
      if (startedAt === null) startedAt = Date.now();
      return;
    }}

    if (startedAt === null) return;
    var ran = Date.now() - startedAt;
    startedAt = null;
    // A turn that was over almost as soon as it began is one the user watched.
    if (ran >= {settle}) announce();
  }}

  function schedule() {{
    if (pending !== null) clearTimeout(pending);
    pending = setTimeout(function () {{
      pending = null;
      sample();
    }}, {debounce});
  }}

  function watch() {{
    // The whole document: the input bar is replaced wholesale when the session
    // changes, so observing the bar itself would mean re-attaching every time
    // it goes. Attribute and character data are not watched -- only the
    // structural swap of one svg child for the other matters here.
    new MutationObserver(schedule).observe(document.body, {{
      childList: true,
      subtree: true
    }});
    // The state as it stands at load, so a page that opens mid-run has a start
    // to measure the end against.
    sample();
  }}

  if (document.body) watch();
  else document.addEventListener('DOMContentLoaded', watch, {{ once: true }});
}})();"#
    )
}

#[cfg(test)]
mod tests {
    use super::{script, DEBOUNCE, SETTLE};

    /// The script is pasted into a webview whole, so the thing worth asserting
    /// is that the parts that vary — both constants and both translated strings
    /// — actually reached it.
    #[test]
    fn carries_its_constants_and_strings() {
        let script = script();

        assert!(script.contains(&SETTLE.to_string()));
        assert!(script.contains(&DEBOUNCE.to_string()));
        assert!(script.contains(t!("对话已完成", "Turn finished")));
    }

    /// Both strings go in through `{:?}`, so a quote or a backslash in either
    /// arrives escaped rather than ending the literal early.
    #[test]
    fn quotes_the_strings_it_pastes() {
        let script = script();
        let title = format!("{:?}", t!("对话已完成", "Turn finished"));

        assert!(script.contains(&format!("var TITLE = {title};")));
    }

    /// A turn has to last a noticeable while before finishing it is news, and
    /// the debounce has to be shorter than that or it would swallow the run it
    /// is smoothing. Checked at compile time: both are constants, so a bad pair
    /// should never get as far as being a failing test.
    #[test]
    fn waits_longer_than_it_debounces() {
        const { assert!(SETTLE > DEBOUNCE) };
    }

    /// The watcher decides on shapes, not on words.
    ///
    /// Confirmed against a real build, whose input bar reported four buttons —
    /// `uV2eYG_add`, `Sh0Q9G_trigger`, `_7KE1Ra_trigger` and `uV2eYG_primary`,
    /// the last of them labelled 发送消息. Three things follow, and each is
    /// something a later edit could quietly break:
    ///
    /// - the class hint is a substring (`primary`), because the hash in front
    ///   of it changes every build;
    /// - the pick is the *last* match, because three of those four buttons are
    ///   ahead of the one that matters;
    /// - the state is read off `svg rect` against `svg path`, because every one
    ///   of those buttons carries an `aria-label` in the user's language and
    ///   matching on that would mean shipping dsh's translations.
    #[test]
    fn reads_the_button_by_shape_not_by_label() {
        let script = script();

        assert!(script.contains(r#"'button[class*="primary"]'"#));
        assert!(script.contains("buttons[buttons.length - 1]"));
        assert!(script.contains("'svg rect'"));
        assert!(script.contains("'svg path'"));
        // Never the label: it is 发送消息 or Send depending on the locale.
        assert!(
            !script.contains("aria-label=\"发送"),
            "the watcher must not depend on a translated label"
        );
    }

}
