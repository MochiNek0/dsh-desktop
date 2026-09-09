# 通知与回答：现状、结论与改造方案

> 状态：设计稿，待实施。撰写于 2026-09-09，基于 dsh-desktop 0.1.13 与 dsh 0.1.2-rc.1。
>
> 这份文档记录的是一次调研的结论。中间大量事实是逐条核实过的（见
> [附录 A](#附录-a已核实的事实与证据)），核实成本远高于阅读成本，所以证据和结论写在一起 ——
> 改动这份方案之前，先看那一节是否仍然成立。

## 1. 现状

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

**后续插件会拆出去、单独发 npm。** 这件事从第一天就要付的代价是三条约束：

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

一处校验缺口，记录在案：`plugins.rs:457` 用 `spec_name()` 组装安装后的待校验集合，而
`spec_name` 对含 `/` 或 `:` 的 spec 返回 `None`（`:798`），本地路径会被丢出那个集合，装完
无法确认。`github:` 现在就是这个待遇 —— 是既有的、已接受的缺口，不是本次新增的。

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

| 平台 | 用什么 | 按钮 | 激活 | 线程 |
| :--- | :--- | :--- | :--- | :--- |
| Windows | `tauri-winrt-notification` | `add_button`，≤5 | `on_activated(Option<String>)` 拿 `arguments` | 回调，不占线程 |
| macOS | `notify-rust` 的 `.action()` | 1 个 → 主按钮；≥2 → "Options" 下拉 | `send()` 的返回值 | **阻塞**，占一个线程 |
| Linux | `notify-rust` xdg | XDG actions，实际显示几个由守护进程决定 | `wait_for_action` | **阻塞**，占一个线程 |

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
  那一个。
- **插件缺失。** 插件没装则没有信号。`resources/preset-plugins.json` 加一行指向随包的
  `plugin/`，先只做面板里可选（自动安装另见 [未决问题 2](#6-未决问题)）；并保留现有
  `turn.rs` / `waiting.rs` 一个版本做兜底；稳定后删除那两个模块（约 700 行含测试）是本次
  改造最大的净收益。
- **版本漂移。** dsh 的客户端包目前是 `0.0.1-rc.*`，契约会动。插件里锁死版本，并接受
  「dsh 升级后插件需要跟版本」这件事。桥断掉时是**加载失败**（响亮），不是误答（安静）。
- **线程上限。** macOS / Linux 每个待回答通知占一个阻塞线程，需要超时与并发上限，否则挂着
  不管的会话会堆线程。

## 5. 实施步骤

```
1. dsh 客户端插件骨架（`plugin/`）：认宿主，`dsh.client.inject` 填核实过的包
   （见 [A.1](#a1-dsh-侧的官方-api)）+ 插件 `inject: ['sessions', 'uiSession']`，
   订阅 sessions.list（running / completed）与 uiSession.pendingInteractions
   （kind / key / sessionId），两路都带 sessionId 上报
   → 验证：切换会话、提问、跑完，Rust 侧收到的事件与侧边栏圆点状态一致；
          `__DSH_VERSION__` 不存在时插件整体不接线；加载日志里没有 inject 解析告警

2. 通知携带 sessionId；点击本体 → reveal() + eval 调插件的 open(id)
   → 验证：多会话下点通知，回到的是提问的那个会话

3. 自建通知层 toast.rs 替换 tauri-plugin-notification，先只做「打开」按钮
   → 验证：Linux 上停掉通知守护进程，日志出现明确失败而非静默；
          三端 toast 的名称与图标仍为已安装应用自身

4. 按 4.2 的表加动作按钮，激活 → 插件提交答案
   → 验证：key 不匹配时拒绝提交并降级为「打开」；
          approval / plan-review / 二选一 question 各走通一次

5. 删除 turn.rs / waiting.rs 及其注入
   → 验证：全量测试通过；无插件时通知功能整体缺席而非报错
```

**第 1 步的交付形态与原计划有一处不同。** 原来括号里写的是「随包资源 + preset 一行」，
实装后拆开了：信号链路（`plugin/` + `src-tauri/src/signal.rs` + `controls.rs` 一条路由）
已经完成并验证，而**随包发货与 preset 那一行没做**。原因是 preset 的 `spec` 是
`resources/preset-plugins.json` 里的静态字符串，随包资源的路径只有运行时才知道
（`resource_dir()`），所以要么在 `install` 里做一次占位符替换，要么走非 preset 的安装路径 ——
不是「一行」。它不影响第 1 步的任何验证条件，所以单独作为一步排在后面。

开发期的装法见 [A.4](#a4-客户端插件的加载契约)：
`dsh plugin --profile web add -w ./plugin`，装出来是指向仓库的 `link:`。

第 1、2 步之后就已经可用（点击回到会话），第 3 步是可诊断性，第 4 步才是新能力。

## 6. 未决问题

1. **`approval` 是否允许在通知上直接点「允许」。** 倾向给，而且核实完更倾向给：
   `ApprovalDecision` 只有 `'allowed-once' | 'rejected'`，这个面根本给不出「一直允许」，
   误答的代价是一次工具调用而不是一项长期授权。保守方案仍是只留「拒绝」和「打开」。
2. **插件是否自动安装。** 自动安装会把这个应用从「能配任何 dsh」变成「需要我们的插件」。
   现有 `notify.rs` 的设计哲学是「没有桥可说，也就没有桥会断」，这一步是明确地立一座桥 ——
   但是一座会响亮地断的桥。随包发货（见 [3.2](#32-交付形态)）把代价压低了不少：不联网，
   装不上的失败面小得多。但「默认装上」和「面板里可选」仍是两种产品姿态，这个问题不因
   交付形态而消失。第 1 步先按可选做。

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
  一起丢弃，永远返回 `Ok`。**这是必须绕过它的唯一原因。**
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

### A.3 一处需要更正的既有注释

`notify.rs` 的模块文档称三个平台对是否投递激活「意见不一」，只有 Windows 可达。按
`notify-rust@4.18.0` 的实际代码，三端的 `show()` 都返回句柄
（`xdg::NotificationHandle` / `macos::NotificationHandle` / `windows::NotificationHandle`），
`tauri-winrt-notification` 也有 `on_activated`。真正的障碍只有插件把句柄扔了这一件，
与平台无关。**实施第 3 步时同步改掉那段说明。**

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
