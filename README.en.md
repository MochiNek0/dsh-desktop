# dsh desktop

> A Tauri desktop client for DeepSeek Harness (`dsh web`) — the dsh UI, in a native window instead of a browser tab.
>
> [中文](README.md) · English
>
> **Unofficial**: a third-party desktop wrapper around DeepSeek Harness, not affiliated with DeepSeek. See the [disclaimer](#disclaimer).

Launching the app starts a local `dsh web` server and loads its UI into a native window. No terminal, no port numbers, no tab management — and sessions, credentials, and settings are shared with the CLI, not stored separately.

## Features

- **Native window for dsh web**: starts the local server and navigates into it automatically, no manual browser setup
- **Self-contained**: the installer ships a Node runtime and `@deepseek-ai/dsh`, so it runs on a machine with neither installed
- **dsh stays current**: checks npm in the background and asks before downloading anything — no traffic behind your back, and the bundled copy is always there as a working floor
- **No port conflicts**: starts with `--port 0` so the OS assigns a free loopback port — it won't clash with a `dsh web` (3080) you're running by hand, and both can coexist
- **Your session stays put**: the window never leaves the dsh server's origin; links to the outside world open in the system browser
- **Clear boot & failure UX**: a loading page while the server starts; if `dsh` is missing or exits early, the error and its output tail are shown in-window
- **Clean lifecycle**: a normal exit kills the whole child process tree; a Windows Job Object backs that up when the app is force-killed, so no orphaned node processes are left behind either way
- **Window memory & tray**: window geometry is remembered; closing the window parks the app in the tray instead of interrupting the session, and quitting goes through the tray menu
- **Auto-update**: signed updates via the Tauri updater, asking before both the download and the restart
- **Cross-platform**: verified on Windows; macOS / Linux work by the same design
- **Data shared with the CLI**: sessions, credentials, and settings stay in `$DSH_HOME` (default `~/.dsh`) — the desktop app stores nothing extra

## Prerequisites

- **To run the installer**: nothing. Node and dsh are in the package. On Windows you also need the WebView2 runtime (built into Win11; the installer can bootstrap it)
- **To build from source**: Rust stable + MSVC toolchain, Node 18+

## Run & build

```sh
npm install
npm run dev            # dev mode, with devtools (no bundled runtime; uses dsh on PATH)
npm run bundle:runtime # stage the bundled runtime on its own (the build runs it for you)
npm run build          # produces installers → src-tauri/target/release/bundle/
```

`npm run build` runs `scripts/bundle-runtime.mjs` first: it downloads the official Node binary and
installs `@deepseek-ai/dsh` into `src-tauri/resources/` before handing off to Tauri. It is skipped
when the staged versions already match. That step builds for the **host platform only** — npm
resolves native optional dependencies against the machine doing the install, so a Windows installer
has to be built on Windows and a macOS one on macOS.

The bundle target is currently Windows NSIS; macOS (app/dmg) and Linux (deb/AppImage) targets can be added on top of `tauri.conf.json`.

## Releasing & updates

Auto-update uses the Tauri updater: the app checks the endpoint configured in `tauri.conf.json` in
the background at startup, and the tray menu's "检查更新…" triggers a check by hand. To ship a
version that others can update to:

1. The signing private key lives at `~/.tauri/dsh-desktop.key` (**not in the repo** — lose it and you
   can no longer sign updates). Its public key is already in `plugins.updater.pubkey`.
2. Pass the key at build time:

   ```sh
   TAURI_SIGNING_PRIVATE_KEY_PATH=~/.tauri/dsh-desktop.key npm run build
   ```

   The key was generated without a password; to add one, generate a fresh pair and update the public
   key in the config.
3. Upload the installer, its `.sig`, and a `latest.json` to a GitHub Release so that
   `releases/latest/download/latest.json` resolves.

## How it works

1. Finds dsh in the order `DSH_BIN` → the app-managed dsh (bundled or downloaded, whichever is newer) → `dsh` on PATH, then starts `dsh web --port 0` with the user's home directory as the working directory, letting the OS pick a free loopback port — so it never fights your manually run `dsh web` (3080) for the port, and both can be up at once.
2. Reads the child's stdout, waiting for the line `dsh web: http://127.0.0.1:<port>` — that line is both the readiness signal and the URL to load. The window shows a loading page meanwhile; if startup fails or dsh exits early, the page shows the tail of its output.
3. Once the URL is known, the window navigates to it. The window stays within that origin; links pointing outside are handed to the system browser so your session is never replaced.
4. On exit, the entire child process tree is killed (`taskkill /T` — killing only the parent would orphan node). The child is also placed in a Windows Job Object with `KILL_ON_JOB_CLOSE` from the start: handles are closed by the kernel however a process dies, so the tree goes down with the app even when it is force-killed and no cleanup code ever runs.

The working directory is just the initial default — pick the real project directory in the UI with the directory picker.

### The two dsh installations

The app manages two copies of dsh and runs whichever has the higher version:

| Location | Where it came from |
| --- | --- |
| `<install dir>/resources/dsh/` | Shipped in the installer. Always present, always works, never changes — the floor for offline first launches and for every failure path |
| `%LOCALAPPDATA%\ai.deepseek.dsh.desktop\dsh\` | Downloaded later because npm had something newer |

After startup it runs `npm view @deepseek-ai/dsh version` in the background — through npm rather than
a request of our own, so the user's `.npmrc` still applies and private registries and corporate
proxies keep working. If something newer exists, a dialog names the version and the download size and
**waits for consent**. The install lands in `dsh.next/` and is swapped in on the **next launch**: a
running server cannot be hot-replaced, and startup is the one moment nothing holds the directory
open, which is what makes the rename safe on Windows.

"Skip this version" records the version in `dsh-skipped` and stops asking about **that** release;
a later one asks again. A 255 MB prompt on every single launch is how an app gets uninstalled.

Any failure just leaves the bundled copy in charge. `DSH_BIN` still has the highest precedence and
bypasses all of this.

### The bundled runtime and profile dependencies

The bundled dsh is installed under `resources/dsh/` and launched with the bundled
`resources/runtime/node`. dsh itself links its dependency closure into
`$DSH_HOME/profiles/node_modules` as junctions on every boot, and **re-points links whose
installation moved** — so running the bundled dsh points the profile's dependencies at the install
directory on its own, with no extra wiring and no network on first launch. Running the system CLI
afterwards points them back.

## Environment variables

| Variable | Purpose |
| --- | --- |
| `DSH_BIN` | Full path to the dsh executable. Highest precedence — use it to override the bundled runtime and run your own dsh instead. |

## Project layout

```
dist/index.html            Loading / error page (no build step; Rust drives it via two eval'd hooks)
scripts/bundle-runtime.mjs Stages the bundled node runtime and dsh before a build
src-tauri/src/main.rs      Window, navigation policy, tray, lifecycle
src-tauri/src/server.rs    Managed dsh web child process and the job object backstop
src-tauri/src/dsh.rs       Choosing between the two dsh installs, version comparison, runtime updates
src-tauri/src/update.rs    The app's own auto-update
```

`cargo test` covers URL-line parsing.

## Roadmap / TODO

- [x] **Self-contained installer** — the Node runtime and `@deepseek-ai/dsh` now ship as bundled resources.
- [x] **Job Object fallback** — force-killing the app no longer orphans node processes.
- [x] **Window size/position memory, tray, auto-update**
- [ ] **macOS / Linux bundle targets** — not configured yet; the staging script is already cross-platform
      but has to run on the target platform.
- [ ] **Release pipeline** — `latest.json` and the signed artifacts are uploaded to a Release by hand;
      there is no CI workflow yet.

## Known limitations

- Self-containment has a price: the bundled resources unpack to roughly 350 MB (node 93 MB + dsh's
  dependency tree at 255 MB across 33k files), which NSIS compresses into a ~55 MB installer
- Only Windows has been systematically verified so far; macOS / Linux bundle targets are not configured yet
- Installers must be built on the platform they target: npm resolves native optional dependencies against the machine doing the install
- The node version is pinned in `scripts/bundle-runtime.mjs`, so bumping it means a code change and a new release. The **bundled** dsh version is pinned there too, but the app catches up to npm's latest at runtime, so that pin only decides the floor
- A dsh update downloads ~255 MB and coexists with the bundled copy, peaking around 600 MB on disk
- Uninstalling removes the install directory and the bundled runtime with it, but leaves `$DSH_HOME` (`~/.dsh`) alone by design — sessions, credentials, and settings are shared with the CLI and shouldn't leave with the desktop app. The side effect is that junctions in `~/.dsh/profiles/node_modules` pointing into the install directory go dangling; running any dsh installation once (the system CLI, or the desktop app reinstalled) re-points them automatically, so no manual cleanup is needed
- The job object is attached just after the child starts, leaving a very short window in which a grandchild would escape it; the ordinary exit path covers the whole tree regardless

## Disclaimer

This is a **third-party** desktop client for [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness),
maintained by an individual developer. It is not affiliated with, sponsored by, or endorsed by DeepSeek,
and it is **not an official DeepSeek product**. It only does desktop packaging — process management,
window and tray, system integration — and changes nothing about what dsh itself does.

"DeepSeek" is a trademark of DeepSeek; this project uses the name nominatively, only to describe what
it runs. The app icon is this project's own artwork and uses none of DeepSeek's marks.

Please file issues about this project here, not with DeepSeek.

## License

[MIT](LICENSE) © 2026 MochiNek0
