# Security Policy · 安全策略

**简体中文** · [English](#english)

dsh desktop 会让手机连到电脑上的 dsh 会话，而 dsh 会话就是一个在你电脑上有 shell 的 agent。远程连接一旦放错了设备进来，交出去的就是整台电脑，所以我们把安全问题当作最高优先级处理。

## 报告漏洞

**请不要在公开 Issue 里报告安全漏洞。**

请通过 GitHub 的私密漏洞报告提交：[Report a vulnerability](https://github.com/MochiNek0/dsh-desktop/security/advisories/new)。

报告里最好包含：受影响的版本与系统、使用的连接通道（局域网 / Tailscale / Cloudflare）、复现步骤，以及你认为的影响范围。我们会在 7 天内回复，确认后尽快发布修复版本，并在公告中致谢（除非你希望匿名）。

## 支持的版本

只有最新的正式版会收到安全修复。应用内置自动更新，请保持更新。

| 版本 | 是否支持 |
| :--- | :--- |
| 最新正式版（见 [Releases](https://github.com/MochiNek0/dsh-desktop/releases/latest)） | ✅ |
| 更早的版本 | ❌ |

## 范围

**属于本项目的范围**：

- 手机连接网关（`src-tauri/src/remote/`）：配对、设备 Cookie、Host / Origin 校验、Cloudflare 隧道
- 自动更新与安装包分发（`src-tauri/src/update.rs`、`updater-proxy/`）
- 随包插件 `dsh-desktop-signal`（`plugin/`）
- 桌面端对 `dsh-desktop://` 协议、命令行参数与右键菜单的处理

**不属于本项目的范围**（请报告给对应项目）：

- `dsh` / `dsh web` 本身 → [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)
- 第三方插件、Tailscale、`cloudflared`、系统 WebView
- 需要攻击者已经能以你的用户身份在本机运行代码的场景（这样的进程本来就能直接驱动本机的 dsh）

## 手机连接的安全模型

dsh 本身没有一行改动。`dsh web` 仍然只监听回环地址，桌面端在它前面放一个网关，手机只能经过网关接入：

```
手机 ──▶ 0.0.0.0:<网关>   来源校验 ─ 设备 Cookie ─ 改写
                │
                └──▶ 127.0.0.1:<dsh>   dsh 自己的 Cookie，只留在桌面端
```

- **两道互相独立的门**：一道是请求来源校验（Host 必须是本机真实地址，跨站请求会被拒绝，用来防 DNS 重绑定和跨站请求），另一道是电脑上弹出的「允许连接」对话框。任何一道都不能被另一道绕过。
- **配对码**：二维码里是 16 字节随机数，旁边的 6 位字符是同一个值，方便手动输入。**5 分钟有效、只能用一次**，连续输错会被限速。配对码本身不授权任何东西，它只换来"提问的资格"，最后放不放行由电脑前的人点头决定。
- **设备 Cookie**：用 HMAC-SHA256 签名，比较时使用常量时间。**有效期 30 天**，每台设备都可以单独断开；「全部断开」会清空设备列表并轮换签名密钥，之前签发的所有 Cookie 立即作废。
- **签名密钥存在磁盘上**：为了让配对能跨重启保持 30 天，密钥保存在应用数据目录里（类 Unix 系统上权限为 0600，Windows 上继承仅当前用户可访问的目录）。能读到这个文件的进程，本来就能直接驱动本机的 dsh。如果你更想要"关掉应用就全部作废"，可以在「手机连接」卡片里打开「退出时断开所有已配对设备」。
- **Cloudflare 通道**：这是三条通道里唯一把电脑暴露到公网的一条，所以单独加了防护：只支持你自己账号下的命名隧道；**闲置 30 分钟自动关闭**；开着期间托盘一直有提醒，退出时还会再问一次；隧道开启时，网关只接受来自回环地址（也就是 `cloudflared`）的连接。隧道 Token 单独存放，不写日志，也不会显示在界面上。
- **局域网 / Tailscale 通道是明文 HTTP**：浏览器只在 HTTPS 下发送 `Sec-Fetch-*` 请求头，所以在这两条通道上，防护靠的是 Host 校验和设备 Cookie。请只在你信任的网络里使用局域网通道。
- **资源上限**：网关最多同时保持 256 个连接，并给请求头设置了读取超时，防止慢速连接长期占用名额。

## 安装包完整性

- 自动更新下载的每个安装包都有 [minisign](https://jedisct1.github.io/minisign/) 签名，公钥内置在应用里，签名校验不通过的更新不会被安装。
- 每个 Release 都附带 `SHA256SUMS.txt`，手动下载后可以这样核对：

  ```sh
  sha256sum -c SHA256SUMS.txt --ignore-missing      # Linux
  shasum -a 256 -c SHA256SUMS.txt --ignore-missing  # macOS
  ```

  ```powershell
  Get-FileHash .\dsh-desktop_<version>_x64-setup.exe -Algorithm SHA256  # Windows，与文件里对应的那一行比对
  ```

- 安装包目前**没有** Windows 代码签名，也没有经过 macOS 公证，所以首次运行时系统可能会拦截（处理方法见 README）。请只从 [GitHub Releases](https://github.com/MochiNek0/dsh-desktop/releases) 或[官网](https://dsh-desktop.cc.cd/)下载。
- npm 上的同名包 `dsh-desktop` **与本项目无关**。本项目不通过 npm 分发。

---

<a id="english"></a>

## English

dsh desktop lets a phone reach the dsh session on your computer, and a dsh session is an agent with a shell on that computer. A gateway that lets the wrong device in hands over the whole machine, so we treat security reports as the top priority.

### Reporting a vulnerability

**Please do not report security issues in public issues.** Use GitHub's private reporting: [Report a vulnerability](https://github.com/MochiNek0/dsh-desktop/security/advisories/new). Please include the version, the OS, the channel (LAN / Tailscale / Cloudflare), steps to reproduce, and the impact you expect. We reply within 7 days and credit reporters in the advisory unless you ask us not to.

Only the latest stable release receives security fixes. The app updates itself.

### Scope

In scope: the phone gateway (`src-tauri/src/remote/`), the updater and installer distribution (`src-tauri/src/update.rs`, `updater-proxy/`), the bundled `dsh-desktop-signal` plugin (`plugin/`), and how the app handles `dsh-desktop://` links, CLI arguments and the context menu.

Out of scope: `dsh` itself (report to [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)), third-party plugins, Tailscale, `cloudflared`, the system WebView, and attacks that require already running code as your user.

### Phone-connection model, in short

- `dsh web` stays on loopback and is never modified. A gateway in front of it has **two independent gates**: a request-origin fence (Host must be one of this machine's real addresses; cross-site requests are refused, which covers DNS rebinding and cross-site requests) and a human clicking "Allow" on the desktop.
- **Pairing code**: 16 random bytes, also shown as 6 characters you can type. It is **valid for 5 minutes, single use**, and wrong guesses are rate-limited. It only earns the right to ask.
- **Device cookie**: HMAC-SHA256, compared in constant time, **valid for 30 days**. You can revoke devices one at a time, and "Disconnect all" also rotates the signing key. The key is stored in the app's user-only data directory so that a pairing survives a restart. Turn on "Disconnect every phone when this app closes" on the phone-connection card if you want the old model, where closing the app revokes everything.
- **Cloudflare**: named tunnels in your own account only. It **closes after 30 minutes idle**, the tray warns you the whole time it is open, and the app asks again when you quit. While it is up, the gateway accepts only loopback peers. The token is stored separately and is never logged.
- **LAN and Tailscale are plain HTTP.** Browsers send `Sec-Fetch-*` only over HTTPS, so on these two channels the defence is the Host check plus the cookie. Use LAN only on networks you trust.

### Installer integrity

Every updater bundle carries a minisign signature, checked against a public key built into the app. Every release also ships a `SHA256SUMS.txt`. Installers are **not** Authenticode-signed or notarized yet. Download only from GitHub Releases or the official site. The npm package named `dsh-desktop` is **not** this project.
