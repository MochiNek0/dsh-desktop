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

- **开箱即用**：Windows 在安装时、macOS / Linux 在首次启动时检测机器上的 Node.js，没有（或低于 22.22.3）就装一份到用户目录，再用 `npm install -g @deepseek-ai/dsh` 装 dsh，默认源不通会自动换镜像；全程不需要管理员权限
- **不抢端口**：以 `--port 0` 启动，系统分配空闲的 loopback 端口，和你手动跑的 `dsh web`（3080）可以同时开着
- **无边框窗口**：最小化/最大化/关闭以 macOS 那三颗圆点画在页面左上角（右上角留给 dsh 自己的控件），静止时半透明、hover 才显出图标
- **终端里也有 `dsh`**：dsh 是全局 npm 包，装完后终端里直接敲 `dsh` 就能用，和 app 跑的是同一份。只在机器上原本没有 dsh 时才装，绝不挤占你自己装的那份
- **dsh 保持最新**：启动时查 npm，有新版先征求同意再下载；你自己装的那份只提醒、不代劳
- **会话不被顶掉**：窗口只停留在 dsh 服务所在的 origin 内，站外链接交给系统浏览器
- **托盘与开机自启动**：关闭窗口收进托盘而不是中断会话；托盘菜单里可以开启开机自启动，这样启动的那次会静默待在托盘里，不弹窗也不查更新
- **单实例 + 干净退出**：一台机器只跑一个；退出时结束整棵子进程树（Windows 用 `taskkill /T`，macOS / Linux 杀整个进程组），Windows 上被强杀还有 Job Object 兜底，不残留孤儿 node
- **自动更新**：基于 Tauri updater 的签名更新，下载与重启都会先征求同意，三个平台共用一份 `latest.json`

## 安装

从 [Releases](../../releases) 下载对应平台的包：Windows 是 `.exe` 安装程序，macOS 是 `.dmg`（同一份同时支持 Apple Silicon 和 Intel），Linux 是 `.deb` 或 `.AppImage`。

需要能联网 —— 安装过程要从 npm 拉 dsh（约 185 MB，几分钟），机器上没有 Node.js 的话还要再拉一个 Node（约 36 MB）。Windows 还需要 WebView2 运行时（Win11 自带，缺失时安装程序会自动下载）；Linux 需要 WebKitGTK 4.1 与 libayatana-appindicator（`.deb` 会声明依赖，AppImage 需要自己装）。

macOS 的包**没有签名也没有公证**（那需要付费的 Apple 开发者账号），第一次打开会被 Gatekeeper 拦下。绕过的办法是右键点图标选「打开」，或者：

```sh
xattr -dr com.apple.quarantine /Applications/dsh-desktop.app
```

## 开发

需要 Rust stable、Node 18+，以及各平台的构建依赖：Windows 是 MSVC 工具链，macOS 是 Xcode 命令行工具，Linux 是 `libwebkit2gtk-4.1-dev`、`libayatana-appindicator3-dev`、`librsvg2-dev`、`patchelf`。

```sh
npm install
npm run dev            # 开发模式，带 devtools（用 PATH 上的 dsh；没有的话首次启动会自己装）
npm run build          # 打包 → src-tauri/target/release/bundle/
```

打包目标按平台分在 `tauri.conf.json`（Windows，NSIS）、`tauri.macos.conf.json`（app + dmg）和 `tauri.linux.conf.json`（deb + AppImage）里，Tauri 会自己按当前系统合并。**只能给自己所在的平台打包** —— 交叉编译要一整套目标系统的 sysroot，所以三个平台的包由 CI 分别构建。

`npm run build` 会先跑 `scripts/bundle-runtime.mjs`，它把两份安装脚本复制进 `src-tauri/resources/`，并（在缺失时）跑一次带追踪的 dsh 启动来生成预热清单。安装包本身不带 Node 也不带 dsh。

### 发布

推一个 `v` 开头的 tag，`.github/workflows/release.yml` 会在三个平台上各构建一次，签名，把安装包和合并好的 `latest.json` 传到一个**草稿** Release：

```sh
# 版本号有三处，改成一样的：package.json、src-tauri/tauri.conf.json、src-tauri/Cargo.toml
# 装包和 latest.json 用的是 tauri.conf.json 里的那个
git commit -am 'chore: 0.2.0' && git tag v0.2.0 && git push --follow-tags
```

草稿是故意的：更新端点指向 `releases/latest`，一旦发布，所有已安装的副本下次启动就会看到它。确认产物没问题再点发布。

签名私钥不在仓库里，走仓库的 Actions secrets —— 只有一个：

| Secret | 内容 |
| --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` | `~/.tauri/dsh-desktop.key` 的**文件内容**（不是路径） |

**没有** `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` 是故意的：这把 key 没有密码，而 GitHub 不接受空值的 secret；workflow 里引用一个不存在的 secret 会渲染成空字符串，环境变量照样被定义为空，签名器要的正是这个。哪天给 key 加了密码，再把这个 secret 建上。

要在本地出一个能被自动更新到的版本，构建时把私钥传进去：

```sh
TAURI_SIGNING_PRIVATE_KEY=~/.tauri/dsh-desktop.key TAURI_SIGNING_PRIVATE_KEY_PASSWORD= npm run build
```

> 这一行**不能照抄进 PowerShell** —— `$env:X = ""` 在 PowerShell 里是删除变量，密码传不进去，构建会卡在密码提示上。用 Git Bash 跑。
>
> 构建前记得退出正在运行的 app 和上一次构建出来的安装程序，两者都锁着文件，失败信息很容易被当成构建成功。

## 工作原理

1. 先把窗口建出来显示加载页，后台检查 dsh 有没有新版（这时还没有 dsh 进程在跑，替换它最安全）；机器上要是压根没有 dsh，就在这里装一份，进度显示在加载页上。
2. 按 `DSH_BIN` → npm 全局前缀里的 `dsh` 的顺序找到 dsh，启动 `dsh web --port 0`。
3. 读它的 stdout，等 `dsh web: http://127.0.0.1:<port>` —— 这一行既是就绪信号也是要加载的地址。启动失败时加载页会显示输出尾部。
4. 窗口导航到该 URL，并保持在这个 origin 内。
5. 退出时结束整棵子进程树。Windows 上用 `taskkill /T`，子进程一起来就被挂进一个设了 `KILL_ON_JOB_CLOSE` 的 Job Object，应用被强杀、根本走不到清理代码时由它兜底；macOS / Linux 上子进程在 exec 前 `setpgid` 成自己那组的组长，退出时一个负 PID 把整组带走 —— 但没有内核级的兜底，见[已知边界](#已知边界)。

窗口的亮/暗跟随 dsh 自己的主题设置（`$DSH_HOME/settings.yaml` 的 `ui-theme.preference`），窗口建出来时读一次，加载页不会先闪一下相反的颜色。

窗口按钮不走 Tauri IPC（那等于把 IPC 权限交给 dsh 页面里的每一行 JS），而是导航到 `dsh-window://close` 这样的自定义 scheme，在 `on_navigation` 里认出来、执行、取消掉这次导航。

### Node 和 dsh 装在哪

安装包既不带 Node 也不带 dsh，两者都由一份安装脚本装上：Windows 是 `scripts/install-deps.ps1`，macOS / Linux 是 `scripts/install-deps.sh`。两份脚本的参数、输出格式和逻辑一一对应，应用在启动时发现缺东西跑的也是它们，所以每个平台只有一份实现。全程不需要管理员权限。

时机上有个区别：Windows 由安装程序在安装时跑一次（`installer-hooks.nsh`），macOS / Linux 上没有可用的安装钩子 —— `.dmg` 和 `.AppImage` 压根没有，`.deb` 的钩子以 root 身份跑，而这是要给**当前用户**装的东西 —— 所以那边由首次启动来做，进度显示在加载页上，和 Windows 联网失败后的回退路径是同一条。

**Node**：机器上有 22.22.3 或更高版本就直接用，绝不动它。没有的话从 nodejs.org 下载官方 standalone 包（失败依次换 npmmirror、阿里云、华为云），校验 SHA256 后解压到应用的数据目录：

| 平台 | 位置 |
| --- | --- |
| Windows | `%LOCALAPPDATA%\ai.deepseek.dsh.desktop\node\` |
| macOS | `~/Library/Application Support/ai.deepseek.dsh.desktop/node/` |
| Linux | `~/.local/share/ai.deepseek.dsh.desktop/node/` |

**dsh**：`npm install -g @deepseek-ai/dsh`，落在上面那个 Node 的全局前缀里；用系统 Node 时就落在你自己的 npm 前缀里 —— 除非那个前缀只有 root 能写（Linux 发行版自带的 Node 通常如此），这时会退到应用数据目录下的 `npm/` 前缀，绝不去要密码。

前缀里的可执行目录会被加进 PATH（前置），所以终端里的 `dsh` 和 app 跑的是同一份。Windows 改的是 `HKCU\Environment`；macOS / Linux 是往 `~/.profile`（zsh 还有 `~/.zshrc`，已存在的 `~/.bashrc`）追加一段带 `# >>> dsh desktop >>>` 标记的块，卸载时按标记原样删掉。

**PATH 上已经有 `dsh` 就整个跳过** —— 已经有一份能用的 dsh，再装 327 MB 没有意义，那份始终归你管，更新检查只提醒、不代劳。

更新检查最多每 6 小时一次（超时 15 秒），发现新版会弹窗告知版本号和体积，同意后就地跑一次 `npm install -g @deepseek-ai/dsh@latest`，装完直接继续启动，不需要重启应用。点「跳过此版本」之后不再为这个版本打扰。

卸载时，Windows 的卸载程序**分别问**要不要卸载 dsh、要不要卸载本应用装的 Node.js（选了删 Node 就会连 dsh 一起删，没有 Node 的 dsh 跑不起来），最后再问一次要不要连 `$DSH_HOME` 一起删，默认都保留。你自己装的 Node 或 dsh 永远不会被动。app 更新和手动重装不会触发这些询问。

macOS / Linux 上没有卸载程序可以问 —— 删掉 .app / 卸掉 .deb 只会删掉应用本身。要把它装的东西一起清掉，手动跑同一个脚本（macOS 把路径换成 `/Applications/dsh-desktop.app/Contents/Resources/resources/install-deps.sh`）：

```sh
sh /usr/lib/dsh-desktop/resources/install-deps.sh -Mode uninstall -RemoveDsh -RemoveNode
```

它遵守和 Windows 卸载程序一样的规矩：只删自己装的那份，`$DSH_HOME` 不动。AppImage 里的那份随镜像挂载，用完就没了，直接拿仓库里的 `scripts/install-deps.sh` 跑效果一样。

## 环境变量

| 变量 | 作用 |
| --- | --- |
| `DSH_BIN` | dsh 可执行文件的完整路径。优先级最高，用来盖掉 PATH 上的那份。 |
| `DSH_HOME` | dsh 的数据目录，默认 `~/.dsh`。 |

## 代码结构

```
dist/index.html               加载页 / 错误页（无构建步骤，Rust 侧通过 eval 调它的钩子）
scripts/install-deps.ps1      检测并安装 Node 与 dsh（Windows）；安装程序和应用共用这一份
scripts/install-deps.sh       同一件事的 macOS / Linux 版，由应用首次启动时调用
scripts/bundle-runtime.mjs    构建前把上面两个脚本放进 resources/，并生成启动预热清单
scripts/boot-trace/           追踪一次 dsh 启动读了哪些文件，预热清单由此而来
.github/workflows/ci.yml      三平台编译与单元测试 + 脚本静态检查（手动触发）
.github/workflows/release.yml v* tag 触发：三平台构建、签名、传草稿 Release
src-tauri/tauri.conf.json     基础配置 + Windows 的 NSIS 目标
src-tauri/tauri.macos.conf.json   app + dmg 目标（Tauri 按平台自动合并）
src-tauri/tauri.linux.conf.json   deb + AppImage 目标
src-tauri/installer-hooks.nsh 安装/卸载时调用 install-deps.ps1，以及对 $DSH_HOME 的处理
src-tauri/src/main.rs         窗口、导航策略、托盘、生命周期
src-tauri/src/controls.rs     注入页面的无边框窗口按钮与拖拽带
src-tauri/src/theme.rs        从 dsh 的设置里读亮/暗，开窗时用一次
src-tauri/src/server.rs       托管的 dsh web 子进程；Job Object 兜底与进程组
src-tauri/src/dsh.rs          dsh 的定位与版本比较，以及何时调安装脚本
src-tauri/src/warm.rs         按预热清单并行预读，抵消 Defender 首次扫描
src-tauri/src/update.rs       应用自身的自动更新
```

## 路线图

- [x] 机器上没有 Node 也能装（安装时检测，缺了就装一份到用户目录）
- [x] Job Object 兜底、托盘与开机自启动、自动更新
- [x] macOS / Linux 的 bundle 目标（dmg、deb、AppImage，以及对应的安装脚本与进程组清理）
- [x] 发布流水线（推 tag 即三平台构建、签名、传草稿 Release）

## 已知边界

- **安装要联网，而且不快**：dsh 的依赖树是 587 个包 / 185 MB 压缩流量 / 解压后 327 MB / 33k 文件，2 MB/s 的连接上约 4 分钟；机器上没有 Node 的话前面还要再下 36 MB。全部镜像都失败的话安装程序会明说，不会假装装好了 —— 下次启动应用时它会再试一次
- **第一次启动比之后慢**：dsh 要 import 的文件第一次从杀软没见过的路径读过去，Defender 会逐个扫描 —— 实测冷启动 14 秒，文件热了之后 1.6 秒。应用启动时会按预热清单多线程预读那批文件，把冷启动压到 4 秒上下；想彻底避开的话给安装目录加排除项（管理员 PowerShell，**路径换成你实际装的那个**）：

  ```powershell
  Add-MpPreference -ExclusionPath "$env:LOCALAPPDATA\dsh-desktop", "$env:LOCALAPPDATA\ai.deepseek.dsh.desktop"
  ```

- **只在 Windows 上真机验证过**：macOS / Linux 的代码路径齐了，CI 保证三个平台都能编译、能打出包，但「装完真的能跑起来」还没有在那两个平台上验证过。碰到问题请提 issue
- **macOS / Linux 没有内核级的兜底**：正常退出会杀掉整个进程组，但应用被强杀（`kill -9`、崩溃）时 dsh 会活下来。Windows 的 Job Object 在这些平台上没有对等物 —— Linux 的 `PR_SET_PDEATHSIG` 绑的是**创建线程**的生命周期，而那个线程在启动流程结束时就退出了，用了反而会误杀。下次启动会被单实例锁挡住，手动 `pkill -f 'dsh web'` 收拾
- **macOS 的包没有签名和公证**：需要付费的 Apple 开发者账号。首次打开要右键「打开」或去掉隔离属性，见[安装](#安装)
- 第一次启动的预热（`warm.rs`）是冲着 Windows Defender 去的，在 macOS / Linux 上只剩预热页缓存的那点收益
- dsh 更新走 `npm install -g`，npm 在替换旧树期间新旧两份并存，磁盘峰值约 650 MB
- 安装完 Node 后 PATH 的变更要等新开的终端才生效（macOS / Linux 上还得是会读 `~/.profile` 或 `~/.zshrc` 的那种）；应用自己不受影响 —— 它从 `bootstrap.json` 里读脚本记下的路径，不依赖继承来的 PATH
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
