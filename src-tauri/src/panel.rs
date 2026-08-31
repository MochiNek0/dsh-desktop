//! The plugin panel, drawn over whatever page the window is showing.
//!
//! It is injected, the way the window controls in [`crate::controls`] are and
//! for the same reason: the page underneath belongs to dsh, not to us. Drawing
//! the panel on top of it — rather than sending the window off to a page of our
//! own and back again — is what keeps opening the plugin list from reloading
//! the harness. Two page loads is what looking at a list used to cost.
//!
//! The one reload left is the one an install earns. pnpm rewrites the profile
//! directory the running server read its plugins out of, so dsh comes down for
//! it and has to be started again afterwards; see [`crate::plugins`].
//!
//! What it draws is [`crate::plugins::listing`]. What it sends back are the
//! three verbs the loading page used to send — install, done, directory — down
//! the same cancelled-navigation channel everything else in this window uses.
//! Rust pushes into it with `window.eval`; see `Splash` in main.rs.

/// The script that draws it, injected into every document the window loads.
///
/// Nothing is built until the panel is first shown: on most launches it never
/// is, and a document this app does not own is not somewhere to leave a card
/// and a stylesheet lying around unasked.
pub fn script() -> String {
    let scheme = crate::controls::SCHEME;
    let font = crate::controls::FONT;

    // One object rather than a literal per string: they are pasted into
    // JavaScript, and a label is one apostrophe away from being a syntax error
    // that takes the panel with it. The two languages live inline here the way
    // they do everywhere else; see `i18n`.
    let labels = serde_json::json!({
        "title": t!("插件", "Plugins"),
        "ledeFirst": t!(
            "这几个是推荐的插件。现在装，或者以后从标题栏菜单里再回来都行。安装时 dsh 会先停下，装完再自动启动它。",
            "These are the plugins we suggest. Install them now, or come back from the titlebar menu later. dsh stops while an install runs and starts again afterwards."
        ),
        "ledeBack": t!(
            "这些插件装进 dsh 的 web profile，和在终端里执行 dsh plugin add 是同一件事。安装时 dsh 会先停下，装完再自动启动它。",
            "These install into dsh’s web profile — the same thing `dsh plugin add` does in a terminal. dsh stops while an install runs and starts again afterwards."
        ),
        "hint": t!(
            "也可以直接填一个 pnpm 认识的包：包名，或 github:owner/repo。",
            "Or name anything pnpm understands: a package name, or github:owner/repo."
        ),
        "empty": t!(
            "没有读到预设插件清单，下面还是可以自己填一个。",
            "The preset list could not be read; the box below still works."
        ),
        "fix": t!("修复", "Fix"),
        "have": t!("已安装", "Installed"),
        // The group headings. `recommended` and `authored` are the two the
        // shipped list uses; `other` catches a section name the list invents
        // that this panel has no heading for. See `section` in plugins.rs.
        "groupRecommended": t!("推荐", "Recommended"),
        "groupAuthored": t!("作者创建", "By the author"),
        "groupOther": t!("其他", "More"),
        "repo": t!("查看仓库", "View repository"),
        "allIn": t!(
            "推荐的插件都装上了。要装别的，用下面的输入框。",
            "Everything on the suggested list is installed. The box below takes anything else."
        ),
        "directory": t!("打开插件目录", "Open the plugin folder"),
        "skip": t!("跳过", "Skip"),
        "back": t!("返回", "Back"),
        "install": t!("安装选中的插件", "Install selected"),
        "remove": t!("卸载选中的", "Remove selected"),
        "removing": t!("正在卸载…", "Removing…"),
        "pick": t!("先勾一个，或者填一个包名。", "Tick one, or name one."),
        "running": t!(
            "正在安装，这一步会下载依赖，可能要几分钟…",
            "Installing. This downloads dependencies and can take a few minutes…"
        ),
        "backToDsh": t!("回到 dsh", "Back to dsh"),
        "leaveIt": t!("先不装了", "Leave it"),
        "more": t!("继续装别的", "Install more"),
        "retry": t!("重试", "Try again"),
    })
    .to_string();

    format!(
        r#"(function () {{
  // The top document only. Drawn anywhere else this is a panel the size of an
  // iframe, with buttons that answer through a navigation no iframe can make.
  // See `controls`.
  if (window.top !== window.self) return;
  if (window.__dshPluginPanel) return;
  window.__dshPluginPanel = true;

  var TEXT = {labels};

  var root = null, lede, list, held, heldList, heldCount, hint, spec, log, note;
  var dir, drop, leave, install;

  // The whole channel back to Rust; see controls.rs. The navigation is
  // cancelled there, so the page under the panel stays exactly where it is.
  function signal(verb) {{
    window.location.href = '{scheme}://' + verb;
  }}

  function make(tag, className, parent) {{
    var node = document.createElement(tag);
    if (className) node.className = className;
    if (parent) parent.appendChild(node);
    return node;
  }}

  function button(parent, text, onclick) {{
    var node = make('button', '', parent);
    node.type = 'button';
    node.textContent = text;
    node.addEventListener('click', onclick);
    return node;
  }}

  function shown() {{
    return !!root && root.classList.contains('dsh-pp-shown');
  }}

  function done() {{
    signal('plugins-done');
  }}

  function say(kind, text) {{
    note.className = 'dsh-pp-note' + (kind ? ' dsh-pp-' + kind : '');
    note.textContent = text || '';
  }}

  function chip(kind, text) {{
    var tag = make('span', 'dsh-pp-chip dsh-pp-' + kind);
    tag.textContent = text;
    return tag;
  }}

  /** Keep a card's selected styling in step with the box inside it. */
  function mark(line, box) {{
    line.classList.toggle('dsh-pp-on', box.checked);
  }}

  function row(preset) {{
    var line = make('label', 'dsh-pp-row dsh-pp-card-row');

    var box = make('input', '', line);
    box.type = 'checkbox';
    box.value = preset.id;
    box.checked = !!preset.checked;

    // The tick the user actually sees. The real checkbox stays in the DOM --
    // it is what `ticked()` reads and what keyboard focus lands on -- but it is
    // taken out of the layout by the stylesheet and drawn as this instead.
    var mock = make('span', 'dsh-pp-tick', line);
    mock.setAttribute('aria-hidden', 'true');

    var body = make('div', 'dsh-pp-body', line);
    var name = make('div', 'dsh-pp-name', body);
    name.appendChild(document.createTextNode(preset.name));
    if (preset.fix) name.appendChild(chip('fix', TEXT.fix));
    make('div', 'dsh-pp-desc', body).textContent = preset.description || '';

    if (preset.url) {{
      // Opened in the user's browser: every navigation out of this app's own
      // origins goes there. See `is_ours` in main.rs.
      var link = make('a', 'dsh-pp-repo', body);
      link.href = preset.url;
      link.textContent = TEXT.repo + ' \u2197';
      // The card is a <label>, so a click anywhere in it toggles the box --
      // including on this link, which would tick the card on the way out to
      // the browser. The link is the one part that is not a tick target.
      link.addEventListener('click', function (event) {{
        event.stopPropagation();
      }});
    }}

    box.addEventListener('change', function () {{
      mark(line, box);
    }});
    mark(line, box);

    return line;
  }}

  /** The heading over one group, with how many are in it. */
  function heading(parent, text, count) {{
    var head = make('div', 'dsh-pp-group', parent);
    make('span', '', head).textContent = text;
    make('span', 'dsh-pp-count', head).textContent = count;
    return head;
  }}

  /** One plugin that is already in the profile, and so can be taken out. */
  function holding(item) {{
    var line = make('label', 'dsh-pp-row dsh-pp-card-row');

    var box = make('input', '', line);
    box.type = 'checkbox';
    box.value = item.name;
    // The remove button only appears while something is ticked: a button that
    // takes things away should not sit on the panel with nothing to act on.
    box.addEventListener('change', function () {{
      mark(line, box);
      offerRemoval();
    }});

    var mock = make('span', 'dsh-pp-tick', line);
    mock.setAttribute('aria-hidden', 'true');

    var body = make('div', 'dsh-pp-body', line);
    make('div', 'dsh-pp-name', body).textContent = item.label || item.name;
    // The package name, where that is not already the label, and the range pnpm
    // recorded. Between them they say which thing this actually is.
    var detail = [item.label === item.name ? '' : item.name, item.version]
      .filter(Boolean)
      .join(' ');
    if (detail) make('div', 'dsh-pp-desc', body).textContent = detail;

    return line;
  }}

  function offerRemoval() {{
    drop.hidden = !heldList.querySelector('input:checked');
  }}

  function ticked(where) {{
    return [].slice.call(where.querySelectorAll('input:checked')).map(function (box) {{
      return box.value;
    }});
  }}

  /** Hand the panel over to a pnpm run: the lists go, the log arrives. */
  function running(message) {{
    install.disabled = true;
    leave.disabled = true;
    drop.hidden = true;
    dir.hidden = true;
    log.textContent = '';
    root.classList.remove('dsh-pp-logged');
    root.classList.add('dsh-pp-running');
    say('', message);
  }}

  /** Both lists, out of one listing. The log and the line above it stay put. */
  function fill(data) {{
    var presets = data.presets || [];
    var installed = data.installed || [];

    list.textContent = '';

    // The order groups are drawn in, and what each is called. Anything whose
    // section is not one of these falls into `other`, which keeps a preset
    // visible even when the shipped list names a group this panel predates.
    var groups = [
      {{ key: 'recommended', label: TEXT.groupRecommended }},
      {{ key: 'authored', label: TEXT.groupAuthored }},
      {{ key: 'other', label: TEXT.groupOther }}
    ];
    var known = {{ recommended: 1, authored: 1, other: 1 }};

    var bucketed = {{}};
    presets.forEach(function (preset) {{
      var key = known[preset.section] ? preset.section : 'other';
      (bucketed[key] = bucketed[key] || []).push(preset);
    }});

    groups.forEach(function (group) {{
      var members = bucketed[group.key];
      if (!members || !members.length) return;

      // Only worth a heading when there is more than one group on screen --
      // a single heading over the whole list is a label for nothing.
      heading(list, group.label, members.length);
      members.forEach(function (preset) {{
        list.appendChild(row(preset));
      }});
    }});

    if (!presets.length) {{
      make('p', 'dsh-pp-lede', list).textContent = installed.length ? TEXT.allIn : TEXT.empty;
    }}

    heldList.textContent = '';
    installed.forEach(function (item) {{
      heldList.appendChild(holding(item));
    }});
    heldCount.textContent = installed.length;
    held.hidden = !installed.length;
    offerRemoval();
  }}

  function start() {{
    var ids = ticked(list);
    var typed = spec.value.trim();

    if (!ids.length && !typed) {{
      say('bad', TEXT.pick);
      return;
    }}

    running(TEXT.running);
    signal(
      'plugins-install?ids=' + encodeURIComponent(ids.join(',')) +
        '&spec=' + encodeURIComponent(typed)
    );
  }}

  function drops() {{
    var names = ticked(heldList);
    if (!names.length) return;

    running(TEXT.removing);
    signal('plugins-remove?names=' + encodeURIComponent(names.join(',')));
  }}

  // dsh's theme is the page's, not the window's, and the panel is drawn over
  // the page. Read exactly the way the titlebar reads it -- see controls.rs,
  // which explains why the media query is only the fallback.
  function paint(card) {{
    var media = window.matchMedia('(prefers-color-scheme:dark)');

    function dark() {{
      if (document.body.hasAttribute('data-ds-dark-theme')) return true;
      var declared = getComputedStyle(document.documentElement).colorScheme || '';
      var light = declared.indexOf('light') !== -1;
      var night = declared.indexOf('dark') !== -1;
      return night !== light ? night : media.matches;
    }}

    function repaint() {{
      card.classList.toggle('dsh-pp-dark', dark());
    }}

    repaint();
    var watch = new MutationObserver(repaint);
    watch.observe(document.documentElement, {{
      attributes: true, attributeFilter: ['style', 'class', 'data-theme']
    }});
    watch.observe(document.body, {{
      attributes: true, attributeFilter: ['style', 'class', 'data-ds-dark-theme']
    }});
    media.addEventListener('change', repaint);
  }}

  function build() {{
    var style = document.createElement('style');
    style.textContent =
      // Under the titlebar's two layers, so minimise, maximise and close stay
      // reachable while the panel is up -- and padded clear of the strip they
      // sit in.
      '.dsh-pp{{position:fixed;inset:0;z-index:2147483644;display:none;' +
      'align-items:center;justify-content:center;box-sizing:border-box;' +
      'padding:calc(var(--dsh-titlebar-height,36px) + 12px) 16px 20px;' +
      'background:rgba(18,18,22,.34);-webkit-backdrop-filter:blur(3px);' +
      'backdrop-filter:blur(3px);font-size:14px;line-height:1.6;' +
      'user-select:none;-webkit-user-select:none;' +
      '--pp-bg:#fff;--pp-fg:#1a1a1a;--pp-muted:#6b7280;--pp-line:#e5e7eb;' +
      '--pp-accent:#4d6bfe;--pp-danger:#b42318;--pp-ok:#12805c;' +
      // `hover` is the border a card takes before it is ticked, and `tint` the
      // wash it takes after: both sit between the line colour and the accent,
      // and both have to be given per theme rather than mixed from the accent.
      '--pp-soft:#f7f8fa;--pp-fix:#b54708;' +
      '--pp-hover:#c3cbe6;--pp-tint:rgba(77,107,254,.07)}}' +
      '.dsh-pp.dsh-pp-dark{{background:rgba(0,0,0,.5);' +
      '--pp-bg:#17171d;--pp-fg:#ececf1;--pp-muted:#9aa0ac;--pp-line:#2b2b34;' +
      '--pp-danger:#f97066;--pp-ok:#3ccb9a;' +
      '--pp-soft:rgba(255,255,255,.04);--pp-fix:#f0a35e;' +
      '--pp-hover:#454554;--pp-tint:rgba(77,107,254,.16)}}' +
      '.dsh-pp.dsh-pp-shown{{display:flex}}' +
      // The page underneath has styles of its own for every tag this is built
      // out of. The family is the one thing worth taking back wholesale; the
      // sizes are written onto each piece below.
      '.dsh-pp,.dsh-pp *{{box-sizing:border-box;font-family:{font}}}' +
      '.dsh-pp-card{{display:flex;flex-direction:column;min-height:0;' +
      'max-height:100%;width:min(760px,100%);padding:22px 24px;' +
      'border-radius:14px;background:var(--pp-bg);color:var(--pp-fg);' +
      'box-shadow:0 24px 64px rgba(0,0,0,.32),0 0 0 .5px var(--pp-line)}}' +
      '.dsh-pp-card h1{{font-size:17px;font-weight:600;line-height:1.4;margin:0 0 6px}}' +
      '.dsh-pp-lede{{margin:0 0 16px;color:var(--pp-muted);font-size:13px}}' +
      '.dsh-pp-list{{flex:1 1 auto;min-height:0;overflow:auto;margin:0 -4px;padding:0 4px}}' +
      // The heading over a group, and the count beside it.
      '.dsh-pp-group{{display:flex;align-items:center;gap:8px;margin:14px 2px 9px;' +
      'font-size:12px;font-weight:600;letter-spacing:.02em;color:var(--pp-muted)}}' +
      '.dsh-pp-group:first-child{{margin-top:2px}}' +
      '.dsh-pp-count{{flex:none;min-width:18px;height:18px;padding:0 6px;' +
      'display:inline-flex;align-items:center;justify-content:center;' +
      'border-radius:999px;background:var(--pp-soft);border:1px solid var(--pp-line);' +
      'font-size:11px;font-weight:600;color:var(--pp-muted)}}' +
      // A card rather than a row: it lifts on hover and takes an accent border
      // when it is ticked, so a selection is visible without reading the box.
      '.dsh-pp-row{{position:relative;display:flex;gap:11px;align-items:flex-start;' +
      'padding:13px 15px;border:1px solid var(--pp-line);border-radius:12px;' +
      'margin-bottom:9px;background:var(--pp-bg);cursor:pointer;' +
      'transition:border-color .15s,background .15s,box-shadow .15s,transform .15s}}' +
      '.dsh-pp-row:hover{{border-color:var(--pp-hover);background:var(--pp-soft);' +
      'transform:translateY(-1px);box-shadow:0 4px 14px rgba(0,0,0,.07)}}' +
      '.dsh-pp-row.dsh-pp-on{{border-color:var(--pp-accent);background:var(--pp-tint)}}' +
      '.dsh-pp-row.dsh-pp-on:hover{{border-color:var(--pp-accent)}}' +
      // The real checkbox: still focusable and still what `ticked()` reads,
      // but out of the layout so the drawn tick can take its place.
      '.dsh-pp-row input{{position:absolute;opacity:0;width:1px;height:1px;' +
      'margin:0;pointer-events:none}}' +
      '.dsh-pp-tick{{flex:none;width:17px;height:17px;margin-top:2px;border-radius:6px;' +
      'border:1.5px solid var(--pp-line);background:var(--pp-bg);position:relative;' +
      'transition:border-color .15s,background .15s}}' +
      '.dsh-pp-row:hover .dsh-pp-tick{{border-color:var(--pp-hover)}}' +
      '.dsh-pp-row.dsh-pp-on .dsh-pp-tick{{border-color:var(--pp-accent);' +
      'background:var(--pp-accent)}}' +
      // The check itself, drawn as a rotated corner rather than a glyph: a
      // character would inherit whatever the page underneath does to fonts.
      '.dsh-pp-tick:after{{content:"";position:absolute;left:5px;top:1.5px;' +
      'width:4px;height:8px;border:solid #fff;border-width:0 2px 2px 0;' +
      'transform:rotate(45deg) scale(0);transition:transform .15s}}' +
      '.dsh-pp-row.dsh-pp-on .dsh-pp-tick:after{{transform:rotate(45deg) scale(1)}}' +
      // Keyboard focus has to land somewhere visible, and the box it lands on
      // is invisible by now.
      '.dsh-pp-row input:focus-visible ~ .dsh-pp-tick{{outline:2px solid var(--pp-accent);' +
      'outline-offset:2px}}' +
      '.dsh-pp-body{{min-width:0;flex:1}}' +
      '.dsh-pp-name{{font-weight:600;display:flex;align-items:center;gap:7px;flex-wrap:wrap}}' +
      '.dsh-pp-desc{{color:var(--pp-muted);font-size:13px;margin-top:3px}}' +
      // A badge, not a bare URL. The repository addresses in the list run long
      // enough to wrap twice and say nothing the name has not already said.
      '.dsh-pp-repo{{display:inline-flex;align-items:center;margin-top:9px;' +
      'padding:3px 9px;border-radius:999px;border:1px solid var(--pp-line);' +
      'background:var(--pp-soft);color:var(--pp-muted);text-decoration:none;' +
      'font-size:11.5px;transition:color .15s,border-color .15s}}' +
      '.dsh-pp-repo:hover{{color:var(--pp-accent);border-color:var(--pp-accent)}}' +
      // Nothing here is load-bearing; a user who asked for less movement can
      // have the same panel without any of it.
      '@media (prefers-reduced-motion:reduce){{.dsh-pp-row,.dsh-pp-tick,' +
      '.dsh-pp-tick:after,.dsh-pp-repo{{transition:none}}' +
      '.dsh-pp-row:hover{{transform:none}}}}' +
      '.dsh-pp-chip{{font-size:11px;font-weight:500;line-height:1.5;padding:0 7px;' +
      'border-radius:999px;border:1px solid currentColor}}' +
      '.dsh-pp-chip.dsh-pp-fix{{color:var(--pp-fix)}}' +
      '.dsh-pp-chip.dsh-pp-installed{{color:var(--pp-ok)}}' +
      // Not the scroller the list above it is: what is installed is a short
      // list by nature, and it should not compete for the card's height with
      // the one being chosen from.
      '.dsh-pp-held{{flex:none;max-height:26vh;overflow:auto;margin:14px -4px 0;padding:0 4px}}' +
      '.dsh-pp-held[hidden]{{display:none}}' +
      // The same heading as a group's, so "Installed" reads as one more group
      // rather than as a different kind of thing.
      '.dsh-pp-sub{{display:flex;align-items:center;gap:8px;margin:0 2px 9px;' +
      'font-size:12px;font-weight:600;letter-spacing:.02em;color:var(--pp-muted)}}' +
      '.dsh-pp-hint{{display:block;margin-top:12px;font-size:12px;color:var(--pp-muted)}}' +
      '.dsh-pp-spec{{width:100%;margin-top:4px;padding:8px 11px;' +
      'border:1px solid var(--pp-line);border-radius:8px;background:var(--pp-bg);' +
      'color:var(--pp-fg);font:13px ui-monospace,Consolas,monospace;' +
      'user-select:text;-webkit-user-select:text}}' +
      '.dsh-pp-spec:focus{{outline:2px solid var(--pp-accent);outline-offset:-1px}}' +
      '.dsh-pp-log{{display:none;flex:1 1 auto;min-height:120px;margin:12px 0 0;' +
      'overflow:auto;padding:12px 14px;border:1px solid var(--pp-line);' +
      'border-radius:8px;background:var(--pp-soft);' +
      'font:12px/1.5 ui-monospace,Consolas,monospace;color:var(--pp-muted);' +
      'white-space:pre-wrap;word-break:break-word;' +
      'user-select:text;-webkit-user-select:text}}' +
      // While it runs the log has the card to itself; once it is over the list
      // comes back above it and the log keeps what it printed, because on a
      // failure that output is the whole of what the user has to go on.
      '.dsh-pp-running .dsh-pp-list,.dsh-pp-running .dsh-pp-hint,' +
      '.dsh-pp-running .dsh-pp-held{{display:none}}' +
      '.dsh-pp-running .dsh-pp-log,.dsh-pp-logged .dsh-pp-log{{display:block}}' +
      '.dsh-pp-logged .dsh-pp-log{{flex:none;max-height:30vh}}' +
      // Wraps, and the buttons keep to their own line once it does. A note is
      // usually a few words — "Plugins installed." — and sits beside the
      // buttons. But a failure explains itself in a paragraph, and a paragraph
      // sharing one row with four buttons squeezes both into an unreadable
      // column, which is what a release-age refusal did. So a note that is
      // marked bad takes the full width and pushes the buttons below it;
      // `justify-content` then keeps them at the end of their own row.
      '.dsh-pp-foot{{display:flex;flex-wrap:wrap;align-items:center;' +
      'justify-content:flex-end;gap:10px;margin-top:14px}}' +
      '.dsh-pp-note{{flex:1 1 auto;min-width:0;font-size:12px;line-height:1.55;' +
      'color:var(--pp-muted)}}' +
      '.dsh-pp-note.dsh-pp-ok{{color:var(--pp-ok)}}' +
      // A whole row of its own, and a little breathing room from the buttons
      // that follow it. Keyed off the class the failure already sets, so no
      // `:has()` — WebKitGTK on the older Linux this ships for predates it.
      '.dsh-pp-note.dsh-pp-bad{{flex:1 1 100%;margin-bottom:2px;' +
      'color:var(--pp-danger)}}' +
      '.dsh-pp button{{all:unset;display:inline-flex;align-items:center;' +
      'justify-content:center;height:32px;padding:0 15px;border-radius:8px;' +
      'border:1px solid var(--pp-line);cursor:pointer;font-size:13px;' +
      'line-height:1;color:var(--pp-fg);white-space:nowrap}}' +
      '.dsh-pp button:hover{{background:var(--pp-soft)}}' +
      '.dsh-pp button.dsh-pp-primary{{background:var(--pp-accent);' +
      'border-color:var(--pp-accent);color:#fff}}' +
      '.dsh-pp button.dsh-pp-primary:hover{{filter:brightness(1.08)}}' +
      // Outlined rather than filled: it should stand apart from the primary
      // action without being the loudest thing on the panel.
      '.dsh-pp button.dsh-pp-danger{{color:var(--pp-danger);' +
      'border-color:var(--pp-danger)}}' +
      '.dsh-pp button.dsh-pp-danger:hover{{background:var(--pp-danger);color:#fff}}' +
      '.dsh-pp button[disabled]{{opacity:.45;cursor:default;pointer-events:none}}' +
      '.dsh-pp button[hidden]{{display:none}}';
    document.head.appendChild(style);

    root = make('div', 'dsh-pp');
    var card = make('div', 'dsh-pp-card', root);
    make('h1', '', card).textContent = TEXT.title;
    lede = make('p', 'dsh-pp-lede', card);
    list = make('div', 'dsh-pp-list', card);

    held = make('div', 'dsh-pp-held', card);
    var haveHead = make('div', 'dsh-pp-sub', held);
    make('span', '', haveHead).textContent = TEXT.have;
    heldCount = make('span', 'dsh-pp-count', haveHead);
    heldList = make('div', '', held);

    hint = make('label', 'dsh-pp-hint', card);
    make('span', '', hint).textContent = TEXT.hint;
    spec = make('input', 'dsh-pp-spec', hint);
    spec.type = 'text';
    spec.spellcheck = false;
    spec.placeholder = 'github:owner/repo';

    log = make('pre', 'dsh-pp-log', card);

    var foot = make('div', 'dsh-pp-foot', card);
    note = make('span', 'dsh-pp-note', foot);
    dir = button(foot, TEXT.directory, function () {{
      signal('plugins-directory');
    }});
    dir.hidden = true;
    drop = button(foot, TEXT.remove, drops);
    drop.className = 'dsh-pp-danger';
    drop.hidden = true;
    leave = button(foot, TEXT.back, done);
    install = button(foot, TEXT.install, start);
    install.className = 'dsh-pp-primary';

    // The way out that is not at the far end of the card. Ignored while an
    // install runs, which is exactly when the button it stands in for is
    // disabled: pnpm is mid-write, and there is nothing to go back to yet.
    document.addEventListener('keydown', function (event) {{
      if (event.key === 'Escape' && shown() && !leave.disabled) done();
    }});

    // Painted before it is in the document, so it is never the wrong colour
    // for a frame.
    paint(root);
    document.body.appendChild(root);
  }}

  function ready(then) {{
    if (document.body) then();
    else document.addEventListener('DOMContentLoaded', then, {{ once: true }});
  }}

  // ------------------------------------------------- what Rust calls in --

  /** The listing, and which of the two ways this was opened. */
  window.__dshPlugins = function (listing, how) {{
    ready(function () {{
      if (!root) build();

      var data;
      try {{
        data = JSON.parse(listing);
      }} catch (error) {{
        data = {{ presets: [] }};
      }}

      var first = how === 'first';
      lede.textContent = first ? TEXT.ledeFirst : TEXT.ledeBack;
      fill(data);

      say('', '');
      log.textContent = '';
      dir.hidden = true;
      install.disabled = false;
      install.textContent = TEXT.install;
      leave.disabled = false;
      // A first launch is a step to skip; the menu is somewhere to come back
      // from.
      leave.textContent = first ? TEXT.skip : TEXT.back;
      root.classList.remove('dsh-pp-running', 'dsh-pp-logged');
      root.classList.add('dsh-pp-shown');
    }});
  }};

  /** Both lists again, after a run changed what they say. */
  window.__dshPluginLists = function (listing) {{
    if (!root) return;
    try {{
      fill(JSON.parse(listing));
    }} catch (error) {{
      // Leaving the lists as they were is the better of the two wrong answers.
    }}
  }};

  /** One line of the install's output, as it happens. */
  window.__dshPluginLog = function (line) {{
    if (!root) return;
    // Pinned to the bottom only while the user has not scrolled up to read
    // something -- an install prints a lot, and yanking the view back down
    // mid-sentence is how a log becomes unreadable.
    var following = log.scrollTop + log.clientHeight >= log.scrollHeight - 24;
    log.textContent += line + '\n';
    if (following) log.scrollTop = log.scrollHeight;
  }};

  /** How it ended. */
  window.__dshPluginDone = function (state, text) {{
    if (!root) return;
    var ok = state === 'ok';
    say(ok ? 'ok' : 'bad', text);
    leave.disabled = false;
    leave.textContent = ok ? TEXT.backToDsh : TEXT.leaveIt;
    install.disabled = false;
    install.textContent = ok ? TEXT.more : TEXT.retry;
    // The one thing an install will not do by itself: pnpm refuses to run a
    // package's build scripts until it is listed in the profile's
    // pnpm-workspace.yaml, and this is where that file lives. See plugins.rs.
    dir.hidden = ok;
    root.classList.remove('dsh-pp-running');
    root.classList.add('dsh-pp-logged');
  }};

  /** Put it away. Rust decides when: leaving means either going back to a dsh
   *  that is still running or starting one that is not. */
  window.__dshPluginHide = function () {{
    if (root) root.classList.remove('dsh-pp-shown');
  }};
}})();"#
    )
}
