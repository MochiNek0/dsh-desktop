# 需求文档：DSH Desktop (Tauri v2) 移动端扫码直连与多通道远程访问架构

## 〇、事实核验基线

本文档的技术前提已对照以下来源核实，修改前请先复核，避免再次基于过时假设设计：

| 事实                                                                                                                                                                               | 来源                                                              |
| ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------- |
| `dsh web` 支持 `--host`（仅接受 `127.0.0.1` / `0.0.0.0`）与 `--trusted-host <authority...>`                                                                                        | `dsh web --help`；`@deepseek-ai/dsh-host-webserver` README        |
| 官方明确「`dsh web --host 0.0.0.0` 仍不受支持」：服务器自身不提供 TLS、认证与来源策略                                                                                              | `dsh-host-webserver` / `dsh-client-connection` README「已知限制」 |
| `/api` 前置「浏览器信任栅栏」：Host 必须是 loopback 或匹配 `trustedHosts`；带 `Origin` 时必须等于 Host；`sec-fetch-site: cross-site` 一律拒绝。用途是防御 DNS rebinding 与跨站请求 | `dsh-client-connection` README；其 `src/api-request-trust.ts`     |
| launch token **只在 `GET /` 被接受**；不接受其他路径的 query token，也不接受 Authorization header token                                                                            | `dsh-client-connection` README                                    |
| 会话 cookie 在确定性名称与签名 payload 中**同时绑定规范化 hostname 与 port**；host-only、`Path=/`、`HttpOnly`、`SameSite=Strict`、无 `Secure`；默认 30 天                          | 同上                                                              |
| token 是**每个 dsh 进程**的，不是一次性的，可重复交换                                                                                                                              | `src-tauri/src/auth.rs`                                           |
| 实时通道是 WebSocket：Gateway 拥有 `/api/remote.mux`，webserver 提供 `registerUpgrade` 精确 upgrade 路由                                                                           | `dsh-api-gateway` / `dsh-host-webserver` README                   |
| 多客户端是设计内的：每 Client 队列 + `clientId`，桌面 webview 与手机可同时在线                                                                                                     | `dsh-api-gateway` README                                          |
| 随附 Web bundle 默认开启 gzip 压缩（level 1，阈值 1024 字节）                                                                                                                      | `dsh-host-webserver` README                                       |
| dsh 提供官方 index 注入机制：`webserver/index-inject` 事件、`tapIndex(transform)`、`collectIndexInjections()`                                                                      | 同上                                                              |
| `tokio` / `hyper 1.11` / `http` / `tower` 已在依赖树中（经 reqwest 引入）                                                                                                          | `src-tauri/Cargo.lock`                                            |

核验时使用的 dsh 版本：`0.1.5-rc.1`。

---

## 一、背景与核心设计原则

我们正在开发基于 **Tauri v2 + Rust** 的开源 DeepSeek Harness 桌面端（`MochiNek0/dsh-desktop`）。
为了让用户在离开电脑时能随时通过手机监控 Agent 思考进度、审批敏感操作权限或追加指令，需要实现「手机端扫码连接」功能。

### 核心铁律（必须严格遵守）：

1. **零源码侵入**：严禁修改任何 `@deepseek-ai/dsh` 官方内核及 WebUI 源码。需要改动 dsh 行为时，只走官方扩展点——cordis 插件（本仓库已有 `plugin/`，即 `dsh-desktop-signal`）与 CLI 参数。
2. **不裸开放 dsh 本体**：`dsh web` 虽然有 `--host 0.0.0.0`，但官方声明该姿态不受支持——服务器自身不带 TLS、认证与来源策略，绑定非回环地址等于把未受保护的路由与静态资源直接暴露给整个网段。因此 dsh 依旧只监听 `127.0.0.1:<port>`，对外由我们自己的网关承担鉴权。
3. **零独立 App（拥抱 Web）**：手机端直接使用系统原生浏览器（iOS Safari / Android Chrome）打开，不引入原生移动 App 维护负担。PWA 全屏能力见第三章第 4 节——它有 secure context 前提，Phase 1 只做有限承诺。
4. **安全零信任（Zero Trust）**：网关必须**自行实现**一整套请求信任检查（见第三章第 2 节），再配合「扫码 + 桌面原生二次确认」握手。**网关改写 Host/Origin 的那一刻，dsh 自己的栅栏对经过网关的流量就全部失效了，这部分防护必须在网关侧原样重建。**
5. **通道抽象与远期兼容**：核心鉴权、Session 管理与代理逻辑必须与底层网络解耦，当前首发局域网直连，后续无缝演进支持 Tailscale、Cloudflare Tunnel 和官方云端中继。
6. **极致轻量**：纯 Rust 异步技术栈实现，严控打包体积开销。`tokio` / `hyper` / `http` / `tower` 已在依赖树中，边际成本主要是 hyper 的 server feature 与本模块自身代码。目标增量 < 500KB，**以实际构建产物 diff 为准，不得倒过来当作设计前提**。

---

## 二、业务交互流程

### 1. 发起连接（电脑端）

- 在菜单按钮右侧添加一个「手机连接 / 远程访问」的按钮，用户点击按钮。
- 首次启用时，若 Windows Defender 防火墙尚未放行本应用的入站连接，先引导用户完成授权（见第三章第 5 节），再展示二维码。
- 桌面弹出原生模态窗，显示动态生成的配对二维码与配对链接。
  - 二维码 URL 形如：`http://<电脑局域网IP>:<动态代理端口>/?pair_token=<一次性随机Nonce>`
- 弹窗呈现「等待移动端扫码连接...」状态。

### 2. 扫码握手与桌面授权（手机 & 电脑双向闭环）

- 手机系统相机扫码，在移动端浏览器打开配对 URL。
- Rust 网关拦截到携带 `pair_token` 的初始握手请求，挂起该连接，并在电脑桌面主动弹出原生授权弹窗：
  > **「检测到移动设备请求连接」**
  > 客户端信息：iPhone (IP: 192.168.x.x)
  > [ 允许连接 ] [ 拒绝 ]
- **电脑点击「允许」**：
  - 网关向手机端响应设置加密签名的持久 HttpOnly Cookie（`dsh_mobile_session`）。
  - 手机端自动进入 DSH 界面。
  - 桌面端弹窗变为「已连接设备列表（当前 1 台在线）」，并提供随时「断开连接」的入口。
- **电脑点击「拒绝」**：手机端直接被重定向至 403 拒绝页面。

### 3. 操作体验（手机端）

- 手机端实时同步官方 DSH 思考流、工具调用折叠卡片、终端日志，可直接作答权限审批（Allow/Deny）。
- 视口与移动端排版通过 dsh 官方 index 注入机制下发（见第三章第 4 节）。

---

## 三、技术架构与详细实现规范 (Rust / Tauri v2)

```text
[ 手机端浏览器 (Safari/Chrome) ]
                │
                ▼
┌────────────────────────────────────────────────────────┐
│  网络通道抽象层 (RemoteTunnel Trait)                    │
│  ├─ 当前: 本地局域网 (LAN Bind 0.0.0.0)                 │
│  ├─ 预留: Tailscale CGNAT / MagicDNS                    │
│  ├─ 预留: Cloudflare Tunnel (cloudflared 子进程)        │
│  └─ 预留: 官方托管 WebSocket 逆向云中继 (Hosted Relay)   │
└────────────────────────────────────────────────────────┘
                │ (HTTP / WebSocket 流量引入)
                ▼
┌────────────────────────────────────────────────────────┐
│  Tauri (Rust) 统一安全鉴权网关                          │
│  ├─ 1. 请求信任栅栏 (Host/Origin/sec-fetch-site 校验)   │
│  ├─ 2. 校验一次性 Pair Token 并触发桌面原生对话框授权   │
│  ├─ 3. 设备 SessionStore (签名 Cookie 签发与作废)       │
│  ├─ 4. 服务端持有 dsh 会话 cookie, 改写 Host/Origin     │
│  └─ 5. 双向透传 WebSocket (Agent 状态流与权限交互)      │
└────────────────────────────────────────────────────────┘
                │ (内网纯回环安全调用)
                ▼
┌────────────────────────────────────────────────────────┐
│  官方未修改的 dsh web 运行时 (127.0.0.1:<dsh_port>)     │
│  + dsh-desktop-signal 插件 (index 注入 / 移动端适配)    │
└────────────────────────────────────────────────────────┘
```

### 1. 局域网反向代理与请求转发

- **网络绑定**：在 `src-tauri` 中新增网关模块，绑定 `0.0.0.0:<动态空闲端口>`。
- **网卡探测**：提供健壮的本地 IPv4 检索函数（过滤回环接口 `127.0.0.1`、Docker/WSL/VMware 等虚拟网卡，优先获取主 Wi-Fi/以太网 IP）。当前依赖树中没有现成能力，需引入 `if-addrs` 或直接调用 `GetAdaptersAddresses` / `getifaddrs`。
- **上游 Host 处理（两条路，择一并在实现中注明理由）**：
  - **方案 A（默认）**：转发时强制把 `Host` 与 `Origin` 改写为 `127.0.0.1:<dsh_port>`，dsh 视其为本机调用。优点是不依赖 dsh CLI 参数，代理端口变化无需重启 dsh。**前提是第 2 节的栅栏必须已在网关侧生效。**
  - **方案 B（备选）**：保持手机侧 Host 原样透传，改为启动 dsh 时传 `--trusted-host <局域网IP>:<网关端口>`。这是官方为反向代理预留的口子，dsh 自己的栅栏继续生效。代价是网关端口必须在 dsh 启动前确定，端口漂移需要重启 dsh。
- **WebSocket 透明双向透传 (WS Upgrade)**：
  - DSH 的实时通道是 Gateway 自有的 `/api/remote.mux` WebSocket，思考过程与审批信号都走它。网关必须支持对 WS 升级请求的劫持。
  - **实现约束**：完成 101 握手转发后直接做**双向裸字节拷贝**，不解析帧、不引入 `tokio-tungstenite`。心跳 Ping/Pong（`websocketHeartbeatIntervalMs`）因此自然穿透。

### 2. 请求信任栅栏（网关侧必须重建）

dsh 的 `api-request-trust` 是防御 DNS rebinding 与跨站浏览器请求的唯一一道墙。方案 A 改写 Host/Origin 之后，它对经过网关的流量恒为通过。**网关必须在改写之前，对手机侧的原始请求施加同等检查**：

- 请求的 `Host` 必须属于本机网卡地址集合 + 当前网关端口（或当前隧道 authority），否则 403；
- 若请求带 `Origin`，必须与该 `Host` 相等，否则 403；
- `sec-fetch-site: cross-site` 一律 403；
- Host 可信但未认证的请求返回 401（与 dsh 的语义保持一致）。

**风险等级说明**：网关背后的 dsh 会话等价于用户机器上的完整 shell 执行权。这一节不是可选项。

### 3. 鉴权体系与会话管理

#### 3.1 网关与 dsh 之间（服务端会话）

- 网关启动后，用 `src-tauri/src/server.rs` 已解析出的 launch token，自行发起一次 `GET http://127.0.0.1:<dsh_port>/?token=<token>`，从 303 响应里**捞出 Set-Cookie 并保存在服务端内存**。
- 此后每个转发请求由网关注入该 cookie。
- **该 cookie 绝不能透传给手机**：它在签名 payload 中绑定了 `127.0.0.1:<dsh_port>`，手机侧 authority 对不上必然 401。网关必须从上游响应中**剥除 `Set-Cookie`**，避免污染手机的 cookie jar。
- **dsh 重启必须重新交换**：token 是每个 dsh 进程的，而 `server.rs` 存在带原端口重连的路径。网关需订阅 dsh 生命周期事件，在 `Event::Ready` 时丢弃旧 cookie 并重做交换。**这是联调期最容易漏、且只有在 dsh 崩溃重启后才暴露的路径。**

#### 3.2 手机与网关之间（设备会话）

- 内存中维护轻量级 `SessionStore`，包含：
  - `pair_token`（一次性、5 分钟有效、阅后即焚）。
  - `authorized_sessions`（记录签名 Cookie、设备标识、授权时间）。
- 网关自签的 `dsh_mobile_session` 必须用 HMAC 签名并做常数时间比较；`HttpOnly`、`Path=/`、`SameSite=Strict`。
- 手机端所有后续 HTTP/WS 请求必须携带该 Cookie，未携带或已失效请求立即阻断（401）。
- 桌面端支持一键「踢出所有设备 / 刷新密钥」，瞬间销毁所有授权状态。
- **鉴权不绑定单一 IP 地址**（基于签名 Token 与设备标识）。这是为 Phase 2/3 的跨网络场景预留的架构约束；Phase 1 无法也不需要验证「Wi-Fi 切 5G 不断线」——彼时局域网 IP 本就不可达。

### 4. 移动端适配与 index 注入

**不要在 Rust 侧做 HTML 流字符串替换**：随附 Web bundle 默认开启 gzip（level 1，阈值 1024 字节），index.html 一定是压缩下发的，流式文本注入拿不到可匹配的字节。

改走 dsh 官方扩展点，由本仓库已有的 `plugin/`（`dsh-desktop-signal`）承担——它当前 host 半边是空的 `apply() {}`，正好是落点：

- 订阅 `webserver/index-inject` 事件（或使用 `tapIndex(transform)`）注入
  `<meta name="viewport" content="width=device-width, initial-scale=1.0, maximum-scale=1.0, user-scalable=no">`。
- 该路径官方支持、不改写任何磁盘文件、不与压缩冲突，且完全符合铁律 #1。

**PWA 范围调整**：完整 PWA 安装（Service Worker、Android Chrome 的安装能力）要求 secure context。Phase 1 的局域网明文 HTTP 不满足该条件，Android Chrome 的「添加到主屏幕」只会生成普通快捷方式，不会 standalone 全屏（iOS Safari 相对宽松）。

因此：

- **Phase 1 不承诺 PWA 全屏体验**，不提供 manifest 与引导提示条，避免给出兑现不了的预期。
- **PWA（manifest、`display: standalone`、图标、引导提示）整体挪到 Phase 2**，与 HTTPS 通道（Cloudflare Tunnel / Tailscale HTTPS）一并交付。届时同样经插件的 index 注入下发 manifest 链接，由网关提供 `/dsh-mobile-manifest.json`。

### 5. 平台与部署约束

- **Windows 防火墙**：绑定 `0.0.0.0` 时，Windows Defender 首次会弹出入站授权对话框并需要提权。未放行时表现为「二维码扫得开但连不上」，且没有任何提示。首启流程必须检测并引导，失败时在桌面弹窗给出明确文案。
- **手机后台挂起**：iOS Safari 会冻结后台标签页，导致心跳超时断连。dsh 客户端自带 500ms → 10s 的抖动退避重连，预期可自愈，但**必须实测**息屏 / 切后台 / 回前台的恢复表现，不能只依据文档判断。

---

## 四、远期演进设计：多通道与广域网扩展（架构预留规范）

**必须在代码层面做好通道解耦，严禁将「局域网 IP 直连」硬编码在业务逻辑中！**

### 1. 通道接口抽象（Transport / Tunnel Trait）

在 Rust 侧抽象统一的 `RemoteTunnel` 特征/接口：

```rust
pub trait RemoteTunnel: Send + Sync {
    /// 启动隧道并返回外部可访问的基地址 (如 http://192.168.1.100:59000 或 https://xyz.cfargotunnel.com)
    fn start(&mut self, local_gateway_port: u16) -> Result<String, TunnelError>;
    /// 停止隧道
    fn stop(&mut self) -> Result<(), TunnelError>;
    /// 当前通道类型描述
    fn tunnel_type(&self) -> TunnelType;
}
```

注意：第三章第 2 节的信任栅栏需要知道「当前合法的 authority 集合」，该信息由通道实现提供，不能在栅栏里硬编码网卡 IP。

### 2. 演进路线规划

- **当前版本（Phase 1）：LAN 局域网通道**
  - 实现基于 `0.0.0.0` 的标准局域网直连（明文 HTTP，不含 PWA）。
- **用户自配置通道（Phase 2 - 预留接入点）**：
  - **Tailscale 模式**：识别并绑定 Tailscale 虚拟网卡 IP（`100.x.y.z`），支持异地广域网安全直连。
  - **Cloudflare Tunnel 模式**：预留配置项接收用户的 `tunnel_token`，桌面端可拉起轻量级 `cloudflared` 进程将本地代理网关暴露到私有域名，**上层鉴权（扫码 + 桌面弹窗确认）依然强制生效**，避免域名裸露公网。
  - **PWA 能力随本阶段的 HTTPS 一并交付**（manifest、standalone、图标、添加到主屏幕引导）。此时网关 cookie 应补上 `Secure`。
- **官方托管云中继（Phase 3 - 商业化/生态预留）**：
  - 预留基于 WebSocket 反向隧道的实现（桌面客户端主动向上游云端中继保活，手机扫码访问中继二级域名完成穿透）。

### 3. 解耦细节约束

- **配对二维码解耦**：二维码由 `get_pairing_url(tunnel_type)` 独立生成，随时支持协议头从 `http://` 切换为 `https://`。二维码图像**在前端 JS 侧编码生成**，不为此引入 Rust crate。
- **Session 与单网段解耦**：鉴权识别基于加密 Token 与设备标识，不绑定单一 IP 地址，为 Phase 2/3 的跨网络会话延续做准备。

---

## 五、本次具体交付清单（Phase 1）

1. **Rust 后端核心代码**：
   - Gateway 模块：反向代理、请求信任栅栏、WebSocket 101 后裸字节双向透传、本地网卡探测。
   - dsh 会话持有：launch token 根交换、服务端 cookie 注入、上游 `Set-Cookie` 剥除、**dsh 重启后自动重新交换**。
   - `SessionStore`：HMAC 签名 cookie、随机 Nonce 配对逻辑、常数时间校验。
   - `RemoteTunnel` 基础 Trait 及当前的 `LanTunnel` 实现（含 authority 集合上报）。
2. **Tauri Command 接口**：
   - 获取当前配对二维码数据/URL。
   - 桌面端确认/拒绝接入指令。
   - 已连接设备查询与踢除指令。
3. **插件侧（`plugin/` host 半边）**：订阅 `webserver/index-inject`，注入移动端 viewport。
4. **首启引导**：Windows 防火墙放行检测与失败文案。
5. **生命周期守卫**：桌面应用关闭时，自动停止代理服务器并清理内存会话。
6. **体积验证**：给出启用网关前后 release 产物的实际大小 diff。

---

## 六、已知风险与非目标

- **明文传输**：Phase 1 走局域网明文 HTTP，网关 cookie 不带 `Secure`，同网段的被动嗅探可获取 bearer cookie。这是 Phase 1 的已接受风险，Phase 2 的 HTTPS 通道是缓解手段。
- **权限面**：一旦手机会话建立，其权限等同于桌面端——包括 Agent 的 shell 执行能力。桌面二次确认是唯一的人工闸门，不可省略。
- **非目标**：Phase 1 不做原生移动 App、不做 PWA 离线能力、不做多用户/多账号隔离、不做公网穿透。
