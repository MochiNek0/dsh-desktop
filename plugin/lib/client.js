/**
 * dsh desktop signal plugin, browser half.
 *
 * The desktop shell needs to know two things it cannot see: that a turn has
 * finished, and that dsh has stopped to ask the user something. It used to work
 * both out by polling dsh's DOM — sniffing whether the send button's svg child
 * was a `rect` or a `path`, and watching three `data-*-key` attributes. Both
 * were inferring a state dsh had already computed and published, and both are
 * now gone: this plugin is the only thing that tells the shell either.
 *
 * This reads the published state instead. `ctx.sessions.list` carries `running`
 * and `completed` per session, and `ctx.uiSession.pendingInteractions` is the
 * live map of what each session is waiting on — keyed by session id, with the
 * carrier itself as the value, so the same object that says a question is
 * pending is the object that can answer it. (dsh 0.2 moved that map into
 * `uiSession.sessionStatus`; see `pendingOf`.)
 *
 * ## Not a module bundle
 *
 * This file is hand-written in the shape dsh's client module system expects: a
 * `window.__ModuleLoader__.load({ id, factory })` call whose factory returns
 * the plugin's exports. The system is a lazy CJS table, not an ESM graph — a
 * bundle is one module node, and `require` resolves only against the frozen
 * platform baseline plus whatever `dsh.client.external` declares. This plugin
 * is one module and requires nothing, so there is nothing for a bundler to do
 * and no build step to keep in step with the Rust half.
 *
 * ## The door the other way
 *
 * One global, `window.__dshSignals`, set while the plugin is wired up. The
 * shell calls three things on it. `open(id)` is how a click on a notification
 * gets back to the session the notification was about, through dsh's own way
 * to switch the current session — `ctx.uiWorkspace.openSession` on dsh 0.2,
 * `ctx.sessions.open` before it. The tray's rows for running and waiting
 * sessions go the same way. `answer(id, key, choice)` is
 * how a press on one of that notification's buttons reaches the request it
 * answers, which is the same carrier the snapshot above reported: the object
 * that says a question is pending is the object that answers it.
 * `openFolder(path)` is how a folder handed to the app from outside becomes a
 * workspace on screen, through dsh's own `workspaces` and `uiWorkspace`.
 *
 * Removed again on teardown, so a click that arrives after the plugin was
 * disabled finds nothing rather than half of something — the shell guards
 * every call on its presence.
 *
 * ## An answer that arrives too late
 *
 * A toast outlives nothing: the request it was raised about can be answered in
 * the window, replaced by a resync, or aborted, all while the popup is still on
 * screen. So `answer` submits nothing it cannot match — the carrier now in the
 * snapshot has to be the one the notification named, key and all — and when it
 * cannot, it says so with a `stale` signal rather than silently doing nothing.
 * The shell turns that into the fallback every toast has anyway: the window,
 * on the session in question, where the user can see what is actually pending.
 * Doing nothing would be the one outcome worse than either, because the user
 * would believe they had answered.
 *
 * ## Outside the desktop shell
 *
 * Nothing. The channel below is a navigation to a scheme only dsh desktop
 * answers; in an ordinary browser it is a real navigation, and the browser may
 * offer to open it with something. So the plugin looks for the shell first —
 * `window.__DSH_VERSION__`, which the shell injects into every document — and
 * wires up nothing at all when it is absent.
 *
 * ## The entry waits for nothing
 *
 * `exports.inject` is empty, and the two services above are waited for one
 * scope down instead. That is not a style choice: dsh's web boot audits every
 * loader entry once the loader has settled, and an entry still PENDING on a
 * service is a *boot failure* — `assertEntriesActive` throws, and the whole
 * window is the "Failed to load plugins" card with
 *
 *   web boot: 1 entry did not activate
 *   dsh-desktop-signal: pending (waiting for service: uiSession)
 *
 * on it. dsh itself is fine; nothing of the user's loads because of us.
 *
 * `uiSession` is exactly the service that cannot be counted on. It arrived in
 * `@deepseek-ai/dsh-client-ui-session` at 0.1.2-alpha.2, which is also where
 * `sessions` arrived, and whether a given machine's web composition carries it
 * is not something a plugin installed beside dsh gets to know. An entry that
 * requires it turns "no toast when dsh asks a question" into "dsh does not
 * start" — the same lesson as `__DSH_TRANSPORT__`: what varies across dsh
 * versions is not something to hard-depend on from out here.
 *
 * So the entry activates immediately, and each half waits for its own service
 * in a child fiber. A child fiber is not a loader entry, so the audit never
 * looks at it and `loader.await()` — which waits on entry tasks, and a PENDING
 * fiber has none — does not hang on it either. A dsh without `uiSession` gets
 * turn-end notifications and no wait notifications, which is the degradation
 * the feature is worth.
 */
window.__ModuleLoader__.load({
  id: 'dsh-desktop-signal',
  factory: (require) => {
    var module = { exports: {} };
    var exports = module.exports;

    /**
     * Nothing. `sessions` and `uiSession` are waited for a scope down; an
     * entry that waits here is an entry that can fail dsh's boot. See above.
     */
    exports.inject = [];
    exports.apply = apply;

    /** The desktop shell's channel; see its `controls.rs`. */
    var SCHEME = 'dsh-window';

    /**
     * Bumped into every URL. Assigning `location.href` only navigates when the
     * value changes, and the shell cancels the navigation, so two identical
     * events would build the same string twice and the second would be a no-op.
     */
    var nonce = 0;

    /**
     * Events waiting to go out, and whether a drain is already scheduled.
     *
     * One event per assignment: `location.href` starts the navigation
     * synchronously, so assigning twice in a tick loses the first. Several
     * sessions can transition in one tick — a reconnect replays every pending
     * request at once — so they queue and leave one at a time.
     */
    var queue = [];
    var draining = false;

    function send(params) {
      queue.push(params);
      if (draining) return;
      draining = true;
      setTimeout(drain, 0);
    }

    function drain() {
      var params = queue.shift();
      if (!params) {
        draining = false;
        return;
      }
      params.n = ++nonce + '.' + Date.now();
      // An array value repeats its key, which is how the option labels of a
      // one-press answer travel: the shell reads the query as pairs, so a
      // repeated `option` arrives as a list and a label needs no separator
      // that a label could contain.
      var pairs = [];
      for (var key of Object.keys(params)) {
        var value = params[key];
        for (var one of Array.isArray(value) ? value : [value]) {
          pairs.push(encodeURIComponent(key) + '=' + encodeURIComponent(one));
        }
      }
      window.location.href = SCHEME + '://signal?' + pairs.join('&');
      setTimeout(drain, 0);
    }

    /**
     * Put up the shell's door, and each watcher behind the service it needs.
     *
     * @param {import('@deepseek-ai/cordis').Context} ctx - client context.
     */
    function apply(ctx) {
      if (!window.__DSH_VERSION__) return;

      // What each half hands the door, while that half is wired up. Read
      // through these rather than off `ctx`: a service this entry does not
      // inject throws on access, and the whole point is that neither of them
      // is required here.
      var sessions = null;
      var waits = null;
      var folders = null;
      var composer = null;

      // Which sessions are mid-turn and which are waiting on the user, for the
      // shell's icons, its tray menu and whether it keeps the machine awake.
      // Each half fills in its own set and empties it when its scope goes;
      // only a change is sent.
      var ids = { running: new Set(), waiting: new Set() };
      var reported = null;
      var report = (name, set) => {
        ids[name] = set;
        var line = status(sessions, ids.running, ids.waiting);
        var text = JSON.stringify(line);
        if (text === reported) return;
        reported = text;
        send(line);
      };

      ctx.effect(() => {
        window.__dshSignals = {
          /**
           * Select a session, for a click that arrived on a notification about
           * it. Called from the shell over `window.eval`; see its signal.rs.
           *
           * @param {string} id - session id, as it was reported from here.
           */
          open: (id) => {
            // dsh 0.2 moved switching sessions off `sessions` — "navigation
            // belongs to view owners" — and onto the workspace view. The old
            // call is kept for a dsh that predates the move.
            if (folders && folders.uiWorkspace.openSession) folders.uiWorkspace.openSession(id);
            else if (sessions && sessions.open) sessions.open(id);
          },
          /**
           * Send a line into a session as its next turn, for a reply typed
           * into a notification. Called from the shell over `window.eval`;
           * see its signal.rs.
           *
           * The session need not be on screen: it is held for exactly as long
           * as the prompt takes to be accepted, through the session face's own
           * `prompt` — the call the composer's send ends in — and let go. The
           * turn runs on the host whether or not anything is holding it here.
           *
           * Answers `replied` or `reply-failed`. The shell keeps the text until
           * it hears which, and on a failure puts it into that session's
           * composer rather than losing what the user typed.
           *
           * @param {string} id - session id, as it was reported from here.
           * @param {string} text - what the user typed.
           */
          reply: (id, text) => {
            var failed = () => send({ event: 'reply-failed', session: id });
            if (!sessions) return failed();
            sessions.using(id, { source: 'controllerOperation' }, (reference) =>
              reference.ready.then((binding) => binding.session.prompt([{ type: 'text', text: text }], 'queue')),
            ).then((result) => {
              if (result && result.ok) send({ event: 'replied', session: id });
              else failed();
            }, failed);
          },
          /**
           * Put text into a session's composer, after whatever is already
           * there, and switch to that session. For a notification reply that
           * could not be sent, and for the clipboard shortcut. Nothing is
           * sent: the user sees it and presses Enter themselves.
           *
           * @param {string} id - session id, or '' for the one on screen.
           * @param {string} text - what to add.
           */
          draft: (id, text) => {
            if (!composer) return;
            var target = id || current(composer.sessions);
            if (!target) return;
            if (id) window.__dshSignals.open(id);
            fill(composer, target, text);
          },
          /**
           * Answer what a session is waiting on, for a press on one of a
           * notification's buttons. Called from the shell over `window.eval`;
           * see its signal.rs, which owns the choice ids.
           *
           * @param {string} id - session id, as it was reported from here.
           * @param {string} key - request key, as it was reported from here.
           * @param {string} choice - which button was pressed.
           */
          answer: (id, key, choice) => {
            var stale = () => send({ event: 'stale', session: id });
            try {
              var pending = waits && waits.getSnapshot().get(id);
              var sent = pending && pending.key === key ? submit(pending, choice) : null;
              // Matched and sent, and it can still fail: a request can abort
              // between the snapshot above and the call below.
              if (sent) return sent.catch(stale);
            } catch (error) {
              // Whatever that was, nothing was submitted, and the one thing
              // this must not do is fall silent about it.
            }
            stale();
          },
          /**
           * Open a folder as a workspace and switch to it, for a folder handed
           * to the shell from outside — `dsh-desktop <path>`. Called from the
           * shell over `window.eval`, and only after `folder-ready`; see its
           * folder.rs.
           *
           * @param {string} path - absolute directory path.
           */
          openFolder: (path) => {
            var failed = (error) => send({
              event: 'folder-failed',
              reason: (error && error.message) || String(error),
            });
            if (!folders) return failed('not ready');
            var current = folders;
            // `create` adopts an existing workspace for the same directory
            // rather than adding a second one, merges the row into the list
            // before it resolves with it, and throws when dsh refuses — so
            // `openWorkspace` can find it at once. `openWorkspace` is the
            // sidebar's own: it reuses a blank session in that workspace or
            // starts one, and says itself when it fails.
            current.workspaces.create({ path: path }).then((workspace) => {
              send({ event: 'folder-opened' });
              return current.uiWorkspace.openWorkspace(workspace.workspaceId);
            }, failed).catch(() => {});
          },
        };
        return () => {
          delete window.__dshSignals;
        };
      }, 'dsh-desktop-signal: the shell\'s way back into dsh');

      // Turn endings, and the session a notification click goes back to.
      ctx.inject(['sessions'], (scope) => {
        scope.effect(() => {
          sessions = scope.sessions;
          var stop = watchTurns(scope, (set) => report('running', set));
          return () => {
            sessions = null;
            stop();
            report('running', new Set());
          };
        }, 'dsh-desktop-signal: turn endings to the desktop shell');
      });

      // What each session is waiting on, and what a press on a toast answers.
      // Absent on a dsh whose web composition has no `uiSession`, and then
      // this scope simply never runs; see the note at the top of the file.
      ctx.inject(['uiSession'], (scope) => {
        scope.effect(() => {
          waits = pendingOf(scope.uiSession);
          var stop = watchWaits(waits, (set) => report('waiting', set));
          return () => {
            waits = null;
            stop();
            report('waiting', new Set());
          };
        }, 'dsh-desktop-signal: pending questions to the desktop shell');
      });

      // A folder opened from outside. `ready` is what tells the shell it can
      // hand one over: a folder that arrives with the launch has to wait for
      // this, and so does one that arrives while dsh restarts.
      ctx.inject(['workspaces', 'uiWorkspace'], (scope) => {
        scope.effect(() => {
          folders = { workspaces: scope.workspaces, uiWorkspace: scope.uiWorkspace };
          send({ event: 'folder-ready' });
          return () => {
            folders = null;
          };
        }, 'dsh-desktop-signal: folders opened from the desktop shell');
      });

      // A session's composer, for `draft`. `conversation` is the service that
      // owns every session's input; a dsh without it has no draft to add to.
      ctx.inject(['sessions', 'conversation'], (scope) => {
        scope.effect(() => {
          composer = { sessions: scope.sessions, conversation: scope.conversation };
          return () => {
            composer = null;
          };
        }, 'dsh-desktop-signal: text into a composer from the desktop shell');
      });

      // The workspaces the shell lists in its tray menu. `sessions` is what
      // says which were used last; see `watchRecent`.
      ctx.inject(['workspaces', 'sessions'], (scope) => {
        scope.effect(() => watchRecent(scope), 'dsh-desktop-signal: recent workspaces to the desktop shell');
      });
    }

    /**
     * The session on screen: the one the main view holds. dsh keeps no
     * "current session" anywhere a plugin can read it, but it does count who
     * holds each one, and the main view holds exactly the one it shows.
     */
    function current(sessions) {
      var list = sessions.list.getSnapshot();
      for (var id of list.ids) {
        var summary = list.byId[id];
        if (summary && summary.retainedBy && summary.retainedBy.mainView > 0) return id;
      }
      return null;
    }

    /** How long `fill` waits for a session it just switched to. */
    var FILL_WAIT = 3000;

    /**
     * Append to a session's draft and focus its composer, once there is one.
     *
     * A session that was just switched to has no scope yet — the main view
     * retains it a moment later — and a composer only exists for a session
     * with a scope, so this waits for one, a frame at a time, for a while.
     * The text goes after the draft on a new line rather than over it: a
     * half-written message is the user's, and replacing it would lose it.
     */
    function fill(composer, id, text, waited) {
      var scope = composer.sessions.scope(id);
      if (!scope) {
        if ((waited || 0) >= FILL_WAIT) return;
        return setTimeout(() => fill(composer, id, text, (waited || 0) + 50), 50);
      }
      var input = composer.conversation.input.for(scope);
      var draft = input.state.getSnapshot().draft;
      input.setDraft(draft ? draft.replace(/\s+$/, '') + '\n' + text : text);
      input.focus();
    }

    /** How many sessions the shell's tray menu lists. */
    var LISTED = 5;

    /**
     * The `status` event: how many sessions are mid-turn and how many are
     * waiting on the user, and the few the tray lists by name.
     *
     * A session waiting on the user is counted as waiting and not as running,
     * though dsh still reports its turn as running: the turn is stopped on the
     * user, and what the shell does with "running" — a blue dot, and keeping
     * the machine awake — is for a turn that is getting somewhere on its own.
     *
     * Waiting ones are listed first, being the ones worth a click. A subagent
     * that is merely running is not listed: one turn can spawn dozens, and the
     * row worth having is the turn that spawned them. One that stops to ask is,
     * because that is where the question is. Titles are dsh's `displayTitle`,
     * read when the line is built, so a title dsh fills in later goes out with
     * the next list change rather than never.
     *
     * The rows travel as two pairs of repeated keys in step, the way the
     * recent workspaces do.
     */
    function status(sessions, running, waiting) {
      var list = sessions ? sessions.list.getSnapshot() : { byId: {} };
      var title = (id) => {
        var summary = list.byId[id];
        return (summary && summary.displayTitle) || '';
      };
      var going = [...running].filter((id) => !waiting.has(id));
      var asking = [...waiting].slice(0, LISTED);
      var listed = going
        .filter((id) => !(list.byId[id] && list.byId[id].origin === 'subagent'))
        .slice(0, LISTED - asking.length);
      return {
        event: 'status',
        running: String(going.length),
        waiting: String(waiting.size),
        wid: asking,
        wtitle: asking.map(title),
        rid: listed,
        rtitle: listed.map(title),
      };
    }

    /** How many workspaces the shell is told about. */
    var RECENT = 5;

    /**
     * The most recently used workspaces, as their list or their sessions
     * change.
     *
     * A workspace has no last-used time of its own — its `updatedAt` moves on
     * a rename or a reorder, not on use — so it is ranked by the newest
     * `updatedAt` among its sessions, which dsh moves on every message. The
     * workspace's own time stands in when none of its sessions is in the list.
     * Sent only when the titles, paths or their order change: the session list
     * moves on every message of a running turn.
     */
    function watchRecent(ctx) {
      var last = null;

      var read = () => {
        var byId = ctx.sessions.list.getSnapshot().byId;
        var recent = ctx.workspaces.list.getSnapshot().items
          .map((workspace) => {
            var at = Date.parse(workspace.updatedAt) || 0;
            for (var id of workspace.sessionIds) {
              var summary = byId[id];
              if (summary && summary.updatedAt > at) at = summary.updatedAt;
            }
            return { workspace: workspace, at: at };
          })
          .sort((a, b) => b.at - a.at)
          .slice(0, RECENT)
          .map((row) => row.workspace);

        var line = JSON.stringify(recent.map((workspace) => [workspace.title, workspace.path]));
        if (line === last) return;
        last = line;
        // Two repeated keys in step, the way a question's options travel.
        send({
          event: 'recent',
          title: recent.map((workspace) => workspace.title),
          path: recent.map((workspace) => workspace.path),
        });
      };

      read();
      var stopWorkspaces = ctx.workspaces.list.subscribe(read);
      var stopSessions = ctx.sessions.list.subscribe(read);
      return () => {
        stopWorkspaces();
        stopSessions();
      };
    }

    /**
     * Hand one choice to the carrier that can act on it.
     *
     * Every branch is a submission the carrier's own contract allows, and
     * nothing else: `ApprovalDecision` has exactly two values and neither of
     * them is a standing permission, a plan review answers with the asker's
     * own approve label verbatim, and a question answers with one whole batch.
     * A choice this cannot place returns null rather than guessing, because
     * the failure mode of a guess is an answer the user did not give.
     *
     * @returns {Promise<void> | null} The submission, or null if it is not one.
     */
    function submit(pending, choice) {
      if (pending.kind === 'approval') {
        if (choice === 'allow') return pending.answer('allowed-once');
        if (choice === 'reject') return pending.answer('rejected');
        return null;
      }

      // A `plan-review` is a request dsh has already narrowed: one question,
      // single choice, declaring the intent whose `approve` names one of its
      // own options. So the approve label is readable straight off the
      // question, and there is no need to reach for `planReviewOf`.
      if (pending.kind === 'plan-review') {
        if (choice === 'revise') return pending.cancel();
        var review = pending.questions[0];
        if (choice !== 'approve' || !review || !review.intent) return null;
        return pending.answer(batch(review.id, review.intent.approve));
      }

      // The generic flow. The shell only offers buttons for a batch of one
      // single-select question, and numbers them by position in `options`.
      if (pending.kind === 'question') {
        var question = pending.questions[0];
        var option = question && (question.options || [])[Number(choice)];
        return option ? pending.answer(batch(question.id, option.label)) : null;
      }

      return null;
    }

    /** One whole answer batch, for a request of one question. */
    function batch(id, label) {
      return { answers: [{ id: id, selected: [label] }] };
    }

    /**
     * The labels of an answer a single press can give, or none.
     *
     * `approval` and `plan-review` need nothing from here: their buttons say
     * the same two things every time, and the shell writes them in the user's
     * own language. A generic question's do not — they are the asker's own
     * option labels — so they travel. Only a batch of one single-select
     * question with one or two options fits on a toast; anything more has
     * answers two buttons cannot express, and the shell offers "open" instead.
     */
    function oneClick(pending) {
      if (pending.kind !== 'question' || pending.questions.length !== 1) return [];
      var question = pending.questions[0];
      if (question.multiSelect === true) return [];
      var options = question.options || [];
      if (options.length < 1 || options.length > 2) return [];
      return options.map((option) => option.label);
    }

    /**
     * A turn ending, as `running` falling for one session.
     *
     * The first snapshot only records a baseline: a page that loads while a
     * session is mid-turn should not announce the turn it did not see start,
     * and neither should one that loads with a finished session on screen.
     *
     * A subagent's turn is not announced: it is a step inside the turn that
     * spawned it, and one turn can spawn dozens. Only the outermost turn ending
     * is news. `origin` is read at the falling edge rather than when the row
     * first appears, because dsh can fill it in after the first snapshot. A
     * subagent that stops to ask something still does — see `watchWaits`.
     */
    function watchTurns(ctx, count) {
      var running = Object.create(null);

      var read = (announce) => {
        var list = ctx.sessions.list.getSnapshot();
        var seen = Object.create(null);
        var busy = new Set();

        for (var id of list.ids) {
          var summary = list.byId[id];
          if (!summary) continue;
          seen[id] = true;
          if (summary.running === true) busy.add(id);
          var was = running[id];
          running[id] = summary.running;
          if (announce && was === true && summary.running === false && summary.origin !== 'subagent') {
            send({ event: 'turn-end', session: id, done: summary.completed ? '1' : '0' });
          }
        }

        // A session that left the list did not finish a turn, it stopped
        // existing. Drop it so a reused id does not inherit the old state.
        for (var known of Object.keys(running)) {
          if (!seen[known]) delete running[known];
        }

        // The baseline included: a page that loads mid-turn still shows busy.
        count(busy);
      };

      read(false);
      return ctx.sessions.list.subscribe(() => read(true));
    }

    /**
     * What each session is waiting on, as a source of a map from session id to
     * carrier.
     *
     * dsh 0.1 published exactly that as `uiSession.pendingInteractions`. dsh
     * 0.2 dropped it and folded the carrier into `uiSession.sessionStatus`, one
     * row per session with the carrier as `pendingInteraction`, so the map is
     * rebuilt from that — once per status snapshot, which is a new object only
     * when something in it changed. The carrier is the same object in both.
     */
    function pendingOf(uiSession) {
      if (uiSession.pendingInteractions) return uiSession.pendingInteractions;
      var status = uiSession.sessionStatus;
      var from = null;
      var map = new Map();
      return {
        getSnapshot: () => {
          var snapshot = status.getSnapshot();
          if (snapshot !== from) {
            from = snapshot;
            map = new Map();
            for (var [id, row] of snapshot) {
              if (row && row.pendingInteraction) map.set(id, row.pendingInteraction);
            }
          }
          return map;
        },
        subscribe: (listener) => status.subscribe(listener),
      };
    }

    /**
     * What each session is waiting on, as the pending-interaction map changes.
     *
     * Keyed by the request's own `key` rather than by presence: a replacement
     * request must use a new key, so a key change is a new question even when
     * the session never stopped waiting.
     */
    function watchWaits(source, count) {
      var keys = Object.create(null);

      var read = (announce) => {
        var snapshot = source.getSnapshot();
        var seen = Object.create(null);
        var waiting = new Set();

        for (var [id, pending] of snapshot) {
          seen[id] = true;
          waiting.add(id);
          if (keys[id] === pending.key) continue;
          keys[id] = pending.key;
          if (announce) {
            var params = {
              event: 'wait',
              session: id,
              kind: pending.kind,
              key: pending.key,
              option: oneClick(pending),
            };
            // What the wait is actually about, when the carrier says. An
            // approval's does: `reason` is the asker's own sentence and
            // `toolName` is the tool that wants the decision, and dsh's own
            // panel renders exactly `reason ?? "tool <name> requests …"`. So
            // both travel and the shell composes the same line, in the user's
            // language — see `waiting_on` in its signal.rs.
            //
            // Sent by name rather than by kind: a later dsh that puts either
            // field on some other kind of wait gets the same treatment for
            // free, and one that puts neither is the generic sentence, which
            // is what every wait said before this.
            if (pending.reason) params.reason = pending.reason;
            if (pending.toolName) params.tool = pending.toolName;
            send(params);
          }
        }

        for (var known of Object.keys(keys)) {
          if (seen[known]) continue;
          delete keys[known];
          if (announce) send({ event: 'wait-over', session: known });
        }

        count(waiting);
      };

      read(false);
      return source.subscribe(() => read(true));
    }

    return module.exports;
  },
});
