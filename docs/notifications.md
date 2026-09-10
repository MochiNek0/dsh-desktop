# 通知与回答：现状、结论与改造方案

> 状态：**全部实施完毕**（第 1–5 步 + 随包发货 + 首次启动自荐）。撰写于 2026-09-09，基于 dsh-desktop
> 0.1.13 与 dsh 0.1.2-rc.1。
>
> 这份文档记录的是一次调研的结论。中间大量事实是逐条核实过的（见
> [附录 A](#附录-a已核实的事实与证据)），核实成本远高于阅读成本，所以证据和结论写在一起 ——
> 改动这份方案之前，先看那一节是否仍然成立。

## 1. 现状

> **这一节是改造之前的样子**，留着是因为下面每一条结论都是冲它来的。改完之后的样子见
> [第 4 节](#4-目标架构)：这里说的两个 DOM watcher 已经不存在了，`tauri-plugin-notification`
> 也换掉了。

一条通知从页面走到操作系统，路径是：

```
dsh 页面 / dsh 插件调用 window.Notification
  ↓  notify.rs::script() 注入的 shim 覆盖了 window.Notification
location.href = "dsh-window://notify?title=…&body=…&n=<nonce>"
  ↓  controls.rs 在 on_navigation 里认领并取消这次导航
notify::received() → notify::show()
  ↓  tauri-plugin-notification
Windows: WinRT Toast   macOS: mac-notification-sys   Linux: D-Bus
```

不涉及任何系统 hook。webview 没有被授予 IPC，通知的载荷是一次被取消的导航的查询串。

「谁来喊」由两个注入脚本回答，都是**轮询 dsh 自己的 DOM**：

| 模块 | 判据 | 周期 |
| :--- | :--- | :--- |
| `turn.rs` | 输入栏主按钮的 svg 子节点是 `rect`（停止）还是 `path`（发送） | 500ms，落沿连续 2 次确认，本轮需跑够 4s |
| `waiting.rs` | `data-approval-key` / `data-plan-review-key` / `data-question-key` 三者之一出现且 key 变化 | 500ms，前 4s 仅记录基线 |

两者都调用被替换掉的标准 API，因此和 dsh 插件自己发的通知汇入同一条路，最终在
`notify::show()` 收口于两道闸门：`settings::notifications` 的开关，以及
`watching()`（可见 + 未最小化 + 有焦点）。

这套结构本身是对的，下面的改造不动它：单一出口、不开 IPC、插件通知与自发通知不重复、
嗅探失效时静默失能而非误报。

## 2. 要解决的问题

1. **点击 toast 什么都不会发生。** 用户把窗口收进托盘，通知来了也回不去。
2. **无法在通知里回答。** dsh 停下来等的三件事（工具授权、计划评审、提问）都需要用户
   给一个值，而通知只能陈述。
3. **判据是猜的。** 两个 watcher 都在推断一个 dsh 已经算好并公开了的状态（见
   [附录 A.1](#a1-dsh-侧的官方-api)）。
4. **发送结果不可知。** `tauri-plugin-notification` 的桌面 `show()` 把通知交给
   `tauri::async_runtime::spawn` 后丢弃返回值，永远回 `Ok`。「Linux 上没有通知守护进程」
   和「通知发成功了」在代码里长得一模一样。
5. **没有会话概念。** 多会话下用户不知道是哪个会话在等他。

## 3. 结论

采用**「结构化信号 + 带按钮的原生通知」**，不自绘通知窗口。

- 信号从 dsh 的官方 API 取，不再嗅探 DOM。
- 通知层自己写，绕过 `tauri-plugin-notification`（它既不给句柄也不给按钮）。
- 通知上放至多 2 个动作按钮，覆盖「允许 / 拒绝 / 批准 / 要改」这类一键回答；需要打字或
  选项过多时只放「打开」，回应用里答。
- 点击通知本体或「打开」→ 恢复窗口并切到提问的那个会话。

### 3.1 被否决的方案

**自绘通知窗口**（第二个 Tauri window，无边框、置顶、右下角）。它是唯一能在通知里
**打字输入**的办法，但要付的账集中在 Linux 和 macOS：

- Wayland 下无法定位。`set_position` 最终走到 `gtk_window_move`，GDK-Wayland 对 toplevel
  是 no-op（`tao-0.35.3/src/platform_impl/linux/event_loop.rs:301`）。GNOME / Fedora /
  Ubuntu 22.04+ 默认都是 Wayland。
- Wayland 下置顶同样无效。`always_on_top` 走到 `gtk_window_set_keep_above`
  （同上 `:387`），该调用在 Wayland 下被忽略。
- macOS 上普通置顶窗口不出现在别的 app 的全屏 Space 上，且显示时容易激活整个 app；要做
  成真正的非激活面板需要 `tauri-nspanel`。
- 不进通知中心，漏看即消失；不尊重专注模式 / 投屏；多一个 WebView2 / WKWebView 实例。

判断：为了「打字」这一个能力，换来一组平台特例和一个额外 webview，不值。改成「一键回答
覆盖高频场景，其余打开应用回答」，代价约为三分之一，且上述七条全部消失。

**DOM 注入回答**（读卡片、`eval()` 点按钮、给 textarea 塞值）。已核实
`@deepseek-ai/dsh-client-ui-user-questions` 的构建产物里，`data-` 属性只有容器上的
`data-question-key` / `data-plan-review-key`（外加两个 scroll 锚点），**选项按钮上没有任何
稳定属性**。靠 nth-child 或哈希类名定位，失效模式是「代替用户提交了一个他没选的答案」——
比漏一条通知严重一个量级。有官方 API 的前提下没有理由走这条路。

### 3.2 交付形态

插件源码放在本仓库（`plugin/`），随安装包作为资源发货，靠 preset 装本地路径。这条路是通的：
`plugins.rs:379` 的 preset 分支不经过 `is_package_spec` 那道闸，闸只管自由文本框里的
`extra` —— 而那道闸拒绝 `file:` 的理由（`plugins.rs:686` 起的注释）是「这个框对窗口里任何
脚本可达」，对随包资源不成立。

这么定的理由：插件唯一的对话方是 `dsh-window://`，认领这个 scheme 的只有本应用的 Rust，
它在别处什么都不做；两半是同步改的（[4.2](#42-按钮方案) 每加一行都要动两边）；本地路径
安装不联网、不过 registry、不撞 README 里那个 pnpm `allowBuilds` 拦截；插件版本天然等于
应用版本。

**~~后续插件会拆出去、单独发 npm。~~ 2026-09-10 复核：不发，留在本仓库。**
理由是这三条 —— 它在 dsh-desktop 之外是个空壳（`client.js` 第一件事就是认 `__DSH_VERSION__`，
认不出整体不接线），它是私有协议的一半而两边没有任何版本协商（事件名、按钮 id、
`__dshSignals` 三处都是两边各自写死的），错配的表现是静默降级而不是报错。发出去换不到受众，
只换来一个版本矩阵。什么时候翻案：协议加了版本握手，或者出现了第二个说 `dsh-window://` 的宿主。

下面三条约束是当初为「要拆」付的，**留着**：第一条和第三条与拆不拆无关（一个自足的目录、
一个认不出宿主就不接线的插件，本来就该这样），第二条是净赚。

- **目录自足。** `plugin/` 带自己的 `package.json`，不从仓库根部引任何东西。拆出去等于
  `git mv` 加一次 publish，不是一次重写。
- **零构建。** 已核实可行，见 [A.4](#a4-客户端插件的加载契约)：宿主只要求 `./client`
  指向的文件存在，从不校验它是否出自打包器；而「一个 bundle 就是一个 module node」，
  我们这个插件本来就是一个模块，打包器无事可做。手写那层 `__ModuleLoader__.load`
  wrapper 是四行。类型包只作 devDependency，配 `// @ts-check` + JSDoc 拿编辑器检查
  （类型导入会被擦除，不产生模块请求）。放弃的只有 HMR —— 它靠构建驱动 `rebuilt()`。
  本仓库现在只有 Rust 工具链（`dist/` 是一个手写 HTML，注入脚本全是 Rust 字符串字面量），
  不加 tsc / esbuild 是净赚。
- **先认宿主再接线。** 发到 npm 之后，装它的人可能没有 dsh-desktop。那时
  `location.href = 'dsh-window://…'` 是一次**真**导航，浏览器可能弹「打开方式」而不是静默
  失败。所以插件启动第一件事是认宿主 —— `main.rs` 已经注入了 `__DSH_VERSION__`，拿它当
  标记 —— 认不出就整体不接线，退化成一个 no-op 插件。

**随包发货欠下的一笔账，卸载时要还。** `bundled:` 装出来的是
`"dsh-desktop-signal": "link:<安装目录>\resources\plugin"`，profile 的 `node_modules` 下是
一条指向应用安装目录的 junction。而 NSIS 的卸载 hook **明确不动 `$DSH_HOME`**
（`installer-hooks.nsh`：「这是用户自己的东西」，那个框默认选「否」）。于是卸载应用之后，
那条 junction 的另一头没了，用户要付两笔：

- `dsh.profile.bundles` 还列着它，之后每次 `dsh web` 都会去激活一个文件已经不在的 layer，
  按 [A.4](#a4-客户端插件的加载契约) 的记录是 fails loudly。
- 断掉的 junction 在 `nodeLinker: hoisted` 下是 **pnpm 自己过不去的墙** —— 原因见
  `repair()` 的注释（目标没了它就不再是目录，pnpm 按文件清，而清文件是 `DeleteFileW`，
  它拒绝目录链接并回 `ERROR_ACCESS_DENIED`）。之后**每一次** `dsh plugin add` 都会栽在这个
  条目上。而 `repair()` 只在这个应用自己的安装路径里跑 —— 应用都卸了，没人再跑它。

改成首次启动自动装之后，这笔账从「主动装过插件的少数人」变成了**所有启动过这个应用的人**，
所以必须还。实装：`install-deps.ps1` 新增 `-Mode unlink-plugin`，卸载 hook 在确认这不是
更新/重装之后**无条件**调一次（不管用户对 Node / dsh 那两个问题怎么答，静默安装也调）。

它只做两件事，且只在断链时做：删掉那条 junction，再从 `package.json` 的 `dependencies` 和
`dsh.profile.bundles` 里摘掉这一项。三个决定：

- **不走 `dsh plugin remove`。** 那条路要一个能跑的 Node + dsh + pnpm，而且要让 pnpm 去动一个
  junction 已经断掉的 profile —— 正好是上面那堵墙；还可能联网。卸载器给不了这些保证，也等不起。
  PowerShell 直接动文件系统，什么都不需要。
- **判据是「链接在、目标没了」，不比对路径。** 这既精确覆盖了要修的那种情况，又天然放过了
  开发者：`dsh plugin add -w ./plugin` 装出来的链接指向他自己的 checkout，应用卸了 checkout 还在。
- **不能用 `Test-Path` 判断断链。** 实测（Windows 11，junction 目标刚被删）`Test-Path` 和
  `[IO.Directory]::Exists` **都答 True** —— 两者都终结在 `GetFileAttributesW`，它报的是链接自己的
  属性，从不跟进去。要问就得问目标：`(Get-Item -Force).Target`（5.1 里是 `String[]`）拿到目标路径
  再 `Test-Path`。注意 Rust 那边不是这样：`Path::exists` 会跟进去，所以 `clear()` 里的写法是对的，
  但不能照抄到 PowerShell。

一处校验缺口，记录在案：`plugins.rs` 用 `spec_name()` 组装 release-age 重试要比对的集合，而
`spec_name` 对含 `/` 或 `:` 的 spec 返回 `None`，本地路径会被丢出那个集合。`github:` 现在就是
这个待遇 —— 是既有的、已接受的缺口，不是本次新增的（对随包资源也无害：本地路径不过 registry，
碰不到那道冷却检查）。

**实装形态。** preset 的 `spec` 写作 `bundled:plugin`，在 `install()` 里解析成随包目录的绝对
路径（`spec_for()` / `bundled()`）—— 装到哪里是用户选的，那条路径只有运行时才知道。目录由
`scripts/bundle-runtime.mjs` 从仓库的 `plugin/` 复制进 `src-tauri/resources/plugin`，走已有
的 `resources/**/*` 那条 glob，不给 bundler 加第二条规则；进去的正好是插件自己
`package.json` 的 `files`（`lib/` + `cordis.patch.yml`）加清单，不含 `test/`。

**一个必须记住的坑：Windows 上这条 spec 要自己加引号。** `dsh plugin` 用 Node 的
`spawnSync(…, { shell: true })` 转发给 pnpm，而 Node 在那个模式下**不加引号** —— 它把参数用
空格拼成一行交给 `cmd.exe`。默认安装目录是 `C:\Program Files\…`，所以不加引号的 spec 到
pnpm 手里是**两个**参数，而 pnpm 不会失败：它按两半各装一个依赖（`Program`、`plugin`），
warn 一句「declares no dsh.bundle」，然后 exit 0。三层都实测过（Rust `Command` 的转义 → Node
的 shell 拼接 → cmd 拆词），加引号能原样穿过去。见 `plugins.rs` 的 `local_spec()`。

## 4. 目标架构

```
┌─ dsh 页面内 ────────────────────────────────────────────┐
│  dsh 客户端插件（新增，inject: ['sessions', 'uiSession']） │
│   · ctx.sessions.list         → running / completed      │
│   · ctx.uiSession.pendingInteractions                    │
│       → Map<SessionId, PendingApproval | PendingQuestion>│
│         信号与 carrier 同一个对象，带 answer() / cancel() │
│   · 暴露 answer(key, …) / open(sessionId)                │
└──────────────┬──────────────────────────┬───────────────┘
      dsh-window:// 取消导航（上行）        eval()（下行）
┌──────────────▼──────────────────────────▼───────────────┐
│  Rust                                                   │
│   notify.rs   闸门（设置开关 + watching）保持不变          │
│   toast.rs    新增：自建通知层，按钮 + 激活回调            │
│   main.rs     reveal() + open(sessionId)                │
└─────────────────────────────────────────────────────────┘
```

上行沿用 `controls.rs` 的 `dsh-window://` 通道，不新开 IPC。下行沿用现有的
`window.eval()`（`controls.rs` / `dialog.rs` 已是这个路子）。

### 4.1 通知层三端实现

`tauri-plugin-notification` 唯一的依赖就是 `notify-rust`，而 `notify-rust` 三端的
`show()` 都返回 `Result<NotificationHandle>`，句柄上都有 `wait_for_response`。所以自建
这一层不是写三份实现，是**一份**加两处平台事实。这一层已实施，见
`src-tauri/src/toast.rs`，下表整张都已落地（macOS 那格仍然是「没接」）。

| 平台 | 按钮 | 激活 | 线程 |
| :--- | :--- | :--- | :--- |
| Windows | 转成 `tauri-winrt-notification` 的 `add_button`，≤5 | `Default`（点本体）/ `Action(id)` | `show()` 不阻塞；等待占一个线程 |
| Linux | XDG actions，实际显示几个由守护进程决定 | 同上；点本体要求先声明一个名为 `default` 的 action | 同上 |
| macOS | **一个都不画**，见下 | **没接**，见下 | 同上 |

- **macOS 上不画按钮。** 不是画不出（`notify-rust` 会把 `.action()` 映射成主按钮或
  "Options" 下拉），是按了没人接 —— 见下一条，那台机器上响应根本回不来。一个按下去可见地
  什么都不发生的按钮，比没有这个按钮更糟，所以那两句 `.action()` 在 macOS 上整体 cfg 掉，
  只留本体点击开应用。
- **macOS 的 `show()` 什么都不显示。** 它只把 notification 包进一个句柄；真正发出去发生在
  `wait_for_response`（同步，走 `NSUserNotificationCenter`，注释要求主 run loop 在转）
  或者句柄的 `Drop`（异步，`.ok()` 掉错误）。取后者：与插件丢句柄的行为逐位相同，所以
  macOS 的 toast 与今天一样，代价是三端里只有它听不到点击。为一个便利功能在非主线程里
  阻塞进 AppKit，且本机无法验证，不值。
- **要用 `wait_for_response`，不是 `wait_for_action`。** 后者的 Windows 分支把「点了本体」
  和「toast 自己超时」折成同一个 `"__closed"`（`notify-rust` `src/windows.rs:111`），
  照它写就是 toast 一超时就把窗口弹到用户面前。

**带按钮的 toast 要给它平台上限的寿命。** Windows 默认只显示约 5 秒，而「读两个选项再决定」
装不进 5 秒；超时之后按钮就只在通知中心里，而那里按不动（见下）。所以有按钮的通知带
`Timeout::Milliseconds(25_000)` —— 25000 正好是 `notify-rust` 映射到 Windows
`Duration::Long` 的门槛，在 XDG 下则是一次普通的 25 秒过期。**不要改用 `Timeout::Never`**：
XDG 下那是「挂到有人扫掉为止」，连带那个等待线程一起。没有按钮的通知维持默认寿命 ——
它只是一句话，进通知中心照样能读。

一条能力边界，三端一样：**只接得住弹窗还在屏幕上时的那次点击。** 超时之后 Windows 和多数
Linux 守护进程会在通知中心留一份，点那一份不送给运行中的进程 —— Windows 要注册一个 COM
activator，是安装器级别的东西，这个应用别处用不到。所以等待随弹窗结束，线程随等待结束。

按钮上限统一按 **2 个动作 + 本体点击（= 打开）** 设计：正好落在 macOS 下拉的能力内，
Linux 上守护进程吝啬时也还能露出来。

### 4.2 按钮方案

carrier 自带判别与提交面，按钮直接从它上面取（见 [A.1](#a1-dsh-侧的官方-api)）：

| 等待类型 | 按钮 | 提交什么 |
| :--- | :--- | :--- |
| `PendingApproval` | `允许` / `拒绝` | `answer('allowed-once')` / `answer('rejected')` |
| `PendingQuestion`，`kind === 'plan-review'` | `批准` / `要改` | `answer()` 带 `PlanReview.approve.label`（原样）/ `cancel()` |
| `PendingQuestion`，`kind === 'question'`，单题单选且选项 ≤2 | 两个选项的 label | `answer(batch)` |
| `PendingQuestion`，其余情况 | 仅 `打开` | —— |
| 轮次结束 | 仅 `打开` | —— |

三处要注意的：

- **`approval` 只能给一次性授权。** `ApprovalDecision` 只有 `'allowed-once' | 'rejected'`
  两个值，这个呈现面**没有**「一直允许」。通知上点「允许」的爆炸半径就是这一次调用。
- **plan-review 在应用里是三个动作**（`Approve` / `Refuse` / `Chat about it`），通知只有两个
  预算。取 `批准` = approve label 与 `要改` = `cancel()`（对应 `Chat about it`，把 composer
  还给用户）；`Refuse`（`decline` label）留给应用内。理由是「拒绝但不说话」在通知上是个
  死胡同，而「要改」必然要打字。
- **多题的 request 不逐题走完。** `QuestionAnswer` 是**整批**提交，通知只说「有 N 个问题在
  等」并给「打开」。一题一条 toast 会在 macOS / Linux 的通知中心里堆叠，不如在应用里翻页。

### 4.3 失效与兜底

- **key 校验。** 提交前比对 carrier 的 `key` 与通知发出时记录的是否一致；不一致直接丢弃并
  降级为「打开」。`key` 的声明是「Opaque request identity; a replacement request must use a
  new key」，正好是这个用途。dsh 会在重连和 resync 时重放待处理请求，用户点到的可能已经
  是过去的那一个。
- **插件缺失。** 插件没装则没有信号，而且现在**没有兜底**：`turn.rs` / `waiting.rs` 已经删了。
  插件保持**面板里可选**、不自动装（`resources/preset-plugins.json` 里的
  `dsh-desktop-signal`，装的是随包那份，不联网）。所以「没装」这件事必须看得见，
  见下面 [4.4](#44-开关与它的前提)。
- **两条路同时在，但不会响两次。** 第 3 步之后信号自己会弹通知，DOM 那两个 watcher 也还
  会弹 —— 所以插件接线时挂一个 `window.__dshSignals`，两个 watcher 的每一 tick 开头看见它
  就整个停手（不是「让一让」，是不干了）。放在页面里而不是 Rust 里，是因为这样刷新一次
  就自动复位、装插件不用重启应用、卸插件自动把兜底还回来 —— 而 Rust 侧的一个标记要处理
  「页面重载了但标记还在」。竞态窗口是零而不是小：两个 watcher 本来就有 4 秒的基线期
  （`turn.rs` 的 `SETTLE`、`waiting.rs` 的 `GRACE`），插件的 `apply` 在文档加载时就跑完，
  正好落在那 4 秒里面。
- **答案怎么回来。** 按钮 id 由 Rust 定（`signal.rs` 的 `buttons()`），插件按 id 翻成
  carrier 上的调用（`client.js` 的 `submit()`）—— 这张表两半都要改，改一半的失效模式是
  「按了没反应」，而下面那条把它变成响亮的。
- **key 不匹配走一次回程。** 原计划写的是「直接丢弃并降级为「打开」」，但 Rust 侧
  `window.eval` 没有返回值，谁校验谁就得说话。所以插件校验不过（key 变了、会话不在等、
  choice 这个 kind 认不出、或者 `answer()` 自己 reject 了）时回一条
  `dsh-window://signal?event=stale`，Rust 收到就唤起窗口并切到那个会话。**静默什么都不做
  是最坏的一种**：用户会以为自己已经答了。
- **通知文案只有一份。** 信号弹的和 watcher 弹的是同一句话，字符串收在 `signal.rs`
  （`turn_ended()` / `waiting_on(kind)`），`turn.rs` 与 `waiting.rs` 从那里取。两份中英文
  对照散在两个地方就是 `i18n` 那套写法专门要避免的漂移，而且要删的是取用的那一半。
- **版本漂移。** dsh 的客户端包目前是 `0.0.1-rc.*`，契约会动。插件里锁死版本，并接受
  「dsh 升级后插件需要跟版本」这件事。桥断掉时是**加载失败**（响亮），不是误答（安静）。
- **线程上限。** 每个还在等点击的通知占一个阻塞线程，上限在 `toast.rs` 的 `LISTENERS`
  （8，超出就照样弹但不听点击）。第 4 步没有把它改紧：待回答的 toast 最终**没有**活得比
  弹窗久，只是拿了平台的长寿命（见 [4.1](#41-通知层三端实现)），等待照样随弹窗结束。
- **`wait-over` 撤不掉已经弹出的通知，这条到此为止。** 原计划说和第 4 步一起做，做不了：
  `notify-rust` 的 Windows `NotificationHandle` **根本没有 `close()`**，而 XDG 那个的
  `close(self)` 与 `wait_for_response(self)` 都按值拿走句柄 —— 二选一，撤销就听不见点击。
  真要撤销得绕过 crate 直接对 `windows::UI::Notifications` 写，成本与收益不成比例（收益
  是「已经答过的问题的 toast 早消失 20 秒」）。答过之后按旧 toast 的按钮不会误答，那条路
  由上面的 key 校验兜住。

### 4.4 开关与它的前提

通知总开关（`settings::notifications`，菜单里的「通知」）本来就有。第 5 步把兜底删掉之后
它多了一个前提：**没装插件，就没有任何东西可通知**。

所以那一项现在有三种状态而不是两种：

| 插件 | 偏好 | 菜单里的样子 | `notify::show` |
| :--- | :--- | :--- | :--- |
| 没装 | 任意 | 变灰、不可点，hover 说明缺什么 | 直接返回 |
| 装了 | 开 | 打勾 | 照常两道闸 |
| 装了 | 关 | 不打勾 | 直接返回 |

三处要注意的：

- **闸门在 `notify::show`，也就是所有通知的唯一出口。** 于是「关」只有一个意思。代价是
  第三方 dsh 插件调 `window.Notification` 也一并静音——本机 dsh `0.1.2-rc.1` 的 222 个包
  里没有任何一个调它（查过），所以今天这个代价是零；哪天不是零了，加第二道闸，而不是把
  这道闸放宽。
- **变灰而不是隐藏。** 一个消失的设置项，用户找不回来问为什么。
- **不用 `disabled` 属性，用 class + `aria-disabled`。** 浏览器不给 `disabled` 的按钮显示
  `title` 气泡，那一行就会灰着却说不出原因。点击的拒绝由 handler 做，Rust 侧
  `toggle_notify_turns` 再挡一次——那条 verb 和其它所有 verb 一样，页面里任何脚本都够得着。
- **偏好本身不被改写。** 拔掉插件不会把存着的 `true` 改成 `false`；装回来开关还在原处。

## 5. 实施步骤

```
1. dsh 客户端插件骨架（`plugin/`）：认宿主，`dsh.client.inject` 填核实过的包
   （见 [A.1](#a1-dsh-侧的官方-api)）+ 插件 `inject: ['sessions', 'uiSession']`，
   订阅 sessions.list（running / completed）与 uiSession.pendingInteractions
   （kind / key / sessionId），两路都带 sessionId 上报
   → 验证：切换会话、提问、跑完，Rust 侧收到的事件与侧边栏圆点状态一致；
          `__DSH_VERSION__` 不存在时插件整体不接线；加载日志里没有 inject 解析告警

2. 自建通知层 toast.rs 替换 tauri-plugin-notification，点击本体 → reveal()
   → 验证：Linux 上停掉通知守护进程，日志出现明确失败而非静默；
          三端 toast 的名称与图标仍为已安装应用自身；
          点 toast 本体，窗口从托盘回到前面

3. 通知从信号里发（携带 sessionId），DOM watcher 让位；点击本体 → reveal() +
   eval 调插件的 open(id)
   → 验证：多会话下点通知，回到的是提问的那个会话；插件在时不会一个事件响两次

4. 按 4.2 的表加动作按钮，激活 → 插件提交答案
   → 验证：key 不匹配时拒绝提交并降级为「打开」；
          approval / plan-review / 二选一 question 各走通一次
   ✅ 已完成

5. 删除 turn.rs / waiting.rs 及其注入
   → 验证：全量测试通过；无插件时通知功能整体缺席而非报错
   ✅ 已完成。「缺席而非报错」按下面 4.4 那条做成了看得见的缺席：菜单里那一项变灰并说明
     为什么

X. 随包发货 + preset 一行（原属第 1 步，见下）
   → 验证：`bundle-runtime` 把 plugin/ 摆进 resources/；面板里能装上且不联网；
          装出来的 link: 指向安装目录，且 `dsh.profile.bundles` 里有它
   ✅ 已完成
```

**第 1 步的交付形态与原计划有一处不同，现已补齐。** 原来括号里写的是「随包资源 + preset
一行」，实装时拆开了：信号链路（`plugin/` + `src-tauri/src/signal.rs` + `controls.rs` 一条
路由）先完成，随包发货与 preset 那一行留到后面单独做——因为 preset 的 `spec` 是
`resources/preset-plugins.json` 里的静态字符串，而随包资源的路径只有运行时才知道
（`resource_dir()`），确实不是「一行」。它现在做完了，形态与踩到的坑见
[3.2](#32-交付形态)。

开发期的装法见 [A.4](#a4-客户端插件的加载契约)：
`dsh plugin --profile web add -w ./plugin`，装出来是指向仓库的 `link:`。

**第 4 步比原计划多了一条上行字段和一个上行事件。** 通用问题的按钮文字是提问方自己的
option label，Rust 画按钮就得拿到它，所以 `wait` 信号多了可重复的 `option=`（只在
「单题、单选、选项 1–2 个」时由插件发，最多两个）；`approval` 和 `plan-review` 的两个按钮
每次都说同一句话，由 Rust 用 `t!` 写，不走这条。回程的 `stale` 见
[4.3](#43-失效与兜底)。第 4 步的能力边界与 `wait-over` 的结论也记在那一节。

**第 3 步比原计划多做了一件事，因为绕不开。** 「通知携带 sessionId」等于通知必须由信号
来发 —— watcher 从 DOM 里看不出 sessionId，靠时间把两边对起来是猜，正是这次改造要去掉的
东西。所以第 3 步把发通知这件事从 watcher 搬到了 `signal.rs`，watcher 只保留兜底（怎么让位
见 [4.3](#43-失效与兜底)）。`wait-over` 目前不撤掉已经弹出的通知：那要求一直持有通知句柄，
和第 4 步「toast 要活得比弹窗久」是同一件事，放在那里一起做。

**2 和 3 调过顺序。** 原来的第 2 步是「点击本体 → reveal()」，但在
`tauri-plugin-notification` 还拿着 toast 的时候，根本没有激活可接 —— 它 `show()` 之后
就把句柄丢了。所以「换掉通知层」必须排在「点击有反应」前面，而不是后面。换过来之后
第 2 步顺带把可诊断性也一起交付了。

第 2 步之后已经可用（收进托盘、点通知回到窗口），第 3 步把「回到窗口」变成「回到提问的
那个会话」，第 4 步才是新能力，第 5 步把旧路拆掉。**到这里整条链路只剩一条路：dsh 的状态
→ 插件 → `signal.rs` → `toast.rs`。**

## 6. 未决问题

三个都结了，结论记在下面。

> ~~1. 插件是否自动安装。~~ **2026-09-10 改判：装，首次启动自动装一次。** 见
> `plugins.rs` 的 `adopt()`，调用点在 `main.rs` 的 `boot` 里、dsh gate 之后、插件面板之前。
>
> 上一版结论是「不装」，理由是「插件后续可能单独发到 npm，自动装会把这个应用从
> 「能配任何 dsh」变成「需要我们的插件」」。**这条理由的前半截已经不成立**：npm 那件事
> 同日结掉了，不发（见 [3.2](#32-交付形态)）。剩下的后半截撑不住代价 —— 代价是通知功能有
> 一个用户可见的前提，而用户没有任何理由知道它存在，只能从一个变灰的菜单项那里学到，那
> 是比「根本不用学」差得多的地方。
>
> 「自动装」的分寸是**一次自荐，不是强制**：
>
> - **只装一次。** 标记文件 `signal-adopted` 记在 `dsh::app_dir` 下，和 `plugins-guided`
>   并排。卸掉它的用户是做了决定，下次启动再装回来就是在跟他争。
> - **先记再装，但装失败要看得见。** 前半截和 `mark_guided` 同一条理由：拖垮这次启动的
>   安装不该把下次也拖垮，所以花掉的是「一次尝试」而不是「一次成功」。后半截是这里和
>   `mark_guided` 不一样的地方 —— 那边标记白花的代价是面板少显示一次，用户随时能自己打开；
>   这边的代价是通知**静默地**永远不工作，直到有人注意到菜单里那一项是灰的。两边不对称，
>   所以 `adopt()` 返回「这次试了但没成」，`boot` 把它变成**把插件面板打开**：面板列着这个
>   插件、点一下就是同一个安装、这次有地方打印失败原因。
>
>   为什么不是「下次启动重试」：现实的失败模式几乎都在 `ensure_pnpm`（可能要
>   `npm install -g pnpm`）和 registry 上，一台长期离线的机器会因此在**每次**启动前卡一次
>   npm 超时。用一次面板换掉一个无上限的重试循环。
> - **托盘自启动的那次不装。** 和上面跳过更新检查同一条理由：没人在看，而这件事够得着网络
>   —— pnpm 不在就是一次 `npm install -g`。留给下一次用户自己打开的启动。
>
> [4.4](#44-开关与它的前提) 那张三态表不变：它描述的是「没装」时长什么样，而「没装」现在
> 的含义从「还没去装」变成了「装过、被拿掉了」。
>
> ~~2. `approval` 是否允许在通知上直接点「允许」。~~ 已结，第 4 步按「给」实施：
> `ApprovalDecision` 只有 `'allowed-once' | 'rejected'`，这个呈现面根本给不出「一直允许」，
> 误答的代价是一次工具调用而不是一项长期授权。
>
> ~~3. 拿 carrier 的路径待定。~~ 已结，见 [A.1](#a1-dsh-侧的官方-api)：carrier 就在
> `ctx.uiSession.pendingInteractions` 这个公开 observable 里，与 composer 座位无关。

## 附录 A：已核实的事实与证据

核实日期 2026-09-09。命令见 [附录 B](#附录-b如何复现这些核实)。

> **2026-09-09 复核：A.1 曾整节写错。** 上一版是从 npm 拉 `@deepseek-ai/dsh-client-runtime@0.0.1-rc.1`
> 的类型声明写的。本机装了 dsh `0.1.2-rc.1` 之后核实：那个包在实际发行的 dsh 里**不存在**，
> `PendingWait` 全仓 grep 不到，`SessionSummary` 上也没有 `pendingInteraction` 字段。
> npm 上那个包是另一条血脉。下面是按本机实际安装重写的。

### A.1 dsh 侧的官方 API

核实自 `/d/nvm4w/nodejs/node_modules/@deepseek-ai/dsh/node_modules/@deepseek-ai/`（dsh
`0.1.2-rc.1` 把整个单仓装在自己的 `node_modules` 里，222 个包）。

**轮次结束**：`@deepseek-ai/dsh-api-session-controller` 声明 `ctx.sessions`
（`lib/types/client/index.d.ts:20`），`contract/sessions.d.ts` 的读写面：

- `readonly list: ObservableSnapshot<SessionListState>`（`:21`）—— `byId` / `ids` /
  `current` / `phase`
- `open(id: SessionId): void`（`:42`）—— 切换当前会话

`sessions/service.d.ts` 的 `SessionSummary` 上和通知有关的两个字段：

```ts
running: boolean;
completed?: boolean;   // "Finished while not selected and not yet opened —
                       //  the sidebar's green 'done' reminder. Absent = false."
```

**三类等待与 carrier**：不在会话列表里。`@deepseek-ai/dsh-client-ui-session` 的
`UiSession` 服务（cordis 名 `uiSession`）上：

```ts
/** Root source of pending UI interactions, independent from Controller snapshots. */
readonly pendingInteractions: HostObservable<SessionPendingInteractionSnapshot>;
```

`lib/client.js:83` 的实现就是一对标准 store 方法，能直接订阅，不需要 React：

```js
pendingInteractions = {
  getSnapshot: () => this.pendingSnapshot,   // ReadonlyMap<SessionId, …>
  subscribe: (listener) => { …; return unsubscribe }
}
```

快照的值类型 `SessionPendingInteraction` 由各域**声明合并**进
`SessionPendingInteractionMap`，本机只有两个供稿者：

| 域 | 键 | carrier 类 | `kind` | 提交面 |
| :--- | :--- | :--- | :--- | :--- |
| `dsh-client-ui-approval` | `approval` | `PendingApproval` | `'approval'` | `answer('allowed-once' \| 'rejected')` |
| `dsh-client-ui-user-questions` | `question` | `PendingQuestion` | `'question' \| 'plan-review'` | `answer(QuestionAnswer)` / `cancel()` |

两个 carrier 的公共身份是 `SessionPendingInteractionBase { key, kind, sessionId }`，
`sessionId` 的注释是「Session whose UI can answer this interaction」。两者都另有
`delegate()`（把未答请求交给瀑布里的下一个监听者）与 `abort(reason)`。

**这就是原未决问题 3 的答案。** carrier 与信号是同一个对象，躺在一个公开 observable 里；
`conversation.composer` 链拿到的 `matched` 只是同一对象的窄化引用。读快照不认领任何东西 ——
呈现权仍归 `dsh-client-ui-user-questions` 的 composer takeover，我们只是也持有同一个
carrier。

`PendingQuestion` 的其余面（`contract/slots.d.ts`）：

- `questions: readonly AskUserQuestionItem[]` —— **数组**，一个 request 可带多题
- `answer(answer)` —— `QuestionAnswer` 的注释是 "one structured answer batch covering
  **every** question of the request"
- `planReviewOf(questions)` —— 收窄成 `PlanReview { id, question, plan, approve, decline? }`；
  只在「单题、声明 intent、plan 在 detail 里、给了 approve label、二元单选」时成立，
  答案必须原样带 option 的 label
- `result: Promise<QuestionAnswer>` / `isDelegation(reason)`

对照现有实现：

| | 现在 | 官方 API |
| :--- | :--- | :--- |
| 轮次结束 | 嗅探 svg 子节点是 `rect` 还是 `path` | `SessionSummary.running` / `.completed` |
| 三类等待 | 查 3 个 `data-*-key` 属性 | `pendingInteractions` 快照的 `kind` |
| 能不能作答 | 做不到 | carrier 自带 `answer()` |
| 多会话 | 无概念 | 快照按 `SessionId` 分键 |
| 回到提问处 | 做不到 | `sessions.open(id)` |

一处待观察：`dsh-vendor-login@0.2.0` 的 `dsh.client.inject` 里写着
`@deepseek-ai/dsh-client-runtime`，而这个包在本机 dsh 里不存在。说明 `inject` 里的未知包名
可能是被容忍的（也可能那个插件在这版 dsh 上其实是坏的）。我们自己只 inject 核实过的包，
第 1 步顺带看一眼加载日志。

### A.2 平台通知能力

| 能力 | Windows | macOS | Linux |
| :--- | :--- | :--- | :--- |
| 点击本体 → 回到应用 | ✅ | ✅ | ✅ |
| 动作按钮 | ✅ ≤5 | ⚠️ 1 主按钮或一个下拉 | ⚠️ 有，显示几个由守护进程决定 |
| 自由文本输入 | ❌ crate 不支持（平台支持） | ⚠️ 仅 `preview-macos-un` | ❌ 协议本身没有 |
| 一次多题 / 表单 | ❌ | ❌ | ❌ |

- `tauri-plugin-notification@2.3.3` 的 `src/desktop.rs`：`show()` 把通知交给
  `tauri::async_runtime::spawn(async move { let _ = notification.show(); })`，句柄和错误
  一起丢弃，永远返回 `Ok`。**这是必须绕过它的唯一原因。** 而它三端唯一的依赖就是
  `notify-rust`，`app_id`（Windows）与 `set_application`（macOS）两处身份设置也都在这
  32 行里 —— 所以「绕过它」等于把这 32 行抄进 `toast.rs`，不是重写一个通知层。
- **`notify-rust@4.18.0` 的 Windows 后端已经把按钮和激活接好了**（`src/windows.rs`）：
  `notification.actions` 每两项转成一次 `add_button`，`on_activated` / `on_dismissed`
  写进一个 mpsc，`show()` 返回持有 `Receiver` 的句柄。所以 `tauri-winrt-notification`
  不必作为直接依赖 —— 它在下面一层。上一版这张表把 Windows 写成要直接对着它写，是多余的。
- **`show()` 之后激活还送得到。** 这是自建这层唯一没法靠读代码确认的事：`show()` 里注册
  `Activated` 的那个 `ToastNotification` 出了函数就析构了。实测送得到 —— `toast.rs` 留了
  一个 `#[ignore]` 的探针（`cargo test -- --ignored --nocapture raises_one_real_toast`），
  弹一条真 toast 然后打印回来的是什么；放着不管，6.5 秒后打印 `Closed(Expired)`，说明
  事件确实从 WinRT 回到了这个进程的线程里。点它则是 `Default`，走的同一条路。
- `tauri-winrt-notification@0.7.3` 的 `src/lib.rs`：XML 只写
  `<action content='' arguments=''/>`，没有 `<input>`，也没有 raw-XML 逃生口；
  `on_activated` 只读 `args.Arguments()`，**不读 `args.UserInput()`**。Windows 平台本身
  完全支持 ToastGeneric 的 `<input type='text'>` / `<input type='selection'>`，要用得直接
  对着 `windows::UI::Notifications` 写。
- `notify-rust@4.18.0`：`src/xdg/mod.rs` 三处注释 `/* XDG does not support inline
  replies */`；`src/response.rs` 里 `NotificationResponse::Reply(String)` 的文档是
  "Only produced by the `preview-macos-un` backend … On all other backends this variant is
  never emitted."
- `notify-rust` 的 macOS 后端 `src/macos/nsusernotifications.rs` 会把 `.action()` 映射成
  `MainButton::SingleAction`（1 个）或 `MainButton::DropdownActions("Options", …)`（多个），
  并把响应的 label 翻译回 id。`send()` 是同步阻塞的。
- 三端各自的环境前提（未变，`notify.rs` 的模块文档已有详述）：Windows 需要 AUMID 能解析到
  开始菜单快捷方式，NSIS 已经盖过 `${BUNDLEID}`，**不要再盖第二次**；macOS 需要 `.app`
  bundle 且 bundle id 已注册；Linux 需要 `org.freedesktop.Notifications` 守护进程。

### A.3 一处需要更正的既有注释（已改）

`notify.rs` 的模块文档称三个平台对是否投递激活「意见不一」，只有 Windows 可达。按
`notify-rust@4.18.0` 的实际代码，三端的 `show()` 都返回句柄
（`xdg::NotificationHandle` / `macos::NotificationHandle` / `windows::NotificationHandle`），
`tauri-winrt-notification` 也有 `on_activated`。真正的障碍只有插件把句柄扔了这一件，
与平台无关。**已随第 2 步改掉**（顺序见 [第 5 节](#5-实施步骤)）：那一节现在讲的是
点击进 `reveal()`、页面自己的 `onclick` 仍然不触发，以及为什么。

### A.4 客户端插件的加载契约

核实自本机 dsh `0.1.2-rc.1`：`@deepseek-ai/dsh-client-modules` 的 `README.md` 与
`lib/types/client/manifest.d.ts`，以及三个已装插件的构建产物。

**客户端 bundle 不是 ESM。** 宿主往 `<head>` 注入 `window.__ModuleLoader__` 门面和
`window.__DSH_BOOT__` 引导图，bundle 是一次惰性 CJS 工厂注册：

```js
window.__ModuleLoader__.load({ id: "<包名>", factory: (require) => {
  var module = { exports: {} };
  // …模块体的副作用都在闭包里，materialize 时才跑
  return module.exports;   // { inject: ['sessions'], apply(ctx) {…} }
}})
```

- **一个 bundle 一个 module node**（README 的 Known Limitations：flat module graph by
  design）。多模块要自己打包成一个文件。
- `require` 只解析冻结的基线表（React、Cordis、静态 UI 库）加 `dsh.client.external`
  声明过的精确 specifier，其余**抛错**。类型导入被擦除，不产生请求。
- 宿主只检查文件存在：README 的 Build requirements 说缺 bundle 会「fails activation
  loudly with one build instruction」—— 它不验证是谁产出的。

**两个同名不同义的 `inject`**，是文档前几版写错的地方：

| 位置 | 内容 | 实例 |
| :--- | :--- | :--- |
| `package.json` 的 `dsh.client.inject` | **包名**，factory 到达顺序 + cordis 组装边 | `['@deepseek-ai/dsh-client-ui-settings']` |
| bundle 导出的 `inject` | **cordis 服务名** | `exports.inject = ['slots']` |

manifest 的其余字段：`dsh.client.platform: 'web'`、`dsh.client.external`、
`dsh.bundle.patch`；`./client` 是独立的 exports 条目。整份 `dsh.client` 声明的类型在
`manifest.d.ts`。

**装进 profile 是两处登记**：`~/.dsh/profiles/web/package.json` 的 `dependencies` 与
`dsh.profile.bundles`。profile 根的 `cordis.yml` 是空数组，树由 bundles 逐层 patch 组成，
用户改的是 `cordis.patch.yml`。

两个会让插件**静默不加载**的坑，都是实装时踩到的：

- **依赖型插件必须声明 `dsh.bundle.patch`。** `dsh plugin add` 会把
  `dsh.profile.bundles` 按已安装状态重新对账（`dsh/lib/plugin-*.js` 的
  `reconcilePlugins`），而只有 manifest 里声明了 `dsh.bundle.patch` 的依赖才进 layer
  stack；没有的话它作为普通依赖装上、打印一条 `declares no dsh.bundle` 警告、然后永远不
  加载。patch 文件本身可以只有一条 insert：

  ```yaml
  - insert:
      - id: desktop-signal
        name: 'dsh-desktop-signal'
  ```

  在盒内的 bundle（`@deepseek-ai/dsh-client-ui-user-questions` 那些）没有这个字段，因为它们
  来自 profile 模板而不是依赖 —— 别照着它们抄 manifest。

- **本地路径是一等公民，但要 `-w`。** `dsh plugin add` 有专门的 `anchorPathSpec`，把
  相对路径 spec 锚到调用者的 cwd（否则 pnpm 的 cwd 是 profile 目录，`add ./plugin` 会
  自链接 profile）。所以 `dsh plugin --profile web add -w ./plugin` 直接可用，装出来是
  `"dsh-desktop-signal": "link:…"`。`-w` 是因为 profile 是个只含自己的 pnpm workspace
  根 —— `plugins.rs:434` 已经在条件性地加这个 flag。


## 附录 B：如何复现这些核实

**本机装了 dsh 时，整个单仓都在本地**，比拉 npm 快得多 —— dsh 把 222 个包装进自己的
`node_modules`：

```sh
D=$(dirname "$(readlink -f "$(which dsh)")")          # dsh 的安装位置
ls "$D/node_modules/@deepseek-ai/dsh/node_modules/@deepseek-ai/"
# 每个包都带 README.md / README.zh.md，写的是契约级文档；类型在 lib/types/ 下，
# 实现在 lib/*.js（客户端半边是 lib/client.js）
```

> **别走 `~/.dsh/profiles/node_modules/@deepseek-ai/`。** 那里面是符号链接，指向**装它
> 那次**用的 Node 版本目录（本机指向 `/d/nvm/v22.22.3/…`），换过 Node 之后有一部分是悬空
> 的、剩下的是旧版本。A.1 第一版就是被这个坑到的：`dsh-client-runtime` 在那里「存在」（一条
> 悬空链接），在实际运行的 dsh 里不存在。以 `which dsh` 解析出来的那份为准。

npm 上也公开发布了一部分包（scoped），没装 dsh 时可以拉：

```sh
npm pack @deepseek-ai/dsh-client-ui-user-questions
npm pack @deepseek-ai/dsh-client-ui-conversation
tar -xzf deepseek-ai-dsh-client-ui-user-questions-*.tgz
```

但**版本会和实际发行的 dsh 不一致**：npm 上有 `@deepseek-ai/dsh-client-runtime@0.0.1-rc.1`
这样的包，dsh `0.1.2-rc.1` 里根本没有它。拉 npm 只能当没有本机安装时的退路，结论要以
本机安装复核。

`@deepseek-ai/dsh` 本身只有 43KB，是 CLI 与转发器，UI 不在里面；它的 `dependencies`
是查包名的最快索引。

Rust 侧的依赖源码在 `~/.cargo/registry/src/index.crates.io-*/` 下，
`tauri-plugin-notification-2.3.3` / `notify-rust-4.18.0` /
`tauri-winrt-notification-0.7.3` / `tao-0.35.3` / `tauri-2.11.5`。
