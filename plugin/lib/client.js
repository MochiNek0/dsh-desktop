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
 * pending is the object that can answer it.
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
 * shell calls two things on it. `open(id)` is how a click on a notification
 * gets back to the session the notification was about — `ctx.sessions.open` is
 * dsh's own way to switch the current session. `answer(id, key, choice)` is
 * how a press on one of that notification's buttons reaches the request it
 * answers, which is the same carrier the snapshot above reported: the object
 * that says a question is pending is the object that answers it.
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
 */
window.__ModuleLoader__.load({
  id: 'dsh-desktop-signal',
  factory: (require) => {
    var module = { exports: {} };
    var exports = module.exports;

    /** Cordis services this plugin waits for before `apply` runs. */
    exports.inject = ['sessions', 'uiSession'];
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
     * Install the two watchers, unless this is not the desktop shell.
     *
     * @param {import('@deepseek-ai/cordis').Context} ctx - client context.
     */
    function apply(ctx) {
      if (!window.__DSH_VERSION__) return;

      ctx.effect(() => {
        var stopTurns = watchTurns(ctx);
        var stopWaits = watchWaits(ctx);
        window.__dshSignals = {
          /**
           * Select a session, for a click that arrived on a notification about
           * it. Called from the shell over `window.eval`; see its signal.rs.
           *
           * @param {string} id - session id, as it was reported from here.
           */
          open: (id) => {
            ctx.sessions.open(id);
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
              var pending = ctx.uiSession.pendingInteractions.getSnapshot().get(id);
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
        };
        return () => {
          delete window.__dshSignals;
          stopTurns();
          stopWaits();
        };
      }, 'dsh-desktop-signal: session state to the desktop shell');
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
     */
    function watchTurns(ctx) {
      var running = Object.create(null);

      var read = (announce) => {
        var list = ctx.sessions.list.getSnapshot();
        var seen = Object.create(null);

        for (var id of list.ids) {
          var summary = list.byId[id];
          if (!summary) continue;
          seen[id] = true;
          var was = running[id];
          running[id] = summary.running;
          if (announce && was === true && summary.running === false) {
            send({ event: 'turn-end', session: id, done: summary.completed ? '1' : '0' });
          }
        }

        // A session that left the list did not finish a turn, it stopped
        // existing. Drop it so a reused id does not inherit the old state.
        for (var known of Object.keys(running)) {
          if (!seen[known]) delete running[known];
        }
      };

      read(false);
      return ctx.sessions.list.subscribe(() => read(true));
    }

    /**
     * What each session is waiting on, as the pending-interaction map changes.
     *
     * Keyed by the request's own `key` rather than by presence: a replacement
     * request must use a new key, so a key change is a new question even when
     * the session never stopped waiting.
     */
    function watchWaits(ctx) {
      var source = ctx.uiSession.pendingInteractions;
      var keys = Object.create(null);

      var read = (announce) => {
        var snapshot = source.getSnapshot();
        var seen = Object.create(null);

        for (var [id, pending] of snapshot) {
          seen[id] = true;
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
      };

      read(false);
      return source.subscribe(() => read(true));
    }

    return module.exports;
  },
});
