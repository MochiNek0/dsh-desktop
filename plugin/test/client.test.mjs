// Runs `lib/client.js` the way dsh's module loader does — a
// `window.__ModuleLoader__.load({ id, factory })` call whose factory is handed
// a `require` and returns the plugin's exports — against stub services, and
// checks what it navigates to.
//
// No dependencies and no runner: `node test/client.test.mjs`. The Rust half of
// this seam is checked from the other side in `src-tauri/src/signal.rs`, which
// parses these very URLs.
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import assert from 'node:assert/strict';

const source = readFileSync(new URL('../lib/client.js', import.meta.url), 'utf8');

// `status` and `recent` are kept apart from everything else the plugin sends:
// they go out on every change to the counts and the workspace list, and most
// checks here count the events they are about.
function load({ host }) {
  const sent = [];
  const statuses = [];
  const recents = [];
  const sessionLists = [];
  let registered = null;
  const window = {
    __ModuleLoader__: { load: (row) => { registered = row; } },
    location: {
      set href(value) {
        const event = new URL(value).searchParams.get('event');
        (event === 'status' ? statuses : event === 'recent' ? recents
          : event === 'sessions' ? sessionLists : sent).push(value);
      },
      get href() { return ''; },
    },
  };
  if (host) window.__DSH_VERSION__ = '0.1.13';
  const context = vm.createContext({ window, setTimeout, console });
  vm.runInContext(source, context);
  assert.ok(registered, 'the bundle should register a factory');
  assert.equal(registered.id, 'dsh-desktop-signal');
  const exports = registered.factory(() => { throw new Error('should require nothing'); });
  return { exports, sent, statuses, recents, sessionLists, window };
}

// Stub services with hand-driven snapshots.
//
// `uiSession: false` is the dsh whose web composition does not carry that
// service — the shape issue #11's follow-up reported from macOS. Cordis never
// runs a scope whose injected service is missing, and this stub's `inject` does
// the same rather than pretending the service is there.
function services({ uiSession = true, folders = null } = {}) {
  // `uiSession: 'status'` is dsh 0.2, which has no `pendingInteractions` and
  // carries the same carriers as `pendingInteraction` on `sessionStatus` rows.
  // `setPending` takes the same map either way.
  let list = { ids: [], byId: {} };
  let pending = new Map();
  const listeners = { list: [], pending: [] };
  const opened = [];
  const teardowns = [];
  const ctx = {
    // The real one keeps each cleanup to run on disposal; this keeps them so a
    // test can run them.
    effect: (fn) => { const off = fn(); if (off) teardowns.push(off); },
    inject: (names, callback) => {
      if (names.every((name) => ctx[name] !== undefined)) callback(ctx);
    },
    sessions: {
      list: {
        getSnapshot: () => list,
        subscribe: (fn) => { listeners.list.push(fn); return () => {}; },
      },
      open: (id) => { opened.push(id); },
    },
  };
  // The two services a folder needs. Off unless a test asks, which is also the
  // dsh that predates them: their scope never runs, and nothing is announced.
  let workspaces = { items: [] };
  const workspaceListeners = [];
  if (folders) {
    ctx.workspaces = {
      create: folders.create,
      list: {
        getSnapshot: () => workspaces,
        subscribe: (fn) => { workspaceListeners.push(fn); return () => {}; },
      },
    };
    ctx.uiWorkspace = { openWorkspace: folders.openWorkspace };
  }
  if (uiSession === 'status') {
    ctx.uiSession = {
      sessionStatus: {
        getSnapshot: () => new Map([...pending].map(([id, carrier]) => [id, { running: true, pendingInteraction: carrier, completionUnread: false }])),
        subscribe: (fn) => { listeners.pending.push(fn); return () => {}; },
      },
    };
  } else if (uiSession) {
    ctx.uiSession = {
      pendingInteractions: {
        getSnapshot: () => pending,
        subscribe: (fn) => { listeners.pending.push(fn); return () => {}; },
      },
    };
  }
  return {
    opened,
    stop() { teardowns.splice(0).forEach((fn) => fn()); },
    ctx,
    setList(next) { list = next; listeners.list.forEach((fn) => fn()); },
    setPending(next) { pending = next; listeners.pending.forEach((fn) => fn()); },
    setWorkspaces(items) { workspaces = { items }; workspaceListeners.forEach((fn) => fn()); },
    setArchived(ids) { workspaces = { ...workspaces, archivedSessionIds: ids }; workspaceListeners.forEach((fn) => fn()); },
  };
}

// The plugin leaves one event per macrotask, so wait on ticks rather than on a
// wall clock: a fixed sleep races the queue it is waiting for.
async function drain(ticks = 10) {
  for (let i = 0; i < ticks; i += 1) await new Promise((resolve) => setTimeout(resolve, 0));
}
const query = (url) => Object.fromEntries(new URL(url).searchParams);
// Repeated keys, which `query` would collapse: the option labels travel that way.
const queryAll = (url, key) => new URL(url).searchParams.getAll(key);

// A stand-in for one of dsh's carriers, recording what was submitted to it.
function carrier(fields) {
  const calls = [];
  return Object.assign(
    {
      calls,
      answer(value) { calls.push(['answer', value]); return Promise.resolve(); },
      cancel() { calls.push(['cancel']); return Promise.resolve(); },
    },
    fields,
  );
}

// One single-select question, the only shape a press can answer generically.
const asked = (options) => [{ id: 'q1', question: 'Which?', options }];

// --- exports shape ---
{
  const { exports } = load({ host: true });
  // Empty, and that is the point: an entry still waiting on a service when
  // dsh's web boot audits the loader is a boot dsh reports as "Failed to load
  // plugins". Both services are waited for a scope down; see `client.js`.
  // Crosses the vm realm, so compare by value rather than by prototype.
  assert.equal(JSON.stringify(exports.inject), '[]');
  assert.equal(typeof exports.apply, 'function');
  console.log('ok  requires no service of the entry itself');
}

// --- a dsh with no `uiSession` at all ---
{
  const { exports, sent, window } = load({ host: true });
  const s = services({ uiSession: false });
  // Activating is the whole test: this is the shape that took the window down.
  exports.apply(s.ctx);

  // The half that has what it needs still works.
  s.setList({ ids: ['a'], byId: { a: { running: true } } });
  s.setList({ ids: ['a'], byId: { a: { running: false, completed: true } } });
  await drain();
  assert.equal(sent.length, 1);
  assert.equal(query(sent[0]).event, 'turn-end');

  // The door is up, and opens a session.
  window.__dshSignals.open('session-7');
  assert.deepEqual(s.opened, ['session-7']);

  // No wait was ever announced, so no toast can carry a press — and one that
  // somehow arrives says so rather than throwing on a service that is not there.
  window.__dshSignals.answer('a', 'k1', 'allow');
  await drain();
  assert.equal(query(sent[1]).event, 'stale');
  console.log('ok  runs on a dsh that has no uiSession, minus the waits');
}

// --- no desktop shell: nothing is wired at all ---
{
  const { exports, sent } = load({ host: false });
  const s = services();
  exports.apply(s.ctx);
  s.setList({ ids: ['a'], byId: { a: { running: true } } });
  s.setList({ ids: ['a'], byId: { a: { running: false } } });
  s.setPending(new Map([['a', { key: 'k1', kind: 'approval' }]]));
  await drain();
  assert.deepEqual(sent, [], 'without __DSH_VERSION__ nothing should be sent');
  console.log('ok  wires up nothing outside the desktop shell');
}

// --- the first snapshot is a baseline ---
{
  const { exports, sent } = load({ host: true });
  const s = services();
  s.setList({ ids: ['a'], byId: { a: { running: true } } });
  s.setPending(new Map([['a', { key: 'k1', kind: 'question' }]]));
  exports.apply(s.ctx);
  await drain();
  assert.deepEqual(sent, [], 'a page that loads mid-turn announces nothing');
  console.log('ok  primes silently on the state it loads into');
}

// --- a turn ending ---
{
  const { exports, sent } = load({ host: true });
  const s = services();
  s.setList({ ids: ['a'], byId: { a: { running: true } } });
  exports.apply(s.ctx);
  s.setList({ ids: ['a'], byId: { a: { running: false, completed: true } } });
  await drain();
  assert.equal(sent.length, 1);
  const q = query(sent[0]);
  assert.equal(q.event, 'turn-end');
  assert.equal(q.session, 'a');
  assert.equal(q.done, '1');
  assert.match(q.n, /^\d+\.\d+$/);
  assert.ok(sent[0].startsWith('dsh-window://signal?'), sent[0]);

  // Still not running is not a new ending.
  s.setList({ ids: ['a'], byId: { a: { running: false, completed: true } } });
  await drain();
  assert.equal(sent.length, 1, 'a repeated snapshot should not re-announce');
  console.log('ok  announces a falling running edge exactly once');
}

// --- a subagent's turn is not the user's ---
{
  const { exports, sent } = load({ host: true });
  const s = services();
  s.setList({ ids: ['a', 'b'], byId: { a: { running: true }, b: { running: true, parentSessionId: 'a' } } });
  exports.apply(s.ctx);
  // `origin` can arrive late: the row was already running without it.
  s.setList({
    ids: ['a', 'b'],
    byId: { a: { running: true }, b: { running: false, completed: true, parentSessionId: 'a', origin: 'subagent' } },
  });
  await drain();
  assert.deepEqual(sent, [], "a subagent finishing is not announced");
  s.setList({
    ids: ['a', 'b'],
    byId: { a: { running: false, completed: true }, b: { running: false, parentSessionId: 'a', origin: 'subagent' } },
  });
  await drain();
  assert.equal(sent.length, 1);
  assert.equal(query(sent[0]).session, 'a');
  console.log('ok  only the outermost turn ending is announced');
}

// --- a session that vanishes did not finish ---
{
  const { exports, sent } = load({ host: true });
  const s = services();
  s.setList({ ids: ['a'], byId: { a: { running: true } } });
  exports.apply(s.ctx);
  s.setList({ ids: [], byId: {} });
  await drain();
  assert.deepEqual(sent, [], 'a pruned session did not end a turn');
  console.log('ok  a pruned session is not a finished turn');
}

// --- waits, keyed by key ---
{
  const { exports, sent } = load({ host: true });
  const s = services();
  exports.apply(s.ctx);
  s.setPending(new Map([['a', { key: 'k1', kind: 'approval' }]]));
  await drain();
  assert.deepEqual(query(sent[0]).event, 'wait');
  assert.equal(query(sent[0]).kind, 'approval');
  assert.equal(query(sent[0]).key, 'k1');

  // Same key again: nothing new.
  s.setPending(new Map([['a', { key: 'k1', kind: 'approval' }]]));
  await drain();
  assert.equal(sent.length, 1, 'the same request should not re-announce');

  // A replacement request uses a new key, so it is a new question.
  s.setPending(new Map([['a', { key: 'k2', kind: 'plan-review' }]]));
  await drain();
  assert.equal(sent.length, 2);
  assert.equal(query(sent[1]).key, 'k2');
  assert.equal(query(sent[1]).kind, 'plan-review');

  // Answered.
  s.setPending(new Map());
  await drain();
  assert.equal(sent.length, 3);
  assert.equal(query(sent[2]).event, 'wait-over');
  assert.equal(query(sent[2]).session, 'a');
  console.log('ok  tracks waits by request key, and their end');
}

// --- what the wait is about, when the carrier says ---
{
  const { exports, sent } = load({ host: true });
  const s = services();
  exports.apply(s.ctx);

  // An approval carries both. dsh's own panel renders `reason ?? "tool <name>
  // requests ..."`, so both travel and the shell composes the same line in the
  // user's language.
  s.setPending(new Map([['a', { key: 'k1', kind: 'approval', toolName: 'bash', reason: 'It wants to delete build/.' }]]));
  await drain();
  assert.equal(query(sent[0]).tool, 'bash');
  assert.equal(query(sent[0]).reason, 'It wants to delete build/.');

  // Only the tool: the shell names it rather than saying "a step".
  s.setPending(new Map([['a', { key: 'k2', kind: 'approval', toolName: 'bash' }]]));
  await drain();
  assert.equal(query(sent[1]).tool, 'bash');
  assert.equal(query(sent[1]).reason, undefined, 'an absent field should not travel as an empty one');

  // Neither, which is every question and every wait an older dsh reports. The
  // shell falls back to the sentence every wait had before any of this.
  s.setPending(new Map([['a', { key: 'k3', kind: 'question', questions: asked([{ label: 'Yes' }, { label: 'No' }]) }]]));
  await drain();
  assert.equal(query(sent[2]).tool, undefined);
  assert.equal(query(sent[2]).reason, undefined);

  // Sent by name rather than by kind, so a later dsh that puts either field on
  // some other wait gets the same treatment without a change here.
  s.setPending(new Map([['a', { key: 'k4', kind: 'something-new', reason: 'Because.' }]]));
  await drain();
  assert.equal(query(sent[3]).reason, 'Because.');
  console.log('ok  reports what a wait is about when the carrier says');
}

// --- a burst leaves one at a time, and every URL is distinct ---
{
  const { exports, sent } = load({ host: true });
  const s = services();
  s.setList({ ids: ['a', 'b', 'c'], byId: { a: { running: true }, b: { running: true }, c: { running: true } } });
  exports.apply(s.ctx);
  s.setList({ ids: ['a', 'b', 'c'], byId: { a: { running: false }, b: { running: false }, c: { running: false } } });
  await drain();
  assert.equal(sent.length, 3, 'three sessions ending in one tick should all be sent');
  assert.equal(new Set(sent).size, 3, 'identical URLs would not navigate twice');
  console.log('ok  queues a burst instead of clobbering it');
}

// --- the door back down ---
{
  const { exports, window } = load({ host: true });
  const s = services();
  exports.apply(s.ctx);

  // Both of the shell's uses of this global: its presence, which is what tells
  // the shell's DOM watchers to stand down, and the call a notification click
  // makes.
  assert.ok(window.__dshSignals, 'the shell reads this to stand its watchers down');
  window.__dshSignals.open('session-7');
  assert.deepEqual(s.opened, ['session-7'], 'open should reach ctx.sessions.open');

  s.stop();
  assert.equal(window.__dshSignals, undefined, 'a torn-down plugin hands the fallback back');
  console.log('ok  opens a session for the shell, and only while wired up');
}

// --- which waits a press can answer, and with what ---
{
  const { exports, sent } = load({ host: true });
  const s = services();
  exports.apply(s.ctx);

  const labels = async (pending) => {
    s.setPending(new Map([['a', pending]]));
    await drain();
    return queryAll(sent[sent.length - 1], 'option');
  };

  // The two kinds whose buttons say the same thing every time send none: the
  // shell writes those in the user's own language.
  assert.deepEqual(await labels(carrier({ key: 'k1', kind: 'approval' })), []);
  assert.deepEqual(
    await labels(carrier({ key: 'k2', kind: 'plan-review', questions: asked([{ label: 'Go' }]) })),
    [],
  );

  // A generic question sends the asker's own labels, in the asker's order.
  assert.deepEqual(
    await labels(carrier({
      key: 'k3',
      kind: 'question',
      questions: asked([{ label: 'Use TypeScript' }, { label: 'Stay on JS' }]),
    })),
    ['Use TypeScript', 'Stay on JS'],
  );

  // Everything a press cannot say in full sends none, and the shell offers
  // "open" instead: too many options, more than one question, multi-select,
  // and a question with no options at all.
  assert.deepEqual(
    await labels(carrier({ key: 'k4', kind: 'question', questions: asked([{ label: 'a' }, { label: 'b' }, { label: 'c' }]) })),
    [],
  );
  assert.deepEqual(
    await labels(carrier({
      key: 'k5',
      kind: 'question',
      questions: [...asked([{ label: 'a' }]), { id: 'q2', question: 'And?', options: [{ label: 'b' }] }],
    })),
    [],
  );
  assert.deepEqual(
    await labels(carrier({ key: 'k6', kind: 'question', questions: [{ id: 'q1', question: 'Which?', multiSelect: true, options: [{ label: 'a' }, { label: 'b' }] }] })),
    [],
  );
  assert.deepEqual(await labels(carrier({ key: 'k7', kind: 'question', questions: asked(undefined) })), []);
  console.log('ok  offers labels only for a question one press can answer');
}

// --- a press, on each kind of wait ---
{
  const { exports, window } = load({ host: true });
  const s = services();
  exports.apply(s.ctx);

  const press = async (pending, choice) => {
    s.setPending(new Map([['a', pending]]));
    await drain();
    window.__dshSignals.answer('a', pending.key, choice);
    await drain();
    return pending.calls;
  };
  // An answer batch is built inside the vm realm, so compare it by value.
  const batch = (calls) => [calls[0][0], JSON.stringify(calls[0][1])];

  // An approval's two decisions, and neither of them is a standing permission.
  assert.deepEqual(
    await press(carrier({ key: 'k1', kind: 'approval' }), 'allow'),
    [['answer', 'allowed-once']],
  );
  assert.deepEqual(
    await press(carrier({ key: 'k2', kind: 'approval' }), 'reject'),
    [['answer', 'rejected']],
  );

  // A plan review answers with the asker's own approve label, verbatim, and
  // its second button hands the composer back rather than refusing in silence.
  const review = {
    key: 'k3',
    kind: 'plan-review',
    questions: [{
      id: 'plan-1',
      question: 'Ship it?',
      detail: '# the plan',
      options: [{ label: 'Looks right' }, { label: 'No' }],
      intent: { kind: 'plan-review', approve: 'Looks right' },
    }],
  };
  assert.deepEqual(
    batch(await press(carrier(review), 'approve')),
    ['answer', JSON.stringify({ answers: [{ id: 'plan-1', selected: ['Looks right'] }] })],
  );
  assert.deepEqual(await press(carrier({ ...review, key: 'k4' }), 'revise'), [['cancel']]);

  // A question answers with the whole batch, and the choice is a position.
  const question = {
    key: 'k5',
    kind: 'question',
    questions: asked([{ label: 'Use TypeScript' }, { label: 'Stay on JS' }]),
  };
  assert.deepEqual(
    batch(await press(carrier(question), '1')),
    ['answer', JSON.stringify({ answers: [{ id: 'q1', selected: ['Stay on JS'] }] })],
  );
  console.log('ok  submits what each button promised, and nothing else');
}

// --- a press that arrives too late ---
{
  const { exports, window, sent } = load({ host: true });
  const s = services();
  exports.apply(s.ctx);

  const pending = carrier({ key: 'k1', kind: 'approval' });
  s.setPending(new Map([['a', pending]]));
  await drain();
  const raised = sent.length;

  const stale = async (...args) => {
    const before = sent.length;
    window.__dshSignals.answer(...args);
    await drain();
    assert.equal(sent.length, before + 1, 'a press that submits nothing should say so');
    return query(sent[sent.length - 1]);
  };

  // The request was replaced while the toast was on screen.
  let said = await stale('a', 'k0', 'allow');
  assert.equal(said.event, 'stale');
  assert.equal(said.session, 'a');

  // The session is not waiting on anything at all.
  said = await stale('b', 'k1', 'allow');
  assert.equal(said.event, 'stale');
  assert.equal(said.session, 'b');

  // A choice this kind has no meaning for, which is what a forged signal
  // would produce: a toast drawn with buttons the carrier cannot honour.
  said = await stale('a', 'k1', 'approve');
  assert.equal(said.event, 'stale');

  assert.deepEqual(pending.calls, [], 'nothing above should have submitted anything');

  // And a submission the carrier itself rejects — a request aborted between
  // the snapshot and the call.
  const refuses = carrier({
    key: 'k2',
    kind: 'approval',
    answer() { return Promise.reject(new Error('aborted')); },
  });
  s.setPending(new Map([['a', refuses]]));
  await drain();
  said = await stale('a', 'k2', 'allow');
  assert.equal(said.event, 'stale');

  // Or throws outright, which is what a dsh whose carrier has moved on under
  // us would look like.
  const throws = carrier({
    key: 'k3',
    kind: 'approval',
    answer() { throw new Error('gone'); },
  });
  s.setPending(new Map([['a', throws]]));
  await drain();
  said = await stale('a', 'k3', 'allow');
  assert.equal(said.event, 'stale');
  console.log('ok  reports a press it could not submit instead of swallowing it');
}

// --- a folder from outside ---
{
  const { exports, sent, window } = load({ host: true });
  const created = [];
  const opened = [];
  const s = services({
    folders: {
      create: (input) => {
        created.push(input.path);
        return Promise.resolve({ workspaceId: 'w1' });
      },
      openWorkspace: (id) => { opened.push(id); return Promise.resolve(); },
    },
  });
  exports.apply(s.ctx);
  await drain();
  // The shell holds a launch's folder until this arrives.
  assert.equal(sent.length, 1);
  assert.equal(query(sent[0]).event, 'folder-ready');

  window.__dshSignals.openFolder('C:\work\demo');
  await drain();
  assert.deepEqual(created, ['C:\work\demo']);
  assert.deepEqual(opened, ['w1']);
  assert.equal(query(sent[1]).event, 'folder-opened');
  assert.equal(sent.length, 2, 'an opened folder is not also reported as failed');
  console.log('ok  opens a folder as a workspace once dsh can');
}

// --- a folder dsh will not take ---
{
  const { exports, sent, window } = load({ host: true });
  const opened = [];
  const s = services({
    folders: {
      create: () => Promise.reject(new Error('not a directory')),
      openWorkspace: (id) => { opened.push(id); return Promise.resolve(); },
    },
  });
  exports.apply(s.ctx);
  window.__dshSignals.openFolder('/nope');
  await drain();
  const failed = sent.map(query).find((params) => params.event === 'folder-failed');
  assert.ok(failed, 'a refusal is reported, not swallowed');
  assert.equal(failed.reason, 'not a directory');
  assert.deepEqual(opened, []);
  console.log('ok  reports a folder dsh refused');
}

// --- a folder before dsh has workspaces ---
{
  const { exports, sent, window } = load({ host: true });
  const s = services();
  exports.apply(s.ctx);
  window.__dshSignals.openFolder('/somewhere');
  await drain();
  assert.equal(sent.length, 1);
  assert.equal(query(sent[0]).event, 'folder-failed');
  console.log('ok  says so when a folder arrives with nothing to open it');
}

// --- how busy dsh is, for the shell's icons ---
{
  const { exports, statuses } = load({ host: true });
  const s = services();
  // Loaded mid-turn: the baseline counts too, unlike a turn ending.
  s.setList({ ids: ['a', 'b'], byId: { a: { running: true }, b: { running: false } } });
  exports.apply(s.ctx);
  await drain();
  assert.deepEqual(statuses.map(query).map((p) => [p.running, p.waiting]), [['1', '0']]);

  // A repeat of the same counts says nothing, and a session stopped on the
  // user is waiting rather than running, though dsh still calls its turn
  // running.
  s.setList({ ids: ['a', 'b'], byId: { a: { running: true }, b: { running: false } } });
  s.setPending(new Map([['a', carrier({ kind: 'approval', key: 'k1' })]]));
  s.setList({ ids: ['a', 'b'], byId: { a: { running: false }, b: { running: false } } });
  await drain();
  assert.deepEqual(
    statuses.map(query).map((p) => [p.running, p.waiting]),
    [['1', '0'], ['0', '1']],
  );

  // A scope going away takes its count with it.
  s.stop();
  await drain();
  assert.deepEqual(statuses.map(query).map((p) => [p.running, p.waiting]).at(-1), ['0', '0']);
  console.log('ok  reports how many sessions are running and waiting');
}

// --- which sessions, by name, for the shell's tray menu ---
{
  const { exports, statuses } = load({ host: true });
  const s = services();
  exports.apply(s.ctx);
  s.setList({
    ids: ['a', 'b', 'c', 'sub'],
    byId: {
      a: { running: true, displayTitle: 'Fix the build' },
      b: { running: true, displayTitle: 'Write the docs' },
      c: { running: false, displayTitle: 'Idle one' },
      sub: { running: true, displayTitle: 'A subagent', origin: 'subagent' },
    },
  });
  s.setPending(new Map([['b', carrier({ kind: 'approval', key: 'k1' })]]));
  await drain();
  const last = statuses.at(-1);
  // Waiting first; a running subagent is counted and not listed.
  assert.deepEqual(queryAll(last, 'wid'), ['b']);
  assert.deepEqual(queryAll(last, 'wtitle'), ['Write the docs']);
  assert.deepEqual(queryAll(last, 'rid'), ['a']);
  assert.deepEqual(queryAll(last, 'rtitle'), ['Fix the build']);
  assert.deepEqual([query(last).running, query(last).waiting], ['2', '1']);

  // A title dsh fills in later goes out with the next list change.
  const before = statuses.length;
  s.setList({
    ids: ['a', 'b', 'c', 'sub'],
    byId: {
      a: { running: true, displayTitle: 'Fix the CI build' },
      b: { running: true, displayTitle: 'Write the docs' },
      c: { running: false, displayTitle: 'Idle one' },
      sub: { running: true, displayTitle: 'A subagent', origin: 'subagent' },
    },
  });
  await drain();
  assert.equal(statuses.length, before + 1);
  assert.deepEqual(queryAll(statuses.at(-1), 'rtitle'), ['Fix the CI build']);
  console.log('ok  lists the waiting and running sessions by name');
}

// --- a line from the quick-ask box starts a new session on it ---
{
  const { exports, sent, window } = load({ host: true });
  const made = [];
  const prompts = [];
  const s = services({
    folders: {
      create: ({ path }) => Promise.resolve({ workspaceId: 'ws-' + path }),
      openWorkspace: () => { throw new Error('a quick ask does not switch workspaces'); },
    },
  });
  s.ctx.sessions.create = (options) => { made.push(options.workspaceId); return Promise.resolve('session-new'); };
  s.ctx.sessions.using = (id, options, operation) => Promise.resolve(operation({
    ready: Promise.resolve({
      session: { prompt: (content, mode) => { prompts.push([id, content[0].text, mode]); return Promise.resolve({ ok: true }); } },
    }),
  }));
  exports.apply(s.ctx);

  window.__dshSignals.ask('/work/a', 'fix the build');
  await drain();
  assert.deepEqual(made, ['ws-/work/a']);
  assert.deepEqual(prompts, [['session-new', 'fix the build', 'queue']]);
  assert.deepEqual(sent.map(query).map((p) => [p.event, p.session]).at(-1), ['asked', 'session-new']);
  assert.deepEqual(s.opened, [], 'nothing is switched to');

  // dsh refusing the workspace is a failure with dsh's reason.
  s.ctx.workspaces.create = () => Promise.reject(new Error('not a directory'));
  window.__dshSignals.ask('/nope', 'x');
  await drain();
  assert.deepEqual([query(sent.at(-1)).event, query(sent.at(-1)).reason], ['ask-failed', 'not a directory']);

  // Into a session that is already there: nothing is made.
  window.__dshSignals.ask('/nope', 'and the tests', 'session-old');
  await drain();
  assert.deepEqual(made, ['ws-/work/a'], 'no new session');
  assert.deepEqual(prompts.at(-1), ['session-old', 'and the tests', 'queue']);
  assert.deepEqual(sent.map(query).map((p) => [p.event, p.session]).at(-1), ['asked', 'session-old']);
  console.log('ok  starts a new session on a quick-ask line, or sends it into one');
}

// --- the models the quick-ask box offers, and one chosen there ---
{
  const { exports, sent, window } = load({ host: true });
  const calls = [];
  const handlers = {};
  let catalog = {
    default: { provider: 'ds', model: 'flash' },
    routableProviders: ['ds'],
    groups: [{ id: 'ds', name: 'DeepSeek', models: [{ id: 'flash', name: 'Flash' }, { id: 'pro', name: 'Pro' }] }],
    failures: [],
  };
  let refuse = false;
  const s = services({
    folders: {
      create: ({ path }) => Promise.resolve({ workspaceId: 'ws-' + path }),
      openWorkspace: () => {},
    },
  });
  s.ctx.sessions.create = () => { calls.push('create'); return Promise.resolve('session-new'); };
  s.ctx.sessions.using = (id, options, operation) => Promise.resolve(operation({
    ready: Promise.resolve({
      session: { prompt: (content) => { calls.push(['prompt', id, content[0].text]); return Promise.resolve({ ok: true }); } },
    }),
  }));
  s.ctx.remote = {
    session: {
      modelCatalog: () => Promise.resolve({ ok: true, value: catalog }),
      selectModel: (request) => {
        calls.push(['select', request.sessionId, request.provider, request.model]);
        return Promise.resolve(refuse
          ? { ok: false, error: { code: 'session/model-unavailable', message: 'no such model' } }
          : { ok: true, value: { selected: request } });
      },
    },
    $on: (name, fn) => { handlers[name] = fn; return () => { delete handlers[name]; }; },
  };
  s.ctx['remote.session'] = s.ctx.remote.session;
  exports.apply(s.ctx);
  await drain();

  const models = () => sent.filter((url) => query(url).event === 'models');
  assert.equal(models().length, 1);
  assert.deepEqual(queryAll(models()[0], 'mid'), ['flash', 'pro']);
  assert.deepEqual(queryAll(models()[0], 'mname'), ['Flash', 'Pro']);
  assert.deepEqual(queryAll(models()[0], 'mprov'), ['ds', 'ds']);
  assert.deepEqual(queryAll(models()[0], 'mpname'), ['DeepSeek', 'DeepSeek']);
  assert.deepEqual([query(models()[0]).dprov, query(models()[0]).dmod], ['ds', 'flash']);

  // Read again when what it is made of changes, and sent only if it did.
  handlers['settings/document-updated']();
  await drain();
  assert.equal(models().length, 1, 'the same catalog is not sent again');
  catalog = { ...catalog, default: { provider: 'ds', model: 'pro' } };
  handlers['credentials/record-updated']();
  await drain();
  assert.equal(models().length, 2);
  assert.equal(query(models()[1]).dmod, 'pro');

  // A model chosen in the box is selected before the line goes in.
  // It is the default now, too, and the catalog is read again to say so.
  catalog = { ...catalog, default: { provider: 'ds', model: 'flash' } };
  window.__dshSignals.ask('/work/a', 'on pro', '', 'ds', 'pro');
  await drain();
  assert.deepEqual(calls, ['create', ['select', 'session-new', 'ds', 'pro'], ['prompt', 'session-new', 'on pro']]);
  assert.equal(models().length, 3);
  assert.equal(query(models()[2]).dmod, 'flash');

  // None chosen, none selected.
  calls.length = 0;
  window.__dshSignals.ask('/work/a', 'as it was', 'session-old', '', '');
  await drain();
  assert.deepEqual(calls, [['prompt', 'session-old', 'as it was']]);

  // dsh refusing the model: nothing goes in, and the box hears why.
  calls.length = 0;
  refuse = true;
  window.__dshSignals.ask('/work/a', 'x', 'session-old', 'ds', 'gone');
  await drain();
  assert.deepEqual(calls, [['select', 'session-old', 'ds', 'gone']]);
  assert.deepEqual([query(sent.at(-1)).event, query(sent.at(-1)).reason], ['ask-failed', 'no such model']);

  s.stop();
  assert.deepEqual(Object.keys(handlers), [], 'the listeners go with the scope');
  console.log('ok  reports the model catalog and selects a model chosen in the quick-ask box');
}

// --- text into a composer, after what is already there ---
{
  const { exports, window } = load({ host: true });
  const s = services();
  let draft = 'half a thought   ';
  const focused = [];
  const scopes = {};
  s.ctx.sessions.scope = (id) => scopes[id];
  s.ctx.conversation = {
    input: {
      for: (scope) => ({
        state: { getSnapshot: () => ({ draft }) },
        setDraft: (text) => { draft = text; },
        focus: () => { focused.push(scope.id); },
      }),
    },
  };
  exports.apply(s.ctx);
  s.setList({
    ids: ['a', 'b'],
    byId: { a: { running: false, retainedBy: {} }, b: { running: false, retainedBy: { mainView: 1 } } },
  });

  // '' is the session on screen, the one the main view holds.
  scopes.b = { id: 'b' };
  window.__dshSignals.draft('', 'error: it broke');
  assert.equal(draft, 'half a thought\nerror: it broke');
  assert.deepEqual(focused, ['b']);

  // A named session is switched to, and waited for until it has a scope.
  draft = '';
  window.__dshSignals.draft('a', 'from the clipboard');
  assert.deepEqual(s.opened, ['a']);
  assert.equal(draft, '', 'nothing to write into yet');
  scopes.a = { id: 'a' };
  await new Promise((resolve) => setTimeout(resolve, 120));
  assert.equal(draft, 'from the clipboard');
  assert.deepEqual(focused, ['b', 'a']);
  console.log('ok  adds text to a composer and focuses it');
}

// --- a click on a notification switches sessions the dsh 0.2 way ---
{
  const { exports, window } = load({ host: true });
  const viewed = [];
  const s = services({
    folders: { create: () => Promise.reject(), openWorkspace: () => {} },
  });
  s.ctx.uiWorkspace.openSession = (id) => { viewed.push(id); };
  exports.apply(s.ctx);
  window.__dshSignals.open('session-7');
  assert.deepEqual(viewed, ['session-7'], 'open should go through uiWorkspace.openSession');
  assert.deepEqual(s.opened, [], 'and not through the sessions.open dsh 0.2 dropped');
  console.log('ok  opens a session through the workspace view on dsh 0.2');
}

// --- dsh 0.2: the waits are on `sessionStatus` ---
{
  const { exports, sent, statuses, window } = load({ host: true });
  const s = services({ uiSession: 'status' });
  exports.apply(s.ctx);
  const approval = carrier({ kind: 'approval', key: 'approval:1', toolName: 'bash' });
  s.setPending(new Map([['a', approval]]));
  await drain();
  const wait = sent.map(query).find((p) => p.event === 'wait');
  assert.ok(wait, 'a wait on a 0.2 dsh is announced');
  assert.equal(wait.key, 'approval:1');
  assert.equal(wait.tool, 'bash');
  assert.equal(query(statuses.at(-1)).waiting, '1');

  // And a press reaches the same carrier.
  window.__dshSignals.answer('a', 'approval:1', 'allow');
  await drain();
  assert.deepEqual(approval.calls, [['answer', 'allowed-once']]);

  s.setPending(new Map());
  await drain();
  assert.equal(query(sent.at(-1)).event, 'wait-over');
  assert.equal(query(statuses.at(-1)).waiting, '0');
  console.log('ok  reads waits off sessionStatus on dsh 0.2');
}

// --- recent workspaces, for the shell's tray ---
{
  const { exports, recents } = load({ host: true });
  const s = services({ folders: { create: () => Promise.reject(), openWorkspace: () => {} } });
  const ws = (id, title, updatedAt, sessionIds = []) =>
    ({ workspaceId: id, title, path: '/' + id, updatedAt, sessionIds });
  s.setList({
    ids: ['s1', 's2'],
    byId: { s1: { running: false, updatedAt: Date.parse('2026-01-05') }, s2: { running: false, updatedAt: Date.parse('2026-01-02') } },
  });
  s.setWorkspaces([
    ws('w1', 'Old', '2026-01-01T00:00:00Z', ['s2']),
    ws('w2', 'Used', '2026-01-01T00:00:00Z', ['s1']),
    ws('w3', 'Renamed & new', '2026-01-04T00:00:00Z'),
  ]);
  exports.apply(s.ctx);
  await drain();
  assert.equal(recents.length, 1);
  // Ranked by the newest session, and the workspace's own time when it has none.
  assert.deepEqual(queryAll(recents[0], 'title'), ['Used', 'Renamed & new', 'Old']);
  assert.deepEqual(queryAll(recents[0], 'path'), ['/w2', '/w3', '/w1']);

  // A session moving without changing the order says nothing.
  s.setList({
    ids: ['s1', 's2'],
    byId: { s1: { running: true, updatedAt: Date.parse('2026-01-06') }, s2: { running: false, updatedAt: Date.parse('2026-01-02') } },
  });
  await drain();
  assert.equal(recents.length, 1, 'the same list should not be sent again');

  // Only the first few.
  s.setWorkspaces(Array.from({ length: 8 }, (_, i) => ws('x' + i, 'X' + i, `2026-02-0${i + 1}T00:00:00Z`)));
  await drain();
  assert.equal(recents.length, 2);
  assert.deepEqual(queryAll(recents[1], 'title'), ['X7', 'X6', 'X5', 'X4', 'X3']);
  console.log('ok  reports the most recently used workspaces');
}

// --- each recent workspace's sessions, for the quick-ask box ---
{
  const { exports, recents, sessionLists } = load({ host: true });
  const s = services({ folders: { create: () => Promise.reject(), openWorkspace: () => {} } });
  const row = (day, more) => ({ running: false, displayTitle: 'T' + day, updatedAt: Date.parse(`2026-02-0${day}`), ...more });
  s.setList({
    ids: ['a1', 'a2', 'a3', 'a4', 'a5', 'b1'],
    byId: {
      a1: row(1),
      a2: row(3),
      a3: row(2, { blank: true }),
      a4: row(4, { origin: 'subagent' }),
      a5: row(5),
      b1: row(6),
    },
  });
  s.setWorkspaces([
    { workspaceId: 'a', title: 'A', path: '/a', updatedAt: '2026-01-01T00:00:00Z', sessionIds: ['a1', 'a2', 'a3', 'a4', 'a5', 'gone'] },
    { workspaceId: 'b', title: 'B', path: '/b', updatedAt: '2026-01-01T00:00:00Z', sessionIds: ['b1'] },
  ]);
  s.setArchived(['a5']);
  exports.apply(s.ctx);
  await drain();
  assert.equal(sessionLists.length, 1);
  // Newest workspace first, newest session first; no blank, subagent,
  // archived or unknown ones.
  assert.deepEqual(queryAll(sessionLists[0], 'sid'), ['b1', 'a2', 'a1']);
  assert.deepEqual(queryAll(sessionLists[0], 'spath'), ['/b', '/a', '/a']);
  assert.deepEqual(queryAll(sessionLists[0], 'stitle'), ['T6', 'T3', 'T1']);
  assert.deepEqual(queryAll(sessionLists[0], 'smod'), ['', '', ''], 'no remote, no models');
  assert.deepEqual(queryAll(sessionLists[0], 'sprov'), ['', '', '']);

  // A session moving up its workspace changes this list and not the tray's.
  s.setList({
    ids: ['a1', 'a2', 'b1'],
    byId: { a1: row(9), a2: row(3), b1: row(6) },
  });
  await drain();
  assert.equal(recents.length, 2, 'A moved above B in the tray, once');
  assert.deepEqual(queryAll(sessionLists.at(-1), 'sid'), ['a1', 'a2', 'b1']);
  const count = sessionLists.length;
  s.setList({
    ids: ['a1', 'a2', 'b1'],
    byId: { a1: row(9, { running: true }), a2: row(3), b1: row(6) },
  });
  await drain();
  assert.equal(sessionLists.length, count, 'the same list is not sent again');
  console.log('ok  reports the sessions of each recent workspace');
}

// --- and the model each of those sessions runs on ---
{
  const { exports, sessionLists } = load({ host: true });
  const s = services({ folders: { create: () => Promise.reject(), openWorkspace: () => {} } });
  const read = [];
  const selection = {
    a1: { lastUsed: { provider: 'ds', model: 'flash' }, next: { provider: 'ds', model: 'flash' } },
    a2: { lastUsed: { provider: 'ds', model: 'flash' }, next: { provider: 'ds', model: 'pro' } },
  };
  s.ctx.remote = {
    session: {
      modelCatalog: () => Promise.resolve({ ok: false, error: { code: 'x', message: 'not now' } }),
      projections: ({ sessionId }) => {
        read.push(sessionId);
        const values = selection[sessionId] ? { modelSelection: selection[sessionId] } : {};
        return Promise.resolve({ ok: true, value: { asOfSeq: 1, values } });
      },
    },
    $on: () => () => {},
  };
  s.ctx['remote.session'] = s.ctx.remote.session;
  const row = (day, more) => ({ running: false, displayTitle: 'T' + day, updatedAt: Date.parse(`2026-02-0${day}`), ...more });
  s.setList({ ids: ['a1', 'a2', 'a3'], byId: { a1: row(1), a2: row(3), a3: row(2) } });
  s.setWorkspaces([
    { workspaceId: 'a', title: 'A', path: '/a', updatedAt: '2026-01-01T00:00:00Z', sessionIds: ['a1', 'a2', 'a3'] },
  ]);
  exports.apply(s.ctx);
  await drain();
  // Newest first; next, else last used, else unknown.
  assert.deepEqual(queryAll(sessionLists.at(-1), 'sid'), ['a2', 'a3', 'a1']);
  assert.deepEqual(queryAll(sessionLists.at(-1), 'smod'), ['pro', '', 'flash']);
  assert.deepEqual(queryAll(sessionLists.at(-1), 'sprov'), ['ds', '', 'ds']);
  assert.deepEqual(read.sort(), ['a1', 'a2', 'a3'], 'one read a session');

  // Not again until the session moves, and not while it runs.
  read.length = 0;
  s.setList({ ids: ['a1', 'a2', 'a3'], byId: { a1: row(1), a2: row(3), a3: row(2) } });
  s.setList({ ids: ['a1', 'a2', 'a3'], byId: { a1: row(5, { running: true }), a2: row(3), a3: row(2) } });
  await drain();
  assert.deepEqual(read, []);

  // Its turn over, and on another model.
  selection.a1 = { lastUsed: { provider: 'ds', model: 'pro' }, next: { provider: 'ds', model: 'pro' } };
  s.setList({ ids: ['a1', 'a2', 'a3'], byId: { a1: row(6), a2: row(3), a3: row(2) } });
  await drain();
  assert.deepEqual(read, ['a1']);
  assert.deepEqual(queryAll(sessionLists.at(-1), 'sid'), ['a1', 'a2', 'a3']);
  assert.deepEqual(queryAll(sessionLists.at(-1), 'smod'), ['pro', 'pro', '']);
  console.log('ok  reports the model each listed session runs on');
}

// --- no desktop shell: no door either ---
{
  const { exports, window } = load({ host: false });
  const s = services();
  exports.apply(s.ctx);

  assert.equal(window.__dshSignals, undefined, 'nothing to open outside the shell');
  console.log('ok  hangs nothing on window outside the desktop shell');
}

console.log('\nall plugin checks passed');
