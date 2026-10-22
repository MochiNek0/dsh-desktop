// The update feed, served from a domain that is reachable from everywhere.
//
// `tauri.conf.json` lists two updater endpoints and the plugin walks them in
// order (see the loop in `tauri-plugin-updater`'s `updater.rs`), so an installed
// copy asks this Worker first and falls back to GitHub only if it gets no
// answer. That order is on purpose: the devices this exists for are the ones
// that cannot open a connection to github.com at all, and for them the fallback
// is the half that fails, not the half that saves the check.
//
// ── Why the bytes moved off the marketing host ─────────────────────────────
//
// This Worker used to do both halves on `dsh-desktop.cc.cd`: hand out the feed
// and stream the installers from `/updater/download/...`. That made the site
// itself a download host, and Google's Safe Browsing report said exactly that —
// "these URLs host malicious or unwanted software" — with no sample URL to
// argue with. Deleting the download links from the pages changed nothing while
// this route kept answering 200 with an `.exe`, so the review kept failing.
//
// There are now two hostnames and one Worker:
//
//   dsh-desktop.cc.cd/updater/*    the feed, and nothing but the feed
//   dl.dsh-desktop.cc.cd/*         the installer bytes
//
// The marketing host never returns an installer again: the legacy download
// path answers 410 instead of streaming, or even redirecting. A redirect was
// the softer option, but a 302 is still "this site points at the download" in a
// security report — just under a different heading — and the point is to be out
// of that chain entirely.
//
// `DOWNLOAD_ORIGIN` decides where the feed points. It is set in wrangler.jsonc;
// leave it empty and the feed names GitHub directly and this Worker serves no
// bytes at all. Empty is the safe setting: it can only make the bytes travel
// further, never make this host serve them again.
//
// Nothing is stored here. Both halves fetch from GitHub Releases at request
// time, so releasing stays exactly what it was — tag, let `release.yml` build
// the draft, publish it — and the new version is served the moment the draft is
// published. There is no second copy of `latest.json` anywhere to forget.
//
// The signature in the feed passes through untouched: minisign signs the
// installer's bytes, not the address they arrived from, so the pubkey in
// `tauri.conf.json` verifies a download through either host exactly as it
// verifies one straight from GitHub.
//
// Deploy from this directory with `npx wrangler deploy`; see wrangler.jsonc for
// the DNS record the download host needs first.

const RELEASES = 'https://github.com/MochiNek0/dsh-desktop/releases';

/** GitHub's release-download prefix. Everything in the feed starts with it. */
const GITHUB_ARTIFACTS = `${RELEASES}/download/`;

/** How long the edge may hold `latest.json`. Long enough that a burst of
 *  startup checks is one request to GitHub, short enough that a release is
 *  live within minutes of being published. */
const FEED_TTL = 300;

/** What an artifact path may look like: `v<tag>/<filename>`.
 *  Without this the download host is an open proxy to any path under
 *  github.com that a `..` can reach. */
const ARTIFACT = /^v[0-9][A-Za-z0-9._-]*\/[A-Za-z0-9._-]+$/;

/** The path the site used to stream installers from. Kept only so it can answer
 *  410 with an explanation instead of the site's HTML 404 page. */
const LEGACY_DOWNLOAD = '/updater/download/';

/** Neither the feed nor the download host is a page; keep them out of indexes
 *  that ignore robots.txt. `static/robots.txt` on the site says the same. */
const NOINDEX = { 'x-robots-tag': 'noindex, nofollow' };

/** Used only when the Worker runs without the wrangler var (a dashboard
 *  deploy, where `env` is empty). */
const DEFAULT_DOWNLOAD_ORIGIN = 'https://dl.dsh-desktop.cc.cd';

function downloadOrigin(env) {
  const value =
    env && env.DOWNLOAD_ORIGIN !== undefined ? env.DOWNLOAD_ORIGIN : DEFAULT_DOWNLOAD_ORIGIN;
  return String(value).trim().replace(/\/+$/, '');
}

/** Where the feed's `url`s point: the download host when one is configured,
 *  GitHub otherwise. */
function artifactBase(env) {
  const origin = downloadOrigin(env);
  return origin ? `${origin}/` : GITHUB_ARTIFACTS;
}

/** True for requests that arrived on the download host, not on the site. */
function isDownloadHost(url, env) {
  const origin = downloadOrigin(env);
  if (!origin) return false;
  try {
    return new URL(origin).host === url.host;
  } catch {
    // A malformed DOWNLOAD_ORIGIN must not turn every host into a download
    // host, which would put the bytes back on the marketing domain.
    return false;
  }
}

async function handleFeed(env) {
  const upstream = await fetch(`${RELEASES}/latest/download/latest.json`, {
    cf: { cacheTtl: FEED_TTL, cacheEverything: true }
  });

  // Anything but a feed is worth failing on rather than passing along: the
  // plugin reads a non-2xx as "this endpoint had nothing" and moves on to
  // GitHub, which is the right thing for it to do.
  if (!upstream.ok) return new Response('upstream unavailable', { status: 502 });

  const feed = await upstream.json();
  const base = artifactBase(env);

  for (const platform of Object.values(feed.platforms ?? {})) {
    if (!platform || typeof platform.url !== 'string') continue;
    // Leave anything that is not a GitHub release asset alone: a change in the
    // upstream feed should not let this Worker rewrite an arbitrary URL.
    if (!platform.url.startsWith(GITHUB_ARTIFACTS)) continue;
    platform.url = base + platform.url.slice(GITHUB_ARTIFACTS.length);
  }

  return Response.json(feed, {
    headers: { 'cache-control': `public, max-age=${FEED_TTL}`, ...NOINDEX }
  });
}

/** Stream an installer from GitHub. Only ever reached on the download host. */
async function handleArtifact(request, artifact) {
  if (!ARTIFACT.test(artifact)) {
    return new Response('not found', { status: 404, headers: NOINDEX });
  }

  // Range is passed through so a resumed download still works, and HEAD stays
  // HEAD. GitHub's 302 to objects.githubusercontent.com is followed here, so
  // the device never has to resolve that host itself — it is blocked in more
  // places than github.com is.
  const headers = new Headers();
  const range = request.headers.get('range');
  if (range) headers.set('range', range);

  const upstream = await fetch(GITHUB_ARTIFACTS + artifact, {
    method: request.method === 'HEAD' ? 'HEAD' : 'GET',
    headers,
    redirect: 'follow'
  });

  const out = new Headers(upstream.headers);
  out.set('x-robots-tag', 'noindex, nofollow');
  return new Response(upstream.body, {
    status: upstream.status,
    statusText: upstream.statusText,
    headers: out
  });
}

async function handle(request, env) {
  const url = new URL(request.url);

  if (request.method !== 'GET' && request.method !== 'HEAD') {
    return new Response('method not allowed', { status: 405 });
  }

  if (isDownloadHost(url, env)) {
    const artifact = url.pathname.startsWith(LEGACY_DOWNLOAD)
      ? url.pathname.slice(LEGACY_DOWNLOAD.length)
      : url.pathname.slice(1);
    return handleArtifact(request, artifact);
  }

  if (url.pathname === '/updater/latest.json') return handleFeed(env);

  if (url.pathname.startsWith(LEGACY_DOWNLOAD)) {
    // Deliberately not a redirect. The site has to stop appearing in the
    // download chain, and a 302 still points at the file.
    return new Response(
      'The installer is no longer served from this host. Updates go through ' +
        'the download host named in /updater/latest.json; the current release ' +
        'is at https://github.com/MochiNek0/dsh-desktop/releases/latest.',
      {
        status: 410,
        headers: {
          'content-type': 'text/plain; charset=utf-8',
          'cache-control': 'no-store',
          ...NOINDEX
        }
      }
    );
  }

  return new Response('not found', { status: 404, headers: NOINDEX });
}

// Deployed as a Worker on two routes (see wrangler.jsonc).
export default { fetch: handle };

// Or dropped into the site as a Pages Function, if that is where the feed is
// served from: this file becomes `functions/updater/[[path]].js` and
// wrangler.jsonc beside it is unused. Only the feed half applies there — there
// is no download host in a Pages deployment, so DOWNLOAD_ORIGIN must be empty
// and the bytes come from GitHub.
export const onRequest = (context) => handle(context.request, context.env ?? {});
