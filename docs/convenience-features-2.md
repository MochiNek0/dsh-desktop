# 易用性功能计划（第二轮）

分支：`feat/convenience-2`

目标和第一轮一样：让日常用起来少走几步。原则不变：dsh 一行不改，安装包保持轻量。

按顺序一条一条做，每条做完单独验证、单独提交。

## 进度

| # | 功能 | 状态 |
| :--- | :--- | :--- |
| 1 | 跑任务时不让电脑睡眠 | 已完成并验证（Windows） |
| 2 | 托盘里列出在跑 / 在等你的会话 | 已完成并验证（托盘菜单的样子没截图） |
| 3 | 通知里直接回复 | 做过，用下来效果不好，已撤掉 |
| 4 | 快捷键唤出的小输入框 | 已完成并验证（Windows） |
| 5 | `dsh-desktop://` 链接 | 已完成并验证（Windows） |
| 6 | 把剪贴板送进当前会话 | 已完成并验证（Windows） |

## 先查清楚的：插件能对会话做什么

4、6（以及撤掉的 3）都要插件往会话里写东西。查的是 dsh 0.2.0-rc.2 的类型声明（`node_modules/@deepseek-ai/dsh/node_modules/@deepseek-ai/*/lib/types`）：

- **新建会话**：`ctx.sessions.create({ workspaceId })` 返回会话 id。
- **发消息**：`ctx.sessions.retain(id, { source })` 拿到引用，等 `reference.ready` 后得到 binding；`binding.ctx` 是这个会话的 scope，`ctx.conversation.send(text)` 在调用者 scope 的会话里排一轮（和输入框回车走同一条路，带本地回显）。底层是 `binding.session.prompt([{ type: 'text', text }], 'queue')`。
- **改输入框**：`ctx.conversation.input.for(scopeCtx)` 是这个会话的输入框，有 `setDraft(text)`、`focus()`，`snapshot` 里的 `draft` 是当前草稿。
- **切换会话**：`ctx.uiWorkspace.openSession(id)`。

**顺带发现的 bug**：插件点通知回会话用的是 `sessions.open(id)`，0.2 的 `ISessions` 里已经没有这个方法了（「navigation belongs to view owners」），所以 0.2 上点通知回不到对应会话。第 2 条一起修：优先 `uiWorkspace.openSession`，老版本退回 `sessions.open`。

## 1. 跑任务时不让电脑睡眠

**要做的**：有会话在跑（`status` 的 `running > 0`）时让系统保持唤醒，归零后放开。设置卡片加一个开关，默认开。

- Windows：`SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED)`。只拦系统睡眠，不拦关屏。这个状态跟线程走，所以要在一个常驻线程里设、在同一个线程里清。
- macOS：`IOPMAssertionCreateWithName(kIOPMAssertionTypePreventUserIdleSystemSleep)`。
- Linux：`systemd-inhibit --what=idle:sleep` 起一个子进程挂着，放开时杀掉。没有 systemd-inhibit 就什么都不做。
- 等待确认（`waiting`）不算在跑：那一轮停着，睡下去也不丢东西。

**完成标准**：跑任务时系统的执行状态带 `ES_SYSTEM_REQUIRED`，跑完就没了；关掉开关后跑任务也没有。

### 已完成

- `src-tauri/src/awake.rs`。`status::set` 每次拿到计数都交给它，`running > 0` 且开关开着就持有，否则放开。开关在设置卡片「跑任务时不睡眠」，存在 `desktop.json` 的 `keepAwake`，默认开。在跑的时候切开关立刻生效。
- 等待确认的会话不算在跑（第 2 条里改的，见下）。
- macOS、Linux 只在 CI 编译，没有实机验证。

**已验证**（Windows，dsh 0.2.0-rc.2）：`powercfg /requests` 要管理员权限，改用 `CallNtPowerInformation(SystemExecutionState)` 读全系统的执行状态（先用另一个进程设 `ES_SYSTEM_REQUIRED` 确认这个读数是全系统的）。经 CDP 在真实页面里发 `status` 信号：`running=1` → 置位；`running=2&waiting=1` → 置位；`running=0` → 清掉；跑着的时候关开关 → 清掉、`desktop.json` 写进 `"keepAwake": false`；再打开 → 置位。卡片截图确认开关和说明文字。

## 2. 托盘里列出在跑 / 在等你的会话

**要做的**：托盘菜单最上面列出正在等你的会话和正在跑的会话（标题），点一下跳到那个会话。

- 插件的 `status` 事件带上会话 id 和标题（等你的排前面，最多 5 行），只在列表变了才发。
- 点击走 `signal::open`，顺带修掉上面说的 `sessions.open` 的 bug。

**完成标准**：跑任务时托盘里出现「正在运行：<标题>」，点了窗口出来并切到那个会话；等待确认时出现「在等你：<标题>」。

### 已完成

- 插件：两半各自维护会话 id 的集合（不再只是计数），合起来由 `status()` 组成 `status` 事件：`running` / `waiting` 两个计数，加上 `wid`/`wtitle`、`rid`/`rtitle` 两对重复键。等你的排前面，合计最多 5 行；只是在跑的子代理不列（一轮能派出几十个），停下来问问题的子代理照列。标题用 dsh 的 `displayTitle`，每次会话列表变化时重新读，所以 dsh 后补的标题会跟上。
- **停在问题上的会话算等待、不算在跑**。dsh 自己还把那一轮当作 running，但蓝点和保持唤醒都是给「自己在往前走」的那一轮用的。所以第 1 条的唤醒在等授权时会放开。
- `status.rs` 存这几行，变了才重建托盘菜单。菜单最上面是「在等你：<标题>」/「正在运行：<标题>」，下面才是最近工作区。点击走 `signal::open`，和点通知是同一条路。
- 修掉 `sessions.open` 的 bug：`__dshSignals.open` 优先用 `uiWorkspace.openSession`，没有再退回 `sessions.open`。

**已验证**（Windows，dsh 0.2.0-rc.2）：经 CDP 在真实页面里提交「用 bash 执行 ping -n 15 127.0.0.1」。日志里托盘行依次是「正在运行：dsh-e2e」→「正在运行：请用 bash 执行 ping -n」→「正在运行：Bash执行Ping命令测试」（标题跟着 dsh 变）→ 等授权时「在等你：Bash执行Ping命令测试」→ 授权后回到「正在运行」→ 跑完清空。同一段时间里系统执行状态：在跑时置位，等授权时清掉，跑完清掉。点「新会话」切走后调 `__dshSignals.open(<那个会话>)`，侧边栏选中项回到那个会话。

**没验证的**：托盘菜单本身要右键才弹出，不模拟鼠标没法截图，菜单的样子和点击菜单项没有实机看过。点击菜单项走的 `status::open` 只是 `reveal` 加上面验证过的 `signal::open`。

## 3. 通知里直接回复

**撤掉了。** 做出来是 Windows 上「对话已完成」的通知里带一个输入框和「发送」按钮（自己用 WinRT 拼 toast XML），真人试过能用，但效果不好，按用户的意思整条去掉：

- 删了 `toast::inline`、`Notice::replyable` / `Click::reply`、`signal` 里的 `Replied` / `ReplyFailed` / `send_reply`、插件的 `__dshSignals.reply`，以及只为它加的 `windows` crate 特性（`Data_Xml_Dom`、`Foundation`、`Foundation_Collections`、`UI_Notifications`）。「对话已完成」回到普通通知，点一下打开那个会话。
- 留下了当时一起加的插件 `__dshSignals.draft(id, text)`（切到会话、把文字接在草稿后面、聚焦输入框），第 6 条在用。

## 4. 快捷键唤出的小输入框

**要做的**：按一个全局快捷键弹出一个小窗口，输入一句话回车，在最近用的工作区里新开一个会话去跑；跑完靠已有的通知告诉你。

- 小窗口是本地页面，不加载 dsh；文字经 `dsh-window://` 通道交给 Rust，再由插件新建会话、发消息。
- 默认快捷键另定，设置卡片里可改，和现有的全局快捷键一样录制。
- 工作区默认用最近的那个；小窗口里可以切到其他最近工作区。

**完成标准**：主窗口在托盘里时按快捷键，输入回车，dsh 里出现一个新会话在跑这句话；跑完弹通知。

## 5. `dsh-desktop://` 链接

**要做的**：注册一个协议，`dsh-desktop://open?path=<路径>` 和 `dsh-desktop <路径>` 一样打开那个文件夹的工作区。

- 只认 `open`，参数只有 `path`。网页能点出这个链接，所以它不能做比「打开一个文件夹」更多的事。

**完成标准**：点 `dsh-desktop://open?path=C:\work\x`，dsh 打开这个文件夹的工作区，冷启动和运行中都行。

### 已完成

- **协议名是 `dsh-desktop://`，不是计划里的 `dsh://`**：`dsh://` 是官方桌面端最可能用的名字，两个应用注册同一个协议，后装的会抢走先装的。
- **没加 `tauri-plugin-deep-link`**。Windows 和 Linux 上链接本来就是作为命令行参数交给程序的，冷启动从 `args` 读，运行中经单实例插件转过来，和 `dsh-desktop <路径>` 是同一条路；macOS 上从 `RunEvent::Opened` 进来，打开文件夹已经在处理这个事件。所以只需要：
  - `folder::requested` 认出 `dsh-desktop://open?path=…`（`folder::link`），路径必须是绝对路径、必须是已有的文件夹；也认 `file://` URI（见下面 Linux）。
  - Windows：`tauri.conf.json` 加 `plugins.deep-link.desktop.schemes`，打包器把它填进安装器模板里现成的注册段（`HKCU\Software\Classes\dsh-desktop`，命令 `"exe" "%1"`），卸载时删掉。
  - macOS：`Info.plist` 里自己写 `CFBundleURLTypes`，`Opened` 里先试 `folder::link`。
  - Linux deb：`.desktop` 加 `x-scheme-handler/dsh-desktop`。协议要 `%U`，所以 `Exec` 从 `%F` 改成 `%U`，文件夹的「用其他应用打开」因此变成传 `file://` URI。AppImage 不装 `.desktop`，没有这个入口。
- macOS、Linux 只在 CI 编译，没有实机验证。

**已验证**（Windows，dsh 0.2.0-rc.2）：

- `npx tauri build --debug --bundles nsis --no-sign` 生成的 `installer.nsi` 里有 `Software\Classes\dsh-desktop` 的注册和卸载清理。
- 按安装器的写法手写注册项指向测试版，`Start-Process "dsh-desktop://open?path=<编码后的路径>"`（浏览器确认后走的就是 ShellExecute）：应用没开时启动并打开了 `C:\Users\EDY\dsh-e2e-link\with space`（带空格），新开一个会话；开着的时候再打开 `C:\Users\EDY\dsh-e2e`，只有一个进程，当前会话落在 dsh-e2e 工作区。Windows 交过来的是 `dsh-desktop://open/?path=…`（多一个斜杠），也能认。测完删掉了注册项。
- 单元测试：编码和未编码的路径、多斜杠、相对路径、未知动作、不存在的路径、`file://` URI。

## 6. 把剪贴板送进当前会话

**要做的**：一个全局快捷键：把剪贴板里的文字追加到当前会话的输入框里，窗口拿到前台、光标在输入框。不自动发送。

- 先只做文字。图片要走 dsh 的附件流程，`draftAttachments` 是内部的，后面再看。

**完成标准**：在别的程序里复制一段报错，按快捷键，dsh 出来，输入框里是原来的草稿加上这段文字。

## 4 和 6 已完成

两条一起提交：都要一个新的全局快捷键，共用下面这部分改动。

### 快捷键改成多个槽位

- `shortcut.rs` 从「一个键」改成三个 `Slot`：显示窗口（`shortcut`，默认 Ctrl+Alt+D，和以前一样）、快速提问（`askShortcut`，默认 Ctrl+Alt+Space，macOS 上 Ctrl+Option+Space 是切换输入法，改用 Ctrl+Option+Q）、粘贴到 dsh（`pasteShortcut`，**默认不设**：Ctrl+Alt+V 是 Office 的「选择性粘贴」，顺手的组合都被各家的粘贴命令占着）。默认值和用户商量过。
- 每个槽位各有自己的注册状态、失败原因和设置卡片上的一行；三行共用一套录制器（`keyRows` 按 verb 分开，`__dshShortcut(verb, …)`）。
- 录一个已经被本应用别的槽位占着的键，直接提示「已经是「…」的快捷键了」，不交给系统（系统会说成被「其他程序」占用）。
- `settings::shortcut` / `set_shortcut` 改成带键名。

### 4. 快速提问

- `ask.rs` + `dist/ask.html`：一个无边框、置顶、不进任务栏的小窗口，第一次用时才建。没有 IPC（capabilities 只给了主窗口），靠 `dsh-window://quick-ask|quick-close|quick-ready` 跟 Rust 说话，只有它自己的导航回调听得到。`dsh-window://ask` 已经被对话框的回答占了，所以用 `quick-` 前缀。
- 窗口里是一个输入框和最近工作区的下拉框（默认最近的那个）。Enter 发送（输入法还在组字时不算），Shift+Enter 换行，Esc 关闭，失焦也关。Rust 只接受最近工作区里有的路径。
- 提交后窗口收起，Rust 调插件 `__dshSignals.ask(path, text)`：`workspaces.create({ path })`（和打开文件夹同一个调用，同一路径复用）→ `sessions.create({ workspaceId })` → `sessions.using` 拿住新会话、`binding.session.prompt(..., 'queue')`，回 `asked` / `ask-failed`。不切换会话；跑完靠已有的通知。
- dsh 还没起来时先存着，等 `folder-ready` 再交出去。失败时小窗口带着原来的文字和原因重新弹出来。
- 第一次建窗口时页面还没加载完，Rust 推的数据会丢，所以页面加载后发 `quick-ready` 再要一次。
- **修掉的坑**：第一次弹出时 WebView2 建子窗口并把焦点交给它，Tauri 把这次转手报成窗口 `Focused(false)` 再 `Focused(true)`，「失焦就隐藏」把窗口立刻藏掉了，看起来像第一次按键没反应。现在失焦后等 150ms 再问一次窗口是否真的没有焦点。

#### 改版：选会话、开新会话、选其他文件夹

用户觉得原生下拉框太丑，并且想在小窗口里直接选会话。改成「紧凑条 + 展开列表」（和用户确认过的样式）：

- 输入框下面两个圆角小标签：工作区、会话（默认「新会话」）。点开不是 `<select>`，是页面自己画的列表，接在输入框下面；列表打开时页面发 `quick-size?h=` 让窗口变高，关上缩回 132。Rust 只接受 132–480 之间的高度。↑↓ 选、Enter 确定、Esc/Tab 收起列表（再按 Esc 才关窗口）；标签获得焦点时按 ↓ 也能打开。右边是圆形发送按钮。
- 工作区列表：最近工作区 + 「打开其他文件夹…」。后者发 `quick-pick`，Rust 在主线程用 `rfd` 弹系统的选文件夹对话框（父窗口是小窗口），选中后推 `__dshAskPicked` 把它加进列表并选上，输入框里的字不动。对话框开着时小窗口会失焦，用 `PICKING` 标记挡住「失焦就隐藏」。选过的文件夹这次运行内都算「给过的」，Rust 才肯收。
- 会话列表：「新会话」+ 这个工作区里的会话。会话来自插件新的 `sessions` 事件（`spath`/`sid`/`stitle` 三个重复键），`watchRecent` 顺带算：最近 5 个工作区，每个最多 8 个，按 `updatedAt` 新到旧，去掉已归档（`archivedSessionIds`）、`blank`（dsh 留着给下一次「新会话」复用的空会话）和子代理的。单独一个事件、单独去重，会话顺序变了不会重画托盘。
- 发进已有会话：`__dshSignals.ask(path, text, session)`，`session` 非空时不建工作区也不建会话，直接 `sessions.using(session)` → `prompt`。Rust 只接受插件列出过的、属于这个工作区的会话 id。仍然不切换会话，跑完靠通知。
- 失败重新弹出时，工作区和会话也恢复成原来选的（选的是临时挑的文件夹也会放回列表）。
- 没有任何工作区时，工作区标签显示「选择文件夹…」，回车直接弹选文件夹对话框。

**已验证**（Windows，dsh 0.2.0-rc.2，热键用 `PostMessage(WM_HOTKEY)`，页面用 CDP）：打开工作区列表后窗口从 132 变成 360，上边不动；列表里是 5 个最近工作区和「打开其他文件夹…」，会话列表里是「新会话」和那个工作区的会话。在 dsh-e2e 里选一个已有会话发一句话：日志「a quick ask goes to session-… in …dsh-e2e」「started」同一个 id，跑完 turn-end 也是这个会话，会话列表里它排到第一，没有多出新会话。点「打开其他文件夹…」弹出系统对话框，对话框开着时小窗口一直在，取消后小窗口回到前台，输入框里的字还在。

**没验证的**（用户说自己来测）：在对话框里真的选一个文件夹再发出去；失败后带着工作区和会话重新弹出；没有工作区时的样子。

#### 再改：胶囊标签、真正的下拉、毛玻璃

用户要的是：标签是有质感的胶囊形，点开是一个浮在下面的下拉，而不是整个小窗口变形；整个小窗口是苹果那种毛玻璃。

- **下拉是第二个窗口**（`ask-menu`，`dist/ask-menu.html`）：页面画不到自己窗口外面，之前只能把小窗口拉高。它 `focusable(false)`，焦点和键盘一直留在小窗口；小窗口发 `quick-menu`（标签在页面里的位置、列表的宽高、各行内容）→ Rust 换算成屏幕坐标放到标签下面 6px，下面放不下就放到标签上面；↑↓ 发 `quick-menu-active` 让下拉跟着高亮；点下拉里的一行是 `menu-pick` → Rust 收起下拉、调小窗口的 `__dshAskChose(i)`。行内容 Rust 先 `serde_json` 解析一遍再交给下拉页，不直接转发 URL 里的字符串。`quick-size` 去掉了，小窗口固定 640×132。
- 「失焦就隐藏」多一个例外：焦点在自己的下拉上（万一 `show` 把焦点交过去）时不藏，并把焦点拿回来。小窗口藏起来时下拉一起藏。
- **毛玻璃**：Windows 上两个窗口都是 `transparent(true)` + Acrylic（`window-vibrancy`，Windows 11 22H2 起是系统的 transient-window 亚克力），再用 `DWMWA_WINDOW_CORNER_PREFERENCE` 要圆角（Windows 10 没有这个属性，保持直角）。页面在 `html.glass` 时背景透明，只加一层半透明的白/黑和顶部一点高光。macOS 透明窗口要开 Tauri 的 `macos-private-api`，Linux 没有窗口背后的模糊，这两个平台保持不透明。
- 标签改成胶囊：999px 圆角、上亮下暗的渐变、0.5px 描边 + 细阴影 + 顶部内高光；打开时换成主色的光圈、箭头翻转。发送按钮是带渐变和投影的圆形。

**已验证**：单元测试（下拉的位置换算：缩放、翻到上方、URL 里的越界值）；无头 Chrome 渲染两个页面的浅色/深色（用一个模糊的彩色背景模拟亚克力）。**没在真机上看过**：亚克力和圆角的实际效果、下拉窗口会不会抢焦点、下拉的位置。


### 6. 粘贴到 dsh

- `paste.rs`：读剪贴板文字 → `reveal` → 插件 `__dshSignals.draft('', text)`（`''` 是当前会话，即 `retainedBy.mainView` 持有的那个；等它有了 scope，接在当前草稿后面另起一行、聚焦，不发送）。只认文字，图片不管，窗口照样出来。
- 不为一段文字加剪贴板 crate：Windows 直接用 Win32 剪贴板（`OpenClipboard` 失败重试几次，读的长度同时受终止符和分配大小限制），macOS 用 `pbpaste`，Linux 依次试 `wl-paste`、`xclip`、`xsel`。
- macOS、Linux 只在 CI 编译。

**已验证**（Windows，dsh 0.2.0-rc.2，按键都是给 global-hotkey 的消息窗口 `PostMessage(WM_HOTKEY)`，没有模拟键盘）：

- 启动后另一个进程 `RegisterHotKey(Ctrl+Alt+Space)` 失败（被应用占着），`Ctrl+Alt+B` 成功（粘贴默认不设）。
- 主窗口收进托盘后按快速提问：第一次就弹出并拿到前台（修之前第一次会被藏掉，日志里是 `Focused(true)/(false)/(true)/(false)`）。截图确认样子。经 CDP 在小窗口里输入一句话回车：窗口收起，日志「a quick ask goes to C:\Users\EDY\dsh-e2e」「a quick ask started session-…」，托盘显示「正在运行：…」，跑完弹出带输入框的通知，主窗口一直在托盘里。
- 再按一次弹出、Esc 关闭、按两次是一开一关。
- 设置卡片三行：Ctrl+Alt+D / Ctrl+Alt+Space / 未设置。在「粘贴到 dsh」上录 Ctrl+Alt+D → 提示已经是「全局快捷键」的快捷键；录 Ctrl+Alt+B → 生效，写进 `pasteShortcut`，别的进程再注册不上。
- 备份用户剪贴板后放进两行报错，主窗口收起，按 Ctrl+Alt+B：主窗口出来，当前会话输入框里是这两行，焦点在输入框；之后还原了剪贴板、清掉了草稿。

**没验证的**：快速提问失败时带着文字重新弹出（插件单元测试覆盖了 `ask-failed`）；在已有草稿后面追加（撤掉的第 3 条当时验证过 `draft` 追加）。
