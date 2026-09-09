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

use tauri::Url;

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

/// Write it down.
///
/// The whole of what this module does today: the plugin's view of dsh, on
/// stderr, so it can be held against the sidebar while the rest is built.
pub fn note(signal: Signal) {
    match signal {
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
    use super::{received, Signal, LIMIT};
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
