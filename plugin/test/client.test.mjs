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

function load({ host }) {
  const sent = [];
  let registered = null;
  const window = {
    __ModuleLoader__: { load: (row) => { registered = row; } },
    location: { set href(value) { sent.push(value); }, get href() { return ''; } },
  };
  if (host) window.__DSH_VERSION__ = '0.1.13';
  const context = vm.createContext({ window, setTimeout, console });
  vm.runInContext(source, context);
  assert.ok(registered, 'the bundle should register a factory');
  assert.equal(registered.id, 'dsh-desktop-signal');
  const exports = registered.factory(() => { throw new Error('should require nothing'); });
  return { exports, sent, window };
}

// Stub services with hand-driven snapshots.
function services() {
  let list = { ids: [], byId: {} };
  let pending = new Map();
  const listeners = { list: [], pending: [] };
  const opened = [];
  let teardown = null;
  return {
    opened,
    stop() { if (teardown) teardown(); },
    ctx: {
      // The real one keeps the cleanup to run on disposal; this keeps it so a
      // test can run it.
      effect: (fn) => { teardown = fn(); },
      sessions: {
        list: {
          getSnapshot: () => list,
          subscribe: (fn) => { listeners.list.push(fn); return () => {}; },
        },
        open: (id) => { opened.push(id); },
      },
      uiSession: {
        pendingInteractions: {
          getSnapshot: () => pending,
          subscribe: (fn) => { listeners.pending.push(fn); return () => {}; },
        },
      },
    },
    setList(next) { list = next; listeners.list.forEach((fn) => fn()); },
    setPending(next) { pending = next; listeners.pending.forEach((fn) => fn()); },
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
  // Crosses the vm realm, so compare by value rather than by prototype.
  assert.equal(JSON.stringify(exports.inject), '["sessions","uiSession"]');
  assert.equal(typeof exports.apply, 'function');
  console.log('ok  exports the cordis services and an apply');
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

// --- no desktop shell: no door either ---
{
  const { exports, window } = load({ host: false });
  const s = services();
  exports.apply(s.ctx);

  assert.equal(window.__dshSignals, undefined, 'nothing to open outside the shell');
  console.log('ok  hangs nothing on window outside the desktop shell');
}

console.log('\nall plugin checks passed');
