// Stage what ships beside the app: the bootstrap scripts, and the warm-up list
// the app reads while dsh starts.
//
// Neither Node nor dsh is shipped any more. The machine's Node is detected, one
// is installed under the app's data directory if there is none, and then comes
// `npm install -g @deepseek-ai/dsh` — all of it in `scripts/install-deps.ps1`
// and its counterpart `scripts/install-deps.sh`, both copied into `resources/`
// here so the bundler picks them up. On Windows the installer runs the first at
// install time (see `src-tauri/installer-hooks.nsh`); everywhere else there is
// no installer hook and the app's first launch runs the second.
//
// What is left to build is the warm-up list, and that needs a dsh tree to record
// against but not to ship one — the install goes to a temporary directory and is
// thrown away. It runs against the host's own Node, so a machine building this
// needs Node on PATH; the versions do not have to match what a user ends up
// with, since all the list carries is paths.
//
// Runs from `beforeBuildCommand`, and by hand as `npm run bundle:runtime`.
//
// Everything it writes lives under `src-tauri/resources/`, which is gitignored.

import { execFileSync, spawn } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

/**
 * The dsh the warm-up list is recorded against. Not what the user gets — the
 * installer asks npm for the newest release at the moment they install — so
 * this only has to be close enough that the two trees put the same files in the
 * same places. A path that moved between the two is one warm-up read that
 * misses, which `src-tauri/src/warm.rs` skips.
 */
const DSH_VERSION = '0.1.0-rc.6';

/** How long the traced boot gets before it counts as broken. */
const BOOT_TIMEOUT_MS = 180_000;

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const resources = join(root, 'src-tauri', 'resources');
const stamp = join(resources, 'bundled.json');

const want = { dsh: DSH_VERSION };

await main();

async function main() {
  mkdirSync(resources, { recursive: true });

  // A tree from a build that still bundled Node and dsh: 300-odd MB the
  // resource glob would otherwise sweep straight into the installer.
  for (const stale of ['runtime', 'dsh']) rmSync(join(resources, stale), { recursive: true, force: true });

  // Always copied rather than checked: they are two small files, and a stale
  // copy of the thing that installs everything else is not worth the saved
  // millisecond.
  //
  // Both go into every bundle. Which one runs is decided at runtime by
  // `src-tauri/src/dsh.rs`, and a few kilobytes of the other one is cheaper
  // than a platform switch in the resource glob.
  for (const script of ['install-deps.ps1', 'install-deps.sh']) {
    copyFileSync(join(root, 'scripts', script), join(resources, script));
  }
  console.log('[bundle] staged install-deps.ps1 and install-deps.sh');

  // Kept behind its own check rather than the stamp's: a trace that failed
  // leaves no list, and retrying it should not depend on anything else.
  if (isStaged() && existsSync(bootSetTarget())) {
    console.log(`[bundle] boot set up to date: dsh ${DSH_VERSION}`);
  } else {
    rmSync(bootSetTarget(), { force: true });
    await traceBootSet();
  }

  // Rewritten whether or not the trace ran, so that a stamp left by an older
  // scheme does not keep claiming a bundled Node this no longer stages.
  if (existsSync(bootSetTarget())) writeFileSync(stamp, `${JSON.stringify(want, null, 2)}\n`);

  console.log('[bundle] done');
}

/** Whether the list already on disk was recorded against the dsh above. */
function isStaged() {
  if (!existsSync(stamp)) return false;
  try {
    const have = JSON.parse(readFileSync(stamp, 'utf8'));
    return Object.keys(want).every((key) => have[key] === want[key]);
  } catch {
    return false;
  }
}

/** The warm-up list `traceBootSet` records and src-tauri/src/warm.rs reads. */
function bootSetTarget() {
  return join(resources, 'dsh-boot-set.txt');
}

/** dsh's own entry point inside a tree `installDsh` produced. */
function dshEntry(dir) {
  return join(dir, 'node_modules', '@deepseek-ai', 'dsh', 'lib', 'bin.js');
}

/**
 * Install dsh into `dir`, laid out the way the global install on a user's
 * machine is: `node_modules/@deepseek-ai/dsh`, relative to a root. What differs
 * is only where that root is, and the list records nothing absolute.
 */
function installDsh(dir) {
  mkdirSync(dir, { recursive: true });
  writeFileSync(
    join(dir, 'package.json'),
    `${JSON.stringify(
      { name: 'dsh-bundle', version: '0.0.0', private: true, dependencies: { '@deepseek-ai/dsh': DSH_VERSION } },
      null,
      2,
    )}\n`,
  );

  console.log(`[bundle] installing @deepseek-ai/dsh@${DSH_VERSION}`);
  // npm's own entry point on the host's Node, rather than the `npm` command. On
  // Windows that command is a .cmd shim, and Node refuses to spawn one without a
  // shell; a shell in turn means unescaped argument concatenation.
  execFileSync(process.execPath, [npmCli(), 'install', '--omit=dev', '--no-audit', '--no-fund'], {
    cwd: dir,
    stdio: 'inherit',
  });
}

/**
 * npm's entry point, for whatever Node is running this script: beside the
 * binary on Windows, and one level up under `lib` everywhere else — the same
 * two layouts `dsh::root_of` and `install-deps.sh` have to cover.
 */
function npmCli() {
  const dir = dirname(process.execPath);
  const cli = [
    join(dir, 'node_modules', 'npm', 'bin', 'npm-cli.js'),
    join(dir, '..', 'lib', 'node_modules', 'npm', 'bin', 'npm-cli.js'),
  ].find((candidate) => existsSync(candidate));

  if (!cli) throw new Error(`[bundle] no npm beside ${process.execPath}`);
  return cli;
}

/**
 * Boot dsh once and write down every file it read, for the app to read again —
 * in parallel, from several threads — while dsh is starting on the user's
 * machine. See src-tauri/src/warm.rs for why that is worth doing.
 *
 * Best effort: a trace that fails costs a slow first launch, not a build. The
 * boot runs against a throwaway `$DSH_HOME` so it neither reads nor rearranges
 * the profile of whoever is doing the build.
 */
async function traceBootSet() {
  const scratch = mkdtempSync(join(tmpdir(), 'dsh-trace-'));
  const dir = join(scratch, 'dsh');
  const trace = join(scratch, 'trace.txt');
  writeFileSync(trace, '');

  console.log('[bundle] recording what a dsh boot reads');
  let recorded;
  let listed;
  try {
    installDsh(dir);
    await bootOnce(dir, trace, scratch);
    recorded = readFileSync(trace, 'utf8');
    // Walked before the cleanup below, which takes the tree it walks with it.
    listed = manifests(dir);
  } catch (error) {
    console.warn(`[bundle] boot trace failed, first launch will be slow: ${error.message}`);
    return;
  } finally {
    // Retried: the boot was killed a moment ago, and on Windows its handles can
    // outlive it just long enough to make the first attempt fail.
    rmSync(scratch, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  }

  const read = recorded
    .split('\n')
    .map((line) => line.split('\t'))
    .filter((fields) => fields.length === 2)
    .sort((left, right) => Number(left[0]) - Number(right[0]))
    .map((fields) => fields[1]);

  // Manifests first: the dependency-closure walk dsh opens with reads them, and
  // Node's resolver reads them again on the way to every module. Neither shows
  // up in the trace — the first is `readFileSync`, the second is internal.
  const files = new Set(listed.map((file) => relative(dir, file)));
  for (const file of read) {
    if (file.startsWith(dir)) files.add(relative(dir, file));
  }

  const list = [...files].map((file) => file.split('\\').join('/'));
  writeFileSync(bootSetTarget(), `${list.join('\n')}\n`);
  console.log(`[bundle] boot reads ${list.length} files`);
}

/**
 * Run `dsh web` until it says it is serving, then stop it.
 *
 * The trace is what this is for, so the port is left to the OS and the URL is
 * only read as the signal to stop.
 */
function bootOnce(dir, trace, home) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, ['--require', join(root, 'scripts', 'boot-trace', 'preload.cjs'), dshEntry(dir), 'web', '--port', '0'], {
      cwd: home,
      env: { ...process.env, DSH_HOME: join(home, 'home'), DSH_BOOT_TRACE: trace },
      stdio: ['ignore', 'pipe', 'pipe'],
    });

    let output = '';
    let settled = false;
    const settle = (error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      stop(child);
      if (error) reject(error);
      else resolve();
    };
    const timer = setTimeout(() => settle(new Error(`dsh did not start within ${BOOT_TIMEOUT_MS / 1000}s`)), BOOT_TIMEOUT_MS);

    for (const stream of [child.stdout, child.stderr]) {
      stream.setEncoding('utf8');
      stream.on('data', (chunk) => {
        output += chunk;
        if (output.includes('dsh web:')) settle();
      });
    }

    child.on('error', (error) => settle(error));
    child.on('exit', (code) => settle(new Error(`dsh exited with ${code} before serving:\n${output.trim()}`)));
  });
}

/**
 * Stop a traced boot along with the workers it spawned. `dsh web` is a server:
 * left alone it would hold the build's temporary directory open for as long as
 * the build runs.
 */
function stop(child) {
  child.removeAllListeners('exit');
  if (process.platform === 'win32') {
    try {
      execFileSync('taskkill', ['/T', '/F', '/PID', String(child.pid)], { stdio: 'ignore' });
    } catch {
      // Already gone, which is the state this wanted.
    }
  } else {
    child.kill('SIGKILL');
  }
}

/** Every package.json in the staged tree, as absolute paths. */
function manifests(dir) {
  const found = [];

  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) found.push(...manifests(path));
    else if (entry.name === 'package.json') found.push(path);
  }

  return found;
}
