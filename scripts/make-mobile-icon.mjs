// Flatten the app icon for a phone's home screen.
//
// `src-tauri/icons/icon.png` is a 512px whale on a transparent ground, which is
// right everywhere it is used today — a window icon, a tray, a dock — and wrong
// in exactly one place. **iOS composites an `apple-touch-icon`'s transparency
// onto black.** The whale is nearly black, so shipping the icon as it stands
// puts a black whale on a black square on the user's home screen, which is to
// say nothing at all on the user's home screen.
//
// So it is composited here instead, onto the white it was drawn against, and
// the gateway serves the result. Android does not need this — a manifest icon
// with an alpha channel gets a background from the system — but one opaque icon
// serves both and a second file to keep in step serves nobody.
//
// Generated rather than committed, for the same reason the installer bitmaps
// are (see `.gitignore`): an image checked into a repository is a blob nobody
// reviews, and the whale it comes from is already tracked next door. Change the
// icon and this follows on the next build.
//
// Run by `npm run bundle:runtime` on every platform — unlike the installer art,
// which only Windows has any use for — and directly as
// `node scripts/make-mobile-icon.mjs`.

import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { readPng, writePng } from "./png.mjs";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

/// What the whale was drawn against, and what the home-screen tile becomes.
const BACKGROUND = [0xff, 0xff, 0xff];

/**
 * Composite over [`BACKGROUND`], dropping the alpha channel.
 *
 * `src * a + background * (1 - a)`, which is the whole of it: PNG stores
 * straight alpha rather than premultiplied, so the source channels are the
 * colour at full strength and this is the ordinary over operator.
 */
function flatten({ width, height, channels, pixels }) {
  const out = Buffer.alloc(width * height * 3);

  for (let from = 0, to = 0; from < pixels.length; from += channels, to += 3) {
    const alpha = channels === 4 ? pixels[from + 3] / 255 : 1;
    for (let c = 0; c < 3; c++) {
      out[to + c] = Math.round(pixels[from + c] * alpha + BACKGROUND[c] * (1 - alpha));
    }
  }

  return { width, height, pixels: out };
}

const source = readPng(join(root, "src-tauri", "icons", "icon.png"));
const target = join(root, "src-tauri", "resources", "mobile-icon.png");

mkdirSync(dirname(target), { recursive: true });
writeFileSync(target, writePng(flatten(source)));

console.log(`[bundle] staged mobile-icon.png (${source.width}x${source.height})`);
