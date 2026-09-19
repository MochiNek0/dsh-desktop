# DSH Desktop 移动端远程连接演进计划：Phase 2 & Phase 3

> 本文档是 `docs/mobile-connection-spec.md` 的续篇，沿用其第一章的六条核心铁律。凡与该文档冲突之处，以该文档为准。

## 〇、修订说明与被推翻的假设

第一版计划有若干处在真机上不成立。这些不是实现细节，是照着做会撞墙的东西，所以先列出来，避免下一次又基于同样的假设设计：

| 第一版的说法 | 实际情况 | 影响章节 |
| :--- | :--- | :--- |
| 公网限流「对来自同一 IP 的错误请求频控」 | 经 cloudflared 转发后，网关 accept 到的 peer address **恒为 `127.0.0.1`**。按它限流等于把全世界当一个 IP，合法手机会被攻击者的失败握手连坐拉黑 | 二.1 |
| 授权弹窗显示「客户端信息：iPhone (IP: 192.168.x.x)」 | 同上，隧道下这里只会显示回环地址，二次确认失去判别依据 | 二.1 |
| 「网关在通过 HTTPS 通道接收到握手时」追加 `Secure` | 网关收到的是 cloudflared 发来的**明文 HTTP**。唯一可靠判据是当前活跃通道的 `scheme()`，绝不能读 `X-Forwarded-Proto`（客户端可控） | 二.2 |
| 通道热切换「无需重启，即时更新配对 URL」 | 会话 cookie 在签名 payload 里绑定了 hostname + port。切换通道 = **所有已配对设备静默 401**。Quick Tunnel 每次重启换域名，等于每次重启都要重新扫码 | 二.3 |
| `RemoteTunnel::start()` 同步返回基地址 | 同章节又要求用 `tokio::process::Command` 拉起 cloudflared 并解析 stderr 等几秒。同步签名要么卡死 UI，要么在 tokio runtime 里 `block_on` 直接 panic | 一.1 |
| Quick Tunnel 面向「普通用户、外网 5G」 | `*.trycloudflare.com` 在国内连通性不可靠；Cloudflare 官方声明 Quick Tunnel 不适合生产（无 SLA、无固定域名） | 四.1 |
| Phase 2「PWA 沉浸全屏：完整支持」 | PWA 安装按 origin 绑定。Quick Tunnel 每次重启换域名 → 已添加到主屏幕的图标全部失效。PWA 只在**稳定域名**下成立 | 六 |
| Phase 3 内网优先直连，「1.5 秒超时后无缝回退」 | `https://` 页面发起的 `http://192.168.x.x` 请求被浏览器当作 active mixed content **直接阻断**，连超时都等不到。这一章按原样无法实现 | 七.2 |
| Phase 3「前端 Service Worker 维持状态」保活 | Service Worker 不能维持长连接，空闲约 30 秒即被终止，锁屏更快。它只能在回到前台时被唤醒后重放握手 | 七.2 |
| Phase 3「中继零知识」「硬件级 E2EE」 | 做加密的 JS 由中继自己下发，中继想看明文只需给特定 session 发一份改过的脚本。这是架构上的死结，不是实现瑕疵。且 Web Crypto 是纯软件，「硬件级」无依据 | 七.1 |

### Phase 1 的实际遗留

1. **通道单例绑定**：`Shared.tunnel: Mutex<LanTunnel>` 硬编码了 LAN。
2. **LAN 模式明文 HTTP**：同网段被动嗅探风险。**Phase 2 不解决这一条**——LAN 通道在可预见的将来都是明文 HTTP，`Secure` 标记只对 HTTPS 通道有意义。第一版把它写成「Phase 2 引入 HTTPS 隧道后必须完成安全加固」是误导。诚实的表述是：LAN 模式接受此风险，需要传输加密的用户应改用 Tailscale 或 Cloudflare 通道。
3. **PWA 缺 Secure Context**：同上，随 HTTPS 通道一并解决，仅限稳定域名。
4. **`authorities()` 在热路径上做系统调用**：`trust.rs` 每个请求都调它，而 `LanTunnel::authorities()` 每次都跑一遍 `if_addrs::get_if_addrs()`。重构时顺手加短 TTL 缓存，但要保留「笔记本换网后不重启也能更新」的语义。

---

# 一、Phase 2 的通道抽象

> **目标**：突破局域网边界。Tailscale 面向已有组网经验的用户，Cloudflare Named Tunnel 面向有自有域名的用户。两者都不需要公网 IP 和端口映射。

```text
┌──────────────────────────────────────────────────────────────┐
│  任一时刻只有一个活跃通道                                     │
│  ┌────────────┬──────────────────┬────────────────────────┐  │
│  │ LanTunnel  │ TailscaleTunnel  │   CloudflareTunnel     │  │
│  │ 0.0.0.0    │ 100.64.0.0/10    │ cloudflared 子进程      │  │
│  └────────────┴──────────────────┴────────────────────────┘  │
│        └── base_url / authorities / scheme / client_ip ──┐   │
└──────────────────────────────────────────────────────────┼───┘
                                                           ▼
        ┌──────────────────────────────────────────────────────┐
        │ 统一安全网关：信任栅栏 + 一次性 pair token + 桌面确认 │
        └──────────────────────────────────────────────────────┘
```

## 1. `RemoteTunnel` trait

第一版给的 `TunnelManager` 是为不存在的需求造的抽象：Phase 2 任何时刻只有一个活跃通道，不需要注册表、不需要并发多通道。`Shared.tunnel` 从 `Mutex<LanTunnel>` 改成 `Mutex<Box<dyn RemoteTunnel>>` 加一个 `switch()`，就是全部所需。

同理，第一版的 `start() -> String` + `health() -> Running { base_url }` + `scheme()` 是三个重叠的真相来源。收敛成一个：

```rust
pub enum Scheme { Http, Https }

pub enum TunnelState {
    Stopped,
    /// 子进程已拉起，地址还没下来。卡片据此显示「启动中」。
    Starting,
    Running { base_url: String },
    Failed(TunnelError),
}

#[async_trait]
pub trait RemoteTunnel: Send + Sync {
    /// 拉起通道。立即返回——cloudflared 从启动到吐出域名要几秒，
    /// 卡片靠 `state()` 轮询，而不是靠这里阻塞。
    async fn start(&mut self, local_gateway_port: u16) -> Result<(), TunnelError>;
    async fn stop(&mut self) -> Result<(), TunnelError>;

    fn tunnel_type(&self) -> TunnelType;
    fn state(&self) -> TunnelState;

    /// 经此通道到达的请求，其 `Host` 的合法取值。
    /// LAN 是「每个本机 IPv4 + 端口」，隧道是「一个主机名、无端口」。
    fn authorities(&self) -> Vec<String>;

    /// 这条通道对手机呈现的传输层协议。
    /// **这是 Cookie `Secure` 标记的唯一判据**，见第二章第 2 节。
    fn scheme(&self) -> Scheme;

    /// 从请求头里取出手机的真实地址。
    /// LAN 通道返回 `None`（用 socket peer address 即可）；
    /// 经反代的通道必须在这里取，见第二章第 1 节。
    fn client_ip(&self, headers: &HeaderMap) -> Option<IpAddr>;
}
```

`base_url` 是唯一权威，`authorities` 与配对 URL 都从它派生。

## 2. 配置持久化

`src-tauri/src/settings.rs`（`desktop.json`）里存 `remote_tunnel_mode` 与各通道的非敏感参数，沿用容错读取铁律（损坏或缺失一律回退到默认的 LAN 模式）。

**Cloudflare Named Tunnel Token 不进 `desktop.json`。** 它是能操作用户 Cloudflare 账号下该隧道的 bearer 凭证，走 OS 凭据库（`keyring` crate，Windows Credential Manager / macOS Keychain / Linux Secret Service）。第一版只写了「安全存储」而没说怎么存——这种含糊到实现时一定会变成明文 JSON，所以这里写死。

---

# 二、三个横切问题（先于任何通道实现）

这一章是第一版最大的空白。下面三条不属于任何单个通道，但任何一条做错，对应的通道就是不安全或不可用的。**建议在写 `TailscaleTunnel` / `CloudflareTunnel` 之前先把这三条落地并加测试。**

## 1. 手机的真实 IP

经 cloudflared（以及 `tailscale serve`，它同样是个反向代理）转发后，网关的 socket peer address 恒为 `127.0.0.1`。两处依赖它：

**限流器**。按回环地址限流，攻击者的失败握手会把合法手机一起拉黑——防御变成了 DoS 自己的开关。必须改用 `trust.rs` 通过 `tunnel.client_ip(headers)` 拿到的地址：Cloudflare 读 `CF-Connecting-IP`，Tailscale Serve 读 `X-Forwarded-For` 的最后一跳。

**桌面授权弹窗**。规格里写的「客户端信息：iPhone (IP: 192.168.x.x)」在隧道下会显示 `127.0.0.1`，用户失去判断「这是不是我」的依据，人工确认这道防线被抽空。同样走 `client_ip()`。

> **为什么此处信任这个 header 是安全的**：不是因为 header 可信，而是因为连接只可能来自本机的 cloudflared 子进程，而该进程是我们自己拉起的。一旦哪天允许任何外部来源直连网关，这个推理立刻失效。`client_ip()` 挂在 trait 上而不是做成全局工具函数，就是为了让这个前提跟着通道走。

## 2. `Secure` 标记的唯一来源

网关看到的永远是 cloudflared 发来的明文 HTTP 请求，所以**不存在**「网关检测到 HTTPS 握手」这回事。判据只有一个：`tunnel.scheme()`。

明确禁止读 `X-Forwarded-Proto`——那是客户端可控的，等于把「要不要加 Secure」交给攻击者决定。

## 3. 切换通道会让所有已配对设备失效

这是产品行为问题，不是 bug，但第一版完全没提。会话 cookie 在确定性名称和签名 payload 里同时绑定了规范化 hostname 与 port，因此：

- LAN → Cloudflare：authority 从 `192.168.1.100:59000` 变成 `xxx.example.com`，所有手机的 cookie 失效，表现为莫名其妙的 401 而不是「请重新扫码」。
- HTTPS → HTTP 回退：带 `Secure` 签发的 cookie 浏览器压根不会在 HTTP 上回发，同样静默失败。
- Quick Tunnel 每次重启换域名：等于每次重启 DSH 都要重新扫码 + 重新点「允许」。

**要定的产品行为**（建议取第一条）：

1. 切换通道前，卡片明确提示「当前 N 台已连接设备需要重新扫码配对」，用户确认后再切。切换完成后卡片直接回到「等待扫码」状态。
2. 或者让 session 绑定到设备指纹而非 authority。更好用，但等于放宽了 cookie 的绑定语义，要重新审一遍 DNS rebinding 的防御是否还成立——不建议在 Phase 2 做。

无论取哪条，**401 必须能被手机端识别成「需要重新配对」并自动跳到配对提示页**，而不是一个裸的错误码。

## 4. 隧道开启期间，栅栏的强度是下降的

现在 `addresses()` 故意排除回环，正是为了保证「`Host: 127.0.0.1` 的请求一定不是手机发的」。隧道一开，authorities 里多了一个公网主机名，而**本机任何进程都可以构造一个 `Host: xxx.example.com` 的请求打到网关**——socket 层区分不出它和 cloudflared。

剩下的防线只有 cookie 和一次性 pair token。这不是 showstopper（纵深防御本来就是这么用的），但第一版「核心铁律坚持」那段的口气像是栅栏在公网下强度不变。写清楚，别让后来的人误以为栅栏还兜着底。

---

# 三、Tailscale 通道

## 1. 适用场景

电脑与手机都已加入同一个 Tailnet。面向开发者和已有组网经验的用户。

## 2. Phase 2.1 只做一件事：认出 `100.64.0.0/10`

第一版把 MagicDNS 探测和 Serve HTTPS 一起塞进了里程碑 1。实际成本比看起来高：

- Windows 上 `tailscale.exe` 默认装在 `C:\Program Files\Tailscale\`，**不在 PATH 里**，`tailscale status --json` 不能直接调。
- Windows 的 LocalAPI 是个带认证的本地 HTTP 服务，token 要从注册表取，不是 Unix socket 那种拿来即用。
- `tailscale cert` / Serve 的 HTTPS 需要 tailnet 管理员在控制台开启 HTTPS Certificates，装了客户端不等于有。

而 2.1 的实际需求只是「给手机一个能连的地址」，`100.x.y.z` 裸 IP 就够了，且判定逻辑是纯本地的、零外部依赖：

1. 遍历网卡，匹配 `100.64.0.0/10`（`100.64.0.0` ~ `100.127.255.255`）；
2. 交叉验证接口名（Windows 友好名含 `Tailscale`，macOS `utun*`，Linux `tailscale0`），避免误认运营商 CGNAT 地址；
3. `base_url` = `http://100.x.y.z:<gateway_port>`，`scheme()` = `Http`。

**MagicDNS 与 Serve HTTPS 推到 2.2，或直接砍掉。** 它们带来的是更好看的域名和 PWA 资格，不是可达性。

## 3. Serve 与 Funnel 不是一回事

第一版写「若 Tailscale 启用了 HTTPS 证书（Serve/Funnel）」，把两者并列在一个括号里。实际上 **Serve 是 tailnet 内 HTTPS，Funnel 是把服务暴露到公网**，安全含义天差地别。如果将来支持：

- Serve 可以做，但它本身是反向代理，会引入和第二章第 1 节完全相同的源 IP 问题，必须一并处理。
- **Funnel 默认不做。** 它和 Cloudflare Quick Tunnel 一样是公网暴露，若要支持必须走和第五章同样的关停与可见性要求。

## 4. 降级引导

未检测到活跃 Tailscale 网卡时，卡片显示：「未检测到 Tailscale 虚拟网卡。请确认 Tailscale 客户端已登录且处于连接状态。」——**并且不要生成二维码**。发一个连不上的二维码比不发更糟。

---

# 四、Cloudflare Tunnel 通道

## 1. 两种模式，以及为什么默认反过来

| 模式 | 第一版定位 | 修订定位 |
| :--- | :--- | :--- |
| Quick Tunnel（`--url`，零配置） | 「普通用户、外网 5G」 | **快速试用 / 海外用户**。`*.trycloudflare.com` 在国内连通性不可靠，且 Cloudflare 官方声明其不适合生产（无 SLA、无固定域名、随时可能限流）。域名每次重启都变，PWA 与已配对设备全部失效（见二.3） |
| Named Tunnel（`--token`，自有域名） | 「稳定生产使用」 | **推荐路径**。域名稳定，PWA 成立，已配对设备跨重启存活 |

顺带修正对比矩阵里的一句：「服务器运营成本 0（复用第三方成熟免费基础设施）」——把一个明确声明「不适合生产」的免费服务当作产品的承载层，成本不是 0，是转嫁给了用户的可用性。

**结论**：国内用户的广域网方案主线是 Tailscale 和自建（见第七章），Cloudflare 是给有自有域名的用户的选项。计划不应假装 Cloudflare 能覆盖「普通用户」。

## 2. `cloudflared` 二进制的获取

第一版一句「缺失时在 UI 引导一键下载或展示安装指引」带过。实际要处理的：

- **不能打包。** 约 50MB，与铁律 #6（极致轻量，目标增量 < 500KB）直接冲突。
- **下载必须校验。** 这是「从网上下一个 exe 然后执行它」，没有校验和或签名验证就是一个供应链缺口。对照 `plugins.rs` 里关于 pnpm `allowBuilds` 的那段推理——那里拒绝替用户决定「一个下载来的包可以在安装时执行代码」，这里是同一个问题的更强形式。
- **macOS 的 Gatekeeper。** 下载的二进制带 quarantine 属性，直接 exec 会被拦；需要 `xattr -d com.apple.quarantine` 或让用户手动放行。
- **国内 GitHub Release 的可达性。** 下载本身可能就下不动。

**建议**：Phase 2.2 只做「探测 + 安装指引」（检测 PATH 与 app local data 目录，找不到就展示各平台的安装命令），**不做应用内自动下载**。自动下载单独评估，它的工作量和风险都比通道本身大。

## 3. 子进程生命周期

这部分第一版写得很好，保留：

- `tokio::process::Command` 拉起，实时读 stderr，正则捕获 `https://[a-z0-9-]+\.trycloudflare\.com`；
- 应用退出（`remote::shutdown`）、通道切换、用户主动停止时显式 kill 并 await 子进程退出；
- **Windows Job Object**（及各平台对应的进程组机制），保证桌面端崩溃时不残留孤儿 `cloudflared`。

补一条：**`cloudflared` 自身的日志不能原样写进应用日志**，它会打印隧道 token 和连接元数据。

## 4. 公网限流

见第二章第 1 节——键必须是 `CF-Connecting-IP`，不是 peer address。策略沿用第一版的（1 分钟内 10 次握手失败 → 拉黑 5 分钟），但加一条：**限流状态在通道停止时清空**，否则换域名重开后旧的黑名单毫无意义还可能误伤。

---

# 五、公网暴露的关停与可见性

第一版完全没有这一章，但它是 Phase 2 引入公网通道后风险变化最大的地方。

LAN 模式忘了关，风险限于同一个 Wi-Fi。公网隧道忘了关一整晚，是另一个量级的事——而 DSH 会话拥有本机的完整终端执行权。

三条，成本都很低：

1. **空闲自动停机。** 无已连接设备且超过 N 分钟（默认 30）无请求，自动停隧道并在卡片上说明原因。
2. **常驻可见性。** 隧道运行期间，托盘图标或主窗口上有一个明确的「本机当前可从公网访问」指示，点击直达关停。不是藏在弹窗里的状态文字。
3. **退出确认。** 应用退出时若隧道仍在运行，显式提示而不是静默关掉——让用户知道刚才那段时间机器是公开的。

这一章进里程碑 2.2，与 Cloudflare 通道同批交付。**不允许先发公网通道、后补关停开关。**

---

# 六、PWA（仅限稳定域名）

## 1. 前提

PWA 安装按 origin 绑定，所以只有 **Named Tunnel 的自有域名**或 **MagicDNS + `tailscale cert`** 下才成立。Quick Tunnel 下每次重启换域名，已添加到主屏幕的图标全部变死链——这种情况下**不注入 manifest**，免得用户装了一个第二天打不开的图标。

## 2. 网关挂载

`/dsh-mobile-manifest.json` 与图标由网关直接提供（这些路径在 dsh 的路由表之外，不会撞）：

```json
{
  "name": "DeepSeek Harness",
  "short_name": "DSH",
  "start_url": "/",
  "display": "standalone",
  "background_color": "#18181b",
  "theme_color": "#18181b",
  "icons": [
    { "src": "/mobile-icon-192.png", "sizes": "192x192", "type": "image/png" },
    { "src": "/mobile-icon-512.png", "sizes": "512x512", "type": "image/png" }
  ]
}
```

`start_url` 不带 `pair_token`（一次性的，且不该被持久化到 manifest）。

## 3. 插件侧注入（`plugin/lib/index.js`）

扩展现有的 `webserver/index-inject` 监听：

```html
<link rel="manifest" href="/dsh-mobile-manifest.json">
<meta name="mobile-web-app-capable" content="yes">
<meta name="apple-mobile-web-app-capable" content="yes">
<meta name="apple-mobile-web-app-status-bar-style" content="black-translucent">
<link rel="apple-touch-icon" href="/mobile-icon-192.png">
```

`apple-mobile-web-app-capable` 已被标记 deprecated，但老 iOS 仍只认它，所以两个都发。

## 4. iOS 的 cookie jar 陷阱

**iOS 上 standalone PWA 与 Safari 的 cookie 存储是隔离的。** 用户在 Safari 里配对成功、点「添加到主屏幕」、从图标打开，得到的是一个全新的未认证会话。

所以配对流程**必须可重入**：从主屏幕图标打开、命中未认证状态时，页面要能引导用户重新扫码（或提供一个「在桌面上确认」的入口），而不是回一个 401 错误页。这是第二章第 3 节那条「401 要能被识别成需要重新配对」的同一个需求，两边一起做。

## 5. 安装引导

检测到 Secure Context 且不在独立窗口中（`!window.matchMedia('(display-mode: standalone)').matches`）时，在页面底部渲染一次性引导浮层。iOS Safari 没有 `beforeinstallprompt`，只能是文字指引：「点击浏览器工具栏「分享」→「添加到主屏幕」」。关闭后记住选择，不要每次都弹。

---

# 七、桌面端卡片 UI（`src-tauri/src/remote/card.rs`）

1. **通道切换**：`[ 局域网 | Tailscale | Cloudflare ]`。切换前按第二章第 3 节弹出「N 台设备需重新配对」确认。
2. **按通道的状态展示**：
   - LAN：本机内网 IP + 防火墙放行状态；
   - Tailscale：节点 `100.x` 地址；未检测到网卡时显示引导文案且**不画二维码**；
   - Cloudflare：`Stopped / Starting / Running / Failed` 四态（这正是 `TunnelState` 存在的原因），Named Tunnel Token 输入框，以及第五章的关停入口。
3. **二维码热重载**：`base_url` 变化时重绘二维码与复制链接。

---

# 八、Phase 2 交付物

| 类别 | 文件 | 说明 |
| :--- | :--- | :--- |
| **通道抽象** | `src-tauri/src/remote/tunnel/mod.rs` | trait 扩充（async start、`state`、`scheme`、`client_ip`）、`Mutex<Box<dyn RemoteTunnel>>`、`authorities` 缓存 |
| | `src-tauri/src/remote/tunnel/lan.rs` | 现有 `LanTunnel` 平移 |
| | `src-tauri/src/remote/tunnel/tailscale.rs` | `100.64.0.0/10` 网段 + 接口名交叉验证 |
| | `src-tauri/src/remote/tunnel/cloudflare.rs` | 子进程生命周期、stderr 捕获、Job Object |
| **横切** | `src-tauri/src/remote/trust.rs` | 经 `client_ip()` 取真实地址；限流中间件 |
| | `src-tauri/src/remote/session.rs` | `Secure` 标记（源为 `scheme()`）；切换通道时作废全部会话 |
| | `src-tauri/src/remote/dialog.rs` | 授权弹窗显示真实客户端 IP |
| **公网守卫** | `src-tauri/src/remote/mod.rs` | 空闲自动停机、退出确认 |
| **UI** | `src-tauri/src/remote/card.rs` | 通道切换、四态展示、关停入口 |
| **配置** | `src-tauri/src/settings.rs` | `remote_tunnel_mode`；Token 走 `keyring` |
| **PWA** | `plugin/lib/index.js` | manifest / apple-touch / 安装引导（仅稳定域名） |
| | `src-tauri/src/remote/static/` | 192 与 512 图标 |

---

# 九、里程碑

每条都带可验证的判据。「实现 X」这种弱判据会导致返工。

## 里程碑 2.0：横切问题（先于任何新通道）

1. trait 扩充 + `Shared.tunnel` 解耦。
   - **verify**：`cargo test remote::` 全绿；LAN 模式行为与 Phase 1 逐字节一致。
2. `client_ip()` 与限流器。
   - **verify**：构造一个伪 `CF-Connecting-IP` 的单元测试——LAN 通道必须**忽略**该头（否则手机可以伪造自己的 IP 绕过限流）。
3. 通道切换的会话作废 + 手机端 401 引导页。
   - **verify**：手动切换通道后，已配对手机刷新看到的是配对引导页，不是 401。

## 里程碑 2.1：Tailscale + PWA 基础

1. `TailscaleTunnel`（仅 `100.x`）。
   - **verify**：手机关 Wi-Fi 走蜂窝网络、加入同一 tailnet，扫码后能进 dsh 并能回答权限审批。
   - **verify**：退出 Tailscale 客户端后，卡片显示降级文案且不生成二维码。
2. PWA 基础设施（manifest / 图标 / 注入 / `Secure` cookie）。
   - **verify**：Secure Context 下 `dsh_mobile_session` 带 `Secure`；非 Secure Context 下不带。
   - **verify**：iOS 上「添加到主屏幕」后从图标打开，落到配对引导页而不是 401（第六章第 4 节）。
3. 卡片支持 LAN / Tailscale 切换。

## 里程碑 2.2：Cloudflare + 公网守卫

1. `CloudflareTunnel`（Named 优先，Quick 标注为试用）。
   - **verify**：kill 桌面进程后 `cloudflared` 不残留（任务管理器 / `ps`）。
   - **verify**：Quick Tunnel 模式下**不注入 PWA manifest**。
2. 限流。
   - **verify**：从两个不同公网 IP 打失败握手，只有超限的那个被拉黑；隧道重启后黑名单清空。
3. 第五章的关停与可见性三条。
   - **verify**：无设备连接 30 分钟后隧道自动停止且卡片说明原因。
4. Token 走 OS 凭据库。
   - **verify**：`desktop.json` 里 grep 不到 token。

---

# 十、Phase 3：重新定位

第一版的 Phase 3（官方托管云中继 + 浏览器端 E2EE）有两章技术上不成立，一章法律与运营风险未评估。下面先说清楚哪里不成立，再给一个我认为应该做的版本。

## 1. 为什么「中继零知识」在纯浏览器方案下不成立

原设计：手机扫码打开 `https://relay.../s/<id>#k=<桌面公钥>`，**加载中继下发的 HTML 壳**，用 Web Crypto 生成密钥对、算 ECDH、AES-256-GCM 加密所有流量；URL fragment 不发给服务器，所以中继看不到桌面公钥。

fragment 那一步是对的。但整个推理的前提是「手机端跑的是可信代码」，而那段代码正是中继自己发的。中继想看明文，只需对特定 session 下发一份改过的 JS。**这是经典的 web crypto 信任引导问题，无解**——除非手机端装原生壳（与铁律 #3「零独立 App」冲突）或浏览器扩展。

还有几个具体问题：

- **X25519 在 SubtleCrypto 里普及得很晚**（Safari 17+ / Chrome 133+）。目标用户里有大量老 iPhone。要么准备 ECDH P-256 回退（支持面广得多，对这个场景安全性完全够），要么带 WASM 实现——后者又是中继下发的代码。
- **手机公钥可被替换**。中继读不到 fragment 里的桌面公钥，所以无法完整 MITM；但它能替换手机上送的公钥，让桌面与一个攻击者控制的「手机」建立会话。此时唯一防线是桌面人工确认，而那个弹窗在中继场景下（见二.1）可能没有任何判别信息。要补两条，都很便宜：**把一次性 `pair_token` 绑进 HKDF 的 salt/info**，以及**在桌面弹窗和手机页面上各显示一个从共享密钥派生的短认证串（SAS）供肉眼比对**。
- **把全部流量塞进 Service Worker 做 AES-GCM** 意味着 dsh 那几 MB 的 JS/CSS bundle 首屏要在 JS 层解密，且 SW 脚本本身也由中继下发。

**结论**：如果仍要做，必须把承诺降级为「防御被动监听与中继侧存储泄露」，**删掉「零知识 / Blind Relay」和「硬件级」这些说法**。技术方案里写下做不到的安全承诺，会让实现者和用户都产生错误的预期。

## 2. 为什么「混合智能故障转移」按原样无法实现

- **内网探测被混合内容策略封死。** `https://relay...` 页面 fetch `http://192.168.1.100:59000` 会被所有现代浏览器当作 active mixed content 直接阻断，fetch / XHR / WebSocket 全部拒绝——连「1.5 秒超时」都等不到，因为请求根本没发出去。Chrome 的 Private Network Access 只会让这更严格。
- **Service Worker 不能保活。** 它是事件驱动的，空闲约 30 秒被终止，锁屏更快。「切后台时 SW 维持状态」做不到。

可行的替代：

- 内网优先改成**顶层导航**（`location.href = 'http://192.168...'`）——不算混合内容，但会丢掉页面状态，且探测失败就是白屏，做不到「无缝」；或者**干脆让用户手动选**「我在家里 / 我在外面」。后者难看但诚实，建议取它。
- 断线恢复改成：resume 凭证存 IndexedDB，前台恢复时被 fetch 事件唤醒后重放握手；**桌面端为断连 session 保留一个有 TTL 的重连窗口**——这才是这个功能的实际工作量所在，第一版没写。

## 3. 匿名 + 无日志 + 公网 shell 中继的风险

第一版自己写了「DSH 会话拥有本地操作系统的完整终端执行权」，又写「社区版保持完全匿名，无需注册账号」「不保留任何用户交互日志」。

这个组合是滥用磁铁（挖矿、跳板、勒索），而且第一版内部就自相矛盾：「不保留任何用户交互日志与明文传输内容」和「仅保留网络连接元数据以满足网络安全合规要求」是两句打架的话。在国内运营还涉及备案与网络安全法下的日志留存义务。

**这不是技术问题，是「要不要做」的问题，应该在动工前定，而不是等中继原型跑起来再想。**

## 4. 建议的 Phase 3：可自托管中继

把「官方托管 + E2EE + SW 代理 + 自动故障转移」整体换成**一个单二进制的自托管中继**：

- Rust（Axum + Tokio + yamux），一条出站 WebSocket 长连接 + 流多路复用，**不做 E2EE，只靠 TLS**；
- 用户自己丢到一台 VPS 上，桌面端填个地址就能用；
- 桌面端侧就是第四个 `RemoteTunnel` 实现，与 Phase 2 的抽象天然契合。

好处：

| | 官方托管 + E2EE | 自托管中继 |
| :--- | :--- | :--- |
| 零知识承诺 | 做不到，且必须承认做不到 | 不需要——用户信的是自己的服务器 |
| 运营成本 / 滥用责任 / 合规 | 全部由我们承担 | 无 |
| 国内可达性 | 取决于我们选的机房 | 用户自己选 |
| 工程量 | 极大 | 约 1/5 |
| 将来要做官方托管 | — | 这份代码可原样复用 |

对这个体量的开源项目，这是投入产出比高得多的一步。官方托管版可以留作 Phase 4，等到（a）E2EE 的信任根问题有了可接受的答案，（b）滥用与合规的责任边界想清楚了，(c) 有人愿意付账单，再启动。

---

# 十一、修正后的对比矩阵

第一版的矩阵有几处需要改：「客户端前置依赖」一行把桌面端依赖和手机端依赖混在了一起（Phase 1 填的是手机端，Phase 2 填的是桌面端）；PWA 一行高估了 Quick Tunnel；E2EE 一行的「硬件级」没有依据。

| 维度 | Phase 1：LAN | Phase 2：Tailscale | Phase 2：Cloudflare | Phase 3：自托管中继 |
| :--- | :--- | :--- | :--- | :--- |
| **网络可达性** | 仅同 Wi-Fi | 广域网（跨网段、蜂窝） | 任意公网 | 任意公网 |
| **手机端依赖** | 系统浏览器 | 系统浏览器 + Tailscale 客户端 | 系统浏览器 | 系统浏览器 |
| **桌面端依赖** | 无 | Tailscale 客户端 | `cloudflared` 二进制 | 无（中继在 VPS 上） |
| **用户配置成本** | 放行防火墙 | 需组网经验 | Named 需自有域名 + CF 账号 | 需一台 VPS |
| **国内可达性** | 不适用 | 良好 | Quick 差、Named 取决于域名 | 取决于 VPS |
| **传输安全** | 明文 HTTP（同网段嗅探风险，不由 Phase 2 解决） | WireGuard 加密（Serve 下为 HTTPS） | HTTPS / TLS | HTTPS / TLS（中继可见明文） |
| **PWA 全屏** | 不支持 | 需 MagicDNS + cert | 仅 Named Tunnel | 支持 |
| **公网暴露** | 否 | 否（Funnel 除外） | **是**，需第五章全部守卫 | **是**，需第五章全部守卫 |
| **运营成本** | 0 | 0 | 0（但依赖第三方可用性） | 用户自付 |
| **定位** | 办公桌旁离座监控 | 开发者主线 | 有自有域名的用户 | 不愿依赖第三方的用户 |

---

# 十二、路线图

```text
[ Phase 1 完成 ]
        │
        ├─▶ 2.0  横切三件事（真实 IP / Secure 来源 / 切换失效）
        │
        ├─▶ 2.1  Tailscale（仅 100.x）+ PWA 基础
        │
        ├─▶ 2.2  Cloudflare + 公网关停与可见性
        │
        └─▶ 3    自托管中继（第四个 RemoteTunnel 实现）

  Phase 4（前置条件未满足，暂不排期）：官方托管中继
        需先回答：E2EE 信任根、滥用与合规责任、账单
```
