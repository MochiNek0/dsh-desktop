// Reading and writing the one PNG shape this repository's build scripts need.
//
// Both callers start from `src-tauri/icons/icon.png` and neither is allowed a
// dependency: `make-installer-art.mjs` draws the NSIS bitmaps from it, and
// `make-mobile-icon.mjs` flattens it for a phone's home screen. The reader was
// written for the first of those and lived there; it is here because the second
// one needs the same sixty lines, and two copies of a PNG filter loop is two
// places for the same bug.
//
// Deliberately narrow. This is not a PNG library: it reads 8-bit truecolour,
// interlaced never, and it writes the same without alpha. Anything else throws
// rather than guessing, because the failure this is avoiding is a build that
// silently produces a wrong image — see `save` in `make-installer-art.mjs` for
// the same reasoning applied to the bitmaps.

import { readFileSync } from "node:fs";
import { deflateSync, inflateSync } from "node:zlib";

/**
 * Read a PNG into `{ width, height, channels, pixels }`.
 *
 * `pixels` is unfiltered, tightly packed, 8 bits a channel: RGB when the file
 * has no alpha and RGBA when it has.
 */
export function readPng(path) {
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
 * CRC-32 over a chunk, as the PNG specification defines it (Annex D).
 *
 * Written out rather than taken from `zlib.crc32`, which arrived in Node 20.12.
 * Ten lines against a version floor a build script would otherwise carry for no
 * other reason.
 */
const TABLE = (() => {
  const table = new Int32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c;
  }
  return table;
})();

function crc(bytes) {
  let c = -1;
  for (const byte of bytes) c = TABLE[(c ^ byte) & 0xff] ^ (c >>> 8);
  return (c ^ -1) >>> 0;
}

/** One length-type-body-CRC chunk. */
function chunk(type, body) {
  const head = Buffer.alloc(8);
  head.writeUInt32BE(body.length, 0);
  head.write(type, 4, "ascii");

  const tail = Buffer.alloc(4);
  tail.writeUInt32BE(crc(Buffer.concat([head.subarray(4), body])), 0);

  return Buffer.concat([head, body, tail]);
}

/**
 * Write an opaque 8-bit truecolour PNG from tightly packed RGB.
 *
 * Every row goes out under filter 0 — stored as-is, with deflate doing all of
 * the work. A filter chooser would shave a few kilobytes off an image this
 * repository writes once per build and never transmits, which is effort spent
 * in the wrong place.
 */
export function writePng({ width, height, pixels }) {
  if (pixels.length !== width * height * 3) {
    throw new Error(`expected ${width * height * 3} bytes of RGB, got ${pixels.length}`);
  }

  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8; // bits per channel
  ihdr[9] = 2; // colour type: truecolour, no alpha
  // The last three — compression, filter method and interlace — are all zero,
  // which is the only combination the format actually defines.

  const stride = width * 3;
  const raw = Buffer.alloc(height * (stride + 1));
  for (let y = 0; y < height; y++) {
    raw[y * (stride + 1)] = 0;
    pixels.copy(raw, y * (stride + 1) + 1, y * stride, (y + 1) * stride);
  }

  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}
