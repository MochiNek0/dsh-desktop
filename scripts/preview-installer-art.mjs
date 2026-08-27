// Turn the installer BMPs into PNGs so a human can look at them.
//
// A development aid, not part of any build: `make-installer-art.mjs` writes BMP
// because that is what NSIS reads, and nothing renders a BMP inline. This
// converts, scaling up by whole pixels so the whale's grid stays crisp.
//
//   node scripts/preview-installer-art.mjs [scale]
//
// Writes into `target/art/`, which is gitignored.

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { deflateSync } from "node:zlib";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const from = join(root, "src-tauri", "installer");
const to = join(root, "target", "art");
const scale = Number(process.argv[2] ?? 3);

/** Read the 24-bit BMPs written by make-installer-art.mjs. */
function readBmp(file) {
  const buffer = readFileSync(file);
  const start = buffer.readUInt32LE(10);
  const width = buffer.readInt32LE(18);
  const height = buffer.readInt32LE(22);
  const stride = Math.ceil((width * 3) / 4) * 4;
  const rgb = Buffer.alloc(width * height * 3);

  for (let y = 0; y < height; y++) {
    // BMP rows run bottom-up.
    const row = start + (height - 1 - y) * stride;
    for (let x = 0; x < width; x++) {
      const at = (y * width + x) * 3;
      rgb[at] = buffer[row + x * 3 + 2];
      rgb[at + 1] = buffer[row + x * 3 + 1];
      rgb[at + 2] = buffer[row + x * 3];
    }
  }
  return { width, height, rgb };
}

function crc(buffer) {
  let value = ~0;
  for (const byte of buffer) {
    value ^= byte;
    for (let bit = 0; bit < 8; bit++) {
      value = (value >>> 1) ^ (0xedb88320 & -(value & 1));
    }
  }
  return ~value >>> 0;
}

function chunk(type, body) {
  const head = Buffer.alloc(8);
  head.writeUInt32BE(body.length, 0);
  head.write(type, 4);
  const tail = Buffer.alloc(4);
  tail.writeUInt32BE(crc(Buffer.concat([Buffer.from(type), body])), 0);
  return Buffer.concat([head, body, tail]);
}

function writePng(file, { width, height, rgb }, factor) {
  const w = width * factor;
  const h = height * factor;
  // One filter byte per row, then the row itself.
  const raw = Buffer.alloc(h * (1 + w * 3));

  for (let y = 0; y < h; y++) {
    const line = y * (1 + w * 3);
    raw[line] = 0;
    for (let x = 0; x < w; x++) {
      const at = (Math.floor(y / factor) * width + Math.floor(x / factor)) * 3;
      raw[line + 1 + x * 3] = rgb[at];
      raw[line + 1 + x * 3 + 1] = rgb[at + 1];
      raw[line + 1 + x * 3 + 2] = rgb[at + 2];
    }
  }

  const header = Buffer.alloc(13);
  header.writeUInt32BE(w, 0);
  header.writeUInt32BE(h, 4);
  header[8] = 8; // bit depth
  header[9] = 2; // truecolour
  writeFileSync(
    file,
    Buffer.concat([
      Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
      chunk("IHDR", header),
      chunk("IDAT", deflateSync(raw)),
      chunk("IEND", Buffer.alloc(0)),
    ])
  );
}

mkdirSync(to, { recursive: true });
for (const name of ["installer-sidebar", "installer-header"]) {
  const image = readBmp(join(from, `${name}.bmp`));
  writePng(join(to, `${name}.png`), image, scale);
  console.log(`${name}: ${image.width}x${image.height} -> ${to}\\${name}.png (x${scale})`);
}
