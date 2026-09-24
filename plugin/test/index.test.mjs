// Checks the node half: the index tap that puts a phone-shaped viewport on
// dsh's boot html, and the fiber it is registered from.
//
// No dependencies and no runner: `node test/index.test.mjs`. The transform is a
// pure string function, which is the whole of what `tapIndex` asks a tap to be,
// so most of this is just markup in and markup out.
import assert from 'node:assert/strict';

import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { MOBILE_CSS, TRANSPORT, apply, downloaded, flag, homescreen, inject, override, rows, viewport } from '../lib/index.js';

// --- the tag itself ---
{
  // dsh's own index, as shipped. The tag is replaced rather than joined by a
  // second one: a structured injection row lands before dsh's head content, and
  // the later viewport tag is the one a browser takes.
  const shipped =
    '<!doctype html><html lang="en"><head>' +
    '<meta charset="utf-8" />' +
    '<meta name="viewport" content="width=device-width, initial-scale=1" />' +
    '<title>DeepSeek Harness</title></head><body><div id="root"></div></body></html>';

  const out = viewport(shipped);
  assert.equal(out.match(/name="viewport"/g).length, 1, 'exactly one viewport tag');
  assert.ok(out.includes('width=device-width, initial-scale=1'), 'the phone-shaped one');
  assert.ok(out.includes('<meta charset="utf-8" />'), 'nothing else is touched');
  assert.ok(out.includes('<div id="root"></div>'));
  console.log('ok  replaces the viewport dsh ships rather than adding a second');
}

// --- zoom stays available, and the display cutout is left alone ---
{
  // Not omissions. `user-scalable=no`: iOS Safari ignores it, Android obeys it,
  // and dsh is a page of text someone may need to zoom into. `viewport-fit`:
  // `cover` only pays off for a layout that pads itself back out with
  // `env(safe-area-inset-*)`, and dsh's stylesheets use those nowhere, so it
  // would put the bottom of the page under the home indicator. See lib/index.js.
  const out = viewport('<head><meta name="viewport" content="width=device-width"></head>');
  assert.ok(!out.includes('user-scalable'), 'pinch to zoom is left alone');
  assert.ok(!out.includes('maximum-scale'));
  assert.ok(!out.includes('viewport-fit'), 'the display cutout is left alone');
  console.log('ok  leaves pinch to zoom and the display cutout alone');
}

// --- a dsh that stopped shipping one ---
{
  const out = viewport('<html><head><title>x</title></head><body></body></html>');
  assert.ok(out.includes('name="viewport"'), 'one is written when there is none');
  assert.ok(
    out.indexOf('name="viewport"') < out.indexOf('<title>'),
    'directly after the opening head tag'
  );
  console.log('ok  writes one into an index that has none');
}

// --- attribute order and quoting are dsh's to change ---
{
  for (const tag of [
    "<meta name='viewport' content='width=device-width'>",
    '<meta content="width=device-width" name="viewport" />',
    '<meta NAME="VIEWPORT" content="width=device-width">',
  ]) {
    const out = viewport(`<head>${tag}</head>`);
    assert.equal(out.match(/name="viewport"/g).length, 1, `one tag left for ${tag}`);
    assert.ok(out.includes('initial-scale=1'), `replaced for ${tag}`);
  }
  console.log('ok  matches the element rather than one spelling of it');
}

// --- a body it does not recognise ---
{
  const odd = '<p>not an index</p>';
  assert.equal(viewport(odd), odd, 'returned untouched');
  console.log('ok  leaves a body with no head exactly as it found it');
}

// --- the entry waits for nothing ---
{
  // The lesson from `./client`: an entry still PENDING on a service fails dsh's
  // whole web boot. `webserver` is waited for one scope down, so a dsh whose
  // composition has no webserver runs no tap and starts perfectly.
  assert.deepEqual(inject, [], 'the entry declares no dependency');

  const taps = [];
  const effects = [];
  let waited = null;

  const listeners = {};
  // `webServer`, capitalised exactly as `dsh-host-webserver` registers it
  // (`super(ctx, 'webServer')`). Spelling it any other way means the fiber
  // waits forever on a service nothing provides and the tap silently never
  // runs, which is a failure with no error attached to it -- so the name is
  // asserted below rather than merely mirrored by this mock.
  const scope = {
    webServer: { tapIndex: (fn) => { taps.push(fn); return () => taps.pop(); } },
    effect: (run, label) => { effects.push(label); return run(); },
    on: (event, handler) => { (listeners[event] ??= []).push(handler); },
  };
  apply({ inject: (deps, callback) => { waited = deps; callback(scope); } });

  assert.deepEqual(waited, ['webServer'], 'and waits for it in a child fiber');
  assert.equal(taps.length, 2, 'both taps registered');
  assert.equal(effects.length, 2, 'through effects, so disposal takes them off');
  assert.ok(taps[0]('<head></head>').includes('name="viewport"'));
  assert.ok(taps[1]('<head></head>').includes('rel="manifest"'));
  assert.deepEqual(
    Object.keys(listeners),
    ['webserver/index-inject'],
    'and subscribes to the injection table from the same fiber'
  );
  console.log('ok  registers the tap one scope down, as an effect');
}

// --- the transport global ---
{
  // dsh reads `globalThis.__DSH_TRANSPORT__` once, at connection boot, and
  // `ownsHost` is what decides whether a page may hold host settings. Without
  // it a phone gets `persistence: 'memory'` and every settings namespace
  // reports unavailable. See the note in lib/index.js.
  assert.equal(TRANSPORT.kind, 'global', 'a table row, not a script this built');
  assert.equal(TRANSPORT.name, '__DSH_TRANSPORT__');
  assert.deepEqual(TRANSPORT.value, { ownsHost: true });

  // The whole point: nothing else is set. `fetch` and `openStream` left
  // undefined is the same shape as no transport at all, so the RPC keeps the
  // page's own fetch and the stream carrier stays absent.
  assert.deepEqual(Object.keys(TRANSPORT.value), ['ownsHost'], 'and nothing else');

  // It is JSON the webserver stringifies into the head, so it must survive a
  // round trip unchanged.
  assert.deepEqual(JSON.parse(JSON.stringify(TRANSPORT)), TRANSPORT);

  // One row per collection, pushed onto the table the webserver passes in.
  const table = ['already here'];
  const listeners = {};
  apply({
    inject: (_deps, callback) => callback({
      webServer: { tapIndex: () => () => {} },
      effect: (run) => run(),
      on: (event, handler) => { (listeners[event] ??= []).push(handler); },
    }),
  });
  for (const handler of listeners['webserver/index-inject']) handler(table);
  assert.deepEqual(table[0], 'already here', 'appended, nothing dropped');
  assert.deepEqual(table[1], TRANSPORT, 'and the transport row is first of ours');
  console.log('ok  contributes the ownsHost transport row');
}

// --- the stylesheet patch is off unless the desktop says otherwise ---
{
  const home = mkdtempSync(join(tmpdir(), 'dsh-plugin-'));
  const previous = process.env.DSH_HOME;
  process.env.DSH_HOME = home;
  try {
    // Same two names the Rust half writes; see remote/style.rs. Spelled
    // differently on either side, the switch silently does nothing.
    assert.equal(flag(), join(home, '.dsh-desktop', 'mobile-css'));

    assert.deepEqual(rows(), [TRANSPORT], 'no flag, no stylesheet');

    mkdirSync(join(home, '.dsh-desktop'), { recursive: true });
    writeFileSync(flag(), 'anything');
    const on = rows();
    assert.equal(on.length, 2, 'the flag adds exactly one row');
    assert.deepEqual(on[0], TRANSPORT, 'and does not disturb the first');
    assert.equal(on[1].kind, 'style', 'a style row');
    assert.equal(on[1].text, MOBILE_CSS);

    // Presence is the signal, matching the Rust half's `path.exists()`.
    writeFileSync(flag(), '');
    assert.equal(rows().length, 2, 'an empty flag file still counts as on');

    rmSync(flag());
    assert.deepEqual(rows(), [TRANSPORT], 'and removing it turns it back off');
    console.log('ok  injects the stylesheet only while the desktop asks for it');
  } finally {
    if (previous === undefined) delete process.env.DSH_HOME;
    else process.env.DSH_HOME = previous;
    rmSync(home, { recursive: true, force: true });
  }
}

// --- a stylesheet of the user's own wins over the built-in ---
{
  const home = mkdtempSync(join(tmpdir(), 'dsh-plugin-'));
  const previous = process.env.DSH_HOME;
  process.env.DSH_HOME = home;
  try {
    assert.equal(override(), join(home, '.dsh-desktop', 'mobile.css'));

    mkdirSync(join(home, '.dsh-desktop'), { recursive: true });
    writeFileSync(flag(), '');
    assert.equal(rows()[1].text, MOBILE_CSS, 'with no file of their own, the built-in');

    // The reason this exists: dsh's markup moves on dsh's release schedule, so
    // fixing the patch must not need a build of the desktop app.
    const mine = '@media (max-width: 560px) { .x { color: red } }';
    writeFileSync(override(), mine);
    assert.equal(rows()[1].text, mine, 'their stylesheet replaces it wholesale');
    assert.equal(rows().length, 2, 'and is still exactly one row');

    // The switch stays the switch: a stylesheet alone injects nothing.
    rmSync(flag());
    assert.deepEqual(rows(), [TRANSPORT], 'a file of their own is not the switch');

    writeFileSync(flag(), '');
    // Fallbacks. A blank file is a half-written one, and `</style` would close
    // the element early and spill the rest of the file into the document.
    for (const [text, why] of [
      ['   ', 'blank falls back'],
      ['<style>.x{}</style>', 'a pasted <style> block falls back'],
      ['.x{} </STYLE >', 'in any spelling'],
    ]) {
      writeFileSync(override(), text);
      assert.equal(rows()[1].text, MOBILE_CSS, why);
    }
    console.log("ok  a stylesheet of the user's own replaces the built-in");

    // The desktop's download is a file of its own, so it can never overwrite
    // the user's: it stands in for the built-in, and theirs still wins.
    assert.equal(downloaded(), join(home, '.dsh-desktop', 'mobile-patch.css'));
    rmSync(override());
    const fetched = '@media (max-width: 560px) { .y { color: blue } }';
    writeFileSync(downloaded(), fetched);
    assert.equal(rows()[1].text, fetched, 'the download replaces the built-in');
    writeFileSync(override(), mine);
    assert.equal(rows()[1].text, mine, "and the user's own replaces the download");
    console.log("ok  a downloaded stylesheet never displaces the user's");
  } finally {
    if (previous === undefined) delete process.env.DSH_HOME;
    else process.env.DSH_HOME = previous;
    rmSync(home, { recursive: true, force: true });
  }
}

// --- a blank DSH_HOME is not a home ---
{
  const previous = process.env.DSH_HOME;
  process.env.DSH_HOME = '   ';
  try {
    // dsh's own rule (`resolveDshHome`). Without it the flag would be looked
    // for relative to whatever directory dsh happened to start in.
    assert.ok(!flag().startsWith('   '), 'a blank override is treated as unset');
    assert.ok(flag().endsWith(join('.dsh', '.dsh-desktop', 'mobile-css')));
    console.log('ok  falls back to ~/.dsh when DSH_HOME is blank');
  } finally {
    if (previous === undefined) delete process.env.DSH_HOME;
    else process.env.DSH_HOME = previous;
  }
}

// --- an unreadable home must not take dsh's index down with it ---
{
  const previous = process.env.DSH_HOME;
  // A NUL byte makes `existsSync`'s underlying stat throw rather than answer.
  process.env.DSH_HOME = 'bad path';
  try {
    assert.deepEqual(rows(), [TRANSPORT], 'a throwing probe reads as off');
    console.log('ok  a home it cannot probe is off, not an exception');
  } finally {
    if (previous === undefined) delete process.env.DSH_HOME;
    else process.env.DSH_HOME = previous;
  }
}

// --- the patch itself ---
{
  // The hashes in dsh's class names change every time dsh is rebuilt, so a
  // selector naming one is a patch with a silent expiry date. See lib/index.js.
  assert.ok(!/[A-Za-z0-9-]{5,}_(panel|nav|content|close)/.test(MOBILE_CSS),
    'no CSS-module hash is named');
  assert.ok(MOBILE_CSS.includes('nav[class*="_nav"]'), 'anchored on the nav element');
  assert.ok(MOBILE_CSS.includes(':has('), 'and on structure rather than a name');

  // Every rule is inside a query, which is what keeps the desktop window
  // untouched. Two of them: width for the dialog that does not fit, and
  // `hover: none` for the controls dsh only reveals on hover -- an iPad and a
  // phone in landscape are both wider than 560px and neither has a pointer, so
  // gating the second on width is what left them unreachable there.
  const opens = (MOBILE_CSS.match(/\{/g) || []).length;
  const closes = (MOBILE_CSS.match(/\}/g) || []).length;
  assert.equal(opens, closes, 'the stylesheet is balanced');
  assert.ok(MOBILE_CSS.startsWith('@media (max-width: 560px) {'), "dsh's own breakpoint");
  assert.ok(MOBILE_CSS.trimEnd().endsWith('}'));
  assert.deepEqual(MOBILE_CSS.match(/@media[^{]*/g).map((one) => one.trim()),
    ['@media (max-width: 560px)', '@media (hover: none)'],
    'and those are the only two');

  // A hover-gated rule inside the width query is the bug this split fixed, so
  // nothing reaches for hover from inside it.
  const [width, hover] = MOBILE_CSS.split('@media (hover: none) {');
  assert.ok(!/_rowActions|_heroWorkspaceRow/.test(width), 'nothing hover-gated is width-gated');
  assert.ok(/_rowActions/.test(hover) && /_heroWorkspaceRow/.test(hover));

  // The chip's own label carries a class matching `_workspace` too, so a
  // descendant selector pads the text as well as the button it sits in.
  assert.ok(hover.includes('[class*="_heroWorkspaceRow"] > [class*="_workspace"]'),
    'the chip is padded, not its label');

  // `width <= 560px` needs Safari 16.4; this has to work on whatever phone is
  // to hand.
  assert.ok(!MOBILE_CSS.includes('<='), 'no range-syntax media query');

  // A `style` row is inlined into a <style> element, so this would end it early.
  assert.ok(!/<\/style/i.test(MOBILE_CSS), 'nothing that closes the element');
  console.log('ok  the patch is hash-free, balanced, and gated on width and hover');
}

// --- the dialog still scrolls ---
{
  // Stacking the panel makes height the flex axis, and a flex item's default
  // `min-height: auto` then pins the content pane at its natural height, so
  // dsh's own scroller inside never gets a bounded height and the panel --
  // which is `overflow: hidden` -- clips the rest with no way to reach it.
  // Measured before the fix: 961px of panel inside 796px, scroller dead.
  // Whoever stacks the panel owes it this declaration.
  const rule = (needle) => {
    const at = MOBILE_CSS.indexOf(needle);
    assert.notEqual(at, -1, `no rule for ${needle}`);
    return MOBILE_CSS.slice(at, MOBILE_CSS.indexOf('}', at));
  };
  assert.ok(MOBILE_CSS.includes('flex-direction: column'), 'the panel is stacked');
  assert.ok(rule('> [class*="_content"]').includes('min-height: 0'),
    'so the content pane is let out of the min-height: auto floor');

  // A plugin may reasonably hide dsh's nav on a phone -- dshmarket ships
  // `[role=dialog]:has([data-dsh-market-root]) > nav { display: none }` at the
  // same 560px -- which is fair against a 188px column and leaves the sections
  // unreachable once the column is a strip along the top.
  assert.ok(rule(') > nav[class*="_nav"] {').includes('display: flex'),
    'the tab strip stays on screen');

  // And a flick that reaches the end must not drag the page behind the dialog.
  assert.ok(rule('[class*="_options"]').includes('overscroll-behavior: contain'),
    "dsh's scroller does not chain to the page");
  console.log('ok  the stacked dialog keeps a live scroller');
}

// --- the sidebar covers the conversation rather than squeezing it ---
{
  const rule = (needle) => {
    const at = MOBILE_CSS.indexOf(needle);
    assert.notEqual(at, -1, `no rule for ${needle}`);
    return MOBILE_CSS.slice(at, MOBILE_CSS.indexOf('}', at));
  };
  // Eight packages dsh ships have a class ending in `_frame`; one of them has a
  // `_sidebarCol` in it. Without the `:has()` these rules would land on
  // whichever of the other seven is on screen.
  const frame = '[class*="_frame"]:has(> [class*="_sidebarCol"]):not([data-sidebar-collapsed])';
  assert.ok(MOBILE_CSS.includes(frame), 'the frame is named by what is inside it');

  // The track pins to 56px -- the same width dsh's own collapsed rail already
  // uses below 1024px -- rather than to zero, so the centre column is the same
  // width whether the sidebar is collapsed or open and does not resize when the
  // sidebar is toggled.
  assert.ok(rule(`${frame} {`).includes('grid-template-columns: 56px minmax(0, 1fr) 0 !important'),
    "the sidebar's track matches the rail it already has when collapsed");
  const layer = rule(`${frame} > [class*="_sidebarCol"] {`);
  assert.ok(layer.includes('position: absolute'), 'and becomes a layer over the centre');
  // dsh's own layers: the drag strip is 11 and the overlay layer dialogs render
  // into is 20. A sidebar above that second one would cover the settings
  // dialog, which is opened from inside the sidebar.
  assert.ok(layer.includes('z-index: 12'), 'between the two layers dsh already has');

  // The rule that is easy to leave out and impossible to see coming: with the
  // sidebar out of the flow, auto-placement slides every remaining child one
  // track to the left -- so the centre would land in the 56px track the
  // sidebar just left, and the conversation would be the thing that shrank.
  assert.ok(rule(`${frame} > [class*="_centerCol"] {`).includes('grid-column: 2'),
    'the centre stays in the track it was in');
  assert.ok(rule(`${frame} > [class*="_rightbarCol"] {`).includes('grid-column: 3'));

  // dsh writes both of these as inline styles and rewrites them on every
  // render, so nothing wins here by being more specific. Two is the whole
  // budget: a third would mean something is being fought that need not be.
  assert.equal((MOBILE_CSS.match(/!important/g) || []).length, 2,
    'only the two inline styles are overridden');

  // The collapsed state is dsh's own answer to a narrow screen -- a 56px rail --
  // and nothing here touches it.
  assert.ok(!MOBILE_CSS.includes('[data-sidebar-collapsed] '), 'the rail is left alone');
  console.log('ok  the open sidebar is laid over the conversation, not beside it');
}

// --- the home-screen tags ---
{
  // iOS has never read a manifest for this: `apple-mobile-web-app-capable` is
  // what opens the home-screen window without Safari's chrome, and it is the
  // one that must not go missing. It needs no HTTPS, unlike the Service Worker
  // beside it.
  const out = homescreen('<html lang="en"><head><title>DeepSeek Harness</title></head><body></body></html>');

  assert.ok(out.includes('<link rel="manifest" href="/dsh-mobile-manifest.json">'), 'Android reads this one');
  assert.ok(out.includes('<link rel="apple-touch-icon" href="/dsh-mobile-icon.png">'),
    'or iOS uses a screenshot of the page as the icon');
  assert.ok(out.includes('name="apple-mobile-web-app-capable"'), 'deprecated, and still the only one older iOS reads');
  assert.ok(out.includes('name="mobile-web-app-capable"'), 'and the standard spelling of it');
  assert.ok(out.includes('content="black-translucent"'),
    "so dsh's own background runs under the status bar");

  // The paths are the gateway's, answered in `remote/proxy.rs` before anything
  // is forwarded. Spell either one differently on either side and the icon
  // silently stops installing, with no error anywhere -- the same two-process
  // agreement `flag()` above is half of.
  assert.ok(out.includes('/dsh-mobile-manifest.json'));
  assert.ok(out.includes('/dsh-mobile-icon.png'));

  assert.ok(out.includes('<title>DeepSeek Harness</title>'), 'nothing else is touched');
  assert.ok(out.indexOf('rel="manifest"') < out.indexOf('<title>'), 'and they land inside the head');
  console.log('ok  puts the home-screen tags on the index');
}

// --- the Service Worker registration, and both of its guards ---
{
  // What answers when the phone opens its home-screen icon and this computer
  // is off -- the failure that is otherwise a blank white window with nothing
  // in it at all. The registration is a script rather than a tag because both
  // of the conditions below have to be asked at run time.
  const out = homescreen('<html><head><title>x</title></head><body></body></html>');

  assert.ok(out.includes("navigator.serviceWorker.register('/dsh-mobile-sw.js')"),
    'the path the gateway answers');
  assert.ok(out.includes('.catch(function(){})'),
    'a registration that fails is not an error on dsh’s page');

  // `isSecureContext`: over the LAN and the tailnet this gateway is plain
  // HTTP, where a worker cannot be registered at all. Nothing here knows which
  // channel is up, and nothing here has to.
  assert.ok(out.includes('window.isSecureContext'), 'not attempted on a plain-HTTP origin');

  // `__dshRemoteCard`: the desktop's own webview, which runs the injected card
  // script before anything on the page. Loopback IS a secure context, so
  // without this the desktop window would register a worker for dsh's origin.
  assert.ok(out.includes('window.__dshRemoteCard'), 'not registered in the desktop window');

  console.log('ok  registers the offline worker, and only where it can help');
}

// --- an index this does not recognise ---
{
  // The same rule the viewport tap follows: a body with no head is returned
  // untouched rather than guessed at. This runs on the path that serves dsh's
  // index, and a plugin that can break that is a plugin that can stop the
  // window opening.
  const odd = '<html><body>no head at all</body></html>';
  assert.equal(homescreen(odd), odd, 'left alone rather than repaired');
  console.log('ok  leaves an index it does not recognise alone');
}

// --- a dsh with no webserver ---
{
  let ran = false;
  apply({ inject: () => { ran = true; } });
  assert.ok(ran, 'the entry itself still activates');
  console.log('ok  activates on a dsh whose composition has no webserver');
}

console.log('\nall plugin host checks passed');
