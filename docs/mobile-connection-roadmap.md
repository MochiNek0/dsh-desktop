# DSH 手机连接：接下来做什么

> 续 `docs/mobile-connection-spec.md`。核心铁律以那份为准，冲突时以那份为准。
> 这份回答两个问题：M1 做完了什么，以及接下来按什么顺序做、每一步做完怎么验。

## 做到哪了

Phase 1 能扫码连上，但有三处让人不想日常使用：

1. 扫码太频繁——每次重启桌面 app 都要重扫；
2. 进入方式笨——要开浏览器输地址，或者掏相机、微信扫一下；
3. 出了局域网就连不上。

**1 和 2 是同一个因**：配对活不过一次重启，于是扫码从一次性动作变成了日常动作，而"日常"才让扫码本身的不顺手暴露出来。修掉 1，2 的大半会跟着消失。3 是另一件事，排在后面。

**1 和 2 已经没了**——那就是 M1，四件事全部落地，外加交付之后补的两个洞（见 M1 末尾）。**3 也没了**：M2 把通道做成可切换的并加上 Tailscale，M3 加上 Cloudflare，于是有自有域名的人也有了一条出局域网的路，而且是唯一带 TLS 的一条——顺带把「电脑关机时主屏幕图标一片白」这个从 M1 就欠着的洞补上了。

## 路线

| | 内容 | 状态 |
| :--- | :--- | :--- |
| **M1** | 配对活过重启 + 重新配对入口 + 主屏幕图标 | **已完成**（`1b5d6fb`，及其后的两处修补） |
| **M2** | 通道抽象 + 三个横切问题 + Tailscale | **已完成**（`8bc45ef`），三处偏离见该节末尾 |
| **M3** | Cloudflare 隧道 + 公网守卫 + 离线壳 | **已完成**，偏离见该节末尾 |
| 之后 | 自托管中继（第四个 `RemoteTunnel` 实现） | 依赖 M2 的抽象，未排期 |

现在手机连接是这样一条路径：桌面 app 一启动网关就在（公网通道除外，见下），卡片上三选一，图标点下去直接进 dsh；凭证没了就在同一个页面里输六个字符，桌面弹框确认；电脑关机时主屏幕图标显示一页说明而不是白屏——前提是当时用的是 Cloudflare 通道，因为 Service Worker 要 secure context，而只有那一条有 TLS。

三条通道各自在什么时候用：

| 通道 | 什么时候 | 代价 |
| :--- | :--- | :--- |
| 局域网 | 手机和电脑同一个 Wi-Fi | 明文 HTTP；出了这个网就没了 |
| Tailscale | 手机在外面，两边登录同一个 tailnet | 要装客户端；仍然是明文 HTTP（WireGuard 加密，但浏览器不认） |
| Cloudflare | 有自己的域名，任何网络下都要能进 | 这台电脑对整个公网开着——所以有一整套守卫 |

---

# M1：让配对活过重启（已完成）

## 为什么排第一

三处代码让配对活不过重启：

| 位置 | 现状 | 后果 |
| :--- | :--- | :--- |
| `remote/mod.rs` `start()` | `bind` 到端口 `0` | 每次启动新端口，**origin 变了** |
| `remote/session.rs` `secret()` | 密钥 "minted at startup and never written anywhere" | 此前签的每个 mac 都验不过 |
| `session.rs` `Inner::devices` | 纯内存 | 重启即清空 |

端口这条最要命，**而且和 cookie 无关**：PWA 的 origin 是 `scheme://host:port`，端口变了就是另一个 origin。主屏幕图标点下去是浏览器自己的「连接被拒」页——不是 401，我们一行代码都跑不到，用户拿不到任何「该去重新扫码」的线索。

顺带澄清一个常见误解：**dsh 重启手机是无感的**，网关会自动重新 exchange launch token，有测试守着（`remote::tests::a_dsh_that_restarted_is_reauthenticated_without_the_phone_noticing`）。要重扫的是**桌面 app** 重启。

## 做四件事

### 1. 固定网关端口

首次 `bind(0)` 拿到端口后写进 `desktop.json`，之后优先复用，被占用才回落到 `0`。

- `mod.rs` 那句「Port `0`，所以 OS 来挑」讲的是 **dsh 不需要提前知道端口**（方案 A 对方案 B 的优势），与端口是否跨重启稳定无关。固定端口不影响方案 A。
- 防火墙不受影响：`firewall.rs` 的规则**按程序**匹配，不按端口。

**verify**：重启桌面 app 三次，卡片上端口不变；先用别的进程占住该端口再启动，网关仍能起来且卡片显示新端口。

### 2. 签名密钥与设备列表落盘

密钥进 `app_dir` 下单独一个 `gateway.key`（unix 上 0600，Windows 上继承用户目录 ACL），设备列表进 `desktop.json`（沿用 `settings.rs` 的容错读取——损坏或缺失一律当空）。

**没有用 `keyring`**，虽然这份文档第一版是这么写的。理由是它在主力平台上并不更强：Windows Credential Manager 对同用户的任何进程都开放，没有按程序的 ACL，和一个用户目录下的文件是同一个保证；而 Linux 要开 secret-service feature，为一个 32 字节的值拖进整棵 zbus，与铁律 #6 正面冲突。macOS Keychain 确实更强（有按程序 ACL，前提是 app 正确签名），但不足以支撑另外两个平台的代价。

两者都要分清：**能读到这个文件的进程 = 以该用户身份运行的进程**，而这样的进程本来就能读 dsh 的 launch token、直接驱动本机的 dsh 会话。密钥是它的一条更慢的路，不是它的第一把钥匙。

这才对得起 `SESSION_TTL`。那里写着 30 天，注释说 "a session that has to be re-paired every launch is one nobody would turn on"，**而当前实现恰恰就是每次启动都要重配**。

保留 `revoke_all` 的语义（它同时清列表并轮换密钥），并在设置里给一个「退出时清除所有配对」的开关。

**verify**：配对一台手机 → 退出 app → 重启 → 手机刷新，不需要重新扫码。
**verify**：点「断开全部」后重启，该手机需要重新扫码。
**verify**：`SESSION_TTL` 过期后仍要求重新配对。
**verify**：打开「退出时清除所有配对」后，第一条 verify 的结果反过来。

### 3. 6 位短码 + 重新配对页

落点是现成的 `proxy.rs::unauthenticated()`——那个页面现在只有一句「回到电脑上扫二维码」，给它加一个输入框。

桌面卡片在二维码旁边多显示一个 6 位短码，网关内部映射到同一个 16 字节 nonce，共享同一个 `PAIR_TTL` 与一次性语义；输进去之后桌面照常弹原生确认框。

- 字母表用去掉易混字符（`I` `L` `O` `U`）的 base32，6 位约 10.7 亿组合。
- 不让用户直接输 `pair_token`：16 字节 base64 是 22 个字符，没人会去输。
- 安全性够：5 分钟 TTL + 一次性 + 限流 + **桌面人工确认这道真正的闸门**。`pair_token` 本来的职责就只是「别让网络上随便谁都能按门铃」，不是身份凭证。

这个方案的价值全在「它什么都不需要」：不碰摄像头，所以不受 secure context 限制，**明文 HTTP 的 LAN 下能用**；而且是**同源导航**，iOS 上不会跳出独立窗口、不会多出第二个图标。

它覆盖的是「服务端活着、只是没凭证」这一类失败——密钥轮换、TTL 到期、点过「断开全部」、iOS 独立窗口的 cookie 隔离。**地址变了那一类它治不了**，见文末「躲不掉的坑」。

**verify**：点「断开全部」→ 手机不碰电脑、不用相机，只把六个字符输进它已经打开的页面，桌面弹框，允许后回到 dsh。
**verify**：iOS 上从主屏幕图标进来走完全程，始终在独立窗口内，没跳出 Safari，没多出第二个图标。
**verify**：短码过期不可兑换；兑换过一次不可再兑换；连续输错触发限流。

### 4. 主屏幕图标（前三件做完才做）

`/dsh-mobile-manifest.json` 与图标由网关直接提供（这些路径在 dsh 的路由表之外，不会撞）；`plugin/lib/index.js` 扩展现有的 `webserver/index-inject`，注入 `<link rel="manifest">`、`apple-touch-icon` 与 `apple-mobile-web-app-capable`（已 deprecated 但老 iOS 只认它，和 `mobile-web-app-capable` 一起发）。`start_url` 不带 `pair_token`。

**LAN 明文 HTTP 下这一步仍然成立，但只成立一半**：iOS 的独立窗口不要求 HTTPS，图标能用、能全屏；Service Worker 要求 secure context，所以没有离线壳。Android 上是普通快捷方式（不是 WebAPK），在浏览器里开，但同样免去了输地址。

**图标不能直接用 `src-tauri/icons/icon.png`**：它是透明底的，而 **iOS 会把 `apple-touch-icon` 的透明合成到黑色**。那只虎鲸是近黑色的，直接用等于在主屏幕上放一个黑方块。要在构建时压到白底再发（`scripts/make-mobile-icon.mjs`），生成物不进 git——和安装器位图同一条规矩。

**门槛是 origin 必须跨重启不变，host 和 port 都算**——所以第 1、2 件事没做完之前一律不注入。装一个第二天就打不开的图标，比没有图标糟得多。

卡片上同时显示可复制的地址文本与短码，让「手机上直接敲地址 + 输六个字符」成为一条不需要相机的完整路径。引导用**系统相机**扫码，不要用微信——微信扫出来是在它自带的 webview 里开。

**verify**：装好图标 → 重启桌面 app → 点图标能打开（这一条同时验了第 1 件事）。

## 交付之后补的两个洞

真用起来才冒出来的两个。都不是新功能，是上面某一条没做到底。

### 一、卡片上的码被扫掉之后不会换

`proxy::pair` 是**在弹确认框之前**就把 nonce 花掉的——故意的，否则被拒绝的人可以一直重试。而卡片上的二维码与六位码，是开卡片那一刻铸的 `Showing` 的一份副本，没人去动它。于是从手机扫到的那一瞬间起，屏幕上印着的就是一组废码，而卡片还在若无其事地画它；想给第二台手机用，只能把卡片关掉重开。

修法是让每条花掉 nonce 的路径都回到同一个地方：`remote::remint()` 铸新的并替换卡片上那份，`proxy::pair` 在 redeem 成功后立刻调 `remote::spent()`——在弹框之前，因为码已经废了，人接下来怎么答都不改变这一点。卡片的六位码下面同时加了一个「换一个码」按钮（verb `remote-refresh`），给「码过期了而人就站在电脑前」的那种情况。

两条边界：`remint()` 只在卡片开着时动作，`showing` 是「有没有卡片」的唯一判据，在这里填它等于让一台无人值守时配对成功的手机，把用户刚关掉的卡片重新弹出来；被换掉的旧 nonce 不吊销，它本来就是一次性、五分钟到期，而 `mint_pair` 刻意不碰其他未过期的 nonce——相机已经对着旧码的那种情况就是靠这条活着的。

**verify**：手机扫码配对 → 桌面卡片上的二维码与六位码立刻变成新的一组；桌面点「拒绝」也照样变。
**verify**：点「换一个码」→ 两个都换，新码能正常走完配对。

### 二、启动时没有人 bind 那个端口

这条更糟，因为它让第 1、2 件事的成果在最常见的路径上直接归零：密钥落了盘、设备列表记住了、端口也固定了，**而唯一会 bind 端口的入口是标题栏那个「手机连接」按钮**。所以哪怕桌面 app 正开着，只要这次启动没人点过它，已配对的手机拿着完全有效的 cookie 也照样是连接被拒。「配对活过重启」这句话在代码里成立，在用户那里不成立——第 2 件事的第一条 verify 在这次修补之前一直是假的。

`remote::resume()` 在启动时把网关拉起来，两道闸：**设备列表非空**，且**「退出时清除所有配对」没打开**。从没配过手机的人因此什么都感觉不到——不开端口、不弹防火墙、网络上不多一个字节；而打开了那个开关的人，要的本来就是关掉 app 等于全部作废，启动时当然也不该有一扇门替他开着。

它不铸码、不弹卡片。拉起来的只是门，新设备照样要过人工确认这道闸。

**verify**：配对一台手机 → 退出 app → 重启 → **不点任何按钮**，手机直接刷新能进。
**verify**：一台从没配过手机的机器，启动后不多出监听端口，也不弹防火墙。

## 这一步改变了安全模型

从「关掉应用等于全部作废」变成「30 天内一直有效，密钥落盘，能读到那份凭据的人就能伪造 cookie」。落地时必须同步改 `session.rs` 的模块文档——那里现在写的是密钥 "never written anywhere"。

---

# M2：通道抽象 + Tailscale（已完成）

## 通道抽象改多少

`Shared.tunnel` 从 `Mutex<LanTunnel>` 改成 `Mutex<Box<dyn RemoteTunnel>>` 加一个 `switch()`，trait 补三件东西：

- `state() -> TunnelState`（`Stopped / Starting / Running { base_url } / Failed`）——`start()` 改成 async 且立即返回，cloudflared 从启动到吐出域名要几秒，卡片靠轮询 `state()` 而不是靠 `start()` 阻塞；
- `scheme()`——cookie `Secure` 标记的唯一判据；
- `client_ip(&HeaderMap)`——手机的真实地址。

**不要注册表、不要多通道并发**：任何时刻只有一个活跃通道。`base_url` 是唯一权威，`authorities` 与配对 URL 都从它派生。

顺带：`trust.rs` 每个请求都调 `authorities()`，而它每次都跑一遍 `if_addrs::get_if_addrs()`。加个短 TTL 缓存，但要保留「笔记本换网后不重启也能更新」的语义。

## 三个横切问题（写任何新通道之前先落地）

**1. 手机的真实 IP。** 经 cloudflared（以及 `tailscale serve`，它同样是反向代理）转发后，网关的 socket peer address 恒为 `127.0.0.1`。两处依赖它，都要改走 `client_ip()`：

- 限流器——按回环地址限流，等于把全世界当一个 IP，攻击者的失败握手会把合法手机连坐拉黑，防御变成 DoS 自己的开关；
- 桌面授权弹窗——规格里那句「客户端信息：iPhone (IP: 192.168.x.x)」在隧道下会显示 `127.0.0.1`，人工确认这道防线被抽空。

此处信任 header 之所以安全，不是因为 header 可信，而是因为连接只可能来自本机那个我们自己拉起的子进程。**`client_ip()` 挂在 trait 上就是为了让这个前提跟着通道走**——LAN 通道必须返回 `None` 并忽略这些头，否则手机可以伪造自己的 IP 绕过限流。

**2. `Secure` 的唯一来源是 `scheme()`。** 网关看到的永远是反代发来的明文 HTTP，不存在「网关检测到 HTTPS 握手」这回事。明确禁止读 `X-Forwarded-Proto`——那是客户端可控的。

**3. 切通道会让所有已配对设备失效。** 原因**不是**我们的 cookie 绑了地址（`session.rs` 签的只有 id 和签发时间，且特意说明了为什么不放地址），而是**浏览器的 cookie 按 host 作用域**——换了通道 host 就变了，手机根本不会把旧 cookie 带上来。带 `Secure` 签发的 cookie 在 HTTP 回退时同样不会回发。

产品行为：切换前提示「当前 N 台已连接设备需要重新扫码」，用户确认后再切，切完卡片回到「等待扫码」。手机侧看到的必须是 M1 那个重新配对页，不是裸 401。

（另一条路是让 session 绑设备指纹而非 host，更好用，但要重新审 DNS rebinding 的防御是否还成立——不建议在这一轮做。）

## Tailscale 只做一件事

匹配 `100.64.0.0/10`（`100.64.0.0` ~ `100.127.255.255`），交叉验证接口名（Windows 友好名含 `Tailscale`，macOS `utun*`，Linux `tailscale0`）避免误认运营商 CGNAT 地址，`base_url` = `http://100.x.y.z:<port>`，`scheme()` = `Http`。纯本地判定，零外部依赖。

**MagicDNS 与 Serve HTTPS 不做**，它们给的是好看的域名和 PWA 的 secure context，不是可达性，而成本不低：Windows 上 `tailscale.exe` 默认不在 PATH，LocalAPI 是带认证的本地 HTTP 服务（token 要从注册表取），`tailscale cert` 还需要 tailnet 管理员在控制台开启 HTTPS Certificates。

Serve 与 Funnel 不是一回事：**Serve 是 tailnet 内 HTTPS，Funnel 是暴露到公网**。Serve 将来若做，会引入和上面第 1 条完全相同的源 IP 问题；**Funnel 默认不做**，要做就得走 M3 那套公网守卫。

未检测到活跃 Tailscale 网卡时，卡片显示引导文案，**并且不生成二维码**——发一个连不上的二维码比不发更糟。

**verify**：手机关 Wi-Fi 走蜂窝、加入同一 tailnet，扫码后能进 dsh 并能回答权限审批。
**verify**：退出 Tailscale 客户端后，卡片显示降级文案且不画二维码。
**verify**：构造一个伪 `CF-Connecting-IP` 的单元测试，LAN 通道必须忽略它。
**verify**：Secure Context 下 `dsh_mobile_session` 带 `Secure`，非 Secure Context 下不带。
**verify**：手动切换通道后，已配对手机刷新看到的是重新配对页，不是 401。

## 落地时的三处偏离

上面是计划，下面是实际做出来的。三处不一样，每一处都是落地时才看清的。

### 一、`state()` 与 async `start()` 没做，留给 M3

理由是 M2 的两个通道都不需要它：LAN 和 Tailscale 都从本机网卡里读答案，微秒级返回，不会「先返回再失败」，卡片也没有可轮询的东西。现在就定下一套状态机，等于在没有任何相关行为的情况下把设计敲死，而真正会几秒钟起不来、会在返回之后失败的是 cloudflared。等它落地时连着真实行为一起改。

trait 只加了 `scheme()` 和 `client_ip()`——这两个今天就有消费者。

（M3 把 `state()` 做了，但没做 async：`start()` 仍然是同步函数，只是不等结果。见 M3 的「偏离一」。）

### 二、切通道不吊销已配对设备

计划里写的是「切完卡片回到等待扫码」，听起来像要清空设备列表。实际没有清，理由是**不清更好而且不花钱**：cookie 按 host 作用域是浏览器的行为，不是我们的吊销——换到 Tailscale 之后手机确实要重新配，但**换回局域网，原来那份配对还在**。清掉会把一个可逆的动作变成不可逆的。

所以卡片在切换前弹框、报出「现在的 N 台设备都要重新扫一次码」，并明说不会吊销；切完设备列表照旧列着。列表本来的含义就是「这个网关放进来过的设备」，不是「此刻连着的设备」——那一列从来没有活连接的概念。

### 三、老通道上的手机拿到的是说明页，不是重新配对页

计划最后一条 verify 要的是「看到重新配对页，不是裸 401」。做出来的是一个**说明页**，因为重新配对页在这里帮不上忙：六位码那条路会发一个 cookie 给一个 host，而下一个请求的 Host 正是栅栏要拒的那个——换句话说，在旧地址上无论如何配不进来。所以那个页面老实说明发生了什么，并指向电脑上的卡片（以及「切回原通道这个地址就又能用」）。

它只发给**带着设备 cookie、且 Host 确实是本机某个地址**的请求：端口扫描器拿到的还是原来那个空 403，`Host: 127.0.0.1` 也是——那个是桌面自己浏览器里的页面，正是栅栏存在的理由。cookie 是先做的那一半，因为拒绝路径上绝对不能每个探测包都去遍历一遍网卡。

### 对应的测试

| verify | 落点 |
| :--- | :--- |
| 伪造 forwarded-for，LAN 通道必须忽略 | `tunnel::tests::a_direct_tunnel_ignores_every_forwarded_for_header`，三种拼法都试 |
| `Secure` 只由 scheme 决定 | `proxy::tests::a_secure_scheme_is_the_only_thing_that_adds_secure`（以及反面那条） |
| 切换后手机不是裸 401 | `tests::a_phone_left_on_the_old_channel_is_told_where_everyone_went`，顺带验了陌生人仍然什么都拿不到 |
| Tailscale 地址判定 | `tunnel::tests::the_tailnet_range_is_the_cgnat_range` 与 `the_adapter_has_to_be_named_like_tailscale_too` |

剩下两条要在真机上验：手机关 Wi-Fi 走蜂窝、加入同一 tailnet 扫码进 dsh 并能回答权限审批；退出 Tailscale 客户端后卡片显示降级文案且不画二维码。

---

# M3：Cloudflare Named Tunnel + 公网守卫（已完成）

## 只推 Named，Quick 标注为试用

Quick Tunnel（`--url`）域名每次重启都变，**已配对设备与主屏幕图标全部失效**——等于把 M1 的成果抵消掉。加上 `*.trycloudflare.com` 在国内连通性不可靠、Cloudflare 官方声明其不适合生产（无 SLA、无固定域名），它只能是「快速试一下」。

Named Tunnel（`--token` + 自有域名）域名稳定，PWA 成立，已配对设备跨重启存活。**Quick Tunnel 模式下不注入 manifest。**

（落地时这一条换了个地方实现：不是「不注入」，而是网关对这四个路径一律 404。注入发生在 plugin 里，而 plugin 写的是 dsh 的 index——那份 HTML 桌面自己的 webview 也要吃，plugin 根本不知道当前是哪条通道。网关是唯一知道的人。离线壳的 worker 一并按这条处理：注册在一个明天就不存在的 origin 上，是纯粹的垃圾。）

这也意味着：计划不应假装 Cloudflare 能覆盖「普通用户」。国内的广域网主线是 Tailscale 和自托管中继，Cloudflare 是给有自有域名的用户的选项。

## cloudflared 二进制

**不打包**（约 50MB，与铁律 #6 的「目标增量 < 500KB」直接冲突）。这一轮**只做探测 + 安装指引**（检测 PATH 与 app local data 目录，找不到就展示各平台安装命令），**不做应用内自动下载**。

自动下载单独评估：它是「从网上下一个 exe 然后执行它」，没有校验和或签名验证就是供应链缺口；macOS 下载的二进制带 quarantine 属性，直接 exec 会被 Gatekeeper 拦；国内 GitHub Release 可能根本下不动。工作量和风险都比通道本身大。

## 子进程生命周期

`tokio::process::Command` 拉起，实时读 stderr 捕获域名；应用退出、通道切换、用户主动停止时显式 kill 并 await 退出；**Windows Job Object**（及各平台对应机制）保证桌面端崩溃时不残留孤儿进程。

**`cloudflared` 自己的日志不能原样写进应用日志**——它会打印隧道 token 和连接元数据。

Named Tunnel Token **不进 `desktop.json`**，和 M1 的签名密钥同一套办法（`app_dir` 下单独的文件，0600）。它是能操作用户 Cloudflare 账号下该隧道的 bearer 凭证——注意这一条和签名密钥的威胁模型**不同**：签名密钥只开这台机器的门，而这个 token 开的是用户 Cloudflare 账号下的门，所以「反正同用户进程本来就能……」那套推理在这里不成立。如果哪天要上 `keyring`，是为它上，不是为签名密钥。

## 公网守卫（与通道同批交付，不允许先发通道后补开关）

LAN 模式忘了关，风险限于同一个 Wi-Fi。公网隧道忘了关一整晚是另一个量级的事——而 DSH 会话拥有本机完整的终端执行权。

1. **空闲自动停机**：无已连接设备且超过 N 分钟（默认 30）无请求，自动停并在卡片上说明原因；
2. **常驻可见性**：隧道运行期间托盘图标或主窗口有明确的「本机当前可从公网访问」指示，点击直达关停——不是藏在弹窗里的一行状态文字；
3. **退出确认**：应用退出时若隧道仍在运行，显式提示而不是静默关掉。

## 必须写进文档的一条事实

**隧道开启期间，信任栅栏的强度是下降的。** `addresses()` 只收在网的物理网卡地址，正是为了保证「`Host: 127.0.0.1` 的请求一定不是手机发的」。隧道一开，authorities 里多了一个公网主机名，而本机任何进程都可以构造一个带该 Host 的请求打到网关——socket 层区分不出它和 cloudflared。剩下的防线只有 cookie 和一次性 nonce。

这不是 showstopper（纵深防御本来就是这么用的），但别让后来的人以为栅栏还兜着底。已经写进 `trust.rs` 的模块文档。

落地时补了一道，把损失收窄了一点：**隧道开着期间，网关只接受 loopback 的 peer**。理由是那个前提——「能通过这条通道连上来的只可能是我们自己拉起的子进程」——在一个 bind 了 `0.0.0.0` 的 socket 上本来就是假的：同一个 Wi-Fi 上的机器可以直接打这个端口，并在 `Host` 里写上隧道的域名，而那正是栅栏刚刚决定要放行的名字。加上这条之后，攻击者必须已经在这台机器上，而不只是在同一个网里。这同时也是 `CF-Connecting-IP` 敢被采信的唯一理由（`cloudflare.rs::client_ip` 里再查一次，两处各自成立）。

它没有把洞补上，只是把「同一个 Wi-Fi」排除掉了。本机进程照旧可以伪造，剩下的防线仍然只有 cookie、一次性 nonce 和人工确认——而针对这一点的答案是下面那套守卫，不是声称栅栏还管用。

**verify**：kill 桌面进程后 `cloudflared` 不残留。
**verify**：从两个不同公网 IP 打失败握手，只有超限的那个被拉黑；隧道重启后黑名单清空。
**verify**：无设备连接 30 分钟后隧道自动停止且卡片说明原因。
**verify**：`desktop.json` 里 grep 不到 token。

## 顺带把白屏补掉

这是 M1 就欠着的洞（见「躲不掉的坑」），一直等的就是一个 secure context，而 Cloudflare 的边缘终结 TLS 正好给了它。

网关多发两个文件：`/dsh-mobile-sw.js` 和 `/dsh-mobile-offline`。Service Worker 只拦**导航请求**，只在 `fetch` 真的失败时（网络层失败，不是 4xx）拿缓存里那一页顶上。注册脚本由 plugin 注入，两道闸：`isSecureContext`（局域网和 Tailscale 都是明文，于是压根不注册）和 `window.__dshRemoteCard`（桌面自己那个 webview——loopback 也算 secure context，不挡住它就会给 dsh 自己的 origin 装一个没人会看的 worker）。

**它刻意不缓存 dsh 的任何东西**。这是对「Service Worker 通常拿来干什么」的一次明确拒绝：dsh 是另一个进程提供的、这个 app 不拥有的应用，缓存它的资源等于让手机拿昨天的前端打今天的后端，而两边都察觉不到。白屏值得修，为了修白屏去赌一个陈旧的 dsh 不值得。

**verify**：Cloudflare 通道下手机装好图标 → 关掉电脑 → 点图标，看到的是说明页而不是白屏，页面上的按钮能重试。
**verify**：局域网通道下 `navigator.serviceWorker.getRegistrations()` 是空的（明文 origin 根本注册不了）。

## 落地时的五处偏离

### 一、`start()` 不是 async，只是「立刻返回」

M2 把这条推给了 M3，落地时做的是：`start()` 仍然是同步函数，但不等结果——它只报「还没开始就能知道」的失败（没有网卡、没有 tailnet、没有 cloudflared、没有 token），其余全部走 `state()`。卡片每 300ms 问一次，最多 45 秒。

不做 async trait 的理由是它在 `Box<dyn>` 后面要么引入 `async-trait` 这个依赖，要么写一堆 `Pin<Box<dyn Future>>`，而换来的东西恰好是零：真正要等的是一个正在读自己 stderr 的子进程，等待发生在另一个线程里，调用方要的只是「别卡住我」。

### 二、Named Tunnel 的域名是用户填的，不是从日志里读的

计划里写「实时读 stderr 捕获域名」，那是 Quick Tunnel 的行为。Named Tunnel 走 `--token`，ingress 规则在 Cloudflare 控制台里（remotely managed），`cloudflared` 自己既不知道也不打印公网域名，`--url` 传了也会被忽略。

所以这条路是反过来的：**域名由用户填**（存 `desktop.json`，它不是秘密），stderr 只用来等一条 `Registered tunnel connection`。作为交换，卡片上直接印出控制台那条 ingress 要指向的地址——`http://localhost:<port>`，端口是 M1 固定下来的那个，这正是它值得被固定的又一个理由。

### 三、`std::process` + 读取线程，不是 `tokio::process`

树里已经有一套拉子进程的办法（`server.rs` 拉 `dsh web` 就是它）：`std::process::Command` + `group_leader` / `tethered` / Windows `Job`。M3 原样复用，包括那个 Job Object。多引一套 tokio 的进程 API 只会让树里有两种拉子进程的写法。

日志按计划不原样落盘：只分类，不镜像；保留的最后一行会把 token 做**精确替换**（token 是我们手里的字符串，不用去猜它长什么样），失败时把这一行拼进卡片上的说明里。

### 四、「切通道会清空限流黑名单」是顺手补的，不是计划里的

计划的 verify 里有「隧道重启后黑名单清空」，但没说为什么。落地时发现理由比计划写的更强：**黑名单里的地址只在某一条通道下有意义**。局域网下那是手机自己，隧道下那是 `CF-Connecting-IP` 说的地址——整个公网。带着跨通道走，等于因为隧道另一端某人用过同一个地址就把用户自己的手机拉黑。所以 `switch` 和 `halt` 都会清。

### 五、公网通道不会在启动时自动拉起

M1 的 `resume()` 在启动时把网关拉回来，让已配对的手机第二天还能直接用。M3 给它加了第三道闸：**公网通道不算**。

理由是这两件事是不同的承诺。「手机明天还能用」是一个承诺；「因为上次关 app 时这台电脑在公网上，这次开机就自动回到公网上」是另一个，用户没做过。所以存着的通道是 Cloudflare 时，`resume` 直接不动手，要等人打开卡片。这也让「关闭公网通道」这个按钮变得干净：它只停隧道、不改通道设置，因为下次启动本来就不会自动拉起来。

### 对应的测试

| verify | 落点 |
| :--- | :--- |
| 隧道开着时，局域网上的机器不能直连网关 | `proxy::tests::a_public_tunnel_takes_nothing_but_loopback` |
| `CF-Connecting-IP` 只在 loopback 连接上采信 | `cloudflare::tests::the_forwarded_address_is_believed_only_from_loopback` |
| `Secure` 只由 scheme 决定，两条通道各验一次 | `tests::the_cookie_is_secure_exactly_when_the_channel_is` |
| Named Tunnel 下这四个都发，且权威是裸域名 | `tests::a_named_tunnel_publishes_the_home_screen_files` |
| 闲置判定要同时看「无活连接」和「无请求」 | `tests::a_live_connection_is_not_an_idle_gateway` |
| worker 脚本是合法 JS，且路径只拼写一次 | `proxy::tests::the_worker_script_is_balanced_javascript` |
| token 不进任何日志行 | `cloudflare::tests::the_token_is_taken_out_of_anything_kept` |
| 隧道怎么知道自己起来了 | `cloudflare::tests::a_named_tunnel_waits_for_a_registered_connection` |
| 切通道清空黑名单 | `trust::tests::changing_the_channel_forgets_everybody` |

剩下的要在真机上验，因为它们要么需要一个 Cloudflare 账号，要么需要把电脑关掉：上面四条 verify 全部，加上 M2 遗留的两条（手机走蜂窝进 tailnet；退出 Tailscale 后卡片降级）。

## 交付之后：临时域名那条通道被去掉了

真机上验的结果是它根本不通：手机扫完码打不开被送去的那个 `*.trycloudflare.com` 地址。于是这条通道整条删掉，而不是留着加一行警告——它本来的理由就只有「不用账号先试一下」，而一条试不通的试用通道是负数。

删掉的不只是那个枚举分支：`installable()` 和网关对那四个主屏幕路径的 404 一起没了，因为它们存在的唯一理由是这条通道（见上面「只推 Named」那节的括号）。`cloudflared` 的 quick 模式解析（`--url`、从 banner 里读域名）也一并没了。

**留在原地的那半个问题**：`*.trycloudflare.com` 连不上，多半意味着这台机器上的 `cloudflared` 根本没能和 Cloudflare 边缘建立连接——那 Named Tunnel 在同一台机器上也一样起不来。删掉临时域名没有修掉这件事，只是不再用一条注定失败的通道去撞它。真要查，看卡片上 `cloudflared` 退出时留下的那行。

存着 `remoteChannel: "cloudflare-quick"` 的 `desktop.json` 不会让 app 起不来：不认识的通道名一律回落到局域网，`settings::channel` 本来就是这么写的，`tunnel::tests::every_channel_name_round_trips` 现在把这个名字钉成「不认识」。

---

# 之后：中继

**自托管中继**——Rust 单二进制（Axum + Tokio + yamux），一条出站 WebSocket 长连接 + 流多路复用，**不做 E2EE，只靠 TLS**，用户自己丢到 VPS 上，桌面端填个地址就能用。它就是第四个 `RemoteTunnel` 实现，与 M2 的抽象天然契合，工作量约官方托管的 1/5，没有运营成本、滥用责任与合规负担，而且将来做官方托管可以原样复用。

**官方托管中继在三个问题回答之前不排期**：

1. **纯浏览器做不出「零知识」。** 手机端做加密的那段 JS 正是中继自己下发的，中继想看明文只需对特定 session 下发一份改过的脚本。这是信任引导问题，无解——除非装原生壳（与铁律 #3 冲突）或浏览器扩展。要做可以，但承诺必须降级为「防御被动监听与中继侧存储泄露」，**不能写「零知识」「硬件级」**。（另外 X25519 在 SubtleCrypto 里普及很晚，Safari 17+ / Chrome 133+，要准备 ECDH P-256 回退。）
2. **滥用与合规。** DSH 会话等于目标机器上的一个 shell。运营一个把它暴露到公网的服务，挖矿、跳板、勒索的责任，以及国内备案与网络安全法下的日志留存义务，全部落在我们身上。「完全匿名 + 无日志」和这件事不相容。
3. **谁付账单。**

---

# 躲不掉的坑

| 坑 | 说明 |
| :--- | :--- |
| **iOS 独立窗口有自己的 cookie jar** | 在 Safari 里配对成功 → 添加到主屏幕 → 从图标打开，是一个全新的未认证会话。所以 M1 的重新配对页不是锦上添花，是**首次安装流程的必经一步** |
| **地址变了，我们一行代码都跑不到** | 端口变、DHCP 换 IP、切通道——图标指向死 origin，用户看到的是浏览器的「连接被拒」。**电脑关机、休眠、app 没运行，是同一类**，而且是日常最常撞到的那一种：iOS 独立窗口没有浏览器界面可显示，呈现出来就是一片白，没有任何提示。唯一的解是 Service Worker 缓存离线壳，而它要求 secure context，**LAN 明文 HTTP 下这条路是堵死的**（自签证书不算 secure context，私有 IP 也签不出真证书）。**M3 把它治了一半**：Cloudflare 的边缘终结 TLS，于是那条通道下有 secure context，离线壳成立——见 M3 的「顺带把白屏补掉」。另一半治不了，而且大概永远治不了：局域网和 Tailscale 仍然是明文 HTTP，那两条通道下白屏依旧。「桌面 app 开着但没人点过按钮」那一半已经由 `remote::resume()` 修掉，见 M1 末尾 |
| **跨 origin 的扫码解决不了图标** | 就算扫到了新地址，导航过去等于离开 PWA 的 scope，iOS 上会跳出 Safari，用户最后有两个图标。UI 上要说实话：扫码解决的是「我能连上」，不是「我的图标还能用」 |
| **扫任意 QR 然后导航过去 = 开放重定向** | 若将来做页内扫码，必须校验形状（`http(s)://<host>[:<port>]/?pair_token=<32 hex>`）并**在跳转前把目标 host 显示给用户确认**，否则这是个很好用的钓鱼跳板 |

# 明确不做

- **不解决 LAN 明文 HTTP 的嗅探风险。** LAN 通道在可预见的将来都是明文 HTTP，`Secure` 只对 HTTPS 通道有意义。诚实的表述是：LAN 模式接受此风险，需要传输加密的用户改用 Tailscale（WireGuard 在链路上加密，虽然浏览器仍然认为自己在明文里）或 Cloudflare 通道。
- **不自动下载 cloudflared。** 见 M3。探测 PATH 和 app 数据目录，找不到就在卡片上给出该平台的安装命令，仅此而已。
- **mDNS**（广播 `dsh-desktop.local`，host 与 IP 脱钩）。它是「DHCP 换 IP 导致 origin 变」的唯一治本解，iOS/macOS 原生支持、Android 参差。列为 M1 之后的可选项，不进这一轮。
- **实时摄像头扫码**（`getUserMedia` 要 secure context）。想在页内扫码，回退方案是 `<input type="file" accept="image/*" capture="environment">` 加纯 JS 解码——普通表单控件，不受 secure context 限制。
- **内网优先直连 + 自动故障转移。** `https://` 页面 fetch `http://192.168.x.x` 会被当作 active mixed content 直接阻断，连超时都等不到。可行的替代是顶层导航（丢页面状态）或干脆让用户手动选「我在家 / 我在外面」——难看但诚实。
- **靠 Service Worker 保活。** 它是事件驱动的，空闲约 30 秒被终止，锁屏更快。断线恢复只能是：resume 凭证存 IndexedDB，回到前台被唤醒后重放握手，**桌面端为断连 session 保留一个有 TTL 的重连窗口**。
