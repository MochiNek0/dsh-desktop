//! What dsh is doing, as dsh itself reports it.
//!
//! Two injected scripts used to work this out by polling dsh's DOM: one
//! sniffed whether the send button's svg child was a `rect` or a `path`, the
//! other watched three `data-*-key` attributes. Both inferred a state dsh had
//! already computed and published to its own client plugins, and both failed
//! silently whenever a dsh release renamed a class or reshaped a button. They
//! are gone; `git log` has them if the reasoning is ever wanted back.
//!
//! The state is read where it is published instead. A client plugin —
//! `plugin/` in this repository — injects dsh's `sessions` and `uiSession`
//! services, subscribes to `sessions.list` (which carries `running` and
//! `completed` per session) and to `uiSession.pendingInteractions` (the live
//! map of what each session is waiting on), and reports each transition here.
//!
//! Which makes the plugin a hard dependency of every notification this app
//! raises, and that is why the switch that turns notifications on is only
//! reachable once the plugin is installed. See [`crate::plugins::signalling`]
//! and [`crate::notify::show`].
//!
//! ## Over the same channel as everything else
//!
//! A navigation to `dsh-window://signal?event=…`, the channel
//! [`crate::controls`] already opened, for the reason set out there: granting
//! IPC to `http://127.0.0.1:*` grants it to every line of JavaScript dsh and
//! its plugins load. The query is one-way and carries a session id, a request
//! kind and a request key.
//!
//! Which means, as with every other verb on that channel, that anything in the
//! window can send one. Nothing here acts on the payload — it names a session
//! and a pending request, and the answer to one goes back out through dsh's own
//! carrier rather than out of this URL — so the strictness that matters is
//! length, applied below.
//!
//! A forged signal is therefore a toast about a session that is not waiting,
//! and a press on its buttons submits nothing: [`answer`] hands the choice to
//! the plugin, which will not find a carrier under that key and says so. What
//! comes back is [`Signal::Stale`], and what that gets is the window.
//!
//! ## What a press answers
//!
//! Only what one press can say in full. [`buttons`] draws two at most, and
//! every one of them maps to a submission dsh's own carrier already allows: an
//! approval's two decisions, a plan review's approve label or its way back to
//! the composer, or one option of a single-choice question. Anything else — a
//! multi-question batch, a plan the user wants to argue with, anything needing
//! typing — gets no button, and the click on the toast body opens the app at
//! that session instead.

use tauri::{AppHandle, Manager, Url};

/// How much of each field survives the trip. Session ids and request keys are
/// short and opaque; this is a bound, not a size.
const LIMIT: usize = 200;

/// How many buttons a toast may carry.
///
/// Two, which is what a toast can be counted on to show: a Linux daemon
/// decides for itself how many of the actions it was handed to draw, and macOS
/// would fold anything past the first into an "Options" menu if it drew any at
/// all (it does not; see [`crate::toast`]). Two plus the body click is the
/// budget the table in [`buttons`] is written against.
const BUTTONS: usize = 2;

/// One transition, as the plugin reported it.
#[derive(Debug, PartialEq, Eq)]
pub enum Signal {
    /// A turn finished: `running` fell for this session. `done` mirrors dsh's
    /// `completed` — finished while not selected and not yet opened, which is
    /// the sidebar's green reminder.
    TurnEnd { session: String, done: bool },
    /// dsh stopped to ask. `kind` is dsh's own discriminator (`approval`,
    /// `question`, `plan-review`) and is carried through rather than parsed
    /// into a closed set: a kind this build does not recognise is still a
    /// session waiting on the user, and belongs in a notification that says so.
    Wait {
        session: String,
        kind: String,
        key: String,
        /// The option labels of a question one press can answer, in the order
        /// the asker offered them: the plugin sends them only for a batch of
        /// one single-select question with one or two options, and nothing at
        /// all for the two kinds whose buttons say the same thing every time.
        /// See [`buttons`].
        options: Vec<String>,
        /// What the wait is about, when the carrier said. Both are empty for a
        /// wait that carries neither, which is every wait a `question` raises
        /// and every one a dsh older than this reports. See [`waiting_on`],
        /// which turns them into the notification's body.
        tool: String,
        reason: String,
    },
    /// The request went away — answered, cancelled, or its session pruned.
    WaitOver { session: String },
    /// A press on a button reached a request that was no longer there, so
    /// nothing was submitted. Answered in the window instead; see [`act`].
    Stale { session: String },
}

/// Read a `dsh-window://signal` navigation. `None` for a query this build has
/// no reading of, which is left to be ignored rather than acted on half.
pub fn received(url: &Url) -> Option<Signal> {
    let mut event = String::new();
    let mut session = String::new();
    let mut kind = String::new();
    let mut key = String::new();
    let mut done = false;
    let mut options = Vec::new();
    let mut tool = String::new();
    let mut reason = String::new();

    for (name, value) in url.query_pairs() {
        match name.as_ref() {
            "event" => event = clamp(&value),
            "session" => session = clamp(&value),
            "kind" => kind = clamp(&value),
            "key" => key = clamp(&value),
            "done" => done = value == "1",
            // Prose rather than an identifier, and so the one field here whose
            // clamp is visible: cut at [`LIMIT`] it loses its last words and
            // says nothing about having lost them. Left that way because the
            // alternative is a second clamp with different manners for one
            // field, and because what is cut is the tail of a sentence whose
            // first 200 characters have already said which tool and why.
            "reason" => reason = clamp(&value),
            "tool" => tool = clamp(&value),
            // Repeated, one per label, and taken in order: the id a press
            // sends back is the label's position. Bounded like the rest, and
            // by count as well — a toast has room for two, and the rest would
            // only be buttons nobody drew.
            "option" if options.len() < BUTTONS => options.push(clamp(&value)),
            _ => {}
        }
    }

    // Every signal is about a session. One that does not name it cannot be
    // matched to a notification, so there is nothing to do with it.
    if session.is_empty() {
        return None;
    }

    match event.as_str() {
        "turn-end" => Some(Signal::TurnEnd { session, done }),
        // A wait with no kind and no key is neither answerable nor
        // identifiable; the plugin always sends both.
        "wait" if !kind.is_empty() && !key.is_empty() => Some(Signal::Wait {
            session,
            kind,
            key,
            options,
            tool,
            reason,
        }),
        "wait-over" => Some(Signal::WaitOver { session }),
        "stale" => Some(Signal::Stale { session }),
        other => {
            eprintln!("dsh-desktop: ignoring unknown session signal {other}");
            None
        }
    }
}

/// Act on one, which for most of them means raising a notification about it.
///
/// Through [`crate::notify::show`], so it passes the same two gates every other
/// notification does -- the preference, and whether the user is already looking
/// at the window -- and so the notification carries the session it is about,
/// and the buttons that answer what that session is waiting on. That is what a
/// signal buys over the DOM watchers' toast: a press on it has somewhere to go.
///
/// [`Signal::Stale`] is the one that is not a notification. It comes back up
/// this channel after a press this app already made, and what it asks for is
/// the window.
///
/// Still written to stderr as well: a signal that arrives and raises nothing —
/// because the window has focus, or the preference is off — is otherwise
/// indistinguishable from one that never arrived.
pub fn act(app: &AppHandle, signal: Signal) {
    match signal {
        Signal::TurnEnd { session, done } => {
            eprintln!("dsh-desktop: turn ended in {session} (unopened: {done})");
            let (title, body) = turn_ended();
            crate::notify::show(app, crate::notify::Notice::about(&session, title, body));
        }
        Signal::Wait {
            session,
            kind,
            key,
            options,
            tool,
            reason,
        } => {
            eprintln!("dsh-desktop: {session} is waiting on {kind} ({key})");
            let (title, body) = waiting_on(&kind, &tool, &reason);
            crate::notify::show(
                app,
                crate::notify::Notice::asking(&session, &key, title, &body, buttons(&kind, options)),
            );
        }
        // Nothing to say. The toast has already been raised, and withdrawing
        // it would mean holding every notification handle open for as long as
        // the request lives; see `docs/notifications.md`.
        Signal::WaitOver { session } => {
            eprintln!("dsh-desktop: {session} is no longer waiting");
        }
        // The press missed. Whatever that session is doing now, the user meant
        // to answer it, so put them in front of it rather than leaving them
        // believing a button they pressed did something.
        Signal::Stale { session } => {
            eprintln!("dsh-desktop: an answer for {session} arrived too late");
            crate::reveal(app);
            open(app, &session);
        }
    }
}

/// What a press can say about one of dsh's waits, in the order the buttons are
/// drawn. Empty for a wait no press can answer, which leaves the toast with
/// only its body to click.
///
/// The ids are this app's own, and the plugin turns each back into a call on
/// dsh's carrier — see `plugin/lib/client.js`, which is the other half of this
/// table and the only place these strings mean anything.
///
/// Two things this deliberately cannot offer. An approval's `允许` is
/// `allowed-once` and nothing else: `ApprovalDecision` has no standing
/// permission in it, so the blast radius of a mis-press is one tool call. And
/// a plan review's second button is `要改` rather than dsh's own `Refuse`:
/// refusing without saying why is a dead end on a toast, and saying why means
/// typing, so the button that leads back to the composer is the useful one.
fn buttons(kind: &str, options: Vec<String>) -> Vec<crate::notify::Button> {
    use crate::notify::Button;

    match kind {
        "approval" => vec![
            Button::new("allow", t!("允许", "Allow")),
            Button::new("reject", t!("拒绝", "Refuse")),
        ],
        "plan-review" => vec![
            Button::new("approve", t!("批准", "Approve")),
            Button::new("revise", t!("要改", "Revise")),
        ],
        // A question's buttons are the asker's own option labels, and the id
        // is where the label sat in the list. The plugin sends them only when
        // one press can answer the whole request.
        _ => options
            .into_iter()
            .enumerate()
            .map(|(at, label)| Button::new(&at.to_string(), &label))
            .collect(),
    }
}

/// A finished turn.
pub fn turn_ended() -> (&'static str, &'static str) {
    (
        t!("对话已完成", "Turn finished"),
        t!(
            "dsh 已经处理完这一轮，可以回来看看了。",
            "dsh has finished this turn and is waiting for you."
        ),
    )
}

/// One of dsh's waits, by dsh's own name for it.
///
/// A kind this build has never heard of is not dropped. [`Signal::Wait`] carries
/// dsh's discriminator through as a string precisely so that a kind a later dsh
/// adds still reads as a session waiting on the user, and the answer to "what do
/// we say about it" has to keep that promise: the vaguest sentence that is still
/// true, rather than silence.
///
/// `tool` and `reason` are what the carrier said this particular wait is about,
/// and either may be empty. Where they are not, the body says the same thing
/// dsh's own panel says about the same request: its `ApprovalPanel` renders
/// `reason ?? "tool <name> requests privileged execution"`, and the two lines
/// below are that fallback in this app's two languages. Written here rather
/// than sent ready-made from the plugin for the reason the buttons are — the
/// user's language is this side's to know.
///
/// The generic sentence stays underneath both, and is still what a wait with
/// nothing to say about itself gets. That is not only the old dsh case: a
/// `question` carries neither field, because what it is about is the question
/// text, and a question is answered by reading it rather than by being told a
/// tool name.
pub fn waiting_on(kind: &str, tool: &str, reason: &str) -> (&'static str, String) {
    let (title, generic) = generic_wait(kind);

    // The asker's own sentence wins over anything this could compose, and
    // naming the tool beats saying "a step". Neither is available for most
    // kinds, so most waits still get the sentence below.
    let body = if !reason.is_empty() {
        reason.to_string()
    } else if !tool.is_empty() && kind == "approval" {
        t!(
            "工具 {} 请求越权执行。",
            "Tool {} requests privileged execution.",
            tool
        )
    } else {
        generic.to_string()
    };

    (title, body)
}

/// The title, and the body for a wait that says nothing about itself.
fn generic_wait(kind: &str) -> (&'static str, &'static str) {
    match kind {
        "approval" => (
            t!("dsh 需要你的授权", "dsh needs your approval"),
            t!(
                "有一步操作在等你允许或拒绝，这一轮暂停在这里。",
                "A step is waiting for you to allow or refuse it; the turn is paused until you do."
            ),
        ),
        "plan-review" => (
            t!("dsh 等你审阅方案", "dsh is waiting on your review"),
            t!(
                "方案已经写好，等你批准或者说说要改哪里。",
                "The plan is written and waiting for you to approve it or say what to change."
            ),
        ),
        "question" => (
            t!("dsh 有问题要问你", "dsh has a question for you"),
            t!(
                "这一轮停在一个问题上，等你回答。",
                "This turn has stopped on a question and is waiting for your answer."
            ),
        ),
        _ => (
            t!("dsh 在等你", "dsh is waiting for you"),
            t!(
                "这一轮停下来等你回应。",
                "This turn has stopped and is waiting for you."
            ),
        ),
    }
}

/// Select a session in the page, for a click that arrived on a notification
/// about it.
///
/// Down the same `window.eval` the rest of the app talks to its injected
/// scripts through, except that what answers here is the plugin rather than
/// anything this app wrote: `ctx.sessions.open(id)` is dsh's own way to switch
/// the current session, and the plugin puts a door to it on `window` while it is
/// wired up. Guarded on the far side because a click can arrive after the
/// document that raised the notification has gone -- a reload, or dsh
/// restarting under the window -- and then there is nothing there to call.
pub fn open(app: &AppHandle, session: &str) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let _ = window.eval(call(session));
}

/// Give dsh the answer a user pressed on a toast.
///
/// The same door as [`open`], and guarded the same way for the same reason: a
/// press can arrive after the document has gone. What the plugin does with it
/// is check that the request named by `key` is still the one that session is
/// waiting on — a toast is on screen for seconds, and dsh replays pending
/// requests on a reconnect — and to send back [`Signal::Stale`] when it is
/// not. So a press either submits the answer the user gave or shows them the
/// session; it never submits a different one.
pub fn answer(app: &AppHandle, session: &str, key: &str, choice: &str) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let _ = window.eval(reply(session, key, choice));
}

/// The call [`open`] makes, as a string so it can be read in a test.
///
/// The id goes in through `{:?}`, like every other string this app pastes into
/// a script. It has never yet been anything but `session-<uuid>`, which is why
/// that is worth pinning rather than trusting.
fn call(session: &str) -> String {
    format!("window.__dshSignals && window.__dshSignals.open({session:?});")
}

/// The call [`answer`] makes. Three strings, each through `{:?}`, and two of
/// them came off a URL the page itself can have written.
fn reply(session: &str, key: &str, choice: &str) -> String {
    format!("window.__dshSignals && window.__dshSignals.answer({session:?}, {key:?}, {choice:?});")
}

/// One field, trimmed on a character boundary. No ellipsis: these are
/// identifiers, and a truncated one should not look like a longer one that
/// happens to end in a dot.
fn clamp(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(LIMIT) {
        Some((cut, _)) => text[..cut].to_string(),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{buttons, call, received, reply, waiting_on, Signal, BUTTONS, LIMIT};
    use tauri::Url;

    fn read(query: &str) -> Option<Signal> {
        received(&Url::parse(&format!("dsh-window://signal?{query}")).unwrap())
    }

    #[test]
    fn reads_a_finished_turn() {
        assert_eq!(
            read("event=turn-end&session=s1&done=1"),
            Some(Signal::TurnEnd {
                session: "s1".into(),
                done: true
            })
        );
        assert_eq!(
            read("event=turn-end&session=s1&done=0"),
            Some(Signal::TurnEnd {
                session: "s1".into(),
                done: false
            })
        );
    }

    /// Absent is the same as `0`: the plugin sends the field, but a build that
    /// stops sending it should read as "not the unopened case" rather than fail.
    #[test]
    fn treats_a_missing_done_as_false() {
        assert_eq!(
            read("event=turn-end&session=s1"),
            Some(Signal::TurnEnd {
                session: "s1".into(),
                done: false
            })
        );
    }

    #[test]
    fn reads_a_wait_and_its_end() {
        assert_eq!(
            read("event=wait&session=s1&kind=approval&key=k1"),
            Some(Signal::Wait {
                session: "s1".into(),
                kind: "approval".into(),
                key: "k1".into(),
                options: vec![],
                tool: String::new(),
                reason: String::new()
            })
        );
        assert_eq!(
            read("event=wait-over&session=s1"),
            Some(Signal::WaitOver {
                session: "s1".into()
            })
        );
    }

    /// The two fields that say what a wait is about, off the query.
    #[test]
    fn reads_what_an_approval_is_about() {
        let Some(Signal::Wait { tool, reason, .. }) = read(
            "event=wait&session=s1&kind=approval&key=k1             &tool=bash&reason=It%20wants%20to%20delete%20build%2F.",
        ) else {
            panic!("a wait");
        };
        assert_eq!(tool, "bash");
        assert_eq!(reason, "It wants to delete build/.");

        // Bounded like every other field off this URL, and a wait that sends
        // neither is every wait a dsh older than this reports.
        let Some(Signal::Wait { tool, reason, .. }) =
            read(&format!("event=wait&session=s1&kind=approval&key=k1&reason={}", "r".repeat(LIMIT * 2)))
        else {
            panic!("a wait");
        };
        assert_eq!(reason.chars().count(), LIMIT);
        assert!(tool.is_empty());
    }

    /// dsh's own discriminator, whatever it is. A kind added by a later dsh is
    /// still a session waiting on the user.
    #[test]
    fn carries_an_unrecognised_kind_through() {
        assert_eq!(
            read("event=wait&session=s1&kind=something-new&key=k1"),
            Some(Signal::Wait {
                session: "s1".into(),
                kind: "something-new".into(),
                key: "k1".into(),
                options: vec![],
                tool: String::new(),
                reason: String::new()
            })
        );
    }

    #[test]
    fn declines_what_it_cannot_use() {
        assert_eq!(read("event=turn-end"), None, "no session");
        assert_eq!(read("session=s1"), None, "no event");
        assert_eq!(read("event=nonsense&session=s1"), None, "unknown event");
        assert_eq!(
            read("event=wait&session=s1&key=k1"),
            None,
            "a wait with no kind"
        );
        assert_eq!(
            read("event=wait&session=s1&kind=approval"),
            None,
            "a wait with no key"
        );
    }

    /// The nonce that makes each navigation different is not a field.
    #[test]
    fn ignores_what_it_does_not_read() {
        assert_eq!(
            read("event=wait-over&session=s1&n=7.1234567890"),
            Some(Signal::WaitOver {
                session: "s1".into()
            })
        );
    }

    /// The seam, held from the other side: three URLs as `plugin/lib/client.js`
    /// actually built them, pasted verbatim. Either half can be edited without
    /// the other noticing, and a request key is `<prefix>:<rpcId>` — a colon,
    /// which the plugin percent-encodes and this has to decode back.
    #[test]
    fn reads_what_the_plugin_sends() {
        let read = |url: &str| received(&Url::parse(url).unwrap());

        assert_eq!(
            read("dsh-window://signal?event=turn-end&session=s-abc&done=1&n=1.1788947724003"),
            Some(Signal::TurnEnd {
                session: "s-abc".into(),
                done: true
            })
        );
        assert_eq!(
            read(
                "dsh-window://signal?event=wait&session=s-abc&kind=plan-review\
                 &key=question%3A42&n=2.1788947724007"
            ),
            Some(Signal::Wait {
                session: "s-abc".into(),
                kind: "plan-review".into(),
                key: "question:42".into(),
                options: vec![],
                tool: String::new(),
                reason: String::new()
            })
        );
        assert_eq!(
            read(
                "dsh-window://signal?event=wait&session=s-abc&kind=question&key=question%3A43\
                 &option=Use%20TypeScript&option=Stay%20on%20JS&n=3.1788947724008"
            ),
            Some(Signal::Wait {
                session: "s-abc".into(),
                kind: "question".into(),
                key: "question:43".into(),
                options: vec!["Use TypeScript".into(), "Stay on JS".into()],
                tool: String::new(),
                reason: String::new()
            })
        );
        assert_eq!(
            read("dsh-window://signal?event=wait-over&session=s-abc&n=4.1788947724009"),
            Some(Signal::WaitOver {
                session: "s-abc".into()
            })
        );
        assert_eq!(
            read("dsh-window://signal?event=stale&session=s-abc&n=5.1788947724010"),
            Some(Signal::Stale {
                session: "s-abc".into()
            })
        );
    }

    /// Every wait either gets buttons a press can honour or gets none, and the
    /// ids are the ones `plugin/lib/client.js` reads.
    ///
    /// The other half of this table is in that file, and neither half can be
    /// edited without the other going quiet: an id this stops sending leaves a
    /// dead branch there, and one it starts sending falls through to the
    /// plugin's stale reply. So the ids are pinned here as strings rather than
    /// derived from anything.
    #[test]
    fn offers_a_press_only_what_the_plugin_can_submit() {
        let ids = |kind: &str, options: Vec<String>| {
            buttons(kind, options)
                .into_iter()
                .map(|button| button.id)
                .collect::<Vec<_>>()
        };

        assert_eq!(ids("approval", vec![]), ["allow", "reject"]);
        assert_eq!(ids("plan-review", vec![]), ["approve", "revise"]);

        // A question's are positions in the list the asker offered.
        assert_eq!(ids("question", vec!["Yes".into(), "No".into()]), ["0", "1"]);
        assert_eq!(ids("question", vec!["Only one".into()]), ["0"]);

        // No options means the plugin found no answer one press could give.
        assert!(ids("question", vec![]).is_empty());
        assert!(
            ids("something-dsh-added-later", vec![]).is_empty(),
            "a kind this build has never heard of gets a toast, not a guess"
        );
    }

    /// The labels are the ones the user reads, so they are not empty and a
    /// question's are the asker's own.
    #[test]
    fn says_what_each_button_does() {
        for kind in ["approval", "plan-review"] {
            let labels: Vec<_> = buttons(kind, vec![])
                .into_iter()
                .map(|button| button.label)
                .collect();
            assert_eq!(labels.len(), BUTTONS);
            assert!(labels.iter().all(|label| !label.is_empty()));
            assert_ne!(
                labels[0], labels[1],
                "two buttons that read the same are one"
            );
        }

        let asked = buttons(
            "question",
            vec!["Use TypeScript".into(), "Stay on JS".into()],
        );
        let labels: Vec<_> = asked.into_iter().map(|button| button.label).collect();
        assert_eq!(labels, ["Use TypeScript", "Stay on JS"]);
    }

    /// A wait ending says nothing: the toast is already up, and taking it down
    /// again would mean holding every notification handle open.
    #[test]
    fn stays_quiet_when_a_wait_ends() {
        // `act` needs an `AppHandle`, so this is the readable half of it: the
        // only signal with no words of its own.
        assert!(matches!(
            received(&Url::parse("dsh-window://signal?event=wait-over&session=s1").unwrap()),
            Some(Signal::WaitOver { .. })
        ));
    }

    /// The three kinds each get their own words, and a fourth kind still gets
    /// some.
    ///
    /// `Signal::Wait` carries dsh's discriminator through as a string so that a
    /// kind a later dsh adds still reads as a session waiting on the user. This
    /// is the other half of that promise: something true to say about it.
    #[test]
    fn has_words_for_a_kind_it_has_never_heard_of() {
        let known: Vec<_> = ["approval", "question", "plan-review"]
            .iter()
            .map(|kind| waiting_on(kind, "", ""))
            .collect();

        assert_eq!(
            known.iter().collect::<std::collections::HashSet<_>>().len(),
            3,
            "each wait should read as itself"
        );

        let (title, body) = waiting_on("something-dsh-added-later", "", "");
        assert!(!title.is_empty() && !body.is_empty());
        assert!(
            !known.contains(&(title, body)),
            "an unknown kind should not borrow another kind's words"
        );
    }

    /// What the wait is about, when the carrier said.
    ///
    /// Three rungs, and the order between them is the point: the asker's own
    /// sentence beats anything this composes, naming the tool beats saying "a
    /// step", and a wait that carries neither still gets the sentence every
    /// wait got before any of this existed.
    #[test]
    fn says_what_an_approval_is_about_when_it_can() {
        let generic = waiting_on("approval", "", "").1;

        let (_, reasoned) = waiting_on("approval", "bash", "It wants to delete build/.");
        assert_eq!(reasoned, "It wants to delete build/.");

        let (_, named) = waiting_on("approval", "bash", "");
        assert!(named.contains("bash"), "the tool should be named: {named}");
        assert_ne!(named, generic);

        // The tool name alone is an approval's fallback and nobody else's: a
        // question is about its own text, and borrowing this line for one
        // would describe it wrongly. A reason, if a later dsh ever sends one,
        // is the asker's own words and reads correctly anywhere.
        let (_, question) = waiting_on("question", "bash", "");
        assert_eq!(question, waiting_on("question", "", "").1);
        assert_eq!(waiting_on("question", "", "Which one?").1, "Which one?");
    }

    /// The call a notification click makes: guarded, because the document that
    /// raised the notification may be gone, and quoted, like every other string
    /// this app pastes into a script.
    #[test]
    fn guards_and_quotes_the_session_it_opens() {
        assert_eq!(
            call("session-0b3fcbc5"),
            "window.__dshSignals && window.__dshSignals.open(\"session-0b3fcbc5\");"
        );
        assert!(call("a\"b").contains("\\\""), "a quote must arrive escaped");
    }

    /// The call a button press makes: the same door as [`super::open`], with
    /// the request key alongside so the plugin can refuse a press that arrived
    /// after the request it was raised for had gone.
    #[test]
    fn hands_the_press_its_session_key_and_choice() {
        assert_eq!(
            reply("session-0b3fcbc5", "question:42", "approve"),
            concat!(
                "window.__dshSignals && window.__dshSignals.answer(",
                "\"session-0b3fcbc5\", \"question:42\", \"approve\");"
            )
        );
        assert!(
            reply("a\"b", "c\"d", "e\"f").matches("\\\"").count() == 3,
            "every one of the three arrives escaped"
        );
    }

    #[test]
    fn bounds_every_field() {
        let long = "k".repeat(LIMIT * 2);
        let Some(Signal::Wait { key, .. }) =
            read(&format!("event=wait&session=s1&kind=approval&key={long}"))
        else {
            panic!("a long key should still read as a wait");
        };
        assert_eq!(key.chars().count(), LIMIT);
    }

    /// A toast has room for [`BUTTONS`], and a query claiming more is not a
    /// reason to build buttons nothing will draw.
    #[test]
    fn takes_no_more_options_than_a_toast_can_show() {
        let Some(Signal::Wait { options, .. }) =
            read("event=wait&session=s1&kind=question&key=k1&option=a&option=b&option=c&option=d")
        else {
            panic!("a wait with too many options should still read as a wait");
        };
        assert_eq!(options, ["a", "b"]);
    }
}
