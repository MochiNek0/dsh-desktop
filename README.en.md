<div align="center">

# dsh desktop

A cross-platform desktop client for DeepSeek Harness (`dsh web`)

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

> Unofficial: a third-party desktop client for [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness), not affiliated with DeepSeek. [Issues](https://github.com/MochiNek0/dsh-desktop/issues) and PRs welcome.

Starts the local `dsh web` in the background on launch and embeds it in a native window — no terminal, no port juggling. Sessions, credentials and config live in `$DSH_HOME` (default `~/.dsh`), the same ones your command-line `dsh` uses.

## Features

- **Light**: Tauri v2 on the system WebView, with no bundled browser engine. Installers are 2.3 MB on Windows, 5.8 MB on macOS, 3.8 MB on Debian.
- **Cross-platform**: the same experience on Windows, macOS and Linux.
- **Works out of the box**: detects Node, installs `dsh` where needed, and picks a free loopback port so it never collides with an instance you started by hand. No administrator rights at any point.
- **Everything is a plugin**: Not a single line of dsh source code is modified. Desktop enhancements (like system notifications) are provided as bundled plugins — remove them and dsh returns to a completely pristine state. Features a built-in visual plugin panel highlighting the [DSH Market](https://dshmarket.com) marketplace, requiring no terminal usage.
- **Safe Mode (recover from broken plugins)**: Plugins load before `dsh web` binds its port, so a crashing plugin leaves the app stuck on the loading page — with the removal panel trapped behind the window that won't open. The loading page provides **Start without plugins**: all user plugins are temporarily unmounted from the layer stack (dsh's built-in layers remain untouched) for a clean launch, opening the panel directly so you can uninstall the culprit, then restore the rest via **Load plugins again** in the menu.
- **Native integration**: theme and interface language follow dsh's own settings and switch without a restart; tray-resident with optional start at login; a system notification when a turn ends or dsh needs you, which returns to that session on click and takes allow/refuse style answers on the notification itself.

## Installation

Download the package for your system from the **[website](https://dsh-desktop.cc.cd/en/)** or [Releases](https://github.com/MochiNek0/dsh-desktop/releases), install, and open it. If no usable Node is found, the Runtime panel opens by itself and installs Node 24 in one click.

| OS | Format | Size | Details |
| :--- | :--- | :--- | :--- |
| **Windows** | `.exe` (NSIS) | 2.3 MB | Needs WebView2; downloaded automatically if missing (verified) |
| **macOS** | `.dmg` | 5.8 MB | Universal binary for Apple Silicon and Intel (verified) |
| **Linux** | `.deb` / `.AppImage` | 3.8 MB / 78 MB | `.AppImage` carries its own WebKit, hence the size, but has the most complete self-updater support (verified on Debian) |

## Notes

- **Node version**: `dsh` needs Node.js 22.19.0 or newer. Neither Node nor `dsh` is bundled, so the first launch needs a connection if they are missing.
- **Closing parks in the tray**: closing the window keeps an in-flight task alive. Use **Quit dsh** in the menu to exit for real.
- **Auto update**: checked silently on launch, raised only when there is one, downloaded only with your consent.
- **Blocked by Gatekeeper on macOS**: right-click the app in Finder and choose "Open", or run `xattr -dr com.apple.quarantine /Applications/dsh-desktop.app`.
- **Installing `github:` plugins**: pnpm blocks build scripts from git sources by default. If it fails, allow the package under `allowBuilds` in `$DSH_HOME/profiles/web/pnpm-workspace.yaml` as the panel describes.

## Configuration

| Variable | Description | Default |
| :--- | :--- | :--- |
| `DSH_BIN` | Absolute path to the `dsh` executable; highest priority, and skips the Node version check | Auto-detected from `PATH` |
| `DSH_HOME` | Directory for `dsh` data, credentials and config | `~/.dsh` |

## Development

Requires Rust stable 1.82+ and Node.js 22+. Linux additionally needs `libwebkit2gtk-4.1-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev`, `patchelf`, `libxdo-dev`, `libssl-dev` and `build-essential`.

```sh
npm install     # install dependencies
npm run dev     # development mode
npm run build   # production bundle, written to src-tauri/target/release/bundle/
```

## Links

- **Website**: [English](https://dsh-desktop.cc.cd/en/) · [中文](https://dsh-desktop.cc.cd/)
- **Upstream**: [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)
- **Friends**: [DSH Market](https://github.com/dsh-market/dsh-market) — the visual plugin market inside dsh; browse, search and install community plugins in one click ([dshmarket.com](https://dshmarket.com))

## License

[MIT](LICENSE)
