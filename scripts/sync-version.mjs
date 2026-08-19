// Carry the version in `package.json` across to the three other places that
// hold a copy of it.
//
// There are four in total — `package.json`, `src-tauri/tauri.conf.json`,
// `src-tauri/Cargo.toml`, and the `dsh-desktop` entry in `src-tauri/Cargo.lock`
// — and they all have to agree. The one in `tauri.conf.json` is what the
// updater compares against the release feed, so a bump that misses it ships an
// installer that offers itself as an update forever.
//
// Runs from npm's `version` lifecycle script, which fires after `npm version
// <patch|minor|major>` has written `package.json` and before it commits, so the
// four move in one commit and the tag lands on all of them:
//
//   npm version patch      # -> 0.1.4 everywhere, committed and tagged
//   git push --follow-tags # -> the tag starts the release workflow
//
// No dependency does this: it is one field in three files, and a version
// bumper that has to be installed to bump a version is a strange thing to add
// to a project that has two dependencies in its package.json.

import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const version = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8')).version;

if (!/^\d+\.\d+\.\d+/.test(version)) {
  console.error(`[version] package.json has no usable version: ${version}`);
  process.exit(1);
}

/** Replace the first match of `pattern` in `file`, or fail loudly. */
function rewrite(file, pattern, replacement) {
  const path = join(root, file);
  const before = readFileSync(path, 'utf8');
  const after = before.replace(pattern, replacement);

  if (after === before) {
    // Either the field moved or it already said this. Both are worth stopping
    // for: the point of this script is that nothing is left behind.
    if (!pattern.test(before)) {
      console.error(`[version] could not find the version field in ${file}`);
      process.exit(1);
    }
    return false;
  }

  writeFileSync(path, after);
  return true;
}

// The bundler's copy, and the one the updater compares against the feed.
rewrite('src-tauri/tauri.conf.json', /"version": "[^"]+"/, `"version": "${version}"`);

// `[package]` is the first table in the file, so its `version` is the first one.
rewrite('src-tauri/Cargo.toml', /^version = "[^"]+"$/m, `version = "${version}"`);

// The lock would otherwise be rewritten by the next build, after the tag.
rewrite(
  'src-tauri/Cargo.lock',
  /(name = "dsh-desktop"\nversion = )"[^"]+"/,
  `$1"${version}"`,
);

console.log(`[version] ${version}`);
