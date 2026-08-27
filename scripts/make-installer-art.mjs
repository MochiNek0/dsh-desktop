// Draw the installer's two bitmaps.
//
// NSIS wants BMP, at two sizes it will not negotiate: a 164x314 sidebar down
// the left of the welcome and finish pages, and a 150x57 strip in the header of
// every page between them. Anything else is ignored or drawn wrong, and the
// format has to be uncompressed BGR — NSIS does not read PNG.
//
// They are generated rather than committed as binaries for the ordinary reason:
// a checked-in .bmp is 150 KB of bytes nobody can review, and changing the
// accent colour would mean opening an image editor. Here the palette is three
// constants shared with the app, and the whale is the same pixel grid as the
// icon.
//
// Written into `src-tauri/installer/`, not into `resources/`: the resource glob
// is `resources/**/*`, so a bitmap left there would be copied into the
// installation directory as though the app needed it at runtime. These are
// consumed by the installer while it is being built and never shipped.
//
// The directory is gitignored, so the images are built rather than committed.
// Run by `npm run bundle:runtime` — which `beforeBuildCommand` calls, so every
// bundle has them — and directly as `node scripts/make-installer-art.mjs`.

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { inflateSync } from "node:zlib";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const out = join(root, "src-tauri", "installer");

// Gitignored, so it is absent on a fresh clone and on CI.
mkdirSync(out, { recursive: true });

// The loading page's palette; see `dist/index.html`. The whale's own colours
// are not here — they come from the icon, decoded below.
const ACCENT = [0x4d, 0x6b, 0xfe];
const MUTED = [0x6b, 0x72, 0x80];

/** A canvas of solid white, addressed in RGB. */
function canvas(width, height) {
  const pixels = new Uint8Array(width * height * 3).fill(0xff);
  return {
    width,
    height,
    set(x, y, [r, g, b], alpha = 1) {
      if (x < 0 || y < 0 || x >= width || y >= height) return;
      const at = (y * width + x) * 3;
      // Straight source-over onto what is already there, so the soft edges
      // below do not need a second buffer.
      pixels[at] = Math.round(pixels[at] * (1 - alpha) + r * alpha);
      pixels[at + 1] = Math.round(pixels[at + 1] * (1 - alpha) + g * alpha);
      pixels[at + 2] = Math.round(pixels[at + 2] * (1 - alpha) + b * alpha);
    },
    get(x, y) {
      const at = (y * width + x) * 3;
      return [pixels[at], pixels[at + 1], pixels[at + 2]];
    },
  };
}

/**
 * A 24-bit BMP.
 *
 * Rows are bottom-up and padded to a multiple of four bytes, and the channel
 * order is BGR — three details that are the whole reason this is written by
 * hand rather than with a library.
 */
function bmp(image) {
  const stride = Math.ceil((image.width * 3) / 4) * 4;
  const body = stride * image.height;
  const buffer = Buffer.alloc(54 + body);

  buffer.write("BM", 0);
  buffer.writeUInt32LE(54 + body, 2);
  buffer.writeUInt32LE(54, 10); // where the pixels start
  buffer.writeUInt32LE(40, 14); // BITMAPINFOHEADER
  buffer.writeInt32LE(image.width, 18);
  buffer.writeInt32LE(image.height, 22);
  buffer.writeUInt16LE(1, 26); // planes
  buffer.writeUInt16LE(24, 28); // bits per pixel
  buffer.writeUInt32LE(body, 34);
  // 2835 = 72 DPI in pixels per metre, which is what image tools expect to see.
  buffer.writeInt32LE(2835, 38);
  buffer.writeInt32LE(2835, 42);

  for (let y = 0; y < image.height; y++) {
    const row = 54 + (image.height - 1 - y) * stride;
    for (let x = 0; x < image.width; x++) {
      const [r, g, b] = image.get(x, y);
      buffer[row + x * 3] = b;
      buffer[row + x * 3 + 1] = g;
      buffer[row + x * 3 + 2] = r;
    }
  }
  return buffer;
}

/** The accent, washed over the top of the sidebar the way the loading page does. */
function glow(image, cx, cy, radius, colour, strength) {
  for (let y = 0; y < image.height; y++) {
    for (let x = 0; x < image.width; x++) {
      const dx = (x - cx) / radius;
      const dy = (y - cy) / radius;
      const distance = Math.sqrt(dx * dx + dy * dy);
      if (distance >= 1) continue;
      // Smoothstep, so the edge of the wash has no visible rim.
      const fade = 1 - distance * distance * (3 - 2 * distance);
      image.set(x, y, colour, fade * strength);
    }
  }
}

/**
 * The app icon, decoded.
 *
 * The whale is drawn once, in `src-tauri/icons`, and that is the copy used
 * here: its colours, its antialiasing and its proportions, rather than a second
 * whale transcribed into this file that would drift from it the moment the icon
 * changed. `icon.png` is the largest of them, so scaling it down loses nothing.
 *
 * Just enough PNG for the files in that directory: 8-bit truecolour with or
 * without alpha, uncompressed row filters, no interlacing. Anything else throws
 * rather than drawing something wrong.
 */
function readPng(path) {
  const buffer = readFileSync(path);
  let at = 8;
  let width = 0;
  let height = 0;
  let channels = 0;
  const parts = [];

  while (at < buffer.length) {
    const length = buffer.readUInt32BE(at);
    const type = buffer.toString("ascii", at + 4, at + 8);
    const body = buffer.subarray(at + 8, at + 8 + length);

    if (type === "IHDR") {
      width = body.readUInt32BE(0);
      height = body.readUInt32BE(4);
      const depth = body[8];
      const colour = body[9];
      if (depth !== 8 || (colour !== 2 && colour !== 6)) {
        throw new Error(`${path}: unsupported PNG (depth ${depth}, colour ${colour})`);
      }
      if (body[12] !== 0) throw new Error(`${path}: interlaced PNG`);
      channels = colour === 6 ? 4 : 3;
    } else if (type === "IDAT") {
      parts.push(body);
    } else if (type === "IEND") {
      break;
    }
    at += 12 + length;
  }

  const raw = inflateSync(Buffer.concat(parts));
  const stride = width * channels;
  const pixels = Buffer.alloc(height * stride);

  // Undo the per-row filters; see the PNG specification, section 9.
  for (let y = 0; y < height; y++) {
    const filter = raw[y * (stride + 1)];
    const line = raw.subarray(y * (stride + 1) + 1, y * (stride + 1) + 1 + stride);
    for (let x = 0; x < stride; x++) {
      const a = x >= channels ? pixels[y * stride + x - channels] : 0;
      const b = y > 0 ? pixels[(y - 1) * stride + x] : 0;
      const c = x >= channels && y > 0 ? pixels[(y - 1) * stride + x - channels] : 0;
      let value = line[x];
      if (filter === 1) value += a;
      else if (filter === 2) value += b;
      else if (filter === 3) value += (a + b) >> 1;
      else if (filter === 4) {
        const p = a + b - c;
        const pa = Math.abs(p - a);
        const pb = Math.abs(p - b);
        const pc = Math.abs(p - c);
        value += pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
      }
      pixels[y * stride + x] = value & 0xff;
    }
  }

  return { width, height, channels, pixels };
}

/**
 * Draw the icon into `image`, `size` pixels square.
 *
 * Box-filtered down from the source rather than sampled, so the pixel art keeps
 * its edges instead of shimmering. The icon is drawn on white, so near-white is
 * treated as background and skipped: the sidebar has a wash behind it, and a
 * white square around the whale would read as a card sitting on top of it.
 */
function icon(image, source, left, top, size) {
  const { width, height, channels, pixels } = source;
  const step = width / size;

  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      let r = 0;
      let g = 0;
      let b = 0;
      let alpha = 0;
      let count = 0;

      // Every source pixel this destination pixel covers.
      for (let sy = Math.floor(y * step); sy < Math.min(height, Math.ceil((y + 1) * step)); sy++) {
        for (let sx = Math.floor(x * step); sx < Math.min(width, Math.ceil((x + 1) * step)); sx++) {
          const at = (sy * width + sx) * channels;
          const a = channels === 4 ? pixels[at + 3] / 255 : 1;
          const light = (pixels[at] + pixels[at + 1] + pixels[at + 2]) / 3;
          const blue = pixels[at + 2] - (pixels[at] + pixels[at + 1]) / 2;
          // Near-white and not the pale blue of the spout: background.
          const background = light > 236 && blue < 24;
          const weight = background ? 0 : a;

          r += pixels[at] * weight;
          g += pixels[at + 1] * weight;
          b += pixels[at + 2] * weight;
          alpha += weight;
          count++;
        }
      }

      if (!count || alpha <= 0) continue;
      image.set(left + x, top + y, [r / alpha, g / alpha, b / alpha], alpha / count);
    }
  }
}

/**
 * Write one bitmap, then read it back and check it is the thing NSIS needs.
 *
 * Worth doing because the obvious check is not: NSIS compiles happily against a
 * PNG that has been renamed `.bmp`, and the mistake surfaces as a blank panel
 * in a shipped installer rather than as a failed build. So the file is verified
 * here, where a wrong header can still be caught.
 */
function save(name, image) {
  const file = join(out, name);
  writeFileSync(file, bmp(image));

  const written = readFileSync(file);
  const stride = Math.ceil((image.width * 3) / 4) * 4;
  const problems = [];

  if (written.toString("ascii", 0, 2) !== "BM") problems.push("not a BMP: missing the BM signature");
  if (written.readUInt32LE(14) !== 40) problems.push("not a BITMAPINFOHEADER");
  if (written.readUInt16LE(28) !== 24) problems.push(`${written.readUInt16LE(28)} bits per pixel, want 24`);
  if (written.readUInt32LE(30) !== 0) problems.push("compressed; NSIS wants BI_RGB");
  if (written.readInt32LE(18) !== image.width) problems.push("width in the header does not match");
  // Positive height means bottom-up rows, which is the order NSIS expects.
  if (written.readInt32LE(22) !== image.height) problems.push("height in the header does not match");
  if (written.length !== 54 + stride * image.height) problems.push("wrong length for its own dimensions");

  if (problems.length) {
    throw new Error(`${name}: ${problems.join("; ")}`);
  }
  return `${name} (${image.width}x${image.height})`;
}

const mark = readPng(join(root, "src-tauri", "icons", "icon.png"));

// ------------------------------------------------------------- the sidebar --
// 164x314, down the left of the welcome and finish pages.
const side = canvas(164, 314);
glow(side, 82, 110, 150, ACCENT, 0.16);
// A second, tighter wash centred on the whale, so it sits in something rather
// than floating.
glow(side, 82, 120, 76, ACCENT, 0.1);
icon(side, mark, 30, 76, 104);
// The accent rule along the bottom, echoing the loading page's progress bar.
for (let x = 22; x < 142; x++) {
  for (let y = 268; y < 270; y++) side.set(x, y, ACCENT, 0.85);
}
for (let x = 22; x < 142; x++) {
  for (let y = 276; y < 277; y++) side.set(x, y, MUTED, 0.25);
}
const sidebar = save("installer-sidebar.bmp", side);

// -------------------------------------------------------------- the header --
// 150x57, top right of every page between welcome and finish. NSIS draws it on
// the header's own white, so this stays white and carries just the mark.
const head = canvas(150, 57);
glow(head, 112, 28, 58, ACCENT, 0.1);
icon(head, mark, 96, 8, 42);
// A hairline between the mark and the page's own title text, which NSIS draws
// to the left of this bitmap.
for (let y = 14; y < 43; y++) head.set(86, y, ACCENT, 0.45);
const header = save("installer-header.bmp", head);

console.log(`installer art: ${sidebar}, ${header}`);


