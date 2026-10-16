# 易用性功能计划（第二轮）

分支：`feat/convenience-2`

目标和第一轮一样：让日常用起来少走几步。原则不变：dsh 一行不改，安装包保持轻量。

按顺序一条一条做，每条做完单独验证、单独提交。

## 进度

| # | 功能 | 状态 |
| :--- | :--- | :--- |
| 1 | 跑任务时不让电脑睡眠 | 已完成并验证（Windows） |
| 2 | 托盘里列出在跑 / 在等你的会话 | 已完成并验证（托盘菜单的样子没截图） |
| 3 | 通知里直接回复 | 未开始 |
| 4 | 快捷键唤出的小输入框 | 未开始 |
| 5 | `dsh://` 链接 | 未开始 |
| 6 | 把剪贴板送进当前会话 | 未开始 |

## 先查清楚的：插件能对会话做什么

3、4、6 都要插件往会话里写东西。查的是 dsh 0.2.0-rc.2 的类型声明（`node_modules/@deepseek-ai/dsh/node_modules/@deepseek-ai/*/lib/types`）：

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

**要做的**：「对话已完成」的通知上加一个输入框和「发送」按钮，输入后不用打开窗口就把这句话发进那个会话。

- 只做 Windows。`tauri-winrt-notification` 不支持输入框，`notify-rust` 的 `Reply` 只有 macOS 的预览后端会给，所以这一种通知自己用 WinRT 拼 toast XML（`<input id="reply" type="text"/>`），从 `ToastActivatedEventArgs.UserInput` 读回文字。
- macOS、Linux 照旧，只有正文点击。
- 发送走插件：`__dshSignals.reply(session, text)`，失败（会话不在了、插件不在）就打开窗口到那个会话，并把文字放进输入框，不丢。

**完成标准**：一轮跑完弹的通知里能输入，发送后这句话出现在那个会话里并开始新的一轮，窗口不出来。

## 4. 快捷键唤出的小输入框

**要做的**：按一个全局快捷键弹出一个小窗口，输入一句话回车，在最近用的工作区里新开一个会话去跑；跑完靠已有的通知告诉你。

- 小窗口是本地页面，不加载 dsh；文字经 `dsh-window://` 通道交给 Rust，再由插件新建会话、发消息。
- 默认快捷键另定，设置卡片里可改，和现有的全局快捷键一样录制。
- 工作区默认用最近的那个；小窗口里可以切到其他最近工作区。

**完成标准**：主窗口在托盘里时按快捷键，输入回车，dsh 里出现一个新会话在跑这句话；跑完弹通知。

## 5. `dsh://` 链接

**要做的**：注册 `dsh://` 协议。`dsh://open?path=<路径>` 和 `dsh-desktop <路径>` 一样打开那个文件夹的工作区。

- 用 `tauri-plugin-deep-link`，安装器模板里已经有注册段。
- 只认 `open`，参数只有 `path`。网页能点出这个链接，所以它不能做比「打开一个文件夹」更多的事。

**完成标准**：在浏览器里点 `dsh://open?path=C:\work\x`，dsh 打开这个文件夹的工作区，冷启动和运行中都行。

## 6. 把剪贴板送进当前会话

**要做的**：一个全局快捷键：把剪贴板里的文字追加到当前会话的输入框里，窗口拿到前台、光标在输入框。不自动发送。

- 先只做文字。图片要走 dsh 的附件流程，`draftAttachments` 是内部的，后面再看。

**完成标准**：在别的程序里复制一段报错，按快捷键，dsh 出来，输入框里是原来的草稿加上这段文字。
