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
//! matters is the noisy one. Four things keep it quiet: the notification is
//! raised only on a true edge — running, then not running — so a redraw that
//! does not change the state raises nothing; that edge has to survive
//! [`CONFIRM`] reads in a row, so a re-render sampled mid-swap is not a turn;
//! the run has to have lasted past [`SETTLE`] before it counts, so clicking send
//! and immediately stopping is not an event; and if the button is never found,
//! the reads simply never see an edge and the feature is absent rather than
//! wrong.
//!
//! ## How it reads it
//!
//! On a timer of its own, every [`PERIOD`]. What the button is showing is a
//! state rather than an event, and the two mistakes this module has already made
//! were both about trying to catch the event: the state it must not read is one
//! of *this app's* own buttons (see `ours` in [`script`]) and the moment it must
//! not miss is the middle of a stream (see [`PERIOD`]).
//!
//! Suppression when the window is focused is not repeated here. It belongs to
//! notify.rs, which can ask the OS; the page cannot — see the note on
//! `document.hidden` there.

/// How long a run has to last before finishing it is worth a toast, in
/// milliseconds. Below this the user is still sitting at the window that just
/// answered them.
const SETTLE: u32 = 4_000;

/// How often the button is read, in milliseconds.
///
/// A clock of its own rather than dsh's mutations. This was a `MutationObserver`
/// over the whole document feeding a trailing debounce, and the debounce was
/// reset by every batch — so while dsh streamed a turn, with mutations far
/// closer together than the wait, it never sampled at all. It never recorded
/// that a turn had started, and so had nothing to announce when it ended: the
/// one moment this module exists for was the one moment it could not see. A
/// fixed period cannot be starved, and it also takes the per-mutation
/// bookkeeping of an observer off dsh's own render path.
const PERIOD: u32 = 500;

/// How many reads in a row have to agree that a run has stopped before it is
/// announced.
///
/// What the old debounce bought, kept: React swaps the two arms of the button in
/// one commit, but a re-render can momentarily drop it out of the tree, and a
/// stop-start flicker inside one turn should not read as two turns. Only the
/// falling edge is confirmed — recording a start a moment early costs nothing,
/// while announcing a finish that was really a re-render is the one loud way
/// this can be wrong.
const CONFIRM: u32 = 2;

/// The watcher, injected into every document the window loads.
pub fn script() -> String {
    let settle = SETTLE;
    let period = PERIOD;
    let confirm = CONFIRM;
    let title = t!("对话已完成", "Turn finished");
    let body = t!(
        "dsh 已经处理完这一轮，可以回来看看了。",
        "dsh has finished this turn and is waiting for you."
    );

    format!(
        r#"(function () {{
  // The top document only. There is one dsh UI to watch, and a copy of this
  // running in every iframe the page opens is a timer apiece announcing
  // through a navigation no iframe can make. See `controls`.
  if (window.top !== window.self) return;
  if (window.__dshTurnWatch) return;
  window.__dshTurnWatch = true;

  var TITLE = {title:?};
  var BODY = {body:?};

  // Null until a turn has actually been seen to start; the timestamp it started
  // at once one has. Kept as a time rather than a flag so the settle test below
  // is a subtraction.
  var startedAt = null;
  // How many reads in a row have said the run has stopped; see CONFIRM in
  // turn.rs.
  var idle = 0;

  /**
   * Whether a node belongs to this app's own chrome rather than to dsh's page.
   *
   * Both of this app's cards draw a button whose class contains `primary` --
   * `dsh-pp-primary` in the plugin panel, `dsh-ask-primary` in a dialog -- and
   * both cards are appended to `document.body`, so they come after dsh's
   * composer in document order. Neither is removed when it is put away: the
   * panel and the dialog both only drop a class. So from the first time either
   * one is opened over dsh's page -- the plugin panel from the window menu, or
   * an "up to date" dialog -- the last `primary` button on the page is one of
   * ours, it has no svg in it, and `running()` would answer null for the rest
   * of the document's life. The feature would be gone with nothing to say so.
   */
  function ours(node) {{
    return !!node.closest('.dsh-ask, .dsh-pp');
  }}

  /** The primary button of the input bar, or null when the bar is not up. */
  function primary() {{
    // The class is hashed per build -- it reads `uV2eYG_primary` in the build
    // this was confirmed against, where `uV2eYG` is the hash and `primary` the
    // name from the stylesheet. So this matches the readable half and never the
    // whole, and it is only ever a hint. See the module docs.
    //
    // The last match rather than the first: `primary` also appears on buttons
    // elsewhere in dsh's chrome, and the input bar is the deepest thing on the
    // page that has one. Skipping this app's own cards on the way back, for the
    // reason `ours` gives.
    var buttons = document.querySelectorAll('button[class*="primary"]');
    for (var i = buttons.length - 1; i >= 0; i--) {{
      if (!ours(buttons[i])) return buttons[i];
    }}

    // No hint matched. The primary button is the last one in the bar that owns
    // the textarea, which is the one piece of this that dsh is not free to
    // rename. The bar is a plain <div>, not a <form>, so this walks up from
    // the textarea rather than asking `closest('form')` for one.
    var input = document.querySelector('textarea[data-phase]');
    if (!input) return null;

    var bar = input.parentElement;
    for (var depth = 0; bar && depth < 6; depth++) {{
      // Ours are skipped here too: six levels up from the textarea can reach
      // `document.body`, and both cards hang off it.
      var found = bar.querySelectorAll('button');
      for (var at = found.length - 1; at >= 0; at--) {{
        if (!ours(found[at])) return found[at];
      }}
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
      // A toast is a courtesy; it is not worth throwing out of a timer dsh's
      // own rendering shares.
    }}
  }}

  /** Read the button and act only on a confirmed change. */
  function sample() {{
    var now = running();
    if (now === null) return;

    if (now) {{
      idle = 0;
      // Already counted; a re-render during a run is not a new one.
      if (startedAt === null) startedAt = Date.now();
      return;
    }}

    if (startedAt === null) return;
    // Not on the first look: see CONFIRM in turn.rs. The reads keep coming
    // whatever the page does, so waiting for the next one cannot mean waiting
    // forever -- which is exactly what it did mean when the sampling was
    // driven by mutations.
    if (++idle < {confirm}) return;

    var ran = Date.now() - startedAt;
    startedAt = null;
    idle = 0;
    // A turn that was over almost as soon as it began is one the user watched.
    if (ran >= {settle}) announce();
  }}

  // No wait for a document: `primary()` only ever queries, and a query against
  // a document that has not built its body yet finds nothing, which reads as
  // "say nothing" like any other quiet moment.
  setInterval(sample, {period});
}})();"#
    )
}

#[cfg(test)]
mod tests {
    use super::{script, CONFIRM, PERIOD, SETTLE};

    /// The script is pasted into a webview whole, so the thing worth asserting
    /// is that the parts that vary — both constants and both translated strings
    /// — actually reached it.
    #[test]
    fn carries_its_constants_and_strings() {
        let script = script();

        assert!(script.contains(&SETTLE.to_string()));
        assert!(script.contains(&PERIOD.to_string()));
        assert!(script.contains(&CONFIRM.to_string()));
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
    /// confirming the finish has to take less than that — otherwise the reads
    /// that confirm it would themselves push every run past [`SETTLE`] and the
    /// threshold would mean nothing. Checked at compile time: all three are
    /// constants, so a bad set should never get as far as being a failing test.
    #[test]
    fn waits_longer_than_it_takes_to_confirm() {
        const { assert!(SETTLE > PERIOD * CONFIRM) };
    }

    /// It samples on a clock of its own, not on dsh's mutations.
    ///
    /// This was a `MutationObserver` over the whole document driving a trailing
    /// debounce that every batch reset. While dsh streamed — mutations far
    /// closer together than the wait — it never sampled, so it never recorded
    /// that a turn had started and had nothing to announce when it ended. The
    /// moment the feature exists for was the moment it could not see.
    #[test]
    fn samples_on_a_clock_of_its_own() {
        let script = script();

        assert!(script.contains(&format!("setInterval(sample, {PERIOD})")));
        assert!(
            !script.contains("MutationObserver"),
            "a mutation-driven wait can be starved by the very stream it is watching"
        );
    }

    /// It never reads one of this app's own buttons.
    ///
    /// Both of this app's cards draw a `…-primary` button, both are appended to
    /// `document.body` — so after dsh's composer in document order — and
    /// neither is removed when it is put away. So from the first time either is
    /// opened over dsh's page, the last `primary` button on the page is one of
    /// ours, it holds no svg, and `running()` answers null for the rest of the
    /// document's life: the notification would be gone with nothing on screen
    /// to say so.
    ///
    /// Written across the three modules on purpose. The classes are pasted into
    /// three separate scripts, and renaming one of the cards without this test
    /// would silence the watcher rather than fail a build.
    #[test]
    fn never_reads_this_app_s_own_buttons() {
        let script = script();
        let panel = crate::panel::script();
        let dialog = crate::dialog::script();

        assert!(script.contains("node.closest('.dsh-ask, .dsh-pp')"));

        // The two roots it excludes, as those panels actually name them.
        assert!(panel.contains("make('div', 'dsh-pp')"));
        assert!(dialog.contains("make('div', 'dsh-ask')"));
        // And both really do draw a button the hint above would otherwise match.
        assert!(panel.contains("'dsh-pp-primary'"));
        assert!(dialog.contains("'dsh-ask-primary'"));
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
    /// - the pick walks *backwards*, because three of those four buttons are
    ///   ahead of the one that matters;
    /// - the state is read off `svg rect` against `svg path`, because every one
    ///   of those buttons carries an `aria-label` in the user's language and
    ///   matching on that would mean shipping dsh's translations.
    #[test]
    fn reads_the_button_by_shape_not_by_label() {
        let script = script();

        assert!(script.contains(r#"'button[class*="primary"]'"#));
        assert!(script.contains("for (var i = buttons.length - 1; i >= 0; i--)"));
        assert!(script.contains("'svg rect'"));
        assert!(script.contains("'svg path'"));
        // Never the label: it is 发送消息 or Send depending on the locale.
        assert!(
            !script.contains("aria-label=\"发送"),
            "the watcher must not depend on a translated label"
        );
    }

}
