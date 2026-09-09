/**
 * dsh desktop signal plugin, browser half.
 *
 * The desktop shell needs to know two things it cannot see: that a turn has
 * finished, and that dsh has stopped to ask the user something. It used to work
 * both out by polling dsh's DOM — sniffing whether the send button's svg child
 * was a `rect` or a `path`, and watching three `data-*-key` attributes. Both
 * were inferring a state dsh had already computed and published.
 *
 * This reads the published one instead. `ctx.sessions.list` carries `running`
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
      var query = Object.keys(params)
        .map((key) => encodeURIComponent(key) + '=' + encodeURIComponent(params[key]))
        .join('&');
      window.location.href = SCHEME + '://signal?' + query;
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
        return () => {
          stopTurns();
          stopWaits();
        };
      }, 'dsh-desktop-signal: session state to the desktop shell');
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
            send({ event: 'wait', session: id, kind: pending.kind, key: pending.key });
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
