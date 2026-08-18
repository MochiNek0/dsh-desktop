# dsh desktop

> DeepSeek Harness（`dsh web`）的跨平台 Tauri 桌面客户端。
>
> [English](README.en.md) · 中文
>
> **非官方项目**：基于 DeepSeek Harness 的第三方桌面封装，与 DeepSeek 官方无关。详见[声明](#声明)。

<br>

![dsh desktop](docs/thumbnail.png)

<br>

启动应用即可自动拉起本地 `dsh web` 服务并内嵌至原生窗口。无需手动打开终端或管理端口，会话、凭证与配置均和 CLI 完全共享（位于 `$DSH_HOME`，默认 `~/.dsh`）。

## ✨ 特性

- **开箱即用**：自动检测并配置 Node.js 及 `dsh` 环境，无需管理员权限。
- **无感共存**：随机分配端口启动，与手动运行的 `dsh web` 互不干扰。
- **轻量原生**：无边框现代 UI、跟随主题、支持系统托盘及开机自启。
- **完整共享**：终端全局可用同一 `dsh` 命令，支持启动检查与一键更新。
- **优雅退出**：单实例运行，退出时自动清理所有子进程。

## 📦 安装

前往 [Releases](https://github.com/MochiNek0/dsh-desktop/releases) 下载对应平台的安装包：
- **Windows**: `.exe` 安装包（需 WebView2，系统缺失会自动下载）
- **macOS**: `.dmg`（通用二进制，支持 Intel 与 Apple Silicon）
- **Linux**: `.AppImage` / `.deb`

> **macOS 提示**：首次打开如遇拦截，请右键选择「打开」，或在终端执行：
> ```sh
> xattr -dr com.apple.quarantine /Applications/dsh-desktop.app
> ```

## 🛠️ 开发与构建

### 运行环境
- Rust stable
- Node.js 18+

### 常用命令
```sh
npm install
npm run dev    # 开发模式（带 DevTools）
npm run build  # 构建安装包（输出至 src-tauri/target/release/bundle/）
```

## ⚙️ 环境变量

| 变量 | 说明 | 默认值 |
| --- | --- | --- |
| `DSH_BIN` | 指定 `dsh` 可执行文件的完整路径（优先级最高） | - |
| `DSH_HOME` | `dsh` 数据与配置目录 | `~/.dsh` |

## 📂 项目结构

```text
dist/index.html               加载与错误页
scripts/                      依赖安装脚本与运行时打包脚本
src-tauri/tauri.*.conf.json   多平台 Tauri 配置
src-tauri/src/                Rust 后端源码（窗口、进程托管、托盘、更新）
```

## ⚠️ 注意事项

- **首次安装/启动**：应用需要联网拉取 `dsh` 及运行环境，请保持网络畅通。
- **自动更新**：支持应用自更新（Linux 仅限 AppImage 格式）。

## 📄 声明

本项目是 [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) 的第三方客户端，由个人开发者维护，与 DeepSeek 无隶属或合作关系。
问题反馈请在本仓库提交 Issue。

## 📜 许可证

[MIT](LICENSE) © 2026 MochiNek0
