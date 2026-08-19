<div align="center">

# dsh desktop

A cross-platform Tauri desktop client for DeepSeek Harness (`dsh web`)

**English** · [简体中文](README.md)

<br/>

[![Release](https://img.shields.io/github/v/release/MochiNek0/dsh-desktop?color=blue)](https://github.com/MochiNek0/dsh-desktop/releases)
[![Tauri](https://img.shields.io/badge/Tauri-v2-24C8D8?logo=tauri&logoColor=white)](https://tauri.app/)
![Platform](https://img.shields.io/badge/Platform-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey)
[![License: MIT](https://img.shields.io/badge/License-MIT-green.svg)](LICENSE)

<br/>
<br/>

<img src="docs/thumbnail-en.png" alt="dsh desktop preview" width="850" />

</div>

<br/>

> **Disclaimer**: This is a third-party desktop client for [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness). It is not affiliated with, sponsored, or endorsed by DeepSeek.

---

## Overview

**dsh desktop** automatically starts the local `dsh web` service in the background and embeds it into a native window upon launch. There is no need to manually manage terminal sessions or port allocations. Sessions, credentials, and configurations are shared seamlessly with the CLI (`$DSH_HOME`, default `~/.dsh`).

## Features

- **Out of the Box**: Automatically detects and sets up Node.js and `dsh` runtime without requiring administrator privileges.
- **Port Conflict-Free**: Starts on an auto-assigned loopback port, coexisting seamlessly with manual `dsh web` instances.
- **Native Experience**: Modern frameless UI, theme synchronization, system tray integration, and auto-start support.
- **Shared Environment**: Shares the same global `dsh` CLI environment, with startup checks and one-click upgrades.
- **Plugins Without a Terminal**: A built-in plugin panel installs dsh plugins, and an "Open a terminal" menu item opens a shell that already has `dsh` on its PATH.
- **System Notifications**: Notifications raised by the dsh page become real system notifications, and are suppressed while the window has focus.
- **Bilingual UI**: Follows the system locale between Chinese and English.
- **Clean Lifecycle**: Single-instance enforcement with complete child process cleanup upon exit; if `dsh web` exits on its own, the window returns to the loading page and offers a restart.

## Installation

> **Important**: It is recommended to install **v0.1.3 and later versions**, as previous versions may have some known issues. Currently, the application has been verified on **Windows** and **Linux (Debian-based systems)**.

Download the latest release package for your operating system from the [Releases page](https://github.com/MochiNek0/dsh-desktop/releases):

| Operating System | Package Format | Details |
| :--- | :--- | :--- |
| **Windows** | `.exe` installer | Requires WebView2 (automatically prompted/downloaded if missing) **[Verified]** |
| **macOS** | `.dmg` image | Universal binary supporting both Apple Silicon and Intel **[Unverified, feedback welcome]** |
| **Linux** | `.AppImage` / `.deb` | `.AppImage` is recommended for built-in self-updater support **[Verified on Debian]** |

> **macOS First-Launch Note**
>
> If blocked by macOS Gatekeeper on first launch, right-click the app in Finder and select "Open", or run the following command in terminal:
> ```sh
> xattr -dr com.apple.quarantine /Applications/dsh-desktop.app
> ```

## Plugins

dsh plugins live in `$DSH_HOME/profiles/web`, and the command is `dsh plugin --profile web add <package>` — which is a thin forwarder to **pnpm**. Installing one by hand therefore needs both `dsh` and `pnpm` on your PATH, and if this app installed Node for you, neither is there: it does not rewrite the user's PATH.

So installing is built in: **Menu → Plugins…**. The panel is drawn over whatever is on screen, so opening and closing it never reloads dsh. Tick what you want, install. The box below the list takes anything pnpm understands — a package name, or `github:owner/repo`. What is installed is listed below the suggestions, where ticking a row offers to remove it — neither direction needs a terminal. If pnpm is missing, npm installs it first. dsh stops for the install and starts again on the way back, which is when the plugins take effect.

The preset list is `src-tauri/resources/preset-plugins.json`. Adding or removing an entry is a pull request against that file, not a code change.

> **About `github:` plugins**: these build on install, and pnpm 10 and later refuse to run build scripts until the package is listed under `allowBuilds` in `$DSH_HOME/profiles/web/pnpm-workspace.yaml`. When that happens the panel shows pnpm's own instruction verbatim and offers to open the folder — the app will not allow a repository's build scripts on your behalf.

To use the CLI directly, **Menu → Open a terminal** opens a shell with that PATH already set, so `dsh` and `dsh plugin` work in it, without touching your system PATH.

## Configuration & Environment Variables

The application behavior can be customized via environment variables:

| Variable | Description | Default |
| :--- | :--- | :--- |
| `DSH_BIN` | Absolute path to the `dsh` executable (highest priority) | Auto-detected from `PATH` |
| `DSH_HOME` | Directory for storing `dsh` data, credentials, and configs | `~/.dsh` |

## Development & Build

### Prerequisites

- **Rust**: Stable toolchain (`stable`)
- **Node.js**: 18.0 or higher

### Commands

```sh
# Install dependencies
npm install

# Start in development mode (with DevTools)
npm run dev

# Build production installer (output to src-tauri/target/release/bundle/)
npm run build
```

## Project Structure

```text
dsh-desktop/
├── dist/index.html               # Frontend loading and error feedback page
├── scripts/                      # Dependency setup and runtime packaging scripts
├── src-tauri/
│   ├── Cargo.toml                # Rust dependencies and build configuration
│   ├── tauri.*.conf.json         # Platform-specific Tauri configurations
│   └── src/                      # Rust core (window management, process supervisor, tray, updater)
└── package.json
```

## Notes

- **Network on First Launch**: The app requires an active internet connection on first launch if local components need to be downloaded.
- **Auto Update**: Integrated auto-updater support (Linux supports AppImage format only).

## Disclaimer

This is a third-party open-source client intended for convenience and personal use. If you encounter any problems or have suggestions, please feel free to open an [Issue](https://github.com/MochiNek0/dsh-desktop/issues) or submit a Pull Request.

## License

This project is licensed under the [MIT License](LICENSE).
