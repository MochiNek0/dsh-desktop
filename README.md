<div align="center">

# dsh desktop

DeepSeek Harness (`dsh web`) 的跨平台桌面客户端

**[官方网站 · 下载](https://dsh-desktop.cc.cd/)** · [English](README.en.md) · **简体中文**

<br/>

[![Website](https://img.shields.io/badge/Website-dsh--desktop.cc.cd-2ea44f?logo=googlechrome&logoColor=white)](https://dsh-desktop.cc.cd/)
[![Release](https://img.shields.io/github/v/release/MochiNek0/dsh-desktop?color=blue)](https://github.com/MochiNek0/dsh-desktop/releases)
![QQ 群](https://img.shields.io/badge/QQ群-1125671315-12B7F5?logo=tencentqq&logoColor=white)
[![Tauri](https://img.shields.io/badge/Tauri-v2-24C8D8?logo=tauri&logoColor=white)](https://tauri.app/)
![Platform](https://img.shields.io/badge/Platform-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey)
[![License: MIT](https://img.shields.io/badge/License-MIT-green.svg)](LICENSE)

<br/>
<br/>

<img src="docs/thumbnail.png" alt="dsh desktop preview" width="850" />

</div>

<br/>

> 非官方项目：基于 [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) 的第三方桌面客户端，与 DeepSeek 官方无隶属关系。欢迎 [Issue](https://github.com/MochiNek0/dsh-desktop/issues) 与 PR。

启动时自动在后台拉起本地 `dsh web` 并嵌入原生窗口，无需开终端、无需管理端口。会话、凭证与配置存放在 `$DSH_HOME`（默认 `~/.dsh`），与命令行里的 `dsh` 是同一份。

## 特性

- **轻量**：Tauri v2 + 系统 WebView，不打包浏览器内核。安装包 Windows 2.3 MB，macOS 5.8 MB，Debian 3.8 MB。
- **跨平台**：Windows / macOS / Linux 同一套体验。
- **开箱即用**：自动检测 Node、按需安装 `dsh`、自动选用空闲回环端口，与终端里手动运行的实例互不干扰；全程无需管理员权限。
- **一切皆插件**：桌面端不改 dsh 一行源码。桌面增强能力（如系统通知）均以随包插件形式供给，卸掉即回到纯净 dsh；内置可视化插件面板并首推 [DSH Market](https://dshmarket.com) 插件市场，安装与卸载插件均不必碰终端。
- **安全模式（坏了能救回来）**：插件在 `dsh web` 绑定端口之前加载，一个会崩的插件会让应用停在加载页，而卸插件的面板恰好在打不开的界面后面。此时加载页会给出「不加载插件启动」：把你装的插件全部摘出层列表（dsh 自带的不动）启动一次，并直接打开面板让你卸掉出问题的那个，之后用菜单里的「重新加载插件」原样装回。
- **原生集成**：主题与界面语言跟随 dsh 自身设置，切换无需重启；支持托盘常驻、开机自启；回合结束或 dsh 等待确认时发送系统通知，点击回到对应会话，允许/拒绝一类的选择可直接在通知上作答。

## 安装

从 **[官网](https://dsh-desktop.cc.cd/)** 或 [Releases](https://github.com/MochiNek0/dsh-desktop/releases) 下载对应系统的安装包，安装后打开即可。若机器上没有可用的 Node，应用会自动弹出「运行环境」面板，一键装一份 Node 24。

| 系统 | 格式 | 体积 | 说明 |
| :--- | :--- | :--- | :--- |
| **Windows** | `.exe`（NSIS） | 2.3 MB | 需 WebView2，缺失时自动引导安装（已验证） |
| **macOS** | `.dmg` | 5.8 MB | 通用二进制，支持 Apple Silicon 与 Intel（已验证） |
| **Linux** | `.deb` / `.AppImage` | 3.8 MB / 78 MB | `.AppImage` 自带 WebKit 故体积较大，但自更新支持最完整（Debian 系已验证） |

## 注意事项

- **Node 版本**：`dsh` 需要 Node.js 22.19.0 或更高。安装包不内置 Node 与 `dsh`，首次启动缺失时需联网拉取。
- **关闭即最小化**：关窗口只收进托盘以免中断任务，退出请用菜单里的「退出 dsh」。
- **自动更新**：启动时静默检查，有更新才提示，下载前先征求同意。
- **macOS 首次运行被拦截**：在访达中右键点按应用选择「打开」，或执行 `xattr -dr com.apple.quarantine /Applications/dsh-desktop.app`。
- **安装 `github:` 形式的插件**：pnpm 默认拦截 git 来源的构建脚本，如报错请按面板提示在 `$DSH_HOME/profiles/web/pnpm-workspace.yaml` 的 `allowBuilds` 中放行。

## 配置

| 环境变量 | 说明 | 默认值 |
| :--- | :--- | :--- |
| `DSH_BIN` | `dsh` 可执行文件的绝对路径，优先级最高，同时跳过 Node 版本检查 | 自动检索 PATH |
| `DSH_HOME` | `dsh` 数据、凭证与配置目录 | `~/.dsh` |

## 开发

需要 Rust 稳定版 1.82+ 与 Node.js 22+。Linux 另需 `libwebkit2gtk-4.1-dev`、`libayatana-appindicator3-dev`、`librsvg2-dev`、`patchelf`、`libxdo-dev`、`libssl-dev`、`build-essential`。

```sh
npm install     # 安装依赖
npm run dev     # 开发模式
npm run build   # 构建发布包，输出至 src-tauri/target/release/bundle/
```

## 交流与反馈

- **QQ 交流群**：`1125671315`（欢迎入群交流使用体验、反馈问题与建议）
- **Issue 反馈**：欢迎在 [GitHub Issues](https://github.com/MochiNek0/dsh-desktop/issues) 提交建议与 Bug

## 相关链接

- **官方网站**：[中文](https://dsh-desktop.cc.cd/) · [English](https://dsh-desktop.cc.cd/en/)
- **上游项目**：[DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)
- **友情链接**：[DSH Market](https://github.com/dsh-market/dsh-market) —— dsh 里的可视化插件市场，浏览、搜索并一键安装社区插件（[dshmarket.com](https://dshmarket.com)）

## 许可证

[MIT](LICENSE)
