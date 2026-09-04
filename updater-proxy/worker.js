// The update feed, served from a domain that is reachable from everywhere.
//
// `tauri.conf.json` lists two updater endpoints and the plugin walks them in
// order (see the loop in `tauri-plugin-updater`'s `updater.rs`), so an installed
// copy asks this Worker first and falls back to GitHub only if it gets no
// answer. That order is on purpose: the devices this exists for are the ones
// that cannot open a connection to github.com at all, and for them the fallback
// is the half that fails, not the half that saves the check.
//
// Nothing is stored here. Both routes fetch from GitHub Releases at request
// time, which is the whole point: releasing stays exactly what it was — tag,
// let `release.yml` build the draft, publish it — and this starts serving the
// new version the moment the draft is published. There is no second copy of
// `latest.json` anywhere to forget to update.
//
// Two routes, and both of them are needed:
//
//   /updater/latest.json             the feed, with the download URLs inside it
//                                    rewritten to point back here
//   /updater/download/<tag>/<file>   the installer's bytes
//
// The rewrite is the part that is easy to leave out. GitHub answers a release
// download with a 302 to objects.githubusercontent.com, which is blocked in more
// places than github.com is; a feed that is proxied but still names GitHub for
// the bytes turns "cannot check for updates" into a progress line that never
// moves, which is worse, because it looks like it is working.
//
// The signature in the feed is passed through untouched and needs nothing else:
// minisign signs the installer's bytes, not the address they arrived from, so
// the pubkey in `tauri.conf.json` verifies a download through here exactly as it
// verifies one straight from GitHub. This is a pipe, not something the app has
// to trust.
//
// Deploy from this directory with `npx wrangler deploy`.

const RELEASES = 'https://github.com/MochiNek0/dsh-desktop/releases';

/** How long the edge may hold `latest.json`. Long enough that a burst of
 *  startup checks is one request to GitHub, short enough that a release is
 *  live within minutes of being published. */
const FEED_TTL = 300;

/** What a download path is allowed to look like: a tag, a slash, a filename.
 *  Without this the second route is an open proxy to any path under github.com
 *  that a `..` can reach. */
const ARTIFACT = /^v[0-9][A-Za-z0-9._-]*\/[A-Za-z0-9._-]+$/;

async function handle(request) {
  const { pathname, origin } = new URL(request.url);

  if (request.method !== 'GET' && request.method !== 'HEAD') {
    return new Response('method not allowed', { status: 405 });
  }

  // The feed. Fetched fresh, then pointed back at this Worker.
  if (pathname === '/updater/latest.json') {
    const upstream = await fetch(`${RELEASES}/latest/download/latest.json`, {
      cf: { cacheTtl: FEED_TTL, cacheEverything: true },
    });

    // Anything but a feed is worth failing on rather than passing along: the
    // plugin reads a non-2xx as "this endpoint had nothing" and moves on to
    // GitHub, which is the right thing for it to do.
    if (!upstream.ok) return new Response('upstream unavailable', { status: 502 });

    const feed = await upstream.json();
    for (const platform of Object.values(feed.platforms ?? {})) {
      platform.url = platform.url.replace(
        `${RELEASES}/download/`,
        `${origin}/updater/download/`,
      );
    }

    return Response.json(feed, {
      headers: { 'cache-control': `public, max-age=${FEED_TTL}` },
    });
  }

  // The installer. `fetch` follows GitHub's 302 itself, so the device never has
  // to resolve objects.githubusercontent.com.
  if (pathname.startsWith('/updater/download/')) {
    const artifact = pathname.slice('/updater/download/'.length);
    if (!ARTIFACT.test(artifact)) {
      return new Response('not found', { status: 404 });
    }
    return fetch(`${RELEASES}/download/${artifact}`);
  }

  return new Response('not found', { status: 404 });
}

// Deployed as a Worker on a route.
export default { fetch: handle };

// Or dropped into the site as a Pages Function, if that is where
// dsh-desktop.cc.cd is served from: this file becomes `functions/updater/
// [[path]].js` and wrangler.jsonc beside it is unused.
export const onRequest = (context) => handle(context.request);
