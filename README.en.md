# dsh desktop

> A Tauri desktop client for DeepSeek Harness (`dsh web`) — the dsh UI, in a native window instead of a browser tab.
>
> [中文](README.md) · English
>
> **Unofficial**: a third-party desktop wrapper around DeepSeek Harness, not affiliated with DeepSeek. See the [disclaimer](#disclaimer).

<br>

![dsh desktop](docs/thumbnail-en.png)

<br>

Launching the app starts a local `dsh web` server and loads its UI into a native window. No terminal, no port numbers, no tab management — and sessions, credentials, and settings are shared with the CLI (all in `$DSH_HOME`, `~/.dsh` by default), not stored separately.

## Features

- **Works out of the box**: the installer ships a Node runtime and uses it to install `@deepseek-ai/dsh` into your user directory during setup, falling back through mirrors if the default registry is unreachable
- **No port conflicts**: starts with `--port 0` so the OS assigns a free loopback port — a `dsh web` (3080) you run by hand can stay up alongside it
- **Frameless window**: minimise, maximise and close are drawn into the top-left of the page as macOS's three dots (top-right belongs to dsh's own controls), translucent at rest and showing their glyphs on hover
- **`dsh` in your terminal too**: after installing, plain `dsh` runs the copy the app manages. Only installed on a machine that has no dsh of its own — it never elbows aside one you installed yourself
- **dsh stays current**: checks npm at startup and asks before downloading anything; a dsh you installed yourself is reported on, never modified
- **Your session stays put**: the window never leaves the dsh server's origin; links to the outside world open in the system browser
- **Tray & login item**: closing the window parks the app in the tray instead of interrupting the session; the tray menu can add a login item, and a launch that comes from it waits quietly in the tray without dialogs or update checks
- **Single instance, clean exit**: one per machine; exiting kills the whole child process tree, with a Windows Job Object backing that up when the app is force-killed, so no orphaned node processes are left behind
- **Auto-update**: signed updates via the Tauri updater, asking before both the download and the restart

## Installing

Grab an installer from [Releases](../../releases). You need a working internet connection — setup fetches dsh from npm (about 185 MB, a few minutes). Node is in the package; on Windows you also need the WebView2 runtime (built into Win11, and downloaded by the installer when it is missing).

Windows only for now; macOS / Linux bundle targets are not configured yet.

## Development

Requires Rust stable + MSVC toolchain and Node 18+.

```sh
npm install
npm run dev            # dev mode, with devtools (no bundled runtime; uses dsh on PATH)
npm run build          # produces installers → src-tauri/target/release/bundle/
```

`npm run build` runs `scripts/bundle-runtime.mjs` first to download the Node binary into `src-tauri/resources/`.
It builds for the **host platform only** — npm resolves native optional dependencies against the machine doing the install, so a Windows installer has to be built on Windows.

To ship a version others can auto-update to, pass the signing key (at `~/.tauri/dsh-desktop.key`, **not in the repo**):

```sh
TAURI_SIGNING_PRIVATE_KEY=~/.tauri/dsh-desktop.key TAURI_SIGNING_PRIVATE_KEY_PASSWORD= npm run build
```

> **Do not transcribe that line into PowerShell** — `$env:X = ""` *deletes* the variable there, so the password never reaches the CLI and the build hangs on its prompt. Use Git Bash.
>
> Quit the running app and any installer left open from a previous build first; both hold files open, and the failure reads a lot like success.

Then upload the installer, its `.sig`, and a `latest.json` to a GitHub Release.

## How it works

1. Builds the window and shows a loading page, then checks for a newer dsh in the background — the one moment it is safe to replace that directory is while no process holds it open.
2. Finds dsh in the order `DSH_BIN` → the app-managed copy → `dsh` on PATH, then starts `dsh web --port 0`.
3. Reads its stdout, waiting for `dsh web: http://127.0.0.1:<port>` — both the readiness signal and the URL to load. If startup fails, the loading page shows the tail of its output.
4. Navigates the window to that URL and keeps it within that origin.
5. On exit, kills the entire child process tree with `taskkill /T`; the child is put in a Windows Job Object with `KILL_ON_JOB_CLOSE` as soon as it starts, which takes the tree down even when the app is force-killed and no cleanup code runs.

The window's light/dark follows dsh's own theme setting (`ui-theme.preference` in `$DSH_HOME/settings.yaml`), read once at window creation, so the loading page never flashes the opposite colour first.

The window buttons do not use Tauri IPC — that would grant IPC to every line of JavaScript running inside dsh's pages. A press navigates to a custom scheme (`dsh-window://close`) which `on_navigation` recognises, acts on, and cancels.

### Where dsh lives

One copy, at `%LOCALAPPDATA%\ai.deepseek.dsh.desktop\dsh\` — not `resources/`, which an app update would overwrite. The installer does not carry dsh; it installs it there during setup.

**If `dsh.cmd` is already on PATH, setup skips this entirely.** There is no point downloading 327 MB alongside a working dsh, and that copy stays yours: the update check reports on it but never writes to it, and the uninstaller leaves it alone.

The update check runs at most once every six hours (15 s timeout). A newer version brings up a dialog naming the version and size; taking it downloads into `dsh.next/`, swaps it in on the next launch, and deletes the old tree in the background. "Skip this version" stops asking about that release.

Uninstalling removes the dsh the app installed and the `dsh` command it put in your terminal, then **asks once** whether to delete `$DSH_HOME` too, defaulting to keeping it. App updates and manual reinstalls do not trigger that question.

## Environment variables

| Variable | Purpose |
| --- | --- |
| `DSH_BIN` | Full path to the dsh executable. Highest precedence — overrides the app-managed copy. |
| `DSH_HOME` | dsh's data directory, `~/.dsh` by default. |

## Project layout

```
dist/index.html               Loading / error page (no build step; Rust drives it via eval'd hooks)
scripts/bundle-runtime.mjs    Stages the node runtime and records the boot warm-up list
scripts/boot-trace/           Traces one dsh boot to find out what it reads
src-tauri/installer-hooks.nsh Handling of dsh and $DSH_HOME at install / uninstall
src-tauri/src/main.rs         Window, navigation policy, tray, lifecycle
src-tauri/src/controls.rs     The frameless window's injected buttons and drag strip
src-tauri/src/theme.rs        Reads dsh's light/dark preference, once, at window creation
src-tauri/src/server.rs       Managed dsh web child process and the job object backstop
src-tauri/src/dsh.rs          Locating the dsh install, version comparison, runtime updates
src-tauri/src/warm.rs         Parallel pre-reads from the warm-up list, against Defender's first scan
src-tauri/src/update.rs       The app's own auto-update
```

## Roadmap

- [x] No Node needed on the machine
- [x] Job Object fallback, tray and login item, auto-update
- [ ] macOS / Linux bundle targets
- [ ] Release pipeline (`latest.json` and signed artifacts are uploaded by hand today)

## Known limitations

- **Installing needs the network, and it is not quick**: dsh's dependency tree is 587 packages, 185 MB compressed, 327 MB unpacked across 33k files — about four minutes on a 2 MB/s link. If every mirror fails the installer says so plainly rather than pretending it succeeded
- **The first launch is slower than the rest**: the files dsh imports get scanned one by one the first time they are read from a path Defender has never seen — measured at 14 s cold against 1.6 s once the same files are warm. The app pre-reads them from several threads at startup, off a list recorded at build time, which brings the cold launch to around 4 s. To avoid it entirely, exclude the install directory (elevated PowerShell, **with the path you actually chose**):

  ```powershell
  Add-MpPreference -ExclusionPath "$env:LOCALAPPDATA\dsh-desktop", "$env:LOCALAPPDATA\ai.deepseek.dsh.desktop"
  ```

- Only Windows has been verified so far; installers must be built on the platform they target
- A dsh update downloads 185 MB and coexists with the outgoing copy while it lands, peaking around 650 MB on disk
- `dsh.cmd` forwards arguments with `%*`, so arguments containing `&`, `|` or `^` are mangled by cmd's second pass when called from PowerShell (the Git Bash shim is unaffected)
- If you keep `$DSH_HOME` at uninstall, junctions in it pointing at the removed tree go dangling; running any dsh once re-points them automatically

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
