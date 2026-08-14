# dsh desktop

> DeepSeek Harness（`dsh web`）的 Tauri 桌面客户端 —— 把浏览器里的 dsh 界面装进原生窗口。
>
> [English](README.en.md) · 中文

启动应用即拉起一个本地 `dsh web` 服务，并把它的界面装进原生窗口。不用自己开终端、记端口、管浏览器标签页；会话、凭据与设置和 CLI 完全共用。

## 特性

- **原生窗口跑 dsh web**：自动拉起本地服务并导航进去，无需手动开浏览器
- **不抢端口**：以 `--port 0` 启动，由系统分配空闲的 loopback 端口，不会和你手动跑的 `dsh web`（3080）冲突，两者可以同时开着
- **会话不被顶掉**：窗口只停留在 dsh 服务所在的 origin 内；指向站外的链接一律交给系统浏览器打开
- **清晰的启动/失败体验**：启动期间显示加载页；dsh 缺失或启动失败时，展示错误信息和它的输出尾部
- **生命周期完整**：退出时结束整棵子进程树，不残留孤儿 node
- **跨平台**：Windows 上已验证；macOS / Linux 按相同的设计工作
- **数据与 CLI 共用**：会话、凭据、设置仍在 `$DSH_HOME`（默认 `~/.dsh`），桌面端不额外存任何东西

## 前置条件

- 已安装 `dsh`（`dsh --version` 能跑通）且在 PATH 中；GUI 会话的 PATH 往往和终端不一样，找不到时用 `DSH_BIN` 指向 dsh 可执行文件的完整路径
- Windows：WebView2 运行时（Win11 自带；安装程序也可引导下载）
- 开发/构建：Rust stable + MSVC 工具链、Node 18+

## 运行

```sh
npm install
npm run dev     # 开发模式，带 devtools
npm run build   # 打包安装程序 → src-tauri/target/release/bundle/
```

当前 bundle 目标为 Windows NSIS；macOS（app/dmg）与 Linux（deb/AppImage）的打包配置可在 `tauri.conf.json` 的基础上补充。

## 工作原理

1. 以用户主目录为工作目录启动 `dsh web --port 0`，让系统分配一个空闲的 loopback 端口，
   因此不会和你手动跑的 `dsh web`（3080）抢端口，两者可以同时开着。
2. 读子进程 stdout，等它打印 `dsh web: http://127.0.0.1:<port>` —— 这一行既是就绪信号，
   也是要加载的地址。期间窗口显示加载页；启动失败或 dsh 提前退出，加载页会显示它的输出尾部。
3. 拿到地址后把窗口导航到该 URL。窗口保持在这个 origin 内；指向站外的链接交给系统浏览器打开，
   不会顶掉你正在进行的会话。
4. 应用退出时结束整棵子进程树（Windows 上是 `cmd.exe` → `dsh.cmd` → node，
   所以用 `taskkill /T`，光杀父进程会留下孤儿 node）。

工作目录只是初始默认值 —— 具体项目目录在界面里用目录选择器选。

## 环境变量

| 变量 | 作用 |
| --- | --- |
| `DSH_BIN` | dsh 可执行文件的完整路径。GUI 会话的 PATH 里找不到 `dsh` 时用它兜底。 |

## 代码结构

```
dist/index.html        加载页 / 错误页（无构建步骤，Rust 侧通过 eval 调它的两个钩子）
src-tauri/src/main.rs   窗口、导航策略、生命周期
src-tauri/src/server.rs 托管的 dsh web 子进程
```

`cargo test` 覆盖 URL 行的解析。

## 路线图 / TODO

按优先级：

- [ ] **自包含安装包** —— 现在依赖系统里已装的 `dsh`，安装包不打包 Node 和 profile 依赖。
      要做到不装 Node 也能跑，需要把 node 运行时 + `@deepseek-ai/dsh` + profile 的
      `node_modules` 作为 sidecar 打进去。体量和复杂度都是另一个量级：
      跨平台二进制、版本与路径管理、更新联动等。
- [ ] **Job Object 兜底** —— 正常退出会 `taskkill /T` 杀掉整棵进程树；但用户强杀应用时
      （`taskkill /F` 不带 `/T`）不会触发清理，会留下孤儿 node。
      方案：用 Windows Job Object 把子进程挂进作业，让内核保证进程树随应用一起消亡
      （约 30 行 unsafe FFI）。
- [ ] **窗口尺寸/位置记忆、托盘、自动更新** —— 记忆窗口位置与大小、系统托盘常驻、
      以及基于 Tauri updater 的自动更新。

## 已知边界

- 需要系统里已装好 `dsh`；自包含安装包见[路线图](#路线图--todo)
- 目前只在 Windows 上做了系统化验证；macOS / Linux 的 bundle 目标尚未配置

## 许可证

[MIT](LICENSE) © 2026 MochiNek0
