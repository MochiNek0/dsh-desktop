//! The desktop half: a QR code, who is connected, and the way to throw them
//! off again.
//!
//! Drawn the way every other card in this app is drawn — an injected script, a
//! card over whatever page the window is showing, the theme read off dsh's own
//! page, and the buttons answering through the cancelled-navigation channel in
//! [`crate::controls`]. See [`crate::dialog`], which is the general case of the
//! same idiom; the difference here is that the card has state that keeps
//! changing under it, so Rust pushes a whole new view rather than asking one
//! question.
//!
//! ## The QR code is drawn here, not encoded in Rust
//!
//! There is no image to fetch and nowhere to fetch it from: the card is drawn
//! into dsh's document, and a `<img src>` would be a request on a page whose
//! network this app does not own. So the matrix is computed in the script and
//! painted as a grid of `<div>`s — which also means no Rust crate was added to
//! the build for something the page can do in two hundred lines.
//!
//! Byte mode, error correction level M, versions 1 through 10. That covers a
//! pairing URL of up to 213 bytes, which is an address, a port and a nonce
//! several times over, and the encoder answers `null` rather than a wrong code
//! for anything longer. Level M is the middle of the four and what most codes
//! in the wild use: enough redundancy to survive a phone camera at an angle
//! without making the modules small enough to matter on a laptop screen.
//!
//! ## Every string comes from Rust
//!
//! Including the ones on the card that never change. The two languages live in
//! one place in this app — see [`crate::i18n`] — and a card that carried its own
//! copies would be a second place for them to drift. They ride along on each
//! view rather than being baked in when the document loads, so a card that is
//! open when dsh's language changes redraws in the new one on its next update
//! without a relabelling path of its own.

use tauri::AppHandle;

use super::session::Device;

/// Everything on the card at one moment.
pub struct View {
    /// The pairing URL the QR code carries. `None` when the gateway could not
    /// be raised, in which case `error` says why.
    pub url: Option<String>,
    pub error: Option<String>,
    pub devices: Vec<Device>,
    /// A line under the code: the firewall warning, or the one about nothing
    /// having connected. `None` most of the time.
    pub hint: Option<String>,
    /// Whether the phone's stylesheet patch is on. See [`crate::remote::style`].
    pub style_patch: bool,
}

/// Put the card up, or update the one already up.
pub fn show(app: &AppHandle, view: &View) {
    let payload = serde_json::json!({
        "url": view.url,
        "error": view.error,
        "hint": view.hint,
        "stylePatch": view.style_patch,
        "devices": view.devices.iter().map(|device| serde_json::json!({
            "id": device.id,
            "label": device.label,
            "address": device.address,
            "since": device.since,
        })).collect::<Vec<_>>(),
        "text": text(),
    })
    .to_string();

    // Serialised twice, as everything on this channel is: once into JSON, and
    // once into a JavaScript string literal the card parses. A quotation mark in
    // a translated string would otherwise end the call it is inside.
    let json = serde_json::to_string(&payload).expect("a string is always serializable");
    crate::controls::eval(
        app,
        &format!("window.__dshRemote && window.__dshRemote({json})"),
    );
}

/// Take it down.
pub fn hide(app: &AppHandle) {
    crate::controls::eval(app, "window.__dshRemoteHide && window.__dshRemoteHide()");
}

/// Every word on the card, in whichever language dsh is in.
fn text() -> serde_json::Value {
    serde_json::json!({
        "title": t!("手机连接", "Connect a phone"),
        "lede": t!(
            "用手机相机扫这个码。电脑上会再问你一次要不要放行。",
            "Scan this with the phone's camera. The computer will ask you once more before letting it in."
        ),
        "waiting": t!("等待手机扫码…", "Waiting for a phone to scan…"),
        "connected": t!("已连接 {} 台设备", "{} connected"),
        "since": t!("{} 起", "since {}"),
        "kick": t!("断开", "Disconnect"),
        "kickAll": t!("断开全部并换密钥", "Disconnect all and change the key"),
        "close": t!("关闭", "Close"),
        "copy": t!("复制链接", "Copy the link"),
        "copied": t!("已复制", "Copied"),
        "unavailable": t!("暂时没法开启", "Cannot start it right now"),
        "stylePatch": t!("给手机套用移动端样式", "Restyle dsh for the phone"),
        "stylePatchWhy": t!(
            "dsh 的设置弹窗在窄屏下会挤成一列。等 dsh 官方适配了就可以关掉。改完刷新手机页面生效。",
            "dsh's settings dialog collapses into a column on a narrow screen. Turn this off once dsh ships its own. Reload the page on the phone after changing it."
        ),
    })
}

/// What Windows is about to do, or what it has quietly already done.
pub fn firewall_hint(state: super::firewall::Firewall) -> Option<String> {
    match state {
        super::firewall::Firewall::Unasked => Some(
            t!(
                "第一次连接时 Windows 防火墙会弹窗问你，记得选「允许访问」。",
                "Windows Firewall will ask the first time something connects. Choose Allow access."
            )
            .to_string(),
        ),
        _ => None,
    }
}

/// What to say when the code has been on screen for a while and nothing has so
/// much as opened a connection.
///
/// This is the firewall failure as the user meets it, and it is worth saying in
/// those words rather than in the firewall's: from the phone the page simply
/// never loads, and from here nothing happened at all.
pub fn silence_hint() -> String {
    t!(
        "手机打不开的话，多半是防火墙拦住了入站连接。在 Windows 安全中心里\
         允许 dsh-desktop 的专用网络入站，再扫一次。也确认一下手机和电脑连的是同一个 Wi-Fi。",
        "If the phone cannot open the page, an inbound firewall rule is usually what is stopping it. \
         Allow dsh-desktop on private networks in Windows Security and scan again. \
         Check that the phone is on the same Wi-Fi, too."
    )
    .to_string()
}

/// The script that draws it, injected into every document the window loads.
pub fn script() -> String {
    let scheme = crate::controls::SCHEME;
    let font = crate::controls::FONT;
    let maker = crate::controls::dom_make();
    let watcher = crate::controls::theme_watcher("dsh-rc-dark");
    let corners = crate::controls::corners(&["dsh-rc"]);
    let encoder = encoder();

    format!(
        r#"(function () {{
  // The top document only, like every other card here: the buttons answer
  // through a navigation no iframe can make. See `controls`.
  if (window.top !== window.self) return;
  if (window.__dshRemoteCard) return;
  window.__dshRemoteCard = true;

  var root = null, sheet, head, lede, code, link, status, list, note, patch, foot;
  var TEXT = {{}};

  function signal(verb) {{
    window.location.href = '{scheme}://' + verb;
  }}

{maker}

{watcher}

{encoder}

  function build() {{
    var style = document.createElement('style');
    style.textContent =
      // Under the titlebar, so the window can still be closed while the card
      // is up, and clear of the strip those buttons sit in.
      '.dsh-rc{{position:fixed;inset:0;z-index:2147483644;display:none;' +
      'align-items:center;justify-content:center;box-sizing:border-box;' +
      'padding:calc(var(--dsh-titlebar-height,36px) + 12px) 16px 20px;' +
      'background:rgba(18,18,22,.34);-webkit-backdrop-filter:blur(3px);' +
      'backdrop-filter:blur(3px);user-select:none;-webkit-user-select:none;' +
      'font:14px/1.6 {font};' +
      '--rc-bg:#fff;--rc-fg:#1a1a1a;--rc-muted:#6b7280;--rc-line:#e5e7eb;' +
      '--rc-accent:#4d6bfe;--rc-danger:#b42318;--rc-hover:rgba(0,0,0,.05);' +
      '--rc-shadow:0 24px 64px rgba(0,0,0,.24),0 0 0 .5px rgba(0,0,0,.08)}}' +
      '.dsh-rc.dsh-rc-dark{{--rc-bg:#232326;--rc-fg:#f2f2f7;--rc-muted:#9ca3af;' +
      '--rc-line:rgba(255,255,255,.12);--rc-hover:rgba(255,255,255,.08);' +
      '--rc-shadow:0 24px 64px rgba(0,0,0,.6),0 0 0 .5px rgba(255,255,255,.1)}}' +
      '.dsh-rc-shown{{display:flex}}' +
      '{corners}' +
      '.dsh-rc-sheet{{width:min(380px,100%);max-height:100%;overflow:auto;' +
      'box-sizing:border-box;padding:22px;border-radius:16px;' +
      'background:var(--rc-bg);color:var(--rc-fg);box-shadow:var(--rc-shadow)}}' +
      '.dsh-rc-head{{margin:0 0 6px;font-size:16px;font-weight:600}}' +
      '.dsh-rc-lede{{margin:0 0 16px;color:var(--rc-muted)}}' +
      // The code itself. A grid of cells rather than an image: nothing is
      // fetched, and the quiet zone is padding on the frame around it.
      '.dsh-rc-code{{display:grid;gap:0;width:236px;margin:0 auto 12px;' +
      'padding:12px;box-sizing:content-box;background:#fff;border-radius:10px}}' +
      '.dsh-rc-code i{{display:block;width:100%;padding-bottom:100%;' +
      'background:#fff}}' +
      '.dsh-rc-code i.on{{background:#000}}' +
      '.dsh-rc-status{{margin:0 0 10px;font-weight:500}}' +
      '.dsh-rc-list{{margin:0 0 14px;padding:0;list-style:none;' +
      'border-top:1px solid var(--rc-line)}}' +
      '.dsh-rc-list li{{display:flex;align-items:center;gap:10px;padding:9px 0;' +
      'border-bottom:1px solid var(--rc-line)}}' +
      '.dsh-rc-who{{flex:1;min-width:0}}' +
      '.dsh-rc-who b{{display:block;font-weight:500}}' +
      '.dsh-rc-who span{{display:block;color:var(--rc-muted);font-size:12px}}' +
      '.dsh-rc-note{{margin:0 0 14px;padding:10px 12px;border-radius:8px;' +
      'background:var(--rc-hover);color:var(--rc-muted);font-size:13px}}' +
      '.dsh-rc-bad{{color:var(--rc-danger)}}' +
      '.dsh-rc-foot{{display:flex;gap:8px;justify-content:flex-end;align-items:center}}' +
      '.dsh-rc button{{all:unset;box-sizing:border-box;padding:7px 14px;' +
      'border-radius:8px;cursor:pointer;font:inherit;font-weight:500;' +
      'color:var(--rc-fg);-webkit-appearance:none;appearance:none}}' +
      '.dsh-rc button:hover{{background:var(--rc-hover)}}' +
      // After `all:unset` and qualified past it, or the rule above takes the
      // whole of this back off again — the link is a `<button>` so that a
      // press on it copies, and `all:unset` is what makes the menu's buttons
      // stop looking like the page's.
      '.dsh-rc button.dsh-rc-link{{display:block;width:100%;box-sizing:border-box;' +
      'margin:0 0 14px;padding:8px 10px;border-radius:8px;' +
      'border:1px solid var(--rc-line);background:none;color:var(--rc-muted);' +
      'font:12px/1.4 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;' +
      'font-weight:400;text-align:center;word-break:break-all;cursor:pointer;' +
      'user-select:text;-webkit-user-select:text}}' +
      '.dsh-rc button.dsh-rc-link:hover{{background:var(--rc-hover);color:var(--rc-fg)}}' +
      '.dsh-rc button.dsh-rc-go{{background:var(--rc-accent);color:#fff}}' +
      '.dsh-rc button.dsh-rc-go:hover{{filter:brightness(1.08)}}' +
      '.dsh-rc button.dsh-rc-quiet{{padding:5px 10px;font-size:13px;' +
      'color:var(--rc-muted);border:1px solid var(--rc-line)}}' +
      '.dsh-rc-spacer{{flex:1}}' +
      '.dsh-rc-patch{{display:flex;gap:9px;align-items:flex-start;margin:0 0 14px;' +
      'cursor:pointer;-webkit-user-select:none;user-select:none}}' +
      '.dsh-rc-patch input{{margin:2px 0 0;flex:0 0 auto;width:14px;height:14px;' +
      'accent-color:var(--rc-accent);cursor:pointer}}' +
      '.dsh-rc-patch span{{flex:1;min-width:0;font-size:12px;line-height:1.5}}' +
      '.dsh-rc-patch small{{display:block;color:var(--rc-muted);font-size:11px;' +
      'line-height:1.5;margin-top:2px}}';
    document.head.appendChild(style);

    root = make('div', 'dsh-rc');
    sheet = make('div', 'dsh-rc-sheet', root);
    head = make('h2', 'dsh-rc-head', sheet);
    lede = make('p', 'dsh-rc-lede', sheet);
    code = make('div', 'dsh-rc-code', sheet);
    link = make('button', 'dsh-rc-link', sheet);
    link.type = 'button';
    status = make('p', 'dsh-rc-status', sheet);
    list = make('ul', 'dsh-rc-list', sheet);
    note = make('p', 'dsh-rc-note', sheet);
    patch = make('label', 'dsh-rc-patch', sheet);
    foot = make('div', 'dsh-rc-foot', sheet);
    document.body.appendChild(root);

    // The card is modal to the page, so the scrim closes it — as the plugin
    // panel's does, and as Escape does below.
    root.addEventListener('mousedown', function (event) {{
      if (event.target === root) close();
    }});
    document.addEventListener('keydown', function (event) {{
      if (event.key === 'Escape' && shown()) {{
        event.preventDefault();
        close();
      }}
    }}, true);

    link.addEventListener('click', function () {{
      var text = link.getAttribute('data-url') || '';
      if (!text) return;
      // Best effort: `writeText` needs a secure context, and dsh's page is
      // plain HTTP on loopback. The fallback is the selection, which is what
      // the user would have made by hand.
      try {{
        navigator.clipboard.writeText(text);
        link.textContent = TEXT.copied || '';
        setTimeout(function () {{ link.textContent = text; }}, 1200);
        return;
      }} catch (e) {{}}
      try {{
        var range = document.createRange();
        range.selectNodeContents(link);
        var selection = window.getSelection();
        selection.removeAllRanges();
        selection.addRange(range);
      }} catch (e) {{}}
    }});
  }}

  function shown() {{
    return !!root && root.classList.contains('dsh-rc-shown');
  }}

  function close() {{
    if (root) root.classList.remove('dsh-rc-shown');
    signal('remote-close');
  }}

  function button(parent, text, className, onclick) {{
    var node = make('button', className || '', parent);
    node.type = 'button';
    node.textContent = text;
    node.addEventListener('click', onclick);
    return node;
  }}

  /** The matrix, as cells. Rebuilt only when the URL changed: the card
   *  updates every time a device connects, and redrawing nine hundred
   *  elements to say the same thing would make the code flicker. */
  function paintCode(url) {{
    if (code.getAttribute('data-for') === url) return;
    code.setAttribute('data-for', url);
    code.textContent = '';

    var matrix = url ? qrMatrix(url) : null;
    code.style.display = matrix ? 'grid' : 'none';
    if (!matrix) return;

    code.style.gridTemplateColumns = 'repeat(' + matrix.length + ',1fr)';
    // One fragment rather than nine hundred insertions into a live tree.
    var fragment = document.createDocumentFragment();
    for (var r = 0; r < matrix.length; r++) {{
      for (var c = 0; c < matrix.length; c++) {{
        var cell = document.createElement('i');
        if (matrix[r][c]) cell.className = 'on';
        fragment.appendChild(cell);
      }}
    }}
    code.appendChild(fragment);
  }}

  function at(seconds) {{
    try {{
      return new Date(seconds * 1000).toLocaleTimeString([], {{
        hour: '2-digit', minute: '2-digit'
      }});
    }} catch (e) {{
      return '';
    }}
  }}

  /** One string with one `{{}}` in it. The labels come from Rust, where both
   *  languages live, and both halves of a pair have the same hole in them. */
  function fill(template, value) {{
    return String(template || '').replace('{{}}', value);
  }}

  window.__dshRemote = function (json) {{
    var view;
    try {{
      view = JSON.parse(json);
    }} catch (e) {{
      return;
    }}
    if (!root) build();
    TEXT = view.text || {{}};

    paint(root);
    head.textContent = TEXT.title || '';
    lede.textContent = TEXT.lede || '';

    paintCode(view.url || '');
    link.style.display = view.url ? 'block' : 'none';
    if (view.url) {{
      link.setAttribute('data-url', view.url);
      link.textContent = view.url;
      link.title = TEXT.copy || '';
    }}

    var count = (view.devices || []).length;
    status.className = 'dsh-rc-status' + (view.error ? ' dsh-rc-bad' : '');
    status.textContent = view.error
      ? (TEXT.unavailable || '') + ' ' + view.error
      : (count ? fill(TEXT.connected, count) : TEXT.waiting || '');

    list.textContent = '';
    list.style.display = count ? 'block' : 'none';
    (view.devices || []).forEach(function (device) {{
      var row = make('li', '', list);
      var who = make('div', 'dsh-rc-who', row);
      make('b', '', who).textContent = device.label;
      make('span', '', who).textContent =
        device.address + ' · ' + fill(TEXT.since, at(device.since));
      button(row, TEXT.kick || '', 'dsh-rc-quiet', function () {{
        signal('remote-kick?id=' + encodeURIComponent(device.id));
      }});
    }});

    note.style.display = view.hint ? 'block' : 'none';
    note.textContent = view.hint || '';

    // A real checkbox inside a real label, so the whole row is the hit area
    // and a keyboard reaches it without this card inventing focus handling.
    patch.textContent = '';
    var box = make('input', '', patch);
    box.type = 'checkbox';
    box.checked = !!view.stylePatch;
    box.addEventListener('change', function () {{
      signal('remote-style?on=' + (box.checked ? '1' : '0'));
    }});
    var why = make('span', '', patch);
    why.textContent = TEXT.stylePatch || '';
    make('small', '', why).textContent = TEXT.stylePatchWhy || '';

    foot.textContent = '';
    if (count) {{
      button(foot, TEXT.kickAll || '', 'dsh-rc-quiet', function () {{
        signal('remote-kick-all');
      }});
    }}
    make('div', 'dsh-rc-spacer', foot);
    button(foot, TEXT.close || '', 'dsh-rc-go', close);

    root.classList.add('dsh-rc-shown');
  }};

  window.__dshRemoteHide = function () {{
    if (root) root.classList.remove('dsh-rc-shown');
  }};
}})();"#
    )
}

/// `function qrMatrix(text)`: the encoder, as JavaScript.
///
/// Its own function rather than inline in [`script`] so that the two hundred
/// lines of tables and finite-field arithmetic are one thing that can be read —
/// or skipped — on its own, and so that the card above stays a card.
///
/// Verified against a reference decoder rather than by eye: every byte length
/// from 1 to 213 was encoded here and read back with `jsQR`, which exercises
/// all ten versions, both character-count widths and every block layout in the
/// table. A QR code that is subtly wrong still looks exactly like a QR code, so
/// looking at one proves nothing.
fn encoder() -> &'static str {
    r#"  // Byte mode, error correction level M, versions 1 to 10.
  //
  // Level M is the middle of the four: about 15% of the code can be damaged and
  // still read, which is what a phone camera at an angle in a dim room needs,
  // without pushing the module count up to where a laptop screen stops
  // resolving them.
  function qrMatrix(text) {
    // Per version: total data codewords, error correction codewords per block,
    // and the block sizes the data is cut into. ISO/IEC 18004 table 9, the
    // level M rows.
    var SPEC = [
      [16, 10, [16]], [28, 16, [28]], [44, 26, [44]], [64, 18, [32, 32]],
      [86, 24, [43, 43]], [108, 16, [27, 27, 27, 27]], [124, 18, [31, 31, 31, 31]],
      [154, 22, [38, 38, 39, 39]], [182, 22, [36, 36, 36, 37, 37]],
      [216, 26, [43, 43, 43, 43, 44]]
    ];
    // Alignment pattern centres, table E.1.
    var ALIGN = [
      [], [6, 18], [6, 22], [6, 26], [6, 30],
      [6, 34], [6, 22, 38], [6, 24, 42], [6, 26, 46], [6, 28, 50]
    ];

    // UTF-8 by hand. `TextEncoder` is not reliably there in an old WKWebView,
    // and a pairing URL is short enough that this costs nothing.
    var data = [];
    for (var i = 0; i < text.length; i++) {
      var point = text.charCodeAt(i);
      if (point < 0x80) data.push(point);
      else if (point < 0x800) data.push(0xc0 | (point >> 6), 0x80 | (point & 0x3f));
      else data.push(0xe0 | (point >> 12), 0x80 | ((point >> 6) & 0x3f), 0x80 | (point & 0x3f));
    }

    var version = 0;
    for (var v = 0; v < SPEC.length; v++) {
      var countBits = v + 1 < 10 ? 8 : 16;
      if (SPEC[v][0] * 8 >= 4 + countBits + data.length * 8) { version = v + 1; break; }
    }
    // Past version 10 at this level. The caller draws nothing rather than
    // drawing a code that cannot hold what it claims to.
    if (!version) return null;

    var totalData = SPEC[version - 1][0];
    var ecPerBlock = SPEC[version - 1][1];
    var blockSizes = SPEC[version - 1][2];

    // ------------------------------------------------------------- the bits --
    var bits = [];
    function put(value, width) {
      for (var b = width - 1; b >= 0; b--) bits.push((value >> b) & 1);
    }

    put(4, 4);                                   // byte mode
    put(data.length, version < 10 ? 8 : 16);
    for (var d = 0; d < data.length; d++) put(data[d], 8);

    var capacity = totalData * 8;
    for (var t = 0; t < 4 && bits.length < capacity; t++) bits.push(0);
    while (bits.length % 8) bits.push(0);

    var codewords = [];
    for (var c = 0; c < bits.length; c += 8) {
      var byte = 0;
      for (var k = 0; k < 8; k++) byte = (byte << 1) | bits[c + k];
      codewords.push(byte);
    }
    // The two pad bytes the standard names, alternating from the first one.
    for (var p = 0; codewords.length < totalData; p++) codewords.push(p % 2 ? 0x11 : 0xec);

    // ------------------------------------------ Reed-Solomon over GF(256) --
    // The field the standard uses: x^8 + x^4 + x^3 + x^2 + 1.
    var EXP = new Array(512), LOG = new Array(256);
    for (var x = 0, value = 1; x < 255; x++) {
      EXP[x] = value;
      LOG[value] = x;
      value <<= 1;
      if (value & 0x100) value ^= 0x11d;
    }
    for (var x2 = 255; x2 < 512; x2++) EXP[x2] = EXP[x2 - 255];

    function multiply(a, b) {
      return a && b ? EXP[LOG[a] + LOG[b]] : 0;
    }

    // The product of (x - a^0)…(x - a^(n-1)), built one root at a time.
    var generator = [1];
    for (var g = 0; g < ecPerBlock; g++) {
      var next = new Array(generator.length + 1);
      for (var n = 0; n < next.length; n++) next[n] = 0;
      for (var gi = 0; gi < generator.length; gi++) {
        next[gi] ^= generator[gi];
        next[gi + 1] ^= multiply(generator[gi], EXP[g]);
      }
      generator = next;
    }

    function remainder(block) {
      var buffer = block.slice();
      for (var pad = 0; pad < ecPerBlock; pad++) buffer.push(0);
      for (var bi = 0; bi < block.length; bi++) {
        var lead = buffer[bi];
        if (!lead) continue;
        for (var gj = 0; gj < generator.length; gj++) {
          buffer[bi + gj] ^= multiply(generator[gj], lead);
        }
      }
      return buffer.slice(block.length);
    }

    var dataBlocks = [], ecBlocks = [], at = 0;
    for (var bs = 0; bs < blockSizes.length; bs++) {
      var block = codewords.slice(at, at + blockSizes[bs]);
      at += blockSizes[bs];
      dataBlocks.push(block);
      ecBlocks.push(remainder(block));
    }

    // Interleaved: one codeword from each block in turn, data first, then the
    // check bytes the same way. This is what spreads a smudge across every
    // block instead of destroying one of them.
    var stream = [], widest = 0;
    for (var w = 0; w < blockSizes.length; w++) widest = Math.max(widest, blockSizes[w]);
    for (var col = 0; col < widest; col++) {
      for (var bl = 0; bl < dataBlocks.length; bl++) {
        if (col < dataBlocks[bl].length) stream.push(dataBlocks[bl][col]);
      }
    }
    for (var ec = 0; ec < ecPerBlock; ec++) {
      for (var eb = 0; eb < ecBlocks.length; eb++) stream.push(ecBlocks[eb][ec]);
    }

    // ---------------------------------------------------------- the modules --
    var size = version * 4 + 17;
    var modules = [], reserved = [], placed = [];
    for (var r = 0; r < size; r++) {
      modules.push(new Array(size));
      reserved.push(new Array(size));
      placed.push(new Array(size));
      for (var c2 = 0; c2 < size; c2++) {
        modules[r][c2] = 0;
        reserved[r][c2] = 0;
        placed[r][c2] = 0;
      }
    }

    // `reserved` is the whole bookkeeping: a module a function pattern owns is
    // one the data skips and the mask leaves alone.
    function set(row, column, dark) {
      modules[row][column] = dark ? 1 : 0;
      reserved[row][column] = 1;
    }

    // The three corner squares, with the white separator around them.
    function finder(row, column) {
      for (var dr = -1; dr <= 7; dr++) {
        for (var dc = -1; dc <= 7; dc++) {
          var rr = row + dr, cc = column + dc;
          if (rr < 0 || rr >= size || cc < 0 || cc >= size) continue;
          var ring = Math.max(Math.abs(dr - 3), Math.abs(dc - 3));
          set(rr, cc, dr >= 0 && dr <= 6 && dc >= 0 && dc <= 6 && ring !== 2);
        }
      }
    }

    finder(0, 0);
    finder(0, size - 7);
    finder(size - 7, 0);

    for (var timing = 8; timing < size - 8; timing++) {
      set(6, timing, timing % 2 === 0);
      set(timing, 6, timing % 2 === 0);
    }

    var centres = ALIGN[version - 1], last = centres.length - 1;
    for (var ai = 0; ai <= last; ai++) {
      for (var aj = 0; aj <= last; aj++) {
        // The three corners a finder already occupies, and only those.
        // Skipping on "already reserved" would drop the patterns that sit on
        // the timing row, which from version 7 is most of them — and a code
        // missing those scans as nothing at all.
        if ((ai === 0 && aj === 0) || (ai === 0 && aj === last) || (ai === last && aj === 0)) {
          continue;
        }
        for (var dr2 = -2; dr2 <= 2; dr2++) {
          for (var dc2 = -2; dc2 <= 2; dc2++) {
            set(centres[ai] + dr2, centres[aj] + dc2,
              Math.max(Math.abs(dr2), Math.abs(dc2)) !== 1);
          }
        }
      }
    }

    // The module that is dark in every code, then the format areas, reserved
    // now and written once a mask has been chosen.
    set(size - 8, 8, true);
    for (var f = 0; f < 9; f++) {
      if (!reserved[8][f]) set(8, f, false);
      if (!reserved[f][8]) set(f, 8, false);
    }
    for (var f2 = 0; f2 < 8; f2++) {
      if (!reserved[8][size - 1 - f2]) set(8, size - 1 - f2, false);
      if (!reserved[size - 1 - f2][8]) set(size - 1 - f2, 8, false);
    }

    // From version 7 the version itself is written twice, BCH(18,6) coded.
    if (version >= 7) {
      var rest = version;
      for (var vb = 0; vb < 12; vb++) rest = (rest << 1) ^ ((rest >> 11) * 0x1f25);
      var info = (version << 12) | rest;
      for (var vi = 0; vi < 18; vi++) {
        var vbit = (info >> vi) & 1;
        set(Math.floor(vi / 3), size - 11 + (vi % 3), vbit);
        set(size - 11 + (vi % 3), Math.floor(vi / 3), vbit);
      }
    }

    // ------------------------------------------------- the data, in a zigzag --
    // Two columns at a time from the bottom right, alternating direction, and
    // stepping over the vertical timing line at column 6.
    var index = 0, upward = true;
    for (var column = size - 1; column > 0; column -= 2) {
      if (column === 6) column = 5;
      for (var step = 0; step < size; step++) {
        var row2 = upward ? size - 1 - step : step;
        for (var side = 0; side < 2; side++) {
          var column2 = column - side;
          if (reserved[row2][column2]) continue;
          // Past the end of the stream are the remainder bits, which are zero
          // and are masked like any other module.
          var bit = index < stream.length * 8
            ? (stream[index >> 3] >> (7 - (index & 7))) & 1
            : 0;
          index++;
          modules[row2][column2] = bit;
          placed[row2][column2] = 1;
        }
      }
      upward = !upward;
    }

    // ------------------------------------------------------------- the mask --
    function masked(pattern, row, column) {
      switch (pattern) {
        case 0: return (row + column) % 2 === 0;
        case 1: return row % 2 === 0;
        case 2: return column % 3 === 0;
        case 3: return (row + column) % 3 === 0;
        case 4: return (Math.floor(row / 2) + Math.floor(column / 3)) % 2 === 0;
        case 5: return ((row * column) % 2) + ((row * column) % 3) === 0;
        case 6: return (((row * column) % 2) + ((row * column) % 3)) % 2 === 0;
        default: return (((row + column) % 2) + ((row * column) % 3)) % 2 === 0;
      }
    }

    // BCH(15,5) over the two level bits and the three mask bits, then the fixed
    // XOR that keeps an all-zero format from being a valid one.
    function writeFormat(grid, pattern) {
      var value = pattern;            // level M is 0b00 in the high two bits
      var rest2 = value;
      for (var fb = 0; fb < 10; fb++) rest2 = (rest2 << 1) ^ ((rest2 >> 9) * 0x537);
      var format = ((value << 10) | rest2) ^ 0x5412;

      for (var fi = 0; fi < 15; fi++) {
        var bit2 = (format >> fi) & 1;
        if (fi < 6) grid[fi][8] = bit2;
        else if (fi === 6) grid[7][8] = bit2;
        else if (fi === 7) grid[8][8] = bit2;
        else if (fi === 8) grid[8][7] = bit2;
        else grid[8][14 - fi] = bit2;

        if (fi < 8) grid[8][size - 1 - fi] = bit2;
        else grid[size - 15 + fi][8] = bit2;
      }
      grid[size - 8][8] = 1;
    }

    // The four penalty rules of the standard. Their only job is to pick the
    // mask that leaves the fewest large blocks, long runs and finder-like
    // sequences, all of which are what a scanner trips over.
    function penalty(grid) {
      var score = 0, dark = 0;

      for (var line = 0; line < size; line++) {
        for (var axis = 0; axis < 2; axis++) {
          var run = 1;
          for (var cell = 1; cell < size; cell++) {
            var here = axis ? grid[cell][line] : grid[line][cell];
            var before = axis ? grid[cell - 1][line] : grid[line][cell - 1];
            if (here === before) run++;
            else {
              if (run >= 5) score += run - 2;
              run = 1;
            }
          }
          if (run >= 5) score += run - 2;
        }
      }

      for (var br = 0; br < size - 1; br++) {
        for (var bc = 0; bc < size - 1; bc++) {
          var corner = grid[br][bc];
          if (corner === grid[br][bc + 1] && corner === grid[br + 1][bc] &&
              corner === grid[br + 1][bc + 1]) {
            score += 3;
          }
        }
      }

      var LOOKALIKE = [1, 0, 1, 1, 1, 0, 1, 0, 0, 0, 0];
      for (var pl = 0; pl < size; pl++) {
        for (var pc = 0; pc + 11 <= size; pc++) {
          var rowLike = true, columnLike = true;
          for (var pi = 0; pi < 11; pi++) {
            if (grid[pl][pc + pi] !== LOOKALIKE[pi]) rowLike = false;
            if (grid[pc + pi][pl] !== LOOKALIKE[pi]) columnLike = false;
          }
          if (rowLike) score += 40;
          if (columnLike) score += 40;
        }
      }

      for (var dr3 = 0; dr3 < size; dr3++) {
        for (var dc3 = 0; dc3 < size; dc3++) if (grid[dr3][dc3]) dark++;
      }
      score += Math.floor(Math.abs((dark * 100) / (size * size) - 50) / 5) * 10;

      return score;
    }

    var best = null, bestScore = Infinity;
    for (var mask = 0; mask < 8; mask++) {
      var candidate = [];
      for (var cr = 0; cr < size; cr++) {
        candidate.push(modules[cr].slice());
        for (var cc = 0; cc < size; cc++) {
          if (placed[cr][cc] && masked(mask, cr, cc)) candidate[cr][cc] ^= 1;
        }
      }
      writeFormat(candidate, mask);

      var score2 = penalty(candidate);
      if (score2 < bestScore) {
        bestScore = score2;
        best = candidate;
      }
    }

    return best;
  }"#
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every brace in the card's body is doubled for `format!`, and one that is
    /// not is a syntax error this app would ship without noticing: the script is
    /// evaluated by the webview, not by the compiler, and a card that throws at
    /// document start is a button that does nothing at all. Balance is the
    /// cheapest check that catches an unescaped `{` having eaten a line.
    ///
    /// `cargo test -- --ignored dumps_the_scripts` writes this out beside the
    /// others for `node --check`, which is the thorough version of the same
    /// worry; see [`crate::dialog`].
    #[test]
    fn the_script_is_balanced() {
        let mut depth = 0i32;
        for character in script().chars() {
            match character {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            assert!(depth >= 0, "a closing brace with nothing open");
        }
        assert_eq!(depth, 0, "unbalanced braces in the card script");
    }

    /// The names Rust calls into. Spelled differently at either end, the card
    /// is a script that loads and is never asked to draw anything.
    #[test]
    fn rust_and_the_card_agree_on_the_hooks() {
        let script = script();
        assert!(script.contains("window.__dshRemote = function (json)"));
        assert!(script.contains("window.__dshRemoteHide = function ()"));
    }

    /// And the verbs it signals back on, each of which
    /// [`crate::controls::action`] has to recognise.
    #[test]
    fn every_verb_the_card_signals_is_one_the_app_answers() {
        let script = script();
        for verb in [
            "remote-close",
            "remote-kick?id=",
            "remote-kick-all",
            "remote-style?on=",
        ] {
            assert!(script.contains(verb), "the card signals {verb}");
        }

        for (url, recognised) in [
            ("dsh-window://remote", true),
            ("dsh-window://remote-close", true),
            ("dsh-window://remote-kick?id=d1", true),
            ("dsh-window://remote-kick-all", true),
            // An id is the whole payload; without one there is nothing to kick.
            ("dsh-window://remote-kick", false),
            ("dsh-window://remote-kick?id=", false),
            ("dsh-window://remote-style?on=1", true),
            ("dsh-window://remote-style?on=0", true),
            // The box sends the state it is in, so a signal with no state is
            // not a request to flip — it is a signal that lost its payload.
            ("dsh-window://remote-style", false),
        ] {
            let parsed = crate::controls::action(&url.parse().unwrap());
            assert_eq!(parsed.is_some(), recognised, "{url}");
        }
    }

    /// Which way round the switch is, held separately: a card that sent `on=0`
    /// and a desktop that read it as "on" would turn the patch on every time it
    /// was switched off, and every test above would still pass.
    #[test]
    fn the_switch_says_which_way_it_was_moved() {
        use crate::controls::Action;
        for (url, expected) in [
            ("dsh-window://remote-style?on=1", true),
            ("dsh-window://remote-style?on=0", false),
        ] {
            match crate::controls::action(&url.parse().unwrap()) {
                Some(Action::RemoteStyle(on)) => assert_eq!(on, expected, "{url}"),
                _ => panic!("{url} did not parse as a style switch"),
            }
        }
    }

    /// The tables the encoder is right or wrong by. Both are transcribed from
    /// the standard, and a digit out of place in either produces a code that
    /// looks perfect and scans as nothing — so the two rows most likely to be
    /// mistyped are pinned here.
    ///
    /// What actually proves the encoder is the decode pass described in
    /// [`encoder`]: every byte length from 1 to 213, read back with a reference
    /// decoder.
    #[test]
    fn the_encoder_carries_the_tables_it_needs() {
        let encoder = encoder();
        // Version 8 at level M: two blocks of 38 and two of 39. The first
        // version whose blocks are not all the same size, and the one an
        // interleaving bug shows up on.
        assert!(encoder.contains("[154, 22, [38, 38, 39, 39]]"));
        // Version 7's alignment centres, the first version with more than one
        // pattern per row.
        assert!(encoder.contains("[6, 22, 38]"));
        // The field polynomial and the two BCH generators. Every one of them is
        // a magic number, and a wrong one is a code no scanner will read.
        assert!(encoder.contains("0x11d"), "GF(256) modulus");
        assert!(encoder.contains("0x537"), "format BCH generator");
        assert!(encoder.contains("0x1f25"), "version BCH generator");
        assert!(encoder.contains("0x5412"), "format mask");
    }

    /// The firewall line is for the one state worth warning about. A machine
    /// Windows has already been asked about gets nothing — there is nothing for
    /// the user to do, and a card that always warns is a card nobody reads.
    #[test]
    fn only_an_unasked_firewall_is_worth_a_line() {
        assert!(firewall_hint(crate::remote::firewall::Firewall::Unasked).is_some());
        assert_eq!(
            firewall_hint(crate::remote::firewall::Firewall::Known),
            None
        );
        assert_eq!(
            firewall_hint(crate::remote::firewall::Firewall::Unknown),
            None
        );
    }

    /// Every label the script reads off the payload has to be in the table Rust
    /// sends, or a button on the card is drawn with no words on it.
    #[test]
    fn every_label_the_card_reads_is_one_rust_sends() {
        let script = script();
        let text = text();
        let sent = text.as_object().expect("a table of labels");

        for key in sent.keys() {
            assert!(
                script.contains(&format!("TEXT.{key}")),
                "nothing on the card reads TEXT.{key}"
            );
        }

        for read in script.split("TEXT.").skip(1) {
            let key: String = read
                .chars()
                .take_while(|character| character.is_ascii_alphanumeric())
                .collect();
            assert!(sent.contains_key(&key), "TEXT.{key} is never sent");
        }
    }
}
