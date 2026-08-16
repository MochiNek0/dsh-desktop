// Stage the runtime the installer ships: a Node binary and npm, so the machine
// has something to install and run dsh with.
//
// dsh itself is *not* shipped. The installer fetches it with the npm staged
// here (see `src-tauri/installer-hooks.nsh`), and the app fetches it on first
// launch if that did not work out. What is staged here instead is the warm-up
// list, which needs a dsh tree to record against but not to ship one — that
// install goes to a temporary directory and is thrown away.
//
// Runs from `beforeBuildCommand`, and by hand as `npm run bundle:runtime`.
// Builds for the host platform only — npm resolves native optional
// dependencies against the machine doing the install, so a Windows installer
// has to be staged on Windows, a macOS one on macOS.
//
// Everything it writes lives under `src-tauri/resources/`, which is gitignored.
// It is skipped entirely when that directory already holds the versions below.

import { execFileSync, spawn } from 'node:child_process';
import { cpSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync, copyFileSync, chmodSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

/** Pinned so a build is reproducible and an update is a visible commit. */
const NODE_VERSION = '24.19.0';

/**
 * The dsh the warm-up list is recorded against. Not what the user gets — the
 * installer asks npm for the newest release at the moment they install — so
 * this only has to be close enough that the two trees put the same files in
 * the same places. A path that moved between the two is one warm-up read that
 * misses, which `src-tauri/src/warm.rs` skips.
 */
const DSH_VERSION = '0.1.0-rc.6';

/** How long the traced boot gets before it counts as broken. */
const BOOT_TIMEOUT_MS = 180_000;

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const resources = join(root, 'src-tauri', 'resources');
const stamp = join(resources, 'bundled.json');

// `dsh` is in here even though no dsh is staged: it is what the warm-up list
// below was recorded against, so bumping it has to invalidate that list.
const want = { node: NODE_VERSION, dsh: DSH_VERSION, platform: process.platform, arch: process.arch };

await main();

async function main() {
  if (isStaged()) {
    console.log(`[bundle] up to date: node ${NODE_VERSION}, dsh ${DSH_VERSION}`);
  } else {
    // `dsh` is no longer staged, but a tree an older build left there is 255 MB
    // that would otherwise be swept into the installer by the resource glob.
    // `resources/` itself is tracked (`.gitkeep`) so that glob always matches.
    for (const stale of ['runtime', 'dsh']) rmSync(join(resources, stale), { recursive: true, force: true });
    rmSync(stamp, { force: true });
    rmSync(bootSetTarget(), { force: true });
    mkdirSync(resources, { recursive: true });

    stageNode();

    writeFileSync(stamp, `${JSON.stringify(want, null, 2)}\n`);
  }

  // Kept out of the staging check: a trace that failed leaves no list, and
  // retrying it should not mean downloading Node and dsh again.
  if (!existsSync(bootSetTarget())) await traceBootSet();

  console.log('[bundle] done');
}

/** Whether a previous run already produced exactly what this one would. */
function isStaged() {
  if (!existsSync(stamp)) return false;
  // A tree from a build that still bundled dsh. The stamp matches — same node,
  // same dsh — so nothing else here would notice, and the resource glob would
  // put all 255 MB of it into the installer.
  if (existsSync(join(resources, 'dsh'))) return false;
  try {
    const have = JSON.parse(readFileSync(stamp, 'utf8'));
    return (
      Object.keys(want).every((key) => have[key] === want[key]) &&
      existsSync(nodeTarget()) &&
      existsSync(npmTarget())
    );
  } catch {
    return false;
  }
}

function nodeTarget() {
  return join(resources, 'runtime', process.platform === 'win32' ? 'node.exe' : 'node');
}

/** npm's own entry point, which the app runs to fetch newer dsh releases. */
function npmTarget() {
  return join(resources, 'runtime', 'node_modules', 'npm', 'bin', 'npm-cli.js');
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
 * Download the official Node build and keep the executable and npm. The rest of
 * the archive — headers, docs, the corepack shims — is dead weight in an
 * installer. npm has to come along because the app installs dsh updates with it
 * at runtime, and doing that through npm inherits the user's registry and proxy
 * settings instead of ignoring them.
 */
function stageNode() {
  const { archive, inner, npm } = nodeArchive();
  const url = `https://nodejs.org/dist/v${NODE_VERSION}/${archive}`;
  const scratch = mkdtempSync(join(tmpdir(), 'dsh-node-'));

  try {
    console.log(`[bundle] downloading ${url}`);
    execFileSync('curl', ['-fsSL', '-o', join(scratch, archive), url], { stdio: 'inherit' });

    console.log('[bundle] extracting node');
    execFileSync(tarBin(), ['-xf', archive], { cwd: scratch, stdio: 'inherit' });

    mkdirSync(join(resources, 'runtime'), { recursive: true });
    copyFileSync(join(scratch, inner), nodeTarget());
    if (process.platform !== 'win32') chmodSync(nodeTarget(), 0o755);

    cpSync(join(scratch, npm), join(resources, 'runtime', 'node_modules', 'npm'), { recursive: true });
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}

/**
 * Windows ships bsdtar as `System32\tar.exe`, which reads the zip Node
 * publishes. A bare `tar` would find Git for Windows' GNU tar first on a
 * developer's PATH, and that one cannot, so name the system binary outright.
 */
function tarBin() {
  return process.platform === 'win32'
    ? join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'tar.exe')
    : 'tar';
}

/** The official archive for this host, and the executable's path inside it. */
function nodeArchive() {
  const arch = { x64: 'x64', arm64: 'arm64' }[process.arch];
  if (!arch) throw new Error(`[bundle] unsupported architecture: ${process.arch}`);

  const build = { win32: `win-${arch}`, darwin: `darwin-${arch}`, linux: `linux-${arch}` }[process.platform];
  if (!build) throw new Error(`[bundle] unsupported platform: ${process.platform}`);

  const base = `node-v${NODE_VERSION}-${build}`;
  return process.platform === 'win32'
    ? { archive: `${base}.zip`, inner: join(base, 'node.exe'), npm: join(base, 'node_modules', 'npm') }
    : {
        archive: `${base}.tar.${process.platform === 'linux' ? 'xz' : 'gz'}`,
        inner: join(base, 'bin', 'node'),
        npm: join(base, 'lib', 'node_modules', 'npm'),
      };
}

/**
 * Install dsh into `dir`, the same way the installer and the app do it at
 * runtime, so that the tree this traces against is laid out like the one the
 * user will end up with.
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
  // Run npm's own entry point on the Node staged above, rather than the `npm`
  // command. On Windows that command is a .cmd shim, and Node refuses to spawn
  // one without a shell; a shell in turn means unescaped argument
  // concatenation. This has neither problem, needs no npm on the host, and is
  // the same pair the app itself installs dsh updates with.
  execFileSync(nodeTarget(), [npmTarget(), 'install', '--omit=dev', '--no-audit', '--no-fund'], {
    cwd: dir,
    stdio: 'inherit',
  });
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
    const child = spawn(nodeTarget(), ['--require', join(root, 'scripts', 'boot-trace', 'preload.cjs'), dshEntry(dir), 'web', '--port', '0'], {
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
