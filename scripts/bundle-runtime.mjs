// Stage the runtime the installer ships: a Node binary and a pre-installed
// `@deepseek-ai/dsh`, so the app has something to run on a machine that has
// neither.
//
// Runs from `beforeBuildCommand`, and by hand as `npm run bundle:runtime`.
// Builds for the host platform only — npm resolves native optional
// dependencies against the machine doing the install, so a Windows installer
// has to be staged on Windows, a macOS one on macOS.
//
// Everything it writes lives under `src-tauri/resources/`, which is gitignored.
// It is skipped entirely when that directory already holds the versions below.

import { execFileSync } from 'node:child_process';
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync, copyFileSync, chmodSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

/** Pinned so a build is reproducible and an update is a visible commit. */
const NODE_VERSION = '24.19.0';
const DSH_VERSION = '0.1.0-rc.6';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const resources = join(root, 'src-tauri', 'resources');
const stamp = join(resources, 'bundled.json');

const want = { node: NODE_VERSION, dsh: DSH_VERSION, platform: process.platform, arch: process.arch };

main();

function main() {
  if (isStaged()) {
    console.log(`[bundle] up to date: node ${NODE_VERSION}, dsh ${DSH_VERSION}`);
    return;
  }

  // Only the two staged trees — `resources/` itself is tracked (`.gitkeep`) so
  // that Tauri's resource glob always matches something.
  for (const stale of ['runtime', 'dsh']) rmSync(join(resources, stale), { recursive: true, force: true });
  rmSync(stamp, { force: true });
  mkdirSync(resources, { recursive: true });

  stageNode();
  stageDsh();

  writeFileSync(stamp, `${JSON.stringify(want, null, 2)}\n`);
  console.log('[bundle] done');
}

/** Whether a previous run already produced exactly what this one would. */
function isStaged() {
  if (!existsSync(stamp)) return false;
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
 * Install dsh into its own tree. The CLI links its dependency closure into
 * `$DSH_HOME/profiles/node_modules` on every boot and re-points links that
 * moved, so this tree becomes the profile's resolution source as soon as the
 * app runs it — no extra wiring, and no network on first launch.
 */
function stageDsh() {
  const dir = join(resources, 'dsh');
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
