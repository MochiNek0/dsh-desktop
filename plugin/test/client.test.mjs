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

// --- no desktop shell: no door either ---
{
  const { exports, window } = load({ host: false });
  const s = services();
  exports.apply(s.ctx);

  assert.equal(window.__dshSignals, undefined, 'nothing to open outside the shell');
  console.log('ok  hangs nothing on window outside the desktop shell');
}

console.log('\nall plugin checks passed');
