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

- **Out of the Box**: Automatically detects and sets up Node.js and the `dsh` runtime without requiring administrator privileges.
- **Port Conflict-Free**: Starts on an auto-assigned loopback port, coexisting seamlessly with manual `dsh web` instances.
- **Native Experience**: Modern frameless UI featuring theme synchronization, a bilingual interface, native system notifications, and auto-start support.
- **Available in Your Terminal**: After installation the `dsh` command works in your own shell, and keeps working when you switch Node versions with nvm, fnm or volta.
- **Plugin Management**: Built-in visual panel to install and remove dsh plugins effortlessly. Alternatively, use the integrated terminal pre-configured with the correct `PATH`.
- **Clean Lifecycle**: Single-instance enforcement with complete child process cleanup upon exit. Automatically offers a restart if `dsh web` crashes unexpectedly.

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

The app includes a built-in visual plugin panel, allowing you to easily manage dsh plugins without using the command line:

- **Visual Management**: Click **Menu → Plugins…** to open the panel. You can install from a preset list, or enter any valid npm package name / GitHub repository (e.g., `github:owner/repo`). 
- **Terminal Access**: If you prefer the CLI, use **Menu → Open a terminal** to open a shell pre-configured with `dsh`. The app puts exactly one directory on your PATH, holding only the `dsh` command, so it cannot shadow your own `node`, `npm` or `npx`. That directory goes first, so the `dsh` in your terminal is the one the window runs: it calls the app's own Node by absolute path and puts the app's own `pnpm` first on PATH, so installing a plugin from a terminal lands where installing it from the panel does. Uninstalling the app takes the directory back off your PATH.
- **Settings**: **Menu → Settings…** holds the login item, turn notifications, the app / dsh / Node versions, and "Remove the dsh runtime" — which deletes the copy this app installed and leaves a Node or dsh you installed yourself alone.

> **Note on `github:` plugins**: pnpm blocks build scripts from git repositories by default for security. If the installation fails, the panel will show the pnpm output. You will need to open the plugin folder and manually add the package to `allowBuilds` in `$DSH_HOME/profiles/web/pnpm-workspace.yaml`.

## Configuration & Environment Variables

The application behavior can be customized via environment variables:

| Variable | Description | Default |
| :--- | :--- | :--- |
| `DSH_BIN` | Absolute path to the `dsh` executable (highest priority; the app will not update it) | The app's own runtime |
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

- **Network on First Launch**: The first launch downloads a dedicated Node and dsh (about 220 MB together) into the app's own directory, so it needs a connection. That runtime is unrelated to any Node on the machine — switching versions with nvm, fnm or volta cannot affect it. After an app update, the first launch tops it up again if the new release pins a different runtime.
- **Auto Update**: Integrated auto-updater support (Linux supports AppImage format only).

## Disclaimer

This is a third-party open-source client intended for convenience and personal use. If you encounter any problems or have suggestions, please feel free to open an [Issue](https://github.com/MochiNek0/dsh-desktop/issues) or submit a Pull Request.

## License

This project is licensed under the [MIT License](LICENSE).
