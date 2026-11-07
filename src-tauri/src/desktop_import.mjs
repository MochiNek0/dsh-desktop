// Bring the official DeepSeek Harness Desktop's settings and plugins into the
// `web` profile this app runs. See desktop_import.rs for when and why; this is
// the half that reads YAML, which Rust here has no parser for.
//
//   node - <plan|apply> <dsh package dir> <dsh home>
//
// Fed on stdin by the app. Prints one JSON object on stdout:
//   { added, conflicts, skipped, plugins, unreachable, missingKeys }
// `apply` also writes the merged `profiles/web/cordis.patch.yml`; the caller has
// already backed it up.
//
// The merge only ever adds. A setting the web profile already has keeps its
// value; a differing value from Desktop is reported as a conflict, not taken.
// Maps merge by key, lists of `{ id: … }` maps (a provider's `models`) by id,
// and anything else is one value.

import { createRequire } from 'node:module';
import { existsSync, readFileSync, renameSync, writeFileSync } from 'node:fs';
import { isAbsolute, join, resolve } from 'node:path';
import { isDeepStrictEqual, parseEnv } from 'node:util';

const [mode, dshDir, home] = process.argv.slice(2);
if ((mode !== 'plan' && mode !== 'apply') || !dshDir || !home) {
  process.stderr.write('usage: node - <plan|apply> <dsh package dir> <dsh home>\n');
  process.exit(2);
}

// dsh's own YAML library, the one its config editor writes this file with:
// comments, `!!js` expressions and key order all survive the round trip.
const { parseDocument, isAlias, isMap, isScalar, isSeq, visit } = createRequire(
  join(dshDir, 'package.json'),
)('yaml');

const JS_TAG = { tag: 'tag:yaml.org,2002:js', resolve: (value) => value };
const desktopDir = join(home, 'profiles', 'desktop');
const webDir = join(home, 'profiles', 'web');
const webPatch = join(webDir, 'cordis.patch.yml');

function readText(path, fallback) {
  try {
    return readFileSync(path, 'utf8');
  } catch (error) {
    if (error.code === 'ENOENT') return fallback;
    throw error;
  }
}

function parse(path) {
  const document = parseDocument(readText(path, '[]\n'), { customTags: [JS_TAG] });
  if (document.errors[0] !== undefined) throw new Error(`${path}: ${document.errors[0].message}`);
  if (document.contents === null) document.contents = document.createNode([]);
  if (!isSeq(document.contents)) throw new Error(`${path}: not a YAML list`);
  let aliased = false;
  visit(document, { Alias() { aliased = true; return visit.BREAK; } });
  // An alias points into its own document; copied into another it points at
  // nothing. Never seen in a profile patch, so refused rather than resolved.
  if (aliased) throw new Error(`${path}: uses YAML aliases (*name), which this cannot copy`);
  return document;
}

function keyOf(node) {
  return isScalar(node) ? String(node.value) : String(node);
}

/** A node as data, with the tag kept so `!!js 'x'` and `'x'` differ. */
function plain(node) {
  if (node === null || node === undefined) return null;
  if (isScalar(node)) return node.tag ? { tag: node.tag, value: node.value } : node.value;
  if (isMap(node)) return node.items.map((pair) => [keyOf(pair.key), plain(pair.value)]);
  if (isSeq(node)) return node.items.map(plain);
  return String(node);
}

function shown(node) {
  const text = node === null || node === undefined ? '(空)' : String(node).trim();
  return text.length > 80 ? `${text.slice(0, 77)}...` : text;
}

function idOf(node) {
  if (!isMap(node)) return undefined;
  const id = node.get('id', true);
  return isScalar(id) && (typeof id.value === 'string' || typeof id.value === 'number')
    ? String(id.value)
    : undefined;
}

function mergeNode(web, desktop, path, out) {
  if (isMap(web) && isMap(desktop)) {
    for (const pair of desktop.items) {
      const key = keyOf(pair.key);
      const there = web.items.find((item) => keyOf(item.key) === key);
      if (there === undefined) {
        web.items.push(pair.clone());
        out.added.push([...path, key]);
      } else {
        mergeNode(there.value, pair.value, [...path, key], out);
      }
    }
    return;
  }

  const byId = (seq) => seq.items.length > 0 && seq.items.every((item) => idOf(item) !== undefined);
  if (isSeq(web) && isSeq(desktop) && byId(desktop) && (web.items.length === 0 || byId(web))) {
    for (const item of desktop.items) {
      const id = idOf(item);
      const there = web.items.find((candidate) => idOf(candidate) === id);
      if (there === undefined) {
        web.items.push(item.clone());
        out.added.push([...path, id]);
      } else {
        mergeNode(there, item, [...path, id], out);
      }
    }
    return;
  }

  if (!isDeepStrictEqual(plain(web), plain(desktop))) {
    out.conflicts.push({ path, kept: shown(web), ignored: shown(desktop) });
  }
}

/** The web row a Desktop row lands on: the last plain row with its id, as dsh's editor picks. */
function rowFor(web, row) {
  const id = idOf(row);
  const name = row.get('name');
  for (let at = web.items.length - 1; at >= 0; at--) {
    const candidate = web.items[at];
    if (idOf(candidate) !== id || candidate.has('insert')) continue;
    const there = candidate.get('name');
    if (name === undefined || there === undefined || name === there) return candidate;
  }
  return undefined;
}

function mergeSettings(out) {
  const desktop = parse(join(desktopDir, 'cordis.patch.yml'));
  const web = parse(webPatch);
  web.contents.flow = false;

  for (const row of desktop.contents.items) {
    const id = idOf(row);
    // An `insert` row adds plugins to the tree rather than configuring one, and
    // Desktop's can name Electron-only ones. Not a setting; left behind.
    if (id === undefined || row.has('insert')) {
      out.skipped.push(id ?? shown(row));
      continue;
    }
    const there = rowFor(web.contents, row);
    if (there === undefined) {
      web.contents.items.push(row.clone());
      out.added.push([id]);
    } else {
      mergeNode(there, row, [id], out);
    }
  }

  out.missingKeys = missingKeys(desktop);
  return web;
}

/** `apiKeyEnv` names Desktop's providers read that nothing here can answer. */
function missingKeys(desktop) {
  const wanted = new Set();
  for (const row of desktop.contents.items) {
    const providers = isMap(row) && row.getIn(['config', 'providers']);
    if (!isMap(providers)) continue;
    for (const pair of providers.items) {
      const name = isMap(pair.value) && pair.value.get('apiKeyEnv');
      if (typeof name === 'string' && name) wanted.add(name);
    }
  }
  if (wanted.size === 0) return [];

  const env = parseEnv(readText(join(home, '.env'), ''));
  let stored = new Set();
  const credentials = parseDocument(readText(join(home, '.credentials.yaml'), ''));
  const refs = credentials.errors.length === 0 ? credentials.get('refs') : undefined;
  if (isMap(refs)) stored = new Set(refs.items.map((pair) => keyOf(pair.key)));

  return [...wanted].filter((name) => !process.env[name] && !env[name] && !stored.has(name));
}

/** Desktop's plugins the web profile does not have, as specs `dsh plugin add` takes. */
function plugins(out) {
  const read = (dir) => {
    const text = readText(join(dir, 'package.json'), undefined);
    return text === undefined ? {} : JSON.parse(text).dependencies ?? {};
  };
  const have = read(webDir);
  out.plugins = [];
  out.unreachable = [];

  for (const [name, range] of Object.entries(read(desktopDir))) {
    if (name in have || typeof range !== 'string') continue;
    const local = /^(link|file):(.*)$/.exec(range);
    if (local) {
      // Relative to Desktop's profile, which is not where the install runs.
      const target = isAbsolute(local[2]) ? local[2] : resolve(desktopDir, local[2]);
      if (existsSync(target)) out.plugins.push(`${local[1]}:${target}`);
      else out.unreachable.push(name);
    } else if (range.startsWith('workspace:')) {
      out.unreachable.push(name);
    } else if (/^[a-z+]+:/.test(range) && !range.startsWith('npm:')) {
      out.plugins.push(range); // github:owner/repo, git+https://…, a tarball URL
    } else {
      out.plugins.push(`${name}@${range}`);
    }
  }
}

try {
  const out = { added: [], conflicts: [], skipped: [] };
  const web = mergeSettings(out);
  plugins(out);

  if (mode === 'apply' && out.added.length > 0) {
    // Through a sibling and a rename, and private, as dsh writes it.
    const sibling = `${webPatch}.new`;
    writeFileSync(sibling, String(web), { mode: 0o600 });
    renameSync(sibling, webPatch);
  }

  process.stdout.write(JSON.stringify(out));
} catch (error) {
  process.stderr.write(String(error?.message ?? error));
  process.exit(1);
}
