<div align="center">

# dsh desktop

DeepSeek Harness (`dsh web`) 的跨平台 Tauri 桌面客户端

**[官方网站 · 下载](https://dsh-desktop.cc.cd/)** · [English](README.en.md) · **简体中文**

<br/>

[![Website](https://img.shields.io/badge/Website-dsh--desktop.cc.cd-2ea44f?logo=googlechrome&logoColor=white)](https://dsh-desktop.cc.cd/)
[![Release](https://img.shields.io/github/v/release/MochiNek0/dsh-desktop?color=blue)](https://github.com/MochiNek0/dsh-desktop/releases)
[![Tauri](https://img.shields.io/badge/Tauri-v2-24C8D8?logo=tauri&logoColor=white)](https://tauri.app/)
![Platform](https://img.shields.io/badge/Platform-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey)
[![License: MIT](https://img.shields.io/badge/License-MIT-green.svg)](LICENSE)

<br/>
<br/>

<img src="docs/thumbnail.png" alt="dsh desktop preview" width="850" />

</div>

<br/>

> **非官方声明**：本项目为基于 [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) 开发的第三方桌面客户端，与 DeepSeek 官方无隶属或合作关系。仅供学习与便利使用，欢迎提交 [Issue](https://github.com/MochiNek0/dsh-desktop/issues) 或 Pull Request。

---

## 概述

**dsh desktop**（DeepSeek Harness 桌面版）启动时会自动在后台拉起本地 `dsh web` 服务并内嵌至原生桌面窗口。无需手动打开终端或管理端口，会话记录、凭证与配置均与 CLI 全局共享（存储于 `$DSH_HOME`，默认 `~/.dsh`）。

## 特性

- **开箱即用**：自动检测机器上的 Node.js 并按需安装 `dsh`，全程无需管理员权限。
- **无感共存**：`dsh web` 监听动态分配的回环端口，与终端里手动运行的实例互不干扰。
- **轻量原生**：无边框窗口、窗口控件内嵌于页面；主题与界面语言跟随 dsh 自身设置（`$DSH_HOME/settings.yaml`），在 dsh 里切换语言无需重启。支持托盘常驻与开机自启。
- **智能通知**：无需安装任何插件，回合结束、或 dsh 在等待你确认（工具授权、计划评审、提问）时自动发出系统通知；窗口在前台时自动静默。
- **环境管理**：内置「运行环境」面板，可枚举机器上的所有 Node、切换所用版本、就地安装 dsh 或安装一份全新 Node；并提供 dsh 版本检查与一键升级。
- **插件管理**：可视化插件面板，无需终端即可一键安装/卸载插件；同时提供自带正确环境的终端入口，不污染系统全局 PATH。
- **稳定守护**：单实例运行，应用退出时自动回收所有关联子进程；`dsh web` 意外退出时返回加载页并提供重启。

## 安装与下载

前往 **[官方网站 dsh-desktop.cc.cd](https://dsh-desktop.cc.cd/)** 或 [GitHub Releases 页面](https://github.com/MochiNek0/dsh-desktop/releases) 下载适用于您操作系统的最新安装包：

| 操作系统 | 安装包格式 | 说明 |
| :--- | :--- | :--- |
| **Windows** | `.exe`（NSIS） | 需系统已安装 WebView2（如缺失将自动引导下载） **[已验证]** |
| **macOS** | `.dmg` 镜像 | 通用二进制架构，原生支持 Apple Silicon 及 Intel 设备 **[暂未验证，欢迎反馈]** |
| **Linux** | `.AppImage` / `.deb` | 推荐使用 `.AppImage` 以获得完整的自更新支持 **[Debian 系已验证]** |

> **macOS 首次运行提示**
>
> 若首次打开时遇到安全拦截提示，可在访达中右键点击应用选择「打开」，或在终端中执行以下命令解除隔离：
> ```sh
> xattr -dr com.apple.quarantine /Applications/dsh-desktop.app
> ```

## 菜单功能

窗口标题栏的菜单（以及托盘菜单）提供以下入口：

| 菜单项 | 说明 |
| :--- | :--- |
| **插件…** | 打开可视化插件面板 |
| **打开终端** | 启动一个已配置好 `dsh` 环境变量的终端，不修改系统全局 PATH |
| **重启 dsh** | 重启后台的 `dsh web` 进程 |
| **更新 dsh…** | 检查并升级 `dsh` 本体 |
| **运行环境…** | 管理 Node.js：查看、切换、安装或删除 |
| **检查应用更新…** | 检查桌面端自身的新版本 |
| **开机自启动** / **通知** | 开关项 |
| **退出 dsh** | 真正退出（直接关闭窗口只会收进托盘） |

### 插件

插件面板支持从预设列表一键安装，或手动输入 npm 包名 / GitHub 仓库地址（如 `github:owner/repo`）安装。

> **提示**：安装 `github:` 形式的插件时，pnpm 出于安全考虑默认会拦截构建脚本。如遇报错，请根据面板提示打开插件目录，在 `$DSH_HOME/profiles/web/pnpm-workspace.yaml` 的 `allowBuilds` 字段中手动放行该插件。

## 配置与环境变量

应用支持通过环境变量自定义运行行为：

| 环境变量 | 说明 | 默认值 |
| :--- | :--- | :--- |
| `DSH_BIN` | 指定 `dsh` 可执行文件的绝对路径（优先级最高，同时跳过 Node 版本检查） | 自动检索系统 PATH |
| `DSH_HOME` | 指定 `dsh` 数据、凭证与配置的存储目录 | `~/.dsh` |

## 开发与构建

### 前置要求

- **Rust**：稳定版工具链，1.82 或更高版本
- **Node.js**：22 或更高版本（仅用于构建；CI 使用 Node 22）
- **Linux 额外依赖**：
  ```sh
  sudo apt-get install -y libwebkit2gtk-4.1-dev libayatana-appindicator3-dev \
    librsvg2-dev patchelf libxdo-dev libssl-dev build-essential
  ```

### 常用命令

```sh
# 安装依赖
npm install

# 启动开发模式（启用 DevTools）
npm run dev

# 构建正式发布包（输出至 src-tauri/target/release/bundle/）
npm run build
```

平台专属的打包目标由 `tauri.linux.conf.json` / `tauri.macos.conf.json` 自动合并，无需额外参数。

## 项目结构

```text
dsh-desktop/
├── dist/index.html               # 前端加载等待与错误提示页面
├── docs/                         # README 配图
├── scripts/                      # 引导脚本（install-deps.ps1 / .sh）与构建辅助脚本
├── src-tauri/
│   ├── Cargo.toml                # Rust 依赖项与构建配置
│   ├── tauri.conf.json           # 基础配置（Windows: NSIS）
│   ├── tauri.{linux,macos}.conf.json  # 平台专属打包目标
│   ├── installer-hooks.nsh       # NSIS 安装 / 卸载钩子
│   └── src/                      # Rust 后端源码（窗口、进程托管、托盘、插件、运行环境、更新）
├── updater-proxy/                # Cloudflare Worker：更新检查端点代理
└── .github/workflows/release.yml # 打 tag 后的多平台构建、签名与草稿发布
```

## 注意事项

- **Node.js 版本要求**：`dsh` 需要 Node.js **22.19.0** 或更高版本。若机器上没有满足要求的 Node，应用会在启动时弹出「运行环境」面板，可从中选择一个已有的 Node，或安装一份全新的 Node 24。
- **首次启动联网**：安装包不内置 Node 与 `dsh`，首次启动时若未检测到本地环境，需联网拉取，请保持网络连通。
- **关闭即最小化**：关闭窗口只会将应用收进托盘（避免中断进行中的任务），真正退出请使用菜单中的「退出 dsh」。
- **自动更新**：支持桌面端应用自动检查并安装更新（Linux 环境仅支持 AppImage 格式）。

## 相关链接

- **官方网站**：[中文](https://dsh-desktop.cc.cd/) · [English](https://dsh-desktop.cc.cd/en/)
- **GitHub**：[仓库](https://github.com/MochiNek0/dsh-desktop) · [版本下载](https://github.com/MochiNek0/dsh-desktop/releases) · [问题反馈](https://github.com/MochiNek0/dsh-desktop/issues)
- **上游项目**：[DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)

<sub>关键词：dsh desktop、DeepSeek Harness 桌面版、dsh web 客户端、DeepSeek 桌面客户端、Tauri、Windows / macOS / Linux AI 编程助手 GUI。</sub>

## 许可证

本项目基于 [MIT 许可证](LICENSE) 开源。
