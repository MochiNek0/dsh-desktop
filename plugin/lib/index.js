/**
 * dsh desktop signal plugin, node half.
 *
 * Four things, none of which can live in the browser half — the first three
 * because they have to be true before dsh's client boots, and the fourth
 * because a browser reads it out of the document it was served: the `<meta
 * name="viewport">` on dsh's index, the page global that tells dsh's connection
 * layer this page owns its host, — when the desktop says so — a small
 * stylesheet that makes dsh's settings dialog usable on a phone, and the head
 * tags that let the phone keep dsh as an icon on its home screen. All four are
 * for the phone that reaches dsh through the desktop's gateway. See `remote` in
 * the Rust half.
 *
 * ## Why they are here and not in Rust
 *
 * The gateway proxies dsh's own index, and the obvious place to edit it looks
 * like the proxy. It is not: the shipped web bundle compresses responses over
 * 1024 bytes at gzip level 1, so what passes through the proxy is a deflate
 * stream, and a text replacement on it matches nothing. Decompressing and
 * recompressing in the proxy to change one tag would be this app rewriting
 * dsh's output — which is the thing the desktop is built not to do.
 *
 * dsh's own seats for exactly this are `tapIndex` and the structured injection
 * table. Both run inside dsh, on the index body, before anything compresses it.
 *
 * ## The viewport tag, and why it replaces rather than adds
 *
 * dsh's index already carries `width=device-width, initial-scale=1`, which is
 * most of what a phone needs. A second meta element would not help: structured
 * injections render immediately after the opening head tag, which is *before*
 * dsh's own, and the later viewport tag is the one a browser takes. So this is
 * a raw tap — the escape hatch the webserver documents for markup no injection
 * row expresses — and it edits the tag dsh already wrote.
 *
 * What it deliberately does not add is `maximum-scale=1, user-scalable=no`.
 * That pair turns off pinch to zoom, iOS Safari has ignored it since iOS 10,
 * and dsh is a wall of text someone may well want to zoom into — turning it off
 * would be an accessibility regression on Android in exchange for nothing on
 * iOS.
 *
 * It also does not add `viewport-fit=cover`, which an earlier draft of this did.
 * `cover` extends the layout viewport into the display cutout and under the home
 * indicator, and it is only an improvement for a page that then pads itself back
 * out with `env(safe-area-inset-*)`. dsh's stylesheets use `safe-area-inset`
 * nowhere — not in the built bundle, not in any plugin's runtime CSS — so on a
 * notched phone `cover` would buy an edge-to-edge background and pay for it with
 * the bottom of the page sitting under the home indicator. Letterboxed is the
 * better of the two until dsh's own layout asks for the insets.
 *
 * Which leaves this tap writing, on today's dsh, the tag dsh already ships. That
 * is the honest state of it: it is here so the phone's viewport is this app's to
 * state rather than a detail of whichever dsh is installed, and the test below
 * holds the content it states.
 *
 * ## The transport global, and why the phone needs it
 *
 * dsh decides whether a page may hold host settings on the client, from the
 * page's own address — `dsh-client-connection`, where the connection handle is
 * built:
 *
 * ```js
 * isLoopback: transport?.ownsHost === true
 *          || pageLocation === undefined
 *          || isLoopbackHostname(pageLocation.hostname)
 * ```
 *
 * and then, in `dsh-client-ui-settings`, `persistence = isLoopback ? 'host' :
 * 'memory'`. A phone's address bar says `192.168.x.x`, so without this the
 * verdict is `memory`: every settings namespace reports `unavailable`, the
 * models page fails with "settings are unavailable in this browser", the plugin
 * cards render nothing, and a change made in general settings never reaches
 * disk. Rewriting `Host` at the gateway does not help — this predicate reads
 * `window.location`, which is the phone's address and nothing else.
 *
 * `globalThis.__DSH_TRANSPORT__` is the one seat dsh leaves for a shell that
 * knows better, read once at connection boot. Setting only `ownsHost` leaves
 * `fetch` and `openStream` undefined, which is the same object shape as no
 * transport at all — the RPC falls back to the page's `fetch`, and the stream
 * carrier stays absent — so the single thing this changes is that verdict, and
 * through it the two consumers that read it.
 *
 * It is set for every page dsh serves, not only the phone's. On the desktop the
 * page is already loopback, so the verdict was `true` before this row existed
 * and is `true` after it; the row is a no-op there by construction.
 *
 * This is deliberately relaxing a fence dsh put up, so it is worth being plain
 * about what is being traded. dsh's fence protects a host whose port might be
 * reachable by something unauthenticated. This gateway's is not: a request only
 * reaches dsh after clearing the trust fence, spending a one-shot pairing nonce
 * and being allowed by a human at the desktop, and a device that has done all
 * three already has shell execution on that machine. Stopping it from changing
 * a font size would not be security.
 *
 * ## The entry waits for nothing
 *
 * `inject` is empty and `webServer` is waited for one scope down, for the
 * reason set out at length at the top of `./client`: dsh's web boot audits
 * every loader entry once the loader settles, and an entry still PENDING on a
 * service is a boot failure — the whole window becomes the "Failed to load
 * plugins" card, and nothing of the user's loads because of us. A child fiber
 * is not an entry, so a dsh whose composition has no webServer simply never
 * runs the tap.
 */

import { existsSync, readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

/** Nothing. `webServer` is waited for a scope down; see above. */
export const inject = [];

/**
 * The injection row that hands dsh's connection layer its verdict.
 *
 * A `global` row rather than a `script` one because that is what the table has
 * for this: the webserver renders it as an assignment to `globalThis`, ahead of
 * every script row, and the value goes through `JSON.stringify` rather than
 * through a string this plugin concatenated.
 *
 * Exported so the test can hold the shape rather than infer it from a mock.
 */
export const TRANSPORT = {
  kind: 'global',
  name: '__DSH_TRANSPORT__',
  value: { ownsHost: true },
};

/**
 * Where the desktop says whether the stylesheet patch is wanted.
 *
 * Its presence is the whole signal; nothing reads the contents.
 *
 * Exported so the test can hold the path, which is half of a two-process
 * agreement — the Rust half pins the same two names in `remote/style.rs`. Spell
 * it differently on either side and the switch silently stops doing anything:
 * the plugin finds no file, injects nothing, and reports no error to anyone.
 */
export function flag() {
  return join(desktop(), 'mobile-css');
}

/**
 * Where a stylesheet of the user's own would be, if they wrote one.
 *
 * This exists so that fixing the patch for a newer dsh does not mean shipping a
 * new build of the desktop app: the CSS is a detail of dsh's markup, which moves
 * on dsh's release schedule, and pinning a cosmetic fix to ours is the slow way
 * round. A file here replaces the built-in stylesheet wholesale, and because the
 * rows are collected per render it applies on the next page load.
 *
 * `.css` against the switch's `mobile-css` with no extension: one is a
 * stylesheet, the other is a boolean that happens to be a file, and a directory
 * listing should say which is which.
 */
export function override() {
  return join(desktop(), 'mobile.css');
}

/**
 * The directory the desktop half and this one both mean.
 *
 * `$DSH_HOME` is dsh's own variable, read the same way on both sides, with the
 * same `~/.dsh` fallback. A blank value counts as unset, which is dsh's own rule
 * (`resolveDshHome` in `dsh-home-paths`) and matters because a blank one would
 * otherwise resolve the home to the working directory.
 */
function desktop() {
  const home = process.env.DSH_HOME;
  const root = home !== undefined && home.trim().length > 0 ? home : join(homedir(), '.dsh');
  return join(root, '.dsh-desktop');
}

/**
 * The stylesheet to inject: the user's, if they left one, else the built-in.
 *
 * Unreadable, blank, or carrying a `</style` all fall back rather than fail. The
 * row is inlined into a `<style>` element, so that last one would close the
 * element early and leave the rest of a hand-edited file loose in the document —
 * worth a cheap check, because the whole point of this file is that it gets
 * hand-edited, and pasting a `<style>…</style>` block into it is an easy slip.
 */
function sheet() {
  let own;
  try {
    own = readFileSync(override(), 'utf8');
  } catch {
    return MOBILE_CSS;
  }
  if (own.trim().length === 0 || /<\/style/i.test(own)) return MOBILE_CSS;
  return own;
}

/**
 * The rows this plugin contributes to one index render.
 *
 * Read per render rather than settled at activation, which is what the webserver
 * asks for — "fresh per call, so subscribers read live state" — and is the whole
 * reason the switch on the desktop card takes effect on the next page load
 * rather than on the next dsh restart.
 *
 * A failed `existsSync` is a `false`, never a throw: this runs on the path that
 * serves dsh's index, and a plugin that can break that is a plugin that can stop
 * the window opening.
 */
export function rows() {
  const table = [TRANSPORT];
  let wanted = false;
  try {
    wanted = existsSync(flag());
  } catch {
    wanted = false;
  }
  if (wanted) table.push({ kind: 'style', text: sheet() });
  return table;
}

/** Host plugin body. The client half is `./client`. */
export function apply(ctx) {
  ctx.inject(['webServer'], (web) => {
    web.effect(() => web.webServer.tapIndex(viewport), 'tapIndex(viewport)');
    web.effect(() => web.webServer.tapIndex(homescreen), 'tapIndex(homescreen)');
    web.on('webserver/index-inject', (table) => table.push(...rows()));
  });
}

/**
 * The stylesheet patch.
 *
 * ## What is wrong
 *
 * dsh's settings dialog is a flex row of a fixed-width `<nav>` and the pane
 * beside it. Measured in a 390px viewport: the dialog is 342px, the nav takes
 * 188px of it, and the pane is left with 154px — inside which a settings row is
 * a label and a control side by side, so the label column collapses and every
 * one of its characters lands on its own line. It is not subtle; it is the
 * reason this file has a stylesheet in it.
 *
 * Everything else dsh serves is fine. The chat screen, the trajectory, the model
 * list and the plugin inventory all ship their own narrow-width rules, and at
 * 390px they render correctly with nothing from here. This is one dialog.
 *
 * ## What it does
 *
 * Stacks that dialog: the nav becomes a horizontal strip of tabs above the pane
 * instead of a column beside it, the pane takes the dialog's full width, and the
 * close button moves into the corner the title row leaves free. dsh's own dialog
 * width, padding, rounding and scrim are left alone, so it still looks like
 * dsh's dialog rather than like this app's idea of one.
 *
 * ## Why the selectors look like that
 *
 * dsh's class names are CSS-module hashes — `VOzbGW_panel`, `hVGvvW_row` — and
 * the hash changes whenever dsh rebuilds. Naming one would mean a patch that
 * works today and silently stops working on the next dsh release, which for a
 * stylesheet means no error anywhere, just the old squeeze coming back.
 *
 * So nothing here names a hash. The anchor is the `<nav>` element, which is
 * semantic markup rather than a generated name, and `:has()` turns it into a
 * statement about structure: the panel that contains a nav. The `_suffix`
 * fragments that remain are the CSS-module *local* names, which are dsh's own
 * source identifiers and change only when dsh renames the thing itself.
 * Verified to match exactly one element while the dialog is open, and none while
 * it is shut.
 *
 * ## Why the media queries
 *
 * There are two, because there are two different things wrong with dsh on a
 * phone and they are not the same question.
 *
 * The first is width: a dialog laid out as a row does not fit. `max-width`,
 * not the `width <= 560px` range syntax dsh's own CSS uses, because the range
 * form needs Safari 16.4 and this has to work on whatever phone is to hand.
 * The breakpoint is dsh's own 560px, so a desktop window narrowed past the
 * point where dsh itself starts adapting gets the same treatment, and a normal
 * desktop window is untouched — confirmed identical, patched and unpatched, at
 * 1280px.
 *
 * The second is hover: a control dsh reveals on `:hover` has no way to be
 * revealed by a finger. Width is the wrong proxy for that. An iPad, a phone in
 * landscape and an unfolded foldable are all wider than 560px and all still
 * have no pointer to hover with, and gating these rules on width is what left
 * the workspace "+" unreachable on every one of them. `(hover: none)` is the
 * claim actually being made, and it leaves a desktop window untouched at any
 * width for the same reason the first query does.
 */
const MOBILE_CSS = [
  '@media (max-width: 560px) {',
  // The dialog: a row of [nav, pane] becomes a column of [tabs, pane].
  '[class*="_panel"]:has(> nav[class*="_nav"]) {',
  '  position: relative;',
  '  flex-direction: column;',
  '}',
  // The nav gives up its fixed width and its divider, and stays on screen.
  //
  // `display` is there because a plugin can reasonably decide this nav costs too
  // much on a phone and hide it -- dshmarket ships exactly that, and at dsh's own
  // breakpoint:
  //
  //     @media (max-width:560px) {
  //       [role=dialog]:has([data-dsh-market-root]) > nav { display: none }
  //     }
  //
  // which is a fair trade against a 188px column inside a 342px dialog, and a bad
  // one once the column is a strip along the top: the sections simply become
  // unreachable until the dialog is closed and reopened. Confirmed to be theirs
  // and not ours -- the nav goes to `display: none` with this stylesheet switched
  // off too. Specificity carries it, (0,4,2) against their (0,2,1), so nothing
  // here needs `!important`.
  '[class*="_panel"]:has(> nav[class*="_nav"]) > nav[class*="_nav"] {',
  '  display: flex;',
  '  width: auto;',
  '  flex: 0 0 auto;',
  '  border-right: 0;',
  '}',
  // Its list lies down and scrolls, so more tabs than fit stay reachable.
  '[class*="_panel"]:has(> nav[class*="_nav"]) > nav[class*="_nav"] [class*="_navList"] {',
  '  flex-direction: row;',
  '  width: auto;',
  '  overflow-x: auto;',
  '  scrollbar-width: none;',
  '}',
  // Each tab takes its own width and does not wrap mid-label.
  '[class*="_panel"]:has(> nav[class*="_nav"]) > nav[class*="_nav"] [class*="_navCell"] {',
  '  width: auto;',
  '  flex: 0 0 auto;',
  '  white-space: nowrap;',
  '}',
  // The pane gets the width the nav was holding. `min-height: 0` is the whole
  // reason the dialog can still be scrolled: dsh writes `flex: 1; min-width: 0`
  // here because it only ever lays the panel out as a row, so the vertical
  // `min-height: auto` floor never bites. Stacking makes height the flex axis,
  // the floor pins this pane at its content height, dsh's own scroller inside
  // (`_options`, which already has `min-height: 0; overflow-y: auto`) never gets
  // a bounded height, and the panel -- `overflow: hidden` -- simply clips the
  // overflow with no way to reach it. Measured: 857px of pane in a 796px panel.
  '[class*="_panel"]:has(> nav[class*="_nav"]) > [class*="_content"] {',
  '  width: auto;',
  '  min-width: 0;',
  '  min-height: 0;',
  '}',
  // And once it scrolls, keep a flick that reaches the end from dragging the
  // page behind the dialog with it.
  '[class*="_panel"]:has(> nav[class*="_nav"]) [class*="_options"] {',
  '  overscroll-behavior: contain;',
  '}',
  // Stacking strands the close button under the tabs; put it in the corner.
  '[class*="_panel"]:has(> nav[class*="_nav"]) [class*="_close"] {',
  '  position: absolute;',
  '  top: 14px;',
  '  right: 14px;',
  '  z-index: 2;',
  '}',
  '}',

  // Row actions that only exist on hover. dsh writes them as
  //
  //     .rowActions { display: none }
  //     .projectRow:hover .rowActions,
  //     .sessionRow:hover .rowActions,
  //     .projectRow.menuOpen .rowActions,
  //     .sessionRow.menuOpen .rowActions { display: inline-flex }
  //
  // and on a phone the first rule is the only one that ever applies, so the
  // "+" that creates a session in a workspace is not merely hard to reach, it
  // is not rendered. Specificity carries the override without `!important`:
  // (0,2,0) against dsh's (0,1,0) base. It does not have to beat dsh's own
  // hover rule at (0,3,0) — that one turns the same thing on.
  '@media (hover: none) {',
  '[class*="_projectRow"] [class*="_rowActions"], [class*="_sessionRow"] [class*="_rowActions"] {',
  '  display: inline-flex;',
  '}',
  // And the hero view's workspace chip, which is dsh's `min-height: 28px` and
  // `padding: 0 8px` -- a comfortable mouse target and a mean thumb one. 36px
  // rather than the 44px Apple asks for: the chip sits in a dense row under
  // the composer, and 44 pushes the hero layout around for a control that is
  // already the widest thing on that line.
  //
  // The child combinator is load-bearing. The chip is a <button> whose label
  // is a <span class="…_workspaceLabel"> inside it, so a descendant selector
  // matches both, and `padding: 0 12px` on a blockified flex item would put
  // 24px of dead space inside a chip that is already `max-width: min(100%,
  // 360px)` with an ellipsis -- paid for out of the path the user is trying
  // to read.
  '[class*="_heroWorkspaceRow"] > [class*="_workspace"] {',
  '  min-height: 36px;',
  '  padding: 0 12px;',
  '}',
  '}',
].join('\n');

export { MOBILE_CSS };

/** What the viewport tag is set to. */
const CONTENT = 'width=device-width, initial-scale=1';

/**
 * The tag dsh writes. Attribute order and quoting are dsh's to change, so this
 * matches the element rather than the string — but it is still a regular
 * expression over markup, which is why it only ever replaces a whole element it
 * has already matched and never tries to read one.
 */
const META = /<meta\s[^>]*name=["']viewport["'][^>]*>/i;

/**
 * Put the viewport tag on an index body.
 *
 * Replaces dsh's if there is one, and writes ours after the opening head tag if
 * there is not — a dsh that stopped shipping the tag is one that needs it more,
 * not less. An index with no head at all is returned untouched: `replace` with
 * no match changes nothing, which is the right answer for a body this does not
 * recognise.
 *
 * Exported so the test can hold it. It is a pure string transform, which is the
 * whole of what dsh asks a tap to be.
 */
export function viewport(html) {
  const tag = `<meta name="viewport" content="${CONTENT}">`;
  if (META.test(html)) return html.replace(META, tag);
  return html.replace(/<head(\s[^>]*)?>/i, (head) => head + tag);
}

/**
 * What turns the phone's tab into an icon on its home screen.
 *
 * ## Why this is worth having at all
 *
 * The feature exists for the user who is not at the computer. Getting to it
 * through a browser means finding the browser, finding the tab or the address,
 * and reading dsh inside a window with an address bar taking a tenth of the
 * screen. An icon removes all three. It is also the one piece of this that
 * cannot be added later without the rest: an installed icon is bound to an
 * origin, so it only means anything once the gateway's origin survives a
 * restart — which is what the fixed port and the stored signing key are for.
 *
 * ## The tags, and what actually reads each one
 *
 * `manifest` is Android's. Chrome reads the name, the icon and `standalone`
 * from it and offers to install.
 *
 * The three `apple-` tags are iOS's, which has never read a manifest for this.
 * `apple-mobile-web-app-capable` is what makes the home-screen window open
 * without Safari's chrome, and **it does not need HTTPS** — unlike a Service
 * Worker, which does. The icon is a real fetch of a real file rather than a
 * screenshot of the page, which is what iOS falls back to without it.
 * `mobile-web-app-capable` is the standard spelling of the first one; the
 * Apple-prefixed name is deprecated and is still the only one older iOS reads,
 * so both go out.
 *
 * ## The worker, and its two guards
 *
 * The last tag registers `/dsh-mobile-sw.js`, which is what answers when the
 * phone opens its home-screen icon and the computer is off — the failure that
 * is otherwise a white screen with nothing on it. See the Rust side for what
 * the worker does and, more to the point, what it refuses to cache.
 *
 * Two conditions, and both are load-bearing.
 *
 * `isSecureContext` is the browser's own answer to whether a worker may be
 * registered at all. On the LAN and on the tailnet this gateway is plain HTTP,
 * so it is false and the call is never made; over a Cloudflare tunnel it is
 * true. Nothing here has to know which channel is up.
 *
 * `__dshRemoteCard` is the desktop's own webview, which runs the injected card
 * script from `remote/card.rs` before any script on the page. Loopback *is* a
 * secure context, so without this check the desktop window would register a
 * worker for dsh's own origin — harmless but pointless, and one more thing
 * caching a page nobody there will ever be shown.
 *
 * `black-translucent` is chosen over `default` deliberately. It is the one
 * value that lets dsh's own background run under the status bar, so a dark
 * theme does not get a white strip stapled to the top of it.
 *
 * ## Why the paths are the gateway's and not dsh's
 *
 * `/dsh-mobile-manifest.json` and `/dsh-mobile-icon.png` are answered by the
 * Rust gateway before anything is forwarded — they are outside dsh's route
 * table, so nothing collides, and dsh is not asked to serve files that are
 * nothing to do with it.
 *
 * ## It goes to the desktop's page too
 *
 * Same as the transport row above, and for the same reason: this plugin sees
 * one index render and cannot tell which side asked for it. A `<link
 * rel="manifest">` in the desktop's webview is inert — Tauri's window is not a
 * browser tab and has no home screen to be added to — so the cost of not
 * distinguishing is nothing, and the alternative is threading a signal through
 * dsh for no gain.
 *
 * Exported so the test can hold it. Pure string in, string out.
 */
export function homescreen(html) {
  const tags =
    '<link rel="manifest" href="/dsh-mobile-manifest.json">' +
    '<link rel="apple-touch-icon" href="/dsh-mobile-icon.png">' +
    '<meta name="mobile-web-app-capable" content="yes">' +
    '<meta name="apple-mobile-web-app-capable" content="yes">' +
    '<meta name="apple-mobile-web-app-status-bar-style" content="black-translucent">' +
    '<script>(function(){' +
    'if(window.__dshRemoteCard)return;' +
    'if(!window.isSecureContext)return;' +
    'if(!navigator.serviceWorker)return;' +
    "navigator.serviceWorker.register('/dsh-mobile-sw.js').catch(function(){});" +
    '})();</script>';

  // An index with no head is returned untouched: `replace` with no match
  // changes nothing, which is the right answer for a body this does not
  // recognise. The same rule the viewport tap follows, for the same reason.
  return html.replace(/<head(\s[^>]*)?>/i, (head) => head + tags);
}
