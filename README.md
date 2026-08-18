<div align="center">

# dsh desktop

DeepSeek Harness (`dsh web`) 的跨平台 Tauri 桌面客户端

[English](README.en.md) · **简体中文**

<br/>

[![Release](https://img.shields.io/github/v/release/MochiNek0/dsh-desktop?color=blue)](https://github.com/MochiNek0/dsh-desktop/releases)
[![Tauri](https://img.shields.io/badge/Tauri-v2-24C8D8?logo=tauri&logoColor=white)](https://tauri.app/)
[![Platform](https://img.shields.io/badge/Platform-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey)]()
[![License: MIT](https://img.shields.io/badge/License-MIT-green.svg)](LICENSE)

<br/>
<br/>

<img src="docs/thumbnail.png" alt="dsh desktop preview" width="850" />

</div>

<br/>

> **非官方声明**：本项目为基于 [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) 开发的第三方桌面客户端，与 DeepSeek 官方无隶属或合作关系。

---

## 概述

**dsh desktop** 启动时会自动在后台拉起本地 `dsh web` 服务并内嵌至原生桌面窗口。无需手动打开终端或管理端口，会话记录、凭证与配置均与 CLI 全局共享（存储于 `$DSH_HOME`，默认 `~/.dsh`）。

## 特性

- **开箱即用**：自动检测并配置 Node.js 及 `dsh` 运行环境，无需系统管理员权限。
- **无感共存**：采用动态端口分配，与终端手动运行的 `dsh web` 互不干扰。
- **轻量原生**：现代无边框 UI，自动跟随系统主题，支持系统托盘托管与开机自启。
- **环境共享**：与全局终端共用同一 `dsh` 命令，支持启动检查与一键无缝升级。
- **优雅退出**：单实例进程守护，应用退出时自动回收所有关联子进程。

## 安装与下载

前往 [Releases 页面](https://github.com/MochiNek0/dsh-desktop/releases) 下载适用于您操作系统的最新安装包：

| 操作系统 | 安装包格式 | 说明 |
| :--- | :--- | :--- |
| **Windows** | `.exe` 安装包 | 需系统已安装 WebView2（如缺失将自动引导下载） |
| **macOS** | `.dmg` 镜像 | 通用二进制架构，原生支持 Apple Silicon 及 Intel 设备 |
| **Linux** | `.AppImage` / `.deb` | 推荐使用 `.AppImage` 以获得完整的自更新支持 |

> **macOS 首次运行提示**
>
> 若首次打开时遇到安全拦截提示，可在访达中右键点击应用选择「打开」，或在终端中执行以下命令解除隔离：
> ```sh
> xattr -dr com.apple.quarantine /Applications/dsh-desktop.app
> ```

## 配置与环境变量

应用支持通过环境变量自定义运行行为：

| 环境变量 | 说明 | 默认值 |
| :--- | :--- | :--- |
| `DSH_BIN` | 指定 `dsh` 可执行文件的绝对路径（优先级最高） | 自动检索系统 PATH |
| `DSH_HOME` | 指定 `dsh` 数据、凭证与配置的存储目录 | `~/.dsh` |

## 开发与构建

### 前置要求

- **Rust**: 稳定版工具链（`stable`）
- **Node.js**: 18.0 或更高版本

### 常用命令

```sh
# 安装依赖
npm install

# 启动开发模式（启用 DevTools）
npm run dev

# 构建正式发布包（输出至 src-tauri/target/release/bundle/）
npm run build
```

## 项目结构

```text
dsh-desktop/
├── dist/index.html               # 前端加载等待与错误提示页面
├── scripts/                      # 依赖初始化与运行时打包脚本
├── src-tauri/
│   ├── tauri.*.conf.json         # 多平台 Tauri 配置
│   └── src/                      # Rust 后端源码（窗口管理、进程托管、托盘、更新）
├── package.json
└── Cargo.toml
```

## 注意事项

- **首次启动联网**：应用首次启动时若未检测到本地环境，需联网拉取 `dsh` 核心组件，请保持网络连通。
- **自动更新**：支持桌面端应用自动检查并安装更新（Linux 环境仅支持 AppImage 格式）。

## 声明

本项目为开源第三方客户端，仅供学习与便利使用。如有任何问题与建议，欢迎提交 [Issue](https://github.com/MochiNek0/dsh-desktop/issues) 或 Pull Request。

## 许可证

本项目基于 [MIT 许可证](LICENSE) 开源。
