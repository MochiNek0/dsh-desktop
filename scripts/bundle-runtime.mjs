// Stage what ships beside the app: the two bootstrap scripts, and nothing else.
//
// Neither Node nor dsh is shipped. The machine's Node is detected, one is
// installed under the app's data directory if there is none, and then comes
// `npm install -g @deepseek-ai/dsh` — all of it in `scripts/install-deps.ps1`
// and its counterpart `scripts/install-deps.sh`, both copied into `resources/`
// here so the bundler picks them up. On Windows the installer runs the first at
// install time (see `src-tauri/installer-hooks.nsh`); everywhere else there is
// no installer hook and the app's first launch runs the second.
//
// One more file ships from that directory without passing through here:
// `preset-plugins.json`, which is source rather than staged output and is
// tracked in place — see the exception in `.gitignore`. Nothing below touches
// it; the deletions are all by name.
//
// Runs from `beforeBuildCommand`, and by hand as `npm run bundle:runtime`. It
// copies two files and never touches the network.
//
// Everything it writes lives under `src-tauri/resources/`, which is gitignored.

import { copyFileSync, mkdirSync, rmSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const resources = join(root, 'src-tauri', 'resources');

mkdirSync(resources, { recursive: true });

// Left behind by builds that staged more than this one does: a bundled Node and
// dsh, which was 300-odd MB, and the boot warm-up list with the version stamp
// that went with it. The resource glob is `resources/**/*`, so anything still
// sitting here would be swept straight into the installer.
for (const stale of ['runtime', 'dsh']) {
  rmSync(join(resources, stale), { recursive: true, force: true });
}
for (const stale of ['bundled.json', 'dsh-boot-set.txt']) {
  rmSync(join(resources, stale), { force: true });
}

// Always copied rather than checked: they are two small files, and a stale copy
// of the thing that installs everything else is not worth the saved millisecond.
//
// Both go into every bundle. Which one runs is decided at runtime by
// `src-tauri/src/dsh.rs`, and a few kilobytes of the other one is cheaper than a
// platform switch in the resource glob.
for (const script of ['install-deps.ps1', 'install-deps.sh']) {
  copyFileSync(join(root, 'scripts', script), join(resources, script));
}
console.log('[bundle] staged install-deps.ps1 and install-deps.sh');
