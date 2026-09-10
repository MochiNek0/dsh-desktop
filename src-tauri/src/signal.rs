//! What dsh is doing, as dsh itself reports it.
//!
//! [`crate::turn`] and [`crate::waiting`] work this out by polling dsh's DOM:
//! one sniffs whether the send button's svg child is a `rect` or a `path`, the
//! other watches three `data-*-key` attributes. Both infer a state dsh has
//! already computed and published to its own client plugins, and both fail
//! silently when a dsh release renames a class or reshapes a button.
//!
//! So the state is read where it is published. A client plugin — `plugin/` in
//! this repository — injects dsh's `sessions` and `uiSession` services,
//! subscribes to `sessions.list` (which carries `running` and `completed` per
//! session) and to `uiSession.pendingInteractions` (the live map of what each
//! session is waiting on), and reports each transition here.
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
//! and a pending request, and the answer to one will come back out of dsh's own
//! carrier rather than out of this URL — so the strictness that matters is
//! length, applied below.
//!
//! ## What it does not do yet
//!
//! Nothing but report. The notification these signals will raise, and the
//! answering that follows, are the next steps; see `docs/notifications.md`.
//! Until then this module is the intake, and the two DOM watchers are still
//! what raises a toast.

use tauri::{AppHandle, Manager, Url};

/// How much of each field survives the trip. Session ids and request keys are
/// short and opaque; this is a bound, not a size.
const LIMIT: usize = 200;

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
    },
    /// The request went away — answered, cancelled, or its session pruned.
    WaitOver { session: String },
}

/// Read a `dsh-window://signal` navigation. `None` for a query this build has
/// no reading of, which is left to be ignored rather than acted on half.
pub fn received(url: &Url) -> Option<Signal> {
    let mut event = String::new();
    let mut session = String::new();
    let mut kind = String::new();
    let mut key = String::new();
    let mut done = false;

    for (name, value) in url.query_pairs() {
        match name.as_ref() {
            "event" => event = clamp(&value),
            "session" => session = clamp(&value),
            "kind" => kind = clamp(&value),
            "key" => key = clamp(&value),
            "done" => done = value == "1",
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
        }),
        "wait-over" => Some(Signal::WaitOver { session }),
        other => {
            eprintln!("dsh-desktop: ignoring unknown session signal {other}");
            None
        }
    }
}

/// Act on one, which today means raising a notification about it.
///
/// Through [`crate::notify::show`], so it passes the same two gates every other
/// notification does -- the preference, and whether the user is already looking
/// at the window -- and so the notification carries the session it is about.
/// That is the whole of what a signal buys over the DOM watchers' toast: a click
/// on it has somewhere to go.
///
/// Still written to stderr as well. The two paths to the same toast are meant to
/// be held against each other for a release before [`crate::turn`] and
/// [`crate::waiting`] are removed, and a log line is how.
pub fn act(app: &AppHandle, signal: Signal) {
    match &signal {
        Signal::TurnEnd { session, done } => {
            eprintln!("dsh-desktop: turn ended in {session} (unopened: {done})");
        }
        Signal::Wait { session, kind, key } => {
            eprintln!("dsh-desktop: {session} is waiting on {kind} ({key})");
        }
        Signal::WaitOver { session } => {
            eprintln!("dsh-desktop: {session} is no longer waiting");
        }
    }

    if let Some((session, (title, body))) = words(&signal) {
        crate::notify::show(app, crate::notify::Notice::about(session, title, body));
    }
}

/// What to say about a signal, and which session to say it about. `None` for a
/// signal there is nothing to say about.
///
/// A wait ending is one of those: the toast for it has already been raised, and
/// withdrawing it would mean holding every notification handle open. What that
/// costs and buys belongs with the answer buttons, which is where a toast starts
/// wanting to outlive its popup; see `docs/notifications.md`.
fn words(signal: &Signal) -> Option<(&str, (&'static str, &'static str))> {
    match signal {
        Signal::TurnEnd { session, .. } => Some((session, turn_ended())),
        Signal::Wait { session, kind, .. } => Some((session, waiting_on(kind))),
        Signal::WaitOver { .. } => None,
    }
}

/// A finished turn.
///
/// Here rather than in [`crate::turn`], which pastes it into the watcher it
/// injects, so that the fallback and this cannot drift apart. The table stays
/// when that module goes.
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
/// Read from here by [`crate::waiting`] too, for the same reason as above -- it
/// spots the same three by their `data-*-key` attributes and has to say the same
/// thing about them.
///
/// A kind this build has never heard of is not dropped. [`Signal::Wait`] carries
/// dsh's discriminator through as a string precisely so that a kind a later dsh
/// adds still reads as a session waiting on the user, and the answer to "what do
/// we say about it" has to keep that promise: the vaguest sentence that is still
/// true, rather than silence.
pub fn waiting_on(kind: &str) -> (&'static str, &'static str) {
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

/// The call [`open`] makes, as a string so it can be read in a test.
///
/// The id goes in through `{:?}`, like every other string this app pastes into
/// a script. It has never yet been anything but `session-<uuid>`, which is why
/// that is worth pinning rather than trusting.
fn call(session: &str) -> String {
    format!("window.__dshSignals && window.__dshSignals.open({session:?});")
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
    use super::{call, received, waiting_on, words, Signal, LIMIT};
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
                key: "k1".into()
            })
        );
        assert_eq!(
            read("event=wait-over&session=s1"),
            Some(Signal::WaitOver {
                session: "s1".into()
            })
        );
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
                key: "k1".into()
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
                key: "question:42".into()
            })
        );
        assert_eq!(
            read("dsh-window://signal?event=wait-over&session=s-abc&n=3.1788947724008"),
            Some(Signal::WaitOver {
                session: "s-abc".into()
            })
        );
    }

    /// Every signal the plugin sends either says something or is deliberately
    /// silent, and the session it says it about is the one it named.
    #[test]
    fn says_something_about_each_signal_worth_a_toast() {
        let turn = Signal::TurnEnd {
            session: "s1".into(),
            done: true,
        };
        let (session, (title, body)) = words(&turn).expect("a finished turn is news");
        assert_eq!(session, "s1");
        assert!(!title.is_empty() && !body.is_empty());

        for kind in ["approval", "question", "plan-review"] {
            let wait = Signal::Wait {
                session: "s2".into(),
                kind: kind.into(),
                key: "k".into(),
            };
            let (session, (title, _)) = words(&wait).expect("a wait is news");
            assert_eq!(session, "s2");
            assert_eq!(title, waiting_on(kind).0);
        }

        // The toast is already up; taking it down again is not this step's.
        assert!(words(&Signal::WaitOver {
            session: "s1".into()
        })
        .is_none());
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
            .map(|kind| waiting_on(kind))
            .collect();

        assert_eq!(
            known.iter().collect::<std::collections::HashSet<_>>().len(),
            3,
            "each wait should read as itself"
        );

        let (title, body) = waiting_on("something-dsh-added-later");
        assert!(!title.is_empty() && !body.is_empty());
        assert!(
            !known.contains(&(title, body)),
            "an unknown kind should not borrow another kind's words"
        );
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
}
