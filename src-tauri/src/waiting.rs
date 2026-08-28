//! Telling the user dsh is blocked on *them*.
//!
//! [`crate::turn`] answers one question — has the run stopped — and raises a
//! toast when it has. That leaves the other half of the wait uncovered. When
//! dsh asks something mid-turn the run has not stopped: the agent is sitting
//! inside a tool call with the answer as its return value, so the turn watcher
//! correctly sees nothing and says nothing. The user, who closed the window
//! into the tray because the turn was going to take a while, is left with a
//! session that will wait for them indefinitely and never says so.
//!
//! This module raises that one. It goes down the same path everything else
//! does — an ordinary `window.Notification`, through the shim
//! [`crate::notify`] installs — so it lands in the same check for whether the
//! user is already looking at the window, and under the same preference.
//!
//! ## What it watches
//!
//! dsh asks all three of its questions by *taking over the composer*: the input
//! bar is unmounted and a card is mounted in its place, because a blocked agent
//! has nothing to do with a message the user types instead. Each card carries a
//! stable data attribute naming the request it is waiting on:
//!
//! - `data-approval-key`, from `dsh-client-ui-conversation`, when a tool asks
//!   to do something privileged;
//! - `data-plan-review-key`, from `dsh-client-ui-user-questions`, when
//!   `exit_plan_mode` wants a verdict on a plan;
//! - `data-question-key`, from the same package, when the `ask_user` tool has a
//!   question.
//!
//! Those are firmer ground than the shapes [`crate::turn`] has to read. That
//! watcher sniffs an svg child because everything nameable about the send
//! button — its label, its class — is either translated or hashed per build.
//! Here the attribute is written by the component itself, in the clear, and no
//! stylesheet rename or new translation moves it. If dsh ever drops them the
//! feature goes quiet rather than wrong, which is the same failure mode as the
//! turn watcher and the acceptable one.
//!
//! ## Keyed by the request, not by presence
//!
//! The attribute's value is the pending request's own key, and that is what is
//! compared — not merely whether a card is on screen. dsh replays pending state
//! on reconnect and on resync, remounting the same card with the same key; a
//! presence flag would announce every one of those as a fresh question.
//! Comparing the key makes a replay silent and a genuinely new request — the
//! next question in a queue, which becomes visible the moment the first is
//! answered — audible.
//!
//! ## The first seconds are a baseline
//!
//! A document that loads with a question already pending records it and says
//! nothing. The alternative announces one on every navigation and every reload
//! that lands while dsh happens to be waiting, which is noise about something
//! the user already knows. Only a change away from what this document arrived
//! with is news.
//!
//! What it arrived with takes a moment to appear, which is why the baseline is a
//! window and not a first look: dsh's page is a shell when this script runs, and
//! a card replaying a pending request is mounted by a render and a resync that
//! both come later. A single look would record "nothing pending" every time and
//! then announce the replay. See [`GRACE`].
//!
//! ## It cannot double up with the turn watcher
//!
//! The two edges are disjoint, and the DOM is what makes them so rather than a
//! flag shared between the modules. While a card is up the input bar is gone,
//! so `turn`'s `primary()` finds no button of dsh's carrying both the hashed
//! `primary` class and an svg — the only other one in the build this was checked
//! against belongs to the models settings dialog, and this app's own cards are
//! skipped by name; see `ours` there — and its `running()` answers `null`, which
//! it treats as "say nothing, keep the last known state". The turn watcher
//! therefore stays quiet for the whole wait and fires once, later, when the
//! answer has been given and the run actually ends.

/// How often the composer is read, in milliseconds.
///
/// A clock of its own, for the reason [`crate::turn::PERIOD`] gives: this was a
/// `MutationObserver` feeding a debounce that every batch reset, and anything
/// still redrawing the page — a stream that has not stopped, an elapsed-time
/// counter — could keep resetting it past the moment a question went up. What is
/// being read is a state, and a state is read on a clock. Short, because unlike
/// a finished turn there is no argument for letting a question sit unannounced.
const PERIOD: u32 = 500;

/// How long after a document starts a card still counts as one that was already
/// there, in milliseconds.
///
/// The baseline cannot be a single first look. dsh's page arrives as a shell:
/// the composer is mounted by the first render and a card replaying a pending
/// request by the resync after that, both of them well after this script runs.
/// A single look would therefore always record "nothing pending" and then
/// announce the replay as a fresh question — on every navigation, and every
/// reload that lands while dsh happens to be waiting, which is exactly the noise
/// the baseline exists to prevent.
///
/// So the baseline is a window rather than a moment: for this long, whatever is
/// on screen is recorded and nothing is announced. The cost is that a genuinely
/// new question raised within it is silent, and that is the right way round —
/// a question that arrives seconds after a load is one the user is sitting in
/// front of, having just navigated or reloaded to get there.
const GRACE: u32 = 4_000;

/// The waits dsh can raise: the attribute that marks each, and what to say.
///
/// A click on a toast leads nowhere — see the module docs in [`crate::notify`]
/// — so each body stands on its own rather than promising that clicking will
/// take the user anywhere.
fn waits() -> [(&'static str, &'static str, &'static str); 3] {
    [
        (
            "data-approval-key",
            t!("dsh 需要你的授权", "dsh needs your approval"),
            t!(
                "有一步操作在等你允许或拒绝，这一轮暂停在这里。",
                "A step is waiting for you to allow or refuse it; the turn is paused until you do."
            ),
        ),
        (
            "data-plan-review-key",
            t!("dsh 等你审阅方案", "dsh is waiting on your review"),
            t!(
                "方案已经写好，等你批准或者说说要改哪里。",
                "The plan is written and waiting for you to approve it or say what to change."
            ),
        ),
        (
            "data-question-key",
            t!("dsh 有问题要问你", "dsh has a question for you"),
            t!(
                "这一轮停在一个问题上，等你回答。",
                "This turn has stopped on a question and is waiting for your answer."
            ),
        ),
    ]
}

/// The watcher, injected into every document the window loads.
pub fn script() -> String {
    let period = PERIOD;
    let grace = GRACE;

    let table = waits()
        .iter()
        // Every string goes in through `{:?}`, so a quote or a backslash in a
        // translation arrives escaped rather than ending the literal early.
        .map(|(attribute, title, body)| {
            format!("{{ attribute: {attribute:?}, title: {title:?}, body: {body:?} }}")
        })
        .collect::<Vec<_>>()
        .join(",\n    ");

    format!(
        r#"(function () {{
  // The top document only. There is one dsh UI to watch, and a copy of this
  // running in every iframe the page opens is a timer apiece announcing
  // through a navigation no iframe can make. See `controls`.
  if (window.top !== window.self) return;
  if (window.__dshWaitWatch) return;
  window.__dshWaitWatch = true;

  // The three ways dsh asks for something. Only one can be up at a time -- a
  // pending request owns the composer -- so the order settles nothing in
  // practice; see waiting.rs.
  var WAITS = [
    {table}
  ];

  // The attribute and the pending request's key, or '' for nothing pending.
  // Recorded rather than compared until the baseline window is over; see GRACE
  // in waiting.rs.
  var seen = '';
  // When this document started, which is when this script ran: the baseline
  // window is measured from here rather than from the first sample.
  var opened = Date.now();

  /** The wait on screen, and the mark identifying the request behind it. */
  function current() {{
    for (var i = 0; i < WAITS.length; i++) {{
      var node = document.querySelector('[' + WAITS[i].attribute + ']');
      if (node) {{
        return {{
          wait: WAITS[i],
          // The key identifies the request. A card that carries an empty one is
          // still a wait: it is the mark that has to differ, and the attribute
          // name alone already does.
          mark: WAITS[i].attribute + '\n' + (node.getAttribute(WAITS[i].attribute) || '')
        }};
      }}
    }}
    return null;
  }}

  function announce(wait) {{
    try {{
      // Straight down the standard API, so this lands where a plugin's own
      // notification would -- including notify.rs's check for whether the user
      // is already looking at the window.
      new Notification(wait.title, {{ body: wait.body, tag: 'dsh-waiting' }});
    }} catch (error) {{
      // A toast is a courtesy; it is not worth throwing out of a timer dsh's
      // own rendering shares.
    }}
  }}

  /** Read the composer and act only on a change. */
  function sample() {{
    var now = current();
    var mark = now ? now.mark : '';

    // The state as this document arrived. Recorded, not announced, and for a
    // window rather than for one look -- dsh mounts its UI, and then replays
    // whatever it was waiting on, after this script has run. See GRACE in
    // waiting.rs.
    if (Date.now() - opened < {grace}) {{
      seen = mark;
      return;
    }}
    if (mark === seen) return;

    seen = mark;
    // A wait going away is the user having answered it, which needs no toast.
    if (now) announce(now.wait);
  }}

  // On a clock rather than on dsh's mutations, and with no wait for a document:
  // `current()` only ever queries, and a query against a document that has not
  // built its body yet finds nothing -- which is the same answer as a composer
  // with nothing pending in it.
  setInterval(sample, {period});
}})();"#
    )
}

#[cfg(test)]
mod tests {
    use super::{script, waits, GRACE, PERIOD};

    /// The script is pasted into a webview whole, so what is worth asserting is
    /// that the parts that vary actually reached it.
    #[test]
    fn carries_its_constants_and_strings() {
        let script = script();

        assert!(script.contains(&PERIOD.to_string()));
        assert!(script.contains(&GRACE.to_string()));
        for (attribute, title, body) in waits() {
            assert!(
                script.contains(attribute),
                "{attribute} is not in the script"
            );
            assert!(script.contains(title), "{title:?} is not in the script");
            assert!(script.contains(body), "{body:?} is not in the script");
        }
    }

    /// Each attribute reaches the script exactly once, from the table.
    ///
    /// There used to be a second copy in the observer's `attributeFilter`, and
    /// an attribute that reached only one of the two was a wait either never
    /// noticed or noticed only when something else happened to redraw. Reading
    /// on a clock removed that whole class of mistake along with the observer —
    /// this pins that it stays removed rather than growing a second list again.
    #[test]
    fn looks_every_wait_up_from_one_table() {
        let script = script();

        for (attribute, _, _) in waits() {
            let quoted = format!("{attribute:?}");
            assert_eq!(
                script.matches(&quoted).count(),
                1,
                "{attribute} must reach the script once, from the table"
            );
        }
    }

    /// It reads on a clock of its own, not on dsh's mutations.
    ///
    /// The mutation-driven debounce could be held off past the moment a card
    /// went up by anything else still redrawing the page; see PERIOD.
    #[test]
    fn samples_on_a_clock_of_its_own() {
        let script = script();

        assert!(script.contains(&format!("setInterval(sample, {PERIOD})")));
        assert!(
            !script.contains("MutationObserver"),
            "a mutation-driven wait can be held off by an unrelated redraw"
        );
    }

    /// Both strings go in through `{:?}`, so a quote in a translation arrives
    /// escaped rather than ending the literal early.
    #[test]
    fn quotes_the_strings_it_pastes() {
        let script = script();
        let (attribute, title, body) = waits()[0];

        assert!(script.contains(&format!(
            "{{ attribute: {attribute:?}, title: {title:?}, body: {body:?} }}"
        )));
    }

    /// The comparison is against the request's key, not against whether a card
    /// happens to be on screen.
    ///
    /// dsh remounts the same card with the same key on reconnect and on resync.
    /// A presence flag would announce every one of those as a new question,
    /// which is precisely the noise this watcher must not make.
    #[test]
    fn compares_the_pending_request_not_its_presence() {
        let script = script();

        assert!(
            script.contains("node.getAttribute(WAITS[i].attribute)"),
            "the mark must carry the request's own key"
        );
        assert!(
            script.contains("if (mark === seen) return;"),
            "a sample matching the last one must do nothing"
        );
    }

    /// The opening seconds record rather than announce, so a reload that lands
    /// while dsh is waiting is not news.
    ///
    /// A window rather than a single look, because dsh's page is a shell when
    /// this script runs: the card replaying a pending request is mounted by a
    /// render and a resync that both come later, so one look would record
    /// "nothing pending" and then announce the replay as a new question.
    ///
    /// The second assertion is written against whitespace-stripped lines rather
    /// than against a literal `"seen = mark;\n      return;"`. That form pins
    /// the indentation and the newline of a source file whose line endings are
    /// not fixed — `.gitattributes` pins `*.sh`, not `*.rs` — so it passes on
    /// the Linux and macOS runners and fails on a Windows checkout with
    /// `core.autocrlf=true`. The three platforms have to agree about whether
    /// the suite passes.
    #[test]
    fn the_first_look_is_a_baseline() {
        let script = script();

        assert!(script.contains(&format!("if (Date.now() - opened < {GRACE}) {{")));

        let lines: Vec<&str> = script.lines().map(str::trim).collect();
        let baseline = lines
            .windows(2)
            .any(|pair| pair[0] == "seen = mark;" && pair[1] == "return;");
        assert!(
            baseline,
            "the baseline must return before it can announce"
        );
    }

    /// A wait going away is the user having answered it. Only its arrival is an
    /// event.
    #[test]
    fn says_nothing_when_a_wait_goes_away() {
        assert!(script().contains("if (now) announce(now.wait);"));
    }
}
