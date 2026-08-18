# dsh desktop

> A cross-platform Tauri desktop client for DeepSeek Harness (`dsh web`).
>
> [中文](README.md) · English
>
> **Unofficial**: A third-party desktop wrapper for DeepSeek Harness, not affiliated with DeepSeek. See [Disclaimer](#disclaimer).

<br>

![dsh desktop](docs/thumbnail-en.png)

<br>

Launches a local `dsh web` server and embeds the interface into a native window. No need to manage terminal sessions or ports. Sessions, credentials, and settings are fully shared with the CLI (`$DSH_HOME`, default `~/.dsh`).

## ✨ Features

- **Out of the Box**: Automatically detects and sets up Node.js and `dsh` without requiring administrator privileges.
- **Port Conflict-Free**: Starts on an auto-assigned loopback port, coexisting seamlessly with manual `dsh web` instances.
- **Native Experience**: Modern frameless UI, theme synchronization, system tray, and auto-start support.
- **Shared Environment**: Terminal uses the same global `dsh`, with automatic update checks and one-click upgrades.
- **Clean Lifecycle**: Single-instance enforcement with complete child process cleanup upon exit.

## 📦 Installation

Download the installer for your platform from [Releases](https://github.com/MochiNek0/dsh-desktop/releases):
- **Windows**: `.exe` installer (requires WebView2, auto-downloaded if missing)
- **macOS**: `.dmg` (Universal binary for Apple Silicon & Intel)
- **Linux**: `.AppImage` / `.deb`

> **macOS Note**: If blocked by Gatekeeper on first launch, right-click and select "Open", or run:
> ```sh
> xattr -dr com.apple.quarantine /Applications/dsh-desktop.app
> ```

## 🛠️ Development

### Prerequisites
- Rust stable
- Node.js 18+

### Commands
```sh
npm install
npm run dev    # Start in dev mode with DevTools
npm run build  # Build production bundles (output in src-tauri/target/release/bundle/)
```

## ⚙️ Environment Variables

| Variable | Description | Default |
| --- | --- | --- |
| `DSH_BIN` | Full path to the `dsh` executable (highest priority) | - |
| `DSH_HOME` | `dsh` data and configuration directory | `~/.dsh` |

## 📂 Project Structure

```text
dist/index.html               Loading and error pages
scripts/                      Dependency setup and runtime bundling scripts
src-tauri/tauri.*.conf.json   Platform-specific Tauri configurations
src-tauri/src/                Rust core (window management, process supervisor, tray, updater)
```

## ⚠️ Notes

- **Initial Setup**: The app downloads `dsh` and required runtimes during first install/launch; internet connection is required.
- **Auto Update**: Built-in self-updater (Linux supports AppImage only).

## 📄 Disclaimer

This is a third-party desktop client for [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness), maintained by individual contributors. It is not affiliated with, sponsored, or endorsed by DeepSeek.
For issues or feedback, please file an issue in this repository.

## 📜 License

[MIT](LICENSE) © 2026 MochiNek0
