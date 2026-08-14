# dsh desktop

> A Tauri desktop client for DeepSeek Harness (`dsh web`) — the dsh UI, in a native window instead of a browser tab.
>
> [中文](README.md) · English

Launching the app starts a local `dsh web` server and loads its UI into a native window. No terminal, no port numbers, no tab management — and sessions, credentials, and settings are shared with the CLI, not stored separately.

## Features

- **Native window for dsh web**: starts the local server and navigates into it automatically, no manual browser setup
- **No port conflicts**: starts with `--port 0` so the OS assigns a free loopback port — it won't clash with a `dsh web` (3080) you're running by hand, and both can coexist
- **Your session stays put**: the window never leaves the dsh server's origin; links to the outside world open in the system browser
- **Clear boot & failure UX**: a loading page while the server starts; if `dsh` is missing or exits early, the error and its output tail are shown in-window
- **Clean lifecycle**: the whole child process tree is terminated on exit, no orphaned node processes
- **Cross-platform**: verified on Windows; macOS / Linux work by the same design
- **Data shared with the CLI**: sessions, credentials, and settings stay in `$DSH_HOME` (default `~/.dsh`) — the desktop app stores nothing extra

## Prerequisites

- `dsh` installed (`dsh --version` works) and on the PATH; the GUI session's PATH often differs from your terminal's, so if it can't be found, point `DSH_BIN` at the full path to the dsh executable
- Windows: WebView2 runtime (built into Win11; the installer can also bootstrap it)
- To build from source: Rust stable + MSVC toolchain, Node 18+

## Run & build

```sh
npm install
npm run dev     # dev mode, with devtools
npm run build   # produces installers → src-tauri/target/release/bundle/
```

The bundle target is currently Windows NSIS; macOS (app/dmg) and Linux (deb/AppImage) targets can be added on top of `tauri.conf.json`.

## How it works

1. Starts `dsh web --port 0` with the user's home directory as the working directory, letting the OS pick a free loopback port — so it never fights your manually run `dsh web` (3080) for the port, and both can be up at once.
2. Reads the child's stdout, waiting for the line `dsh web: http://127.0.0.1:<port>` — that line is both the readiness signal and the URL to load. The window shows a loading page meanwhile; if startup fails or dsh exits early, the page shows the tail of its output.
3. Once the URL is known, the window navigates to it. The window stays within that origin; links pointing outside are handed to the system browser so your session is never replaced.
4. On exit, the entire child process tree is killed (on Windows that's `cmd.exe` → `dsh.cmd` → node, hence `taskkill /T` — killing only the parent would orphan node).

The working directory is just the initial default — pick the real project directory in the UI with the directory picker.

## Environment variables

| Variable | Purpose |
| --- | --- |
| `DSH_BIN` | Full path to the dsh executable. Fallback for when `dsh` is not on the GUI session's PATH. |

## Project layout

```
dist/index.html        Loading / error page (no build step; Rust drives it via two eval'd hooks)
src-tauri/src/main.rs   Window, navigation policy, lifecycle
src-tauri/src/server.rs Managed dsh web child process
```

`cargo test` covers URL-line parsing.

## Roadmap / TODO

In priority order:

- [ ] **Self-contained installer** — the app currently depends on a system-installed `dsh`; the installer doesn't bundle Node or profile dependencies.
      To run without Node installed, the node runtime + `@deepseek-ai/dsh` + the profile's `node_modules` must ship as a sidecar —
      another order of magnitude in size and complexity (cross-platform binaries, version/path management, update coordination, …).
- [ ] **Job Object fallback** — a clean exit already `taskkill /T`s the whole tree, but when the user force-kills the app
      (`taskkill /F` without `/T`) no cleanup runs and orphaned node processes are left behind.
      Plan: attach the child to a Windows Job Object so the kernel kills the tree with the app (~30 lines of unsafe FFI).
- [ ] **Window size/position memory, tray, auto-update** — remember window geometry, live in the system tray,
      and auto-update via the Tauri updater.

## Known limitations

- Requires a system-installed `dsh`; the self-contained installer is on the [roadmap](#roadmap--todo)
- Only Windows has been systematically verified so far; macOS / Linux bundle targets are not configured yet

## License

[MIT](LICENSE) © 2026 MochiNek0
