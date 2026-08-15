# dsh desktop

> DeepSeek Harness（`dsh web`）的 Tauri 桌面客户端 —— 把浏览器里的 dsh 界面装进原生窗口。
>
> [English](README.en.md) · 中文
>
> **非官方项目**：基于 DeepSeek Harness 的第三方桌面封装，与 DeepSeek 官方无关。详见[声明](#声明)。

启动应用即拉起一个本地 `dsh web` 服务，并把它的界面装进原生窗口。不用自己开终端、记端口、管浏览器标签页；会话、凭据与设置和 CLI 完全共用。

## 特性

- **原生窗口跑 dsh web**：自动拉起本地服务并导航进去，无需手动开浏览器
- **自包含**：安装包内置 Node 运行时和 `@deepseek-ai/dsh`，机器上没装 Node 也能跑
- **dsh 保持最新**：启动后台查 npm，有新版就征求同意再下载，不偷跑流量；内置那份永远是能用的保底
- **不抢端口**：以 `--port 0` 启动，由系统分配空闲的 loopback 端口，不会和你手动跑的 `dsh web`（3080）冲突，两者可以同时开着
- **会话不被顶掉**：窗口只停留在 dsh 服务所在的 origin 内；指向站外的链接一律交给系统浏览器打开
- **清晰的启动/失败体验**：启动期间显示加载页；dsh 缺失或启动失败时，展示错误信息和它的输出尾部
- **主题跟随 dsh**：标题栏和加载页读 dsh 自己的亮/暗设置，不会出现深色界面配浅色窗框；在界面里改了主题，窗框立刻跟上
- **生命周期完整**：正常退出结束整棵子进程树；被强杀时由 Windows Job Object 兜底，任何情况下都不残留孤儿 node
- **窗口记忆与托盘**：记住窗口位置与大小；关闭窗口收进托盘而不是中断会话，退出走托盘菜单
- **自动更新**：基于 Tauri updater 的签名更新，下载与重启都会先征求同意
- **跨平台**：Windows 上已验证；macOS / Linux 按相同的设计工作
- **数据与 CLI 共用**：会话、凭据、设置仍在 `$DSH_HOME`（默认 `~/.dsh`），桌面端不额外存任何东西

## 前置条件

- **运行安装包**：什么都不需要。Node 与 dsh 都在包里。Windows 还需要 WebView2 运行时（Win11 自带；安装程序也可引导下载）
- **开发/构建**：Rust stable + MSVC 工具链、Node 18+

## 运行

```sh
npm install
npm run dev            # 开发模式，带 devtools（不打包运行时，用 PATH 上的 dsh）
npm run bundle:runtime # 单独暂存内置运行时（构建时会自动跑）
npm run build          # 打包安装程序 → src-tauri/target/release/bundle/
```

`npm run build` 会先执行 `scripts/bundle-runtime.mjs`：下载官方 Node 二进制、把 `@deepseek-ai/dsh`
装进 `src-tauri/resources/`，再交给 Tauri 打包。产物已暂存且版本一致时会跳过。
这一步只为**当前平台**打包——npm 会按执行安装的机器解析原生可选依赖，所以 Windows 安装包必须在
Windows 上构建，macOS 的在 macOS 上构建。

当前 bundle 目标为 Windows NSIS；macOS（app/dmg）与 Linux（deb/AppImage）的打包配置可在 `tauri.conf.json` 的基础上补充。

## 发布与更新

自动更新走 Tauri updater：应用启动后台检查 `tauri.conf.json` 里配置的端点，
托盘菜单的「检查更新…」可手动触发。要发一个能被更新到的版本：

1. 签名私钥在 `~/.tauri/dsh-desktop.key`（**不在仓库里**，丢了就没法再签更新包）。
   对应公钥已写进 `tauri.conf.json` 的 `plugins.updater.pubkey`。
2. 构建时提供私钥：

   ```sh
   TAURI_SIGNING_PRIVATE_KEY=~/.tauri/dsh-desktop.key npm run build
   ```

   这个变量收密钥本身或者密钥文件的路径，两者都认；没有 `..._PATH` 这个变体。
   漏了它，安装包照样出得来，只是最后签名那步会失败退出，没有 `.sig`。

   密钥生成时没设密码；要加密码就重新生成一对，并同步更新配置里的公钥。
3. 非交互地跑（CI、后台任务）再加一个空密码：

   ```sh
   TAURI_SIGNING_PRIVATE_KEY=~/.tauri/dsh-desktop.key TAURI_SIGNING_PRIVATE_KEY_PASSWORD= npm run build
   ```

   密钥没设密码，但 CLI 仍然会问一次；stdin 不是终端时它就一直等下去，
   看起来和编译卡死一模一样（最后一行停在 `expect a prompt for password`）。
4. 构建前退出正在运行的 app。它锁着 `target/release/dsh-desktop.exe`，
   打包器读不了这个文件，会以 `os error 32`（另一个程序正在使用此文件）失败。
   注意失败之前安装包已经写出来了，只是没签名，很容易被当成构建成功。
5. 把安装包、`.sig` 文件和一份 `latest.json` 一起传到 GitHub Release，
   使得 `releases/latest/download/latest.json` 可访问。

## 工作原理

1. 按 `DSH_BIN` → app 管理的 dsh（内置与已下载的取版本高者）→ PATH 上的 `dsh` 的顺序找到 dsh，
   以用户主目录为工作目录启动 `dsh web --port 0`，让系统分配一个空闲的 loopback 端口，
   因此不会和你手动跑的 `dsh web`（3080）抢端口，两者可以同时开着。
   这一步在建窗口**之前**做：dsh 起来要一两秒，WebView2 初始化也要，两件事互不相干，可以同时进行。
2. 读子进程 stdout，等它打印 `dsh web: http://127.0.0.1:<port>` —— 这一行既是就绪信号，
   也是要加载的地址。期间窗口显示加载页；启动失败或 dsh 提前退出，加载页会显示它的输出尾部。
3. 拿到地址后把窗口导航到该 URL。窗口保持在这个 origin 内；指向站外的链接交给系统浏览器打开，
   不会顶掉你正在进行的会话。
4. 应用退出时结束整棵子进程树（`taskkill /T`，光杀父进程会留下孤儿 node）。
   同时子进程一开始就被挂进一个 Windows Job Object，设了 `KILL_ON_JOB_CLOSE`：
   进程死了句柄就由内核关闭，因此即使应用被强杀、根本没走到清理代码，进程树也会一起消亡。

工作目录只是初始默认值 —— 具体项目目录在界面里用目录选择器选。

窗口的亮/暗不是另一套设置：dsh 把界面主题存在 `$DSH_HOME/settings.yaml` 的
`ui-theme.preference`（`light`/`dark`/`system`），窗口读同一个字段，建窗口时就带上，
所以标题栏和加载页不会先闪一下相反的颜色。之后每 50 ms 看一眼这个文件的时间戳，
在界面里换主题，窗框跟着换，不用重启 —— dsh 点下就写文件，这个间隔就是页面变色和
窗框跟上之间的全部差距，肉眼能捕捉到的差距会被看成「分两步」。

### dsh 的两份安装

app 管着两份 dsh，谁的版本高就跑谁：

| 位置 | 来源 |
| --- | --- |
| `<安装目录>/resources/dsh/` | 安装包自带。永远在、永远能用、永不变化，是离线首启和一切失败路径的保底 |
| `%LOCALAPPDATA%\ai.deepseek.dsh.desktop\dsh\` | 后来从 npm 下载的更新版 |

**界面起来之后**（不是启动那一刻，免得和正在启动的 dsh 抢磁盘和网络）后台跑一次
`npm view @deepseek-ai/dsh version`（走 npm 而不是自己发请求，是为了继承用户
的 `.npmrc`——私有 registry 和公司代理才不会失效）。发现新版就弹窗告知版本号和体积，
**用户同意才下载**。装到 `dsh.next/`，**下次启动**时才换进去——正在跑的服务没法热替换，
而且启动那一刻没有任何进程占着目录，是 Windows 上唯一能安全改名的时机。

点「跳过此版本」会把版本号记进 `dsh-skipped`，之后不再为**这个**版本打扰；
再出新版还是会问。255 MB 的提示每次启动都弹一遍，很快就会让人想卸载。

任何一步失败都只是回到内置那份，不影响使用。`DSH_BIN` 优先级仍然最高，会盖掉这套机制。

### 内置运行时与 profile 依赖

内置的 dsh 装在 `resources/dsh/` 下，用内置的 `resources/runtime/node` 启动。
dsh 自己会在每次启动时把它的依赖闭包以 junction 的形式链接进
`$DSH_HOME/profiles/node_modules`，并且**幂等地重指向已移动的安装位置** ——
所以内置 dsh 一跑起来，profile 的依赖就自动指到安装目录里，
既不需要额外接线，首次启动也不需要联网。反过来，之后再用系统 CLI 跑一次，链接又会指回去。

## 环境变量

| 变量 | 作用 |
| --- | --- |
| `DSH_BIN` | dsh 可执行文件的完整路径。优先级最高，用来盖掉内置运行时、改用自己那份 dsh。 |

## 代码结构

```
dist/index.html            加载页 / 错误页（无构建步骤，Rust 侧通过 eval 调它的两个钩子）
scripts/bundle-runtime.mjs 构建前暂存内置的 node 运行时与 dsh
src-tauri/src/main.rs      窗口、导航策略、托盘、生命周期
src-tauri/src/server.rs    托管的 dsh web 子进程；Job Object 兜底
src-tauri/src/dsh.rs       两份 dsh 安装的选择、版本比较与运行期更新
src-tauri/src/update.rs    应用自身的自动更新
```

`cargo test` 覆盖 URL 行的解析。

## 路线图 / TODO

- [x] **自包含安装包** —— Node 运行时与 `@deepseek-ai/dsh` 已作为资源打进安装包。
- [x] **Job Object 兜底** —— 强杀应用不再残留孤儿 node。
- [x] **窗口尺寸/位置记忆、托盘、自动更新**
- [ ] **macOS / Linux 的 bundle 目标** —— 打包配置尚未补齐；打包脚本本身已是跨平台的，
      但必须在目标平台上执行。
- [ ] **发布流水线** —— 目前 `latest.json` 与签名产物要手工传到 Release，还没有 CI 工作流。

## 已知边界

- 自包含是有代价的：内置资源解压后约 350 MB（node 93 MB + dsh 依赖树 255 MB / 33k 文件），
  NSIS 压缩后安装包约 55 MB
- 装好之后第一次启动明显慢：内置 dsh 是 33k 个文件，第一次从一条杀软没见过的路径
  把整棵依赖树读一遍，Windows Defender 的实时扫描能把它拖到几十秒；同一个路径上
  之后再启动就回到 1–2 秒。介意的话给安装目录加排除项 —— 管理员 PowerShell，
  **路径换成你安装时实际选的那个**（默认 `%LOCALAPPDATA%\dsh-desktop`）：

  ```powershell
  Add-MpPreference -ExclusionPath "$env:LOCALAPPDATA\dsh-desktop", "$env:LOCALAPPDATA\ai.deepseek.dsh.desktop"
  ```

  安装程序不替你做这件事：把一个目录从实时防护里挖掉是用户自己的安全取舍，
  而且按用户安装本来就没有管理员权限，想做也做不了
- 目前只在 Windows 上做了系统化验证；macOS / Linux 的 bundle 目标尚未配置
- 安装包必须在目标平台上构建：npm 按执行安装的机器解析原生可选依赖
- node 版本在 `scripts/bundle-runtime.mjs` 里写死，升级要改代码重新发版。
  dsh 的**内置**版本同理，但运行期会自动追上 npm 上的最新版，所以那个 pin 只决定保底版本
- dsh 更新一次要下 ~255 MB，且和内置那份并存，磁盘峰值约 600 MB
- 卸载会删干净安装目录（含内置运行时），`$DSH_HOME`（`~/.dsh`）按设计保留 —— 会话、凭据、
  设置与 CLI 共用，不该被桌面端的卸载带走。副作用是 `~/.dsh/profiles/node_modules` 里
  指向安装目录的 junction 会变成死链；再跑一次任意 dsh 安装（系统 CLI 或重装桌面端）
  就会自动重指向修好，不需要手工清理
- Job Object 的挂载发生在子进程启动之后，中间有一个极短的窗口，
  此刻新建的孙进程不会进作业；正常退出路径本来就覆盖整棵树

## 声明

本项目是 [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) 的**第三方**桌面客户端，
由个人开发者维护，与 DeepSeek 没有任何隶属、合作或背书关系，**不是 DeepSeek 的官方产品**。
它只做桌面封装 —— 进程管理、窗口与托盘、系统集成，不改动 dsh 本身的功能。

「DeepSeek」是 DeepSeek 的商标，本项目仅在说明用途时指代性地使用该名称。
应用图标是本项目自己画的，没有使用 DeepSeek 的任何标识。

使用本项目遇到的问题请在本仓库提 issue，不要提给 DeepSeek 官方。

## 许可证

[MIT](LICENSE) © 2026 MochiNek0
