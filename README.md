# dsh desktop

> DeepSeek Harness（`dsh web`）的 Tauri 桌面客户端 —— 把浏览器里的 dsh 界面装进原生窗口。
>
> [English](README.en.md) · 中文
>
> **非官方项目**：基于 DeepSeek Harness 的第三方桌面封装，与 DeepSeek 官方无关。详见[声明](#声明)。

<br>

![dsh desktop](docs/thumbnail.png)

<br>

启动应用即拉起一个本地 `dsh web` 服务，并把它的界面装进原生窗口。不用自己开终端、记端口、管浏览器标签页；会话、凭据与设置和 CLI 完全共用（都在 `$DSH_HOME`，默认 `~/.dsh`）。

## 特性

- **开箱即用**：安装时检测机器上的 Node.js，没有（或低于 22.22.3）就装一份到用户目录，再用 `npm install -g @deepseek-ai/dsh` 装 dsh，默认源不通会自动换镜像；全程不需要管理员权限
- **不抢端口**：以 `--port 0` 启动，系统分配空闲的 loopback 端口，和你手动跑的 `dsh web`（3080）可以同时开着
- **无边框窗口**：最小化/最大化/关闭以 macOS 那三颗圆点画在页面左上角（右上角留给 dsh 自己的控件），静止时半透明、hover 才显出图标
- **终端里也有 `dsh`**：dsh 是全局 npm 包，装完后终端里直接敲 `dsh` 就能用，和 app 跑的是同一份。只在机器上原本没有 dsh 时才装，绝不挤占你自己装的那份
- **dsh 保持最新**：启动时查 npm，有新版先征求同意再下载；你自己装的那份只提醒、不代劳
- **会话不被顶掉**：窗口只停留在 dsh 服务所在的 origin 内，站外链接交给系统浏览器
- **托盘与开机自启动**：关闭窗口收进托盘而不是中断会话；托盘菜单里可以开启开机自启动，这样启动的那次会静默待在托盘里，不弹窗也不查更新
- **单实例 + 干净退出**：一台机器只跑一个；退出时结束整棵子进程树，被强杀时由 Windows Job Object 兜底，不残留孤儿 node
- **自动更新**：基于 Tauri updater 的签名更新，下载与重启都会先征求同意

## 安装

从 [Releases](../../releases) 下载安装包。需要能联网 —— 安装过程要从 npm 拉 dsh（约 185 MB，几分钟），机器上没有 Node.js 的话还要再拉一个 Node（约 36 MB）。Windows 还需要 WebView2 运行时（Win11 自带，缺失时安装程序会自动下载）。

目前只提供 Windows 安装包，macOS / Linux 的打包目标尚未配置。

## 开发

需要 Rust stable + MSVC 工具链、Node 18+。

```sh
npm install
npm run dev            # 开发模式，带 devtools（用 PATH 上的 dsh；没有的话首次启动会自己装）
npm run build          # 打包安装程序 → src-tauri/target/release/bundle/
```

`npm run build` 会先跑 `scripts/bundle-runtime.mjs`，它把 `scripts/install-deps.ps1` 复制进 `src-tauri/resources/`，并（在缺失时）跑一次带追踪的 dsh 启动来生成预热清单。安装包本身不带 Node 也不带 dsh。

要出一个能被自动更新到的版本，构建时提供签名私钥（在 `~/.tauri/dsh-desktop.key`，**不在仓库里**）：

```sh
TAURI_SIGNING_PRIVATE_KEY=~/.tauri/dsh-desktop.key TAURI_SIGNING_PRIVATE_KEY_PASSWORD= npm run build
```

> 这一行**不能照抄进 PowerShell** —— `$env:X = ""` 在 PowerShell 里是删除变量，密码传不进去，构建会卡在密码提示上。用 Git Bash 跑。
>
> 构建前记得退出正在运行的 app 和上一次构建出来的安装程序，两者都锁着文件，失败信息很容易被当成构建成功。

然后把安装包、`.sig` 和 `latest.json` 一起传到 GitHub Release。

## 工作原理

1. 先把窗口建出来显示加载页，后台检查 dsh 有没有新版（这时还没有 dsh 进程在跑，替换它最安全）；机器上要是压根没有 dsh，就在这里装一份，进度显示在加载页上。
2. 按 `DSH_BIN` → npm 全局前缀里的 `dsh` 的顺序找到 dsh，启动 `dsh web --port 0`。
3. 读它的 stdout，等 `dsh web: http://127.0.0.1:<port>` —— 这一行既是就绪信号也是要加载的地址。启动失败时加载页会显示输出尾部。
4. 窗口导航到该 URL，并保持在这个 origin 内。
5. 退出时用 `taskkill /T` 结束整棵子进程树；子进程一起来就被挂进一个设了 `KILL_ON_JOB_CLOSE` 的 Job Object，应用被强杀、根本走不到清理代码时由它兜底。

窗口的亮/暗跟随 dsh 自己的主题设置（`$DSH_HOME/settings.yaml` 的 `ui-theme.preference`），窗口建出来时读一次，加载页不会先闪一下相反的颜色。

窗口按钮不走 Tauri IPC（那等于把 IPC 权限交给 dsh 页面里的每一行 JS），而是导航到 `dsh-window://close` 这样的自定义 scheme，在 `on_navigation` 里认出来、执行、取消掉这次导航。

### Node 和 dsh 装在哪

安装包既不带 Node 也不带 dsh，两者都由 `scripts/install-deps.ps1` 在安装时装上 —— 应用在启动时发现缺东西也会跑同一个脚本，所以只有一份实现。全程不需要管理员权限。

**Node**：机器上有 22.22.3 或更高版本就直接用，绝不动它。没有的话从 nodejs.org 下载官方 standalone zip（失败依次换 npmmirror、阿里云、华为云），校验 SHA256 后解压到 `%LOCALAPPDATA%\ai.deepseek.dsh.desktop\node\`。

**dsh**：`npm install -g @deepseek-ai/dsh`，落在上面那个 Node 的全局前缀里；用系统 Node 时就落在你自己的 npm 前缀里。前缀目录会被加进 `HKCU\Environment` 的 PATH（前置），所以终端里的 `dsh` 和 app 跑的是同一份。

**PATH 上已经有 `dsh` 就整个跳过** —— 已经有一份能用的 dsh，再装 327 MB 没有意义，那份始终归你管，更新检查只提醒、不代劳。

更新检查最多每 6 小时一次（超时 15 秒），发现新版会弹窗告知版本号和体积，同意后就地跑一次 `npm install -g @deepseek-ai/dsh@latest`，装完直接继续启动，不需要重启应用。点「跳过此版本」之后不再为这个版本打扰。

卸载时**分别问**要不要卸载 dsh、要不要卸载本应用装的 Node.js（选了删 Node 就会连 dsh 一起删，没有 Node 的 dsh 跑不起来），最后再问一次要不要连 `$DSH_HOME` 一起删，默认都保留。你自己装的 Node 或 dsh 永远不会被动。app 更新和手动重装不会触发这些询问。

## 环境变量

| 变量 | 作用 |
| --- | --- |
| `DSH_BIN` | dsh 可执行文件的完整路径。优先级最高，用来盖掉 PATH 上的那份。 |
| `DSH_HOME` | dsh 的数据目录，默认 `~/.dsh`。 |

## 代码结构

```
dist/index.html               加载页 / 错误页（无构建步骤，Rust 侧通过 eval 调它的钩子）
scripts/install-deps.ps1      检测并安装 Node 与 dsh；安装程序和应用共用这一份
scripts/bundle-runtime.mjs    构建前把上面那个脚本放进 resources/，并生成启动预热清单
scripts/boot-trace/           追踪一次 dsh 启动读了哪些文件，预热清单由此而来
src-tauri/installer-hooks.nsh 安装/卸载时调用 install-deps.ps1，以及对 $DSH_HOME 的处理
src-tauri/src/main.rs         窗口、导航策略、托盘、生命周期
src-tauri/src/controls.rs     注入页面的无边框窗口按钮与拖拽带
src-tauri/src/theme.rs        从 dsh 的设置里读亮/暗，开窗时用一次
src-tauri/src/server.rs       托管的 dsh web 子进程；Job Object 兜底
src-tauri/src/dsh.rs          dsh 的定位与版本比较，以及何时调 install-deps.ps1
src-tauri/src/warm.rs         按预热清单并行预读，抵消 Defender 首次扫描
src-tauri/src/update.rs       应用自身的自动更新
```

## 路线图

- [x] 机器上没有 Node 也能装（安装时检测，缺了就装一份到用户目录）
- [x] Job Object 兜底、托盘与开机自启动、自动更新
- [ ] macOS / Linux 的 bundle 目标
- [ ] 发布流水线（目前 `latest.json` 与签名产物要手工传到 Release）

## 已知边界

- **安装要联网，而且不快**：dsh 的依赖树是 587 个包 / 185 MB 压缩流量 / 解压后 327 MB / 33k 文件，2 MB/s 的连接上约 4 分钟；机器上没有 Node 的话前面还要再下 36 MB。全部镜像都失败的话安装程序会明说，不会假装装好了 —— 下次启动应用时它会再试一次
- **第一次启动比之后慢**：dsh 要 import 的文件第一次从杀软没见过的路径读过去，Defender 会逐个扫描 —— 实测冷启动 14 秒，文件热了之后 1.6 秒。应用启动时会按预热清单多线程预读那批文件，把冷启动压到 4 秒上下；想彻底避开的话给安装目录加排除项（管理员 PowerShell，**路径换成你实际装的那个**）：

  ```powershell
  Add-MpPreference -ExclusionPath "$env:LOCALAPPDATA\dsh-desktop", "$env:LOCALAPPDATA\ai.deepseek.dsh.desktop"
  ```

- 目前只在 Windows 上验证；`install-deps.ps1` 是 PowerShell 脚本，macOS / Linux 需要另写一份
- dsh 更新走 `npm install -g`，npm 在替换旧树期间新旧两份并存，磁盘峰值约 650 MB
- 安装完 Node 后 PATH 的变更要等新开的终端才生效；应用自己不受影响 —— 它从 `bootstrap.json` 里读脚本记下的路径，不依赖继承来的 PATH
- `dsh.cmd` 用 `%*` 转发参数，从 PowerShell 传入带 `&` `|` `^` 的参数会被 cmd 二次解析弄坏（Git Bash 的 shim 没这问题）
- 卸载若保留 `$DSH_HOME`，其中指向已删目录的 junction 会变成死链；再跑一次任意 dsh 就会自动修好

## 声明

本项目是 [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) 的**第三方**桌面客户端，
由个人开发者维护，与 DeepSeek 没有任何隶属、合作或背书关系，**不是 DeepSeek 的官方产品**。
它只做桌面封装 —— 进程管理、窗口与托盘、系统集成，不改动 dsh 本身的功能。

「DeepSeek」是 DeepSeek 的商标，本项目仅在说明用途时指代性地使用该名称。
应用图标是本项目自己画的，没有使用 DeepSeek 的任何标识。

使用本项目遇到的问题请在本仓库提 issue，不要提给 DeepSeek 官方。

## 许可证

[MIT](LICENSE) © 2026 MochiNek0
