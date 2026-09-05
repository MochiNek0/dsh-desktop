<div align="center">

# dsh desktop

A cross-platform Tauri desktop client for DeepSeek Harness (`dsh web`)

**[Official Website · Download](https://dsh-desktop.cc.cd/en/)** · **English** · [简体中文](README.md)

<br/>

[![Website](https://img.shields.io/badge/Website-dsh--desktop.cc.cd-2ea44f?logo=googlechrome&logoColor=white)](https://dsh-desktop.cc.cd/en/)
[![Release](https://img.shields.io/github/v/release/MochiNek0/dsh-desktop?color=blue)](https://github.com/MochiNek0/dsh-desktop/releases)
[![Tauri](https://img.shields.io/badge/Tauri-v2-24C8D8?logo=tauri&logoColor=white)](https://tauri.app/)
![Platform](https://img.shields.io/badge/Platform-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey)
[![License: MIT](https://img.shields.io/badge/License-MIT-green.svg)](LICENSE)

<br/>
<br/>

<img src="docs/thumbnail-en.png" alt="dsh desktop preview" width="850" />

</div>

<br/>

> **Disclaimer**: This is a third-party desktop client for [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness). It is not affiliated with, sponsored, or endorsed by DeepSeek. It is intended for convenience and personal use — please feel free to open an [Issue](https://github.com/MochiNek0/dsh-desktop/issues) or submit a Pull Request.

---

## Overview

**dsh desktop** (the DeepSeek Harness desktop app) automatically starts the local `dsh web` service in the background and embeds it into a native window upon launch. There is no need to manually manage terminal sessions or port allocations. Sessions, credentials, and configurations are shared seamlessly with the CLI (`$DSH_HOME`, default `~/.dsh`).

## Features

- **Out of the Box**: Detects the Node.js installations on your machine and installs `dsh` where needed — no administrator privileges required.
- **Port Conflict-Free**: `dsh web` listens on an auto-assigned loopback port, coexisting seamlessly with manual instances started from a terminal.
- **Native Experience**: Frameless window with the controls drawn inside the page; theme and interface language follow the settings dsh itself keeps in `$DSH_HOME/settings.yaml`, and switching the language inside dsh takes effect without a restart. Tray-resident, with optional start at login.
- **Smart Notifications**: No plugin required — a native notification is raised when a turn finishes, or when dsh is waiting on you (a tool approval, a plan review, a question). Suppressed while the window is in front.
- **Runtime Management**: A built-in **Runtime** panel enumerates every Node on the machine and lets you switch between them, install dsh into one, or install a fresh Node. Also handles dsh version checks and one-click upgrades.
- **Plugin Management**: Built-in visual panel to install and remove dsh plugins effortlessly. Alternatively, use the integrated terminal pre-configured with the correct `PATH`.
- **Clean Lifecycle**: Single-instance enforcement with complete child process cleanup upon exit. Returns to the loading page and offers a restart if `dsh web` crashes unexpectedly.

## Installation

Download the latest release package for your operating system from the **[official website dsh-desktop.cc.cd](https://dsh-desktop.cc.cd/en/)** or the [GitHub Releases page](https://github.com/MochiNek0/dsh-desktop/releases):

| Operating System | Package Format | Details |
| :--- | :--- | :--- |
| **Windows** | `.exe` (NSIS) | Requires WebView2 (automatically prompted/downloaded if missing) **[Verified]** |
| **macOS** | `.dmg` image | Universal binary supporting both Apple Silicon and Intel **[Unverified, feedback welcome]** |
| **Linux** | `.AppImage` / `.deb` | `.AppImage` is recommended for built-in self-updater support **[Verified on Debian]** |

> **macOS First-Launch Note**
>
> If blocked by macOS Gatekeeper on first launch, right-click the app in Finder and select "Open", or run the following command in terminal:
> ```sh
> xattr -dr com.apple.quarantine /Applications/dsh-desktop.app
> ```

## Menu

The titlebar menu (and the tray menu) offers:

| Item | Description |
| :--- | :--- |
| **Plugins…** | Open the visual plugin panel |
| **Open a terminal** | A shell pre-configured with `dsh` and its environment, without modifying your global system PATH |
| **Restart dsh** | Restart the background `dsh web` process |
| **Update dsh…** | Check for and install a newer `dsh` |
| **Runtime…** | Manage Node.js: list, switch, install, or remove |
| **Check for app updates…** | Check for a newer version of the desktop app itself |
| **Start at login** / **Notifications** | Toggles |
| **Quit dsh** | Quit for real (closing the window only parks it in the tray) |

### Plugins

The plugin panel installs from a preset list, or from any valid npm package name / GitHub repository (e.g., `github:owner/repo`).

> **Note on `github:` plugins**: pnpm blocks build scripts from git repositories by default for security. If the installation fails, the panel will show the pnpm output. You will need to open the plugin folder and manually add the package to `allowBuilds` in `$DSH_HOME/profiles/web/pnpm-workspace.yaml`.

## Configuration & Environment Variables

The application behavior can be customized via environment variables:

| Variable | Description | Default |
| :--- | :--- | :--- |
| `DSH_BIN` | Absolute path to the `dsh` executable (highest priority; also skips the Node version check) | Auto-detected from `PATH` |
| `DSH_HOME` | Directory for storing `dsh` data, credentials, and configs | `~/.dsh` |

## Development & Build

### Prerequisites

- **Rust**: Stable toolchain, 1.82 or newer
- **Node.js**: 22 or newer (build only; CI uses Node 22)
- **Additional Linux dependencies**:
  ```sh
  sudo apt-get install -y libwebkit2gtk-4.1-dev libayatana-appindicator3-dev \
    librsvg2-dev patchelf libxdo-dev libssl-dev build-essential
  ```

### Commands

```sh
# Install dependencies
npm install

# Start in development mode (with DevTools)
npm run dev

# Build production installer (output to src-tauri/target/release/bundle/)
npm run build
```

Platform-specific bundle targets are merged in automatically from `tauri.linux.conf.json` / `tauri.macos.conf.json` — no extra flags needed.

## Project Structure

```text
dsh-desktop/
├── dist/index.html               # Frontend loading and error feedback page
├── docs/                         # README images
├── scripts/                      # Bootstrap scripts (install-deps.ps1 / .sh) and build helpers
├── src-tauri/
│   ├── Cargo.toml                # Rust dependencies and build configuration
│   ├── tauri.conf.json           # Base configuration (Windows: NSIS)
│   ├── tauri.{linux,macos}.conf.json  # Platform-specific bundle targets
│   ├── installer-hooks.nsh       # NSIS install / uninstall hooks
│   └── src/                      # Rust core (window, process supervisor, tray, plugins, runtime, updater)
├── updater-proxy/                # Cloudflare Worker proxying the update endpoint
└── .github/workflows/release.yml # Tag-triggered multi-platform build, signing, and draft release
```

## Notes

- **Node.js Requirement**: `dsh` needs Node.js **22.19.0** or newer. If no suitable Node is found, the app opens the **Runtime** panel on launch so you can pick an existing one or install a fresh Node 24.
- **Network on First Launch**: Neither Node nor `dsh` is bundled in the installer, so the first launch needs an internet connection if the local components are missing.
- **Closing Parks in the Tray**: Closing the window leaves the app running in the tray (so an in-flight task is not torn down). Use **Quit dsh** in the menu to exit for real.
- **Auto Update**: Integrated auto-updater support (Linux supports AppImage format only).

## Links

- **Official Website**: [English](https://dsh-desktop.cc.cd/en/) · [中文](https://dsh-desktop.cc.cd/)
- **GitHub**: [Repository](https://github.com/MochiNek0/dsh-desktop) · [Releases](https://github.com/MochiNek0/dsh-desktop/releases) · [Issues](https://github.com/MochiNek0/dsh-desktop/issues)
- **Upstream project**: [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)

<sub>Keywords: dsh desktop, DeepSeek Harness desktop app, dsh web GUI client, DeepSeek desktop client, Tauri, AI coding agent GUI for Windows / macOS / Linux.</sub>

## License

This project is licensed under the [MIT License](LICENSE).
