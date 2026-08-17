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

- **Works out of the box**: Windows does it during setup and macOS / Linux on the first launch — the machine's Node.js is detected, one is installed into your user directory if there is none (or it is older than 22.22.3), and then comes `npm install -g @deepseek-ai/dsh`, falling back through mirrors if the default registry is unreachable. No administrator rights needed at any point
- **No port conflicts**: starts with `--port 0` so the OS assigns a free loopback port — a `dsh web` (3080) you run by hand can stay up alongside it
- **Frameless window**: minimise, maximise and close are drawn into the top-left of the page as macOS's three dots (top-right belongs to dsh's own controls), translucent at rest and showing their glyphs on hover
- **`dsh` in your terminal too**: after installing, plain `dsh` runs the copy the app manages. Only installed on a machine that has no dsh of its own — it never elbows aside one you installed yourself
- **dsh stays current**: checks npm at startup and asks before downloading anything; a dsh you installed yourself is reported on, never modified
- **Your session stays put**: the window never leaves the dsh server's origin; links to the outside world open in the system browser
- **Tray & login item**: closing the window parks the app in the tray instead of interrupting the session; the tray menu can add a login item, and a launch that comes from it waits quietly in the tray without dialogs or update checks
- **Single instance, clean exit**: one per machine; exiting kills the whole child process tree — `taskkill /T` on Windows, the whole process group on macOS and Linux — with a Windows Job Object backing that up when the app is force-killed, so no orphaned node processes are left behind
- **Auto-update**: signed updates via the Tauri updater, asking before both the download and the restart, off one `latest.json` covering all three platforms

## Installing

Grab a package for your platform from [Releases](../../releases): a `.exe` installer on Windows, a `.dmg` on macOS (one build, both Apple Silicon and Intel), a `.deb` or an `.AppImage` on Linux.

You need a working internet connection — setup fetches dsh from npm (about 185 MB, a few minutes), plus a Node runtime (about 36 MB) if the machine does not already have one. On Windows you also need the WebView2 runtime (built into Win11, and downloaded by the installer when it is missing); on Linux, WebKitGTK 4.1 and libayatana-appindicator (declared as dependencies by the `.deb`, your own problem with the AppImage).

The macOS build is **neither signed nor notarised** — that takes a paid Apple developer account — so Gatekeeper stops the first launch. Right-click the icon and choose Open, or:

```sh
xattr -dr com.apple.quarantine /Applications/dsh-desktop.app
```

## Development

Requires Rust stable, Node 18+, and the platform's own build dependencies: the MSVC toolchain on Windows, the Xcode command line tools on macOS, and `libwebkit2gtk-4.1-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev` and `patchelf` on Linux.

```sh
npm install
npm run dev            # dev mode, with devtools (uses dsh on PATH; installs one on first launch if absent)
npm run build          # produces packages → src-tauri/target/release/bundle/
```

The bundle targets are split by platform across `tauri.conf.json` (Windows, NSIS), `tauri.macos.conf.json` (app + dmg) and `tauri.linux.conf.json` (deb + AppImage), which Tauri merges according to the host it is building on. It builds for the **host platform only** — cross-compiling would take a full sysroot of the target — so the three platforms' packages are built separately, in CI.

`npm run build` runs `scripts/bundle-runtime.mjs` first, which copies both install scripts into `src-tauri/resources/` and — when it is missing — records the boot warm-up list by tracing one dsh startup. The package itself carries neither Node nor dsh.

### Releasing

Push a tag starting with `v`. `.github/workflows/release.yml` runs the checks first (`cargo test` on all three platforms, and a lint of `install-deps.sh`), then builds on all three, signs everything, and uploads the packages and a merged `latest.json` to a **draft** release:

```sh
# Three places carry the version; keep them equal: package.json,
# src-tauri/tauri.conf.json, src-tauri/Cargo.toml. The packages and
# latest.json use the one in tauri.conf.json.
git commit -am 'chore: 0.2.0' && git tag v0.2.0 && git push --follow-tags
```

Draft on purpose: the updater endpoint points at `releases/latest`, so publishing is what puts the release in front of every installed copy on its next launch. Look the artifacts over first.

To find out whether the other two platforms are still happy before committing to a tag, start the same workflow by hand (the Run workflow button, or `gh workflow run release.yml`) — the bundling is guarded on the ref being a tag, so a manual run stops after the checks.

The signing key is not in the repo; it goes in the repository's Actions secrets — just the one:

| Secret | Contents |
| --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` | the **contents** of `~/.tauri/dsh-desktop.key`, not the path |

There is deliberately **no** `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`: that key has no password, GitHub will not store an empty secret, and a secret that does not exist renders as the empty string — the variable is still defined, empty, which is what the signer wants. Put a password on the key and this is where the secret to match it goes.

To build a version others can auto-update to locally, pass the key in:

```sh
TAURI_SIGNING_PRIVATE_KEY=~/.tauri/dsh-desktop.key TAURI_SIGNING_PRIVATE_KEY_PASSWORD= npm run build
```

> **Do not transcribe that line into PowerShell** — `$env:X = ""` *deletes* the variable there, so the password never reaches the CLI and the build hangs on its prompt. Use Git Bash.
>
> Quit the running app and any installer left open from a previous build first; both hold files open, and the failure reads a lot like success.

## How it works

1. Builds the window and shows a loading page, then checks for a newer dsh in the background — no dsh is running yet, which is the one moment it is safe to replace. If the machine has no dsh at all, it installs one here, with progress on the loading page.
2. Finds dsh in the order `DSH_BIN` → `dsh` in the npm global prefix, then starts `dsh web --port 0`.
3. Reads its stdout, waiting for `dsh web: http://127.0.0.1:<port>` — both the readiness signal and the URL to load. If startup fails, the loading page shows the tail of its output.
4. Navigates the window to that URL and keeps it within that origin.
5. On exit, kills the entire child process tree. On Windows that is `taskkill /T`, and the child is put in a Job Object with `KILL_ON_JOB_CLOSE` as soon as it starts, which takes the tree down even when the app is force-killed and no cleanup code runs. On macOS and Linux the child `setpgid`s itself into a group of its own before the exec, and a signal to the negative pid takes the group with it — but there is no kernel-side backstop; see [known limitations](#known-limitations).

The window's light/dark follows dsh's own theme setting (`ui-theme.preference` in `$DSH_HOME/settings.yaml`), read once at window creation, so the loading page never flashes the opposite colour first.

The window buttons do not use Tauri IPC — that would grant IPC to every line of JavaScript running inside dsh's pages. A press navigates to a custom scheme (`dsh-window://close`) which `on_navigation` recognises, acts on, and cancels.

### Where Node and dsh live

The package carries neither. Both are put in place by one install script — `scripts/install-deps.ps1` on Windows, `scripts/install-deps.sh` on macOS and Linux. The two take the same arguments, print the same lines and make the same decisions, and the app runs whichever one belongs to the platform when a launch finds something missing, so there is one implementation per platform rather than two. None of it needs administrator rights.

*When* it runs differs. Windows does it from the installer (`installer-hooks.nsh`); macOS and Linux have no usable installer hook — a `.dmg` and an `.AppImage` have none at all, and a `.deb`'s runs as root, which is the wrong user to install a per-user Node as — so there the first launch does it, with the progress on the loading page. It is the same path Windows falls back to when its installer could not reach the network.

**Node**: a machine with 22.22.3 or newer keeps what it has, untouched. Otherwise the official standalone archive is downloaded from nodejs.org (falling back to npmmirror, then Aliyun, then Huawei Cloud), verified against its published SHA256, and unpacked into the app's data directory:

| Platform | Location |
| --- | --- |
| Windows | `%LOCALAPPDATA%\ai.deepseek.dsh.desktop\node\` |
| macOS | `~/Library/Application Support/ai.deepseek.dsh.desktop/node/` |
| Linux | `~/.local/share/ai.deepseek.dsh.desktop/node/` |

**dsh**: `npm install -g @deepseek-ai/dsh`, landing in that Node's global prefix — or in your own npm prefix when the machine's Node is used, unless only root can write there (which is the usual case for a distribution's `/usr` Node), in which case it falls back to a prefix under the app's data directory rather than asking for a password.

The prefix's binary directory is prepended to PATH, so the `dsh` in your terminal is the same copy the app runs. On Windows that means `HKCU\Environment`; on macOS and Linux, a block marked with `# >>> dsh desktop >>>` appended to `~/.profile` (plus `~/.zshrc` for a zsh user, and `~/.bashrc` if it already exists), taken back out by the same marker at uninstall.

**If `dsh` is already on PATH, setup skips this entirely.** There is no point downloading 327 MB alongside a working dsh, and that copy stays yours: the update check reports on it but never writes to it.

The update check runs at most once every six hours (15 s timeout). A newer version brings up a dialog naming the version and size; taking it runs `npm install -g @deepseek-ai/dsh@latest` in place and carries straight on into the boot — no restart. "Skip this version" stops asking about that release.

Uninstalling on Windows **asks separately** about dsh and about the Node.js this app installed (removing Node takes dsh with it — a dsh without a Node cannot run), then asks once more about `$DSH_HOME`, all defaulting to keeping things. A Node or dsh you installed yourself is never touched. App updates and manual reinstalls trigger none of these questions.

macOS and Linux have no uninstaller to ask anything: deleting the .app or removing the .deb takes only the app itself. To clear out what it installed, run the same script by hand (on macOS, from `/Applications/dsh-desktop.app/Contents/Resources/resources/install-deps.sh`):

```sh
sh /usr/lib/dsh-desktop/resources/install-deps.sh -Mode uninstall -RemoveDsh -RemoveNode
```

It follows the same rules the Windows uninstaller does: only what it installed itself, and `$DSH_HOME` left alone. The copy inside an AppImage goes away with the mount, so use `scripts/install-deps.sh` from the repository instead.

## Environment variables

| Variable | Purpose |
| --- | --- |
| `DSH_BIN` | Full path to the dsh executable. Highest precedence — overrides the one on PATH. |
| `DSH_HOME` | dsh's data directory, `~/.dsh` by default. |

## Project layout

```
dist/index.html               Loading / error page (no build step; Rust drives it via eval'd hooks)
scripts/install-deps.ps1      Detects and installs Node and dsh on Windows; shared by installer and app
scripts/install-deps.sh       The same thing for macOS and Linux, run by the app's first launch
scripts/bundle-runtime.mjs    Stages both scripts into resources/ and records the boot warm-up list
scripts/boot-trace/           Traces one dsh boot to find out what it reads
.github/workflows/release.yml On a v* tag: check, then build all three, sign, upload to a draft release
src-tauri/tauri.conf.json     Base configuration, and the NSIS target for Windows
src-tauri/tauri.macos.conf.json   app + dmg targets (Tauri merges these per platform)
src-tauri/tauri.linux.conf.json   deb + AppImage targets
src-tauri/installer-hooks.nsh Calls install-deps.ps1 at install / uninstall, and handles $DSH_HOME
src-tauri/src/main.rs         Window, navigation policy, tray, lifecycle
src-tauri/src/controls.rs     The frameless window's injected buttons and drag strip
src-tauri/src/theme.rs        Reads dsh's light/dark preference, once, at window creation
src-tauri/src/server.rs       Managed dsh web child process, the job object backstop, process groups
src-tauri/src/dsh.rs          Locating the dsh install, version comparison, runtime updates
src-tauri/src/warm.rs         Parallel pre-reads from the warm-up list, against Defender's first scan
src-tauri/src/update.rs       The app's own auto-update
```

## Roadmap

- [x] Works without Node on the machine (detected at install time, installed into the user directory when absent)
- [x] Job Object fallback, tray and login item, auto-update
- [x] macOS / Linux bundle targets (dmg, deb, AppImage, with the install script and process-group teardown to go with them)
- [x] Release pipeline (a tag builds all three platforms, signs them, and uploads a draft release)

## Known limitations

- **Installing needs the network, and it is not quick**: dsh's dependency tree is 587 packages, 185 MB compressed, 327 MB unpacked across 33k files — about four minutes on a 2 MB/s link, preceded by another 36 MB if the machine has no Node. If every mirror fails the installer says so plainly rather than pretending it succeeded — and the next app launch tries again
- **The first launch is slower than the rest**: the files dsh imports get scanned one by one the first time they are read from a path Defender has never seen — measured at 14 s cold against 1.6 s once the same files are warm. The app pre-reads them from several threads at startup, off a list recorded at build time, which brings the cold launch to around 4 s. To avoid it entirely, exclude the install directory (elevated PowerShell, **with the path you actually chose**):

  ```powershell
  Add-MpPreference -ExclusionPath "$env:LOCALAPPDATA\dsh-desktop", "$env:LOCALAPPDATA\ai.deepseek.dsh.desktop"
  ```

- **Only Windows has been verified on real hardware**: the macOS and Linux code paths are all there and CI proves all three platforms compile and package, but nobody has yet confirmed that an install on those two actually comes up. Please open an issue if it does not
- **No kernel-side backstop on macOS or Linux**: an ordinary exit takes the process group down, but a force-killed app (`kill -9`, a crash) leaves dsh running. There is no equivalent of the Windows Job Object — Linux's `PR_SET_PDEATHSIG` is tied to the lifetime of the *thread* that spawned the child, which is a boot thread that finishes long before the app does, so using it would kill the server mid-session. The next launch runs into the single-instance lock; `pkill -f 'dsh web'` clears it
- **The macOS build is unsigned and unnotarised**: that needs a paid Apple developer account. The first launch takes a right-click → Open, or clearing the quarantine attribute — see [Installing](#installing)
- **Only the AppImage updates itself on Linux**: Tauri's updater supports no other Linux format. A `.deb` install has to be replaced from Releases by hand — the deb entry in `latest.json` is something the bundler writes anyway, and nothing reads it
- The first-launch warm-up (`warm.rs`) is aimed at Windows Defender; on macOS and Linux all that is left of it is the page cache
- Packages must be built on the platform they target
- A dsh update goes through `npm install -g`, which coexists with the outgoing tree while it lands, peaking around 650 MB on disk
- PATH changes from installing Node only reach newly opened terminals — and on macOS and Linux, only ones that read `~/.profile` or `~/.zshrc`; the app itself is unaffected, since it reads the paths the script recorded in `bootstrap.json` rather than trusting the PATH it inherited
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
