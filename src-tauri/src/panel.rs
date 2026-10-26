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

use tauri::AppHandle;

/// The call [`relabel`] makes, and the one the script answers on.
///
/// A constant because the name is written in both places and the call is
/// guarded by `&&` — the guard is there because a document that has not
/// finished loading has no `__dsh*` on it, and it would swallow a typo just as
/// quietly.
pub(crate) const RELABEL: &str = "__dshPluginText";

/// Every string the panel draws, as the object the script indexes.
///
/// One object rather than a literal per string: they are pasted into
/// JavaScript, and a label is one apostrophe away from being a syntax error
/// that takes the panel with it. The two languages live inline here the way
/// they do everywhere else; see [`crate::i18n`].
///
/// A function rather than a literal inside [`script`], because the panel is
/// written twice: into the script when the window is built, and again by
/// [`relabel`] when dsh changes language under a window that is not going to
/// be built a second time. Two copies of these strings would be the drift
/// [`crate::i18n`] is arranged to prevent.
fn labels() -> String {
    serde_json::json!({
        "title": t!("插件", "Plugins"),
        "ledeFirst": t!(
            "这几个是推荐的插件。现在装，或者以后从标题栏菜单里再回来都行。安装时 dsh 会先停下，装完再自动启动它。",
            "These are the plugins we suggest. Install them now, or come back from the titlebar menu later. dsh stops while an install runs and starts again afterwards."
        ),
        // The fold the free-text box and the two local buttons live behind,
        // and what goes in the box once it is open.
        "other": t!("从包名或本地安装", "Install from a package name or this machine"),
        "specHint": t!("包名或 github:owner/repo", "Package name or github:owner/repo"),
        "empty": t!(
            "没有读到预设插件清单，下面还是可以自己填一个。",
            "The preset list could not be read; the box below still works."
        ),
        "fix": t!("修复", "Fix"),
        // Over the list: what narrows it once there are more plugins than fit
        // on the card at once. The tabs are counted, so each is one word.
        "search": t!("搜索插件…", "Search plugins…"),
        "tabInstalled": t!("已安装", "Installed"),
        "tabOffered": t!("可安装", "Available"),
        "tabUpdates": t!("有更新", "Updates"),
        "none": t!("没有符合条件的插件。", "No plugin matches."),
        "noUpdates": t!("没有待更新的插件。", "No updates waiting."),
        // The strip over the list while anything is behind, and the one press
        // that brings all of it forward in a single pnpm run.
        "updatesWaiting": t!("{} 个插件有新版本", "Updates available: {}"),
        "updateAll": t!("全部更新", "Update all"),
        "updating": t!("正在更新…", "Updating…"),
        // The thing a card can be that nobody wants to see: a name dsh still
        // loads that nothing installed. See `plugins::holdings`.
        "residue": t!("残留", "Residue"),
        // On the one button that changes the machine from a card rather than
        // from the bar at the bottom. The version it would move to is in the
        // label, because the button is an icon and the number is the whole
        // reason to press it.
        "update": t!("更新到 {}", "Update to {}"),
        "repo": t!("查看仓库", "View repository"),
        "allIn": t!(
            "推荐的插件都装上了。要装别的，用下面的输入框。",
            "Everything on the suggested list is installed. The box below takes anything else."
        ),
        "directory": t!("打开插件目录", "Open the plugin folder"),
        // The button beside the box, for a plugin that is a folder on this
        // machine rather than a package anywhere; see `plugins::pick_local`.
        "local": t!("文件夹…", "Folder…"),
        "tarball": t!(".tgz…", ".tgz…"),
        "unpick": t!("不装这个", "Leave it out"),
        // Under the lede, every time the panel is up: whatever goes in from
        // here runs as the user, and the box and the two buttons beside it will
        // take a package from anywhere.
        "trustLead": t!(
            "请确认插件来源可信。",
            "Only install plugins from sources you trust."
        ),
        "trust": t!(
            "插件在本机以你的权限运行，来源不明的插件可能损坏 DeepSeek Harness，或读取和泄露你的数据。",
            "A plugin runs on this machine with your permissions; one from an unknown source can break DeepSeek Harness, or read and leak your data."
        ),
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
    .to_string()
}

/// Put the panel into the language dsh has just switched to.
///
/// The dialogs in [`crate::dialog`] need nothing like this: their words come
/// from Rust at the moment they are asked, so they are already in whatever
/// language is current. This card's do not. The script below is an
/// initialization script — evaluated at every document load, but composed once,
/// when the window is built — so the labels pasted into it are the language the
/// app started in, and would stay it for as long as the app runs. Not just
/// while a document lasts: a reload runs the same string again.
///
/// So they are sent again, and the panel throws away whatever it has already
/// built. See `__dshPluginText`.
pub fn relabel(app: &AppHandle) {
    crate::controls::eval(
        app,
        &format!("window.{RELABEL} && window.{RELABEL}({})", labels()),
    );
}

/// The script that draws it, injected into every document the window loads.
///
/// Nothing is built until the panel is first shown: on most launches it never
/// is, and a document this app does not own is not somewhere to leave a card
/// and a stylesheet lying around unasked.
pub fn script() -> String {
    let scheme = crate::controls::SCHEME;
    let font = crate::controls::FONT;
    let maker = crate::controls::dom_make();
    let icons = crate::controls::lucide();
    let watcher = crate::controls::theme_watcher("dsh-pp-dark");
    let corners = crate::controls::corners(&["dsh-pp"]);
    let labels = labels();
    let relabel = RELABEL;

    format!(
        r#"(function () {{
  // The top document only. Drawn anywhere else this is a panel the size of an
  // iframe, with buttons that answer through a navigation no iframe can make.
  // See `controls`.
  if (window.top !== window.self) return;
  if (window.__dshPluginPanel) return;
  window.__dshPluginPanel = true;

  var TEXT = {labels};

  var root = null, sheet, lede, list, spec, log, note;
  // What narrows the list, and what it narrows. `rows` is every card `fill`
  // built, with the text a search is matched against worked out once there;
  // a keystroke only flips `hidden`, so a long list costs nothing to search.
  var bar, search, tabs = {{}}, rows = [], notice = null, none, ups, upsText;
  var query = '', filter = 'installed';
  // The folder chosen with the system picker, shown but never sent: Rust holds
  // the path and the install only says whether to use it. See `pick_local`.
  var local, picked, chosen = false;
  var dir, drop, leave, install;
  // Set when the language moved under a card that was already built; see
  // `__dshPluginText`.
  var stale = false;

  // The whole channel back to Rust; see controls.rs. The navigation is
  // cancelled there, so the page under the panel stays exactly where it is.
  function signal(verb) {{
    window.location.href = '{scheme}://' + verb;
  }}

{maker}

{icons}

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

  // What each mark on a card is, named for what it means here rather than for
  // the shape it happens to be. The shapes are `LUCIDE` above -- the set the
  // titlebar draws from too, for the same reason it exists at all: the page
  // underneath has an icon set of its own and none of it is reachable here.
  var ICONS = {{
    // The arrow leaving the box: this one goes out to the user's browser.
    repo: 'externalLink',
    search: 'search',
    // On the fold the free-text box sits behind; turns down when it opens.
    more: 'chevronRight',
    // What a plugin that ships no picture of its own wears in its place.
    plugin: 'puzzle',
    // The one inside a ticked box. A shape rather than a character, because a
    // glyph would inherit whatever the page underneath does to fonts.
    tick: 'check',
    // The same plugin come round at the next version. Not an arrow off a
    // line, which is what a file being uploaded wears, and this card uploads
    // nothing.
    update: 'refreshCw',
    trust: 'shieldAlert'
  }};

  /** One of them at the size a card draws its marks, which is all of them. */
  function svg(name) {{
    return lucide(name, 16);
  }}

  /** The picture at the head of a card: the plugin's own where it ships one,
   *  and otherwise the default one -- a puzzle piece on a colour worked out
   *  from its name, so two plugins without a picture are still told apart,
   *  and the same plugin wears the same tile every time the panel opens. */
  function face(parent, item) {{
    var tile = make('span', 'dsh-pp-icon', parent);
    tile.setAttribute('aria-hidden', 'true');

    function blank() {{
      tile.className = 'dsh-pp-icon dsh-pp-blank';
      var hash = 0;
      for (var i = 0; i < item.value.length; i++) {{
        hash = (hash * 31 + item.value.charCodeAt(i)) | 0;
      }}
      tile.style.setProperty('--pp-hue', String(Math.abs(hash) % 360));
      tile.innerHTML = lucide(ICONS.plugin, 18);
    }}

    if (!item.icon) {{
      blank();
      return;
    }}
    var picture = make('img', '', tile);
    picture.alt = '';
    picture.decoding = 'async';
    // A file that turns out not to be the image its name says still gets a
    // tile rather than the broken-image glyph.
    picture.addEventListener('error', blank);
    picture.src = item.icon;
  }}

  /** One pressable icon on a card's bottom line.
   *
   *  The card is a `<label>`, so a click anywhere inside it ticks the box --
   *  which is not what a press on one of these means. The click stops here
   *  instead, the way the repository link has always done. */
  function act(bar, icon, label, tag) {{
    var node = make(tag || 'button', 'dsh-pp-act', bar);
    if (!tag) node.type = 'button';
    node.innerHTML = svg(icon);
    node.title = label;
    node.setAttribute('aria-label', label);
    node.addEventListener('click', function (event) {{
      event.stopPropagation();
    }});
    return node;
  }}

  /** Keep a card's selected styling in step with the box inside it. */
  function mark(line, box) {{
    line.classList.toggle('dsh-pp-on', box.checked);
  }}

  /** One card, whichever half of the listing it came out of.
   *
   *  There is one kind of card. A preset and something already installed are
   *  the same thing looked at before and after -- the same name, package,
   *  description, repository and section -- and they used to be two lists,
   *  two builders and a heading apiece to say so. What actually differs is
   *  what a tick on the card would do: install it, or take it away. So that
   *  is what is drawn, as the tag at the end of the name.
   *
   *  Fed the shape `fill` normalises both halves into. */
  function card(item) {{
    var line = make('label', 'dsh-pp-row' + (item.installed ? ' dsh-pp-in' : ''));

    var box = make('input', '', line);
    box.type = 'checkbox';
    box.value = item.value;
    box.checked = !!item.checked;

    face(line, item);

    var body = make('div', 'dsh-pp-body', line);
    var name = make('div', 'dsh-pp-name', body);
    // Its own element rather than a bare text node, so the stylesheet can make
    // the name the part of this line that gives way when there is not room for
    // all of it. See `.dsh-pp-title`.
    var title = make('span', 'dsh-pp-title', name);
    title.textContent = item.name;
    // What is actually going on the machine -- the names are translated, so
    // `插件市场` on its own names nothing pnpm has heard of. On the hover
    // rather than on the line: it is looked up, not read. Search still finds it.
    if (item.detail) title.title = item.detail;

    // The only tags left on a row are the rare ones. "Installed" went with
    // the "all" tab: every row under a tab is the same kind of thing.
    if (item.fix) name.appendChild(chip('fix', TEXT.fix));
    if (item.stale) name.appendChild(chip('stale', TEXT.residue));

    // One line of it; the whole of a longer one is on the hover.
    if (item.description) {{
      var about = make('div', 'dsh-pp-desc', body);
      about.textContent = item.description;
      about.title = item.description;
    }}

    var bar = make('div', 'dsh-pp-tools', line);

    if (item.url) {{
      // A link rather than a button: the shell sends every navigation out of
      // this app's own origins to the user's browser, which is the whole of
      // what this has to do. See `is_ours` in main.rs.
      act(bar, ICONS.repo, TEXT.repo, 'a').href = item.url;
    }}

    // Only where there is one to do. A card with no newer release has no
    // button rather than a disabled one: a control that never does anything is
    // the one worth not drawing.
    //
    // It acts on the one plugin it is on, not on whatever happens to be ticked.
    // The tick means install or remove, and a button that quietly obeyed it
    // would update a card the pointer was nowhere near.
    if (item.update) {{
      var bump = act(bar, ICONS.update, TEXT.update.replace('{{}}', item.update.to));
      // The number on the button itself: it is the reason to press it.
      bump.classList.add('dsh-pp-bump');
      make('span', '', bump).textContent = item.update.to;
      bump.addEventListener('click', function () {{
        running(TEXT.updating);
        // The version goes with the name, and the update does not work without
        // it. `pnpm add <name>` on a package that is already a dependency whose
        // recorded range still admits what the lockfile pinned is a no-op —
        // pnpm answers "Already up to date" and rewrites nothing, so the button
        // ran, reported success, and left the old version in place. `outdated`
        // has no such scruples, so the card kept offering the same update.
        //
        // `@to` is what the button already says it will do, which makes this
        // the version the user was promised rather than whatever the registry
        // has by the time the click lands.
        signal('plugins-update?names=' +
          encodeURIComponent(item.value + '@' + item.update.to));
      }});
    }}

    // The tick the user actually sees, at the far end of the line. The real
    // checkbox stays in the DOM -- it is what `ticked()` reads and what
    // keyboard focus lands on -- but it is taken out of the layout by the
    // stylesheet and drawn as this instead.
    var mock = make('span', 'dsh-pp-tick', line);
    mock.innerHTML = svg(ICONS.tick);
    mock.setAttribute('aria-hidden', 'true');

    box.addEventListener('change', function () {{
      mark(line, box);
      offerRemoval();
    }});
    mark(line, box);

    return line;
  }}

  // Which cards a tick is read off, now that both halves are one list: the
  // ones being chosen from, and the ones being taken away.
  var OFFERED = '.dsh-pp-row:not(.dsh-pp-in) input:checked';
  var HELD = '.dsh-pp-in input:checked';

  function offerRemoval() {{
    // A button that takes things away should not sit on the panel with
    // nothing to act on.
    drop.hidden = !list.querySelector(HELD);
  }}

  function ticked(which) {{
    return [].slice.call(list.querySelectorAll(which)).map(function (box) {{
      return box.value;
    }});
  }}

  /** Hand the panel over to a pnpm run: the list goes, the log arrives. */
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

  /** The one list, out of the listing's two halves. The log and the line
   *  above it stay put.
   *
   *  `keep` is a redraw under a panel already in use -- the update check
   *  behind it landing, mostly, seconds after the user started ticking -- and
   *  keeps what they ticked and where they had scrolled to. Keyed by which half
   *  a card is in as well as by its value: a plugin an install or a removal
   *  has just moved across is a different card, and starts from its default. */
  function fill(data, keep) {{
    var presets = data.presets || [];
    var installed = data.installed || [];

    var was = {{}};
    if (keep) {{
      [].forEach.call(list.querySelectorAll('.dsh-pp-row input'), function (box) {{
        var half = box.parentNode.classList.contains('dsh-pp-in') ? 'in:' : 'out:';
        was[half + box.value] = box.checked;
      }});
    }}
    var scrolled = keep ? list.scrollTop : 0;

    list.textContent = '';
    rows = [];
    notice = null;

    // Recommended first, then the ones this app's author wrote, then a section
    // this panel has no order for. Nothing on a card says which is which any
    // more, so this order is what is left grouping them. `sort` is stable in both
    // engines this ships on, so the shipped order holds inside a section.
    var rank = {{ recommended: 0, authored: 1 }};
    var offered = presets.slice().sort(function (one, two) {{
      var a = rank[one.section], b = rank[two.section];
      return (a === undefined ? 2 : a) - (b === undefined ? 2 : b);
    }});

    // Both halves, as the one shape `card` draws. They differ in almost
    // nothing: a preset is named by the id pnpm is asked to install, an
    // installed plugin by the package name pnpm would be asked to remove, and
    // that value is what the tick on the card carries.
    var cards = offered.map(function (preset) {{
      return {{
        value: preset.id,
        name: preset.name,
        icon: preset.icon,
        detail: preset.package === preset.name ? '' : preset.package,
        description: preset.description,
        url: preset.url,
        section: preset.section,
        fix: preset.fix,
        checked: preset.checked,
        installed: false
      }};
    }}).concat(installed.map(function (item) {{
      return {{
        value: item.name,
        name: item.label || item.name,
        icon: item.icon,
        // The package name, where that is not already the label, and the
        // range pnpm recorded. Between them they say which thing this is.
        detail: [item.label === item.name ? '' : item.name, item.version]
          .filter(Boolean)
          .join(' '),
        description: item.description,
        url: item.url,
        section: item.section,
        installed: true,
        stale: !!item.stale,
        // Absent until the check behind the panel comes back, and absent for
        // good on a plugin that is current; see `look_for_updates` in main.rs.
        update: item.update || null
      }};
    }}));

    // At the end, which is where the installed ones are: nothing is offered
    // after the first thing that is already here.
    if (!presets.length) {{
      notice = make('p', 'dsh-pp-lede', list);
      notice.textContent = installed.length ? TEXT.allIn : TEXT.empty;
    }}

    // Built off to one side and put in at once, so a long list is one layout
    // rather than one per card.
    var batch = document.createDocumentFragment();
    cards.forEach(function (item) {{
      var key = (item.installed ? 'in:' : 'out:') + item.value;
      if (key in was) item.checked = was[key];
      var node = card(item);
      batch.appendChild(node);
      rows.push({{
        node: node,
        installed: item.installed,
        update: !!item.update,
        // What "update all" sends for this card; see the card's own button.
        spec: item.update ? item.value + '@' + item.update.to : '',
        hay: [item.name, item.value, item.detail, item.description]
          .join('\n')
          .toLowerCase()
      }});
    }});
    list.appendChild(batch);
    none = make('p', 'dsh-pp-lede dsh-pp-none', list);
    none.textContent = TEXT.none;

    count();
    narrow();
    list.scrollTop = scrolled;

    offerRemoval();
  }}

  /** Whether a card belongs under the tab that is up. */
  function under(row, which) {{
    if (which === 'installed') return row.installed;
    if (which === 'offered') return !row.installed;
    return which === 'updates' && row.update;
  }}

  /** The number on each tab, and the strip that updates everything at once.
   *
   *  The updates tab stays even at nothing: a tab that comes and goes moves
   *  the other three under the pointer, and "0" is an answer worth seeing. */
  function count() {{
    Object.keys(tabs).forEach(function (which) {{
      var n = rows.filter(function (row) {{ return under(row, which); }}).length;
      tabs[which].count.textContent = String(n);
      tabs[which].node.classList.toggle('dsh-pp-has', which === 'updates' && n > 0);
    }});
    var behind = pending().length;
    // Not over the plugins being offered: none of those has anything to update.
    ups.hidden = !behind || filter === 'offered';
    // And installing lives under the tab of things to install -- the button
    // and the free-text fold both. Elsewhere they are words with nothing to do.
    root.classList.toggle('dsh-pp-offering', filter === 'offered');
    install.hidden = filter !== 'offered';
    upsText.textContent = TEXT.updatesWaiting.replace('{{}}', String(behind));
    Object.keys(tabs).forEach(function (which) {{
      var on = which === filter;
      tabs[which].node.classList.toggle('dsh-pp-on', on);
      tabs[which].node.setAttribute('aria-selected', on ? 'true' : 'false');
    }});
  }}

  /** Show the cards the tab and the search both admit, and hide the rest.
   *
   *  Every word of the search has to be somewhere in the card -- name,
   *  package, or description -- in any order, so `搜索 免费` and `free search`
   *  both find the same plugin. Hiding rather than rebuilding is what keeps a
   *  ticked card ticked while it is out of view. */
  function narrow() {{
    var words = query.toLowerCase().split(/\s+/).filter(Boolean);
    var seen = 0;
    rows.forEach(function (row) {{
      var show = under(row, filter) && words.every(function (word) {{
        return row.hay.indexOf(word) !== -1;
      }});
      row.node.hidden = !show;
      if (show) seen++;
    }});
    // The sentence about the shipped list is about what can be installed, and
    // says nothing to a search or to the installed tab.
    var told = !!notice && !words.length && filter === 'offered';
    if (notice) notice.hidden = !told;
    none.hidden = !!seen || told;
    none.textContent = filter === 'updates' && !words.length ? TEXT.noUpdates : TEXT.none;
  }}

  /** Every update the list is offering, as the specs the cards would send. */
  function pending() {{
    return rows.filter(function (row) {{ return row.spec; }}).map(function (row) {{
      return row.spec;
    }});
  }}

  /** All of them in one pnpm run: dsh comes down once, not once per plugin. */
  function updateAll() {{
    var specs = pending();
    if (!specs.length) return;
    running(TEXT.updating);
    signal('plugins-update?names=' + encodeURIComponent(specs.join(',')));
  }}

  function pickTab(which) {{
    filter = which;
    count();
    narrow();
    list.scrollTop = 0;
  }}

  function start() {{
    var ids = ticked(OFFERED);
    var typed = spec.value.trim();

    if (!ids.length && !typed && !chosen) {{
      say('bad', TEXT.pick);
      return;
    }}

    running(TEXT.running);
    signal(
      'plugins-install?ids=' + encodeURIComponent(ids.join(',')) +
        '&spec=' + encodeURIComponent(typed) +
        (chosen ? '&local=1' : '')
    );
  }}

  function choose(path) {{
    chosen = !!path;
    picked.textContent = path || '';
    // Cut short with an ellipsis when it is long; the whole of it on hover.
    picked.title = path || '';
    local.hidden = !chosen;
    if (chosen) root.classList.add('dsh-pp-opened');
  }}

  function drops() {{
    var names = ticked(HELD);
    if (!names.length) return;

    running(TEXT.removing);
    signal('plugins-remove?names=' + encodeURIComponent(names.join(',')));
  }}

{watcher}

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
      '--pp-hover:#c3cbe6;--pp-tint:rgba(77,107,254,.07);' +
      '--pp-warn:#d97706;' +
      // What the islands sit on: a shade off the islands themselves, so they
      // stand apart without a border apiece.
      '--pp-canvas:#f1f2f5}}' +
      '.dsh-pp.dsh-pp-dark{{background:rgba(0,0,0,.5);--pp-canvas:#111115;' +
      '--pp-bg:#1c1c23;--pp-fg:#ececf1;--pp-muted:#9aa0ac;--pp-line:#2b2b34;' +
      '--pp-danger:#f97066;--pp-ok:#3ccb9a;' +
      '--pp-soft:rgba(255,255,255,.04);--pp-fix:#f0a35e;' +
      '--pp-hover:#454554;--pp-tint:rgba(77,107,254,.16);' +
      '--pp-warn:#f5b545}}' +
      '.dsh-pp.dsh-pp-shown{{display:flex}}' +
      // The page underneath has styles of its own for every tag this is built
      // out of. The family is the one thing worth taking back wholesale; the
      // sizes are written onto each piece below.
      '.dsh-pp,.dsh-pp *{{box-sizing:border-box;font-family:{font}}}' +
      // The corners are the other thing worth taking back wholesale: without
      // this the capsule on a chip, the cards and the tick were all drawn as
      // superellipses. See `controls::corners`, which says why.
      '{corners}' +
      // The card is the canvas; what is on it is grouped into islands, each a
      // rounded block of its own, and the gap between them is what separates
      // one group from the next.
      '.dsh-pp-card{{display:flex;flex-direction:column;gap:10px;min-height:0;' +
      'max-height:100%;width:min(760px,100%);padding:12px;' +
      'border-radius:20px;background:var(--pp-canvas);color:var(--pp-fg);' +
      'box-shadow:0 24px 64px rgba(0,0,0,.32),0 0 0 .5px var(--pp-line)}}' +
      '.dsh-pp-island{{flex:none;padding:14px 16px;border-radius:14px;' +
      'background:var(--pp-bg);box-shadow:0 1px 2px rgba(0,0,0,.04),' +
      '0 0 0 .5px var(--pp-line)}}' +
      // The list's island is the one that grows and scrolls.
      '.dsh-pp-main{{flex:1 1 auto;min-height:0;display:flex;' +
      'flex-direction:column;padding:12px}}' +
      // The heading sits on the canvas, not in an island: the title, and the
      // trust warning as one quiet line beside it.
      '.dsh-pp-head{{flex:none;display:flex;flex-wrap:wrap;align-items:center;' +
      'gap:4px 12px;padding:2px 6px 0}}' +
      '.dsh-pp-card h1{{flex:1;font-size:17px;font-weight:600;line-height:1.4;margin:0}}' +
      '.dsh-pp-lede{{margin:0;color:var(--pp-muted);font-size:13px}}' +
      '.dsh-pp-head .dsh-pp-lede{{flex-basis:100%}}' +
      '.dsh-pp-lede[hidden]{{display:none}}' +
      // The lead on the line, the reason on the hover. A shield in amber says
      // "mind this" without the red the failures wear.
      '.dsh-pp-trust{{display:inline-flex;gap:5px;align-items:center;' +
      'font-size:12px;color:var(--pp-muted);cursor:help}}' +
      '.dsh-pp-trust svg{{flex:none;width:13px;height:13px;color:var(--pp-warn)}}' +
      // The search and the tabs, on one line while there is room for both.
      '.dsh-pp-bar{{display:flex;flex-wrap:wrap;align-items:center;gap:8px 12px;' +
      'margin:0 0 8px}}' +
      // Over the list while anything is behind: how many, and one press for
      // all of them.
      '.dsh-pp-ups{{display:flex;align-items:center;gap:10px;margin:0 0 8px;' +
      'padding:6px 6px 6px 12px;border-radius:10px;background:var(--pp-tint);' +
      'font-size:12.5px;color:var(--pp-fg)}}' +
      '.dsh-pp-ups[hidden]{{display:none}}' +
      '.dsh-pp-ups>span{{flex:1;min-width:0}}' +
      '.dsh-pp .dsh-pp-ups button{{height:28px;padding:0 12px;gap:6px;' +
      'font-size:12.5px}}' +
      '.dsh-pp-ups svg{{display:block;width:14px;height:14px}}' +
      '.dsh-pp-find{{position:relative;flex:1 1 220px;min-width:0}}' +
      '.dsh-pp-find svg{{position:absolute;left:10px;top:50%;margin-top:-7px;' +
      'width:14px;height:14px;color:var(--pp-muted);pointer-events:none}}' +
      '.dsh-pp-find input{{width:100%;height:32px;padding:0 11px 0 31px;' +
      'border:1px solid transparent;border-radius:9px;background:var(--pp-soft);' +
      'color:var(--pp-fg);font-size:13px;user-select:text;-webkit-user-select:text}}' +
      '.dsh-pp-find input::placeholder{{color:var(--pp-muted)}}' +
      '.dsh-pp-find input:focus{{outline:2px solid var(--pp-accent);outline-offset:-1px}}' +
      // A segmented control: the tabs sit in one soft trough, and the one that
      // is up is lifted out of it.
      '.dsh-pp-tabs{{display:flex;flex:none;gap:2px;padding:2px;border-radius:9px;' +
      'background:var(--pp-soft);box-shadow:inset 0 0 0 1px var(--pp-line)}}' +
      // Written with the class twice over to outrank `.dsh-pp button` below.
      '.dsh-pp .dsh-pp-tab{{height:28px;padding:0 10px;gap:5px;border:0;' +
      'border-radius:7px;font-size:12.5px;color:var(--pp-muted)}}' +
      '.dsh-pp .dsh-pp-tab:hover{{background:none;color:var(--pp-fg)}}' +
      '.dsh-pp .dsh-pp-tab.dsh-pp-on{{background:var(--pp-bg);color:var(--pp-fg);' +
      'font-weight:600;box-shadow:0 1px 2px rgba(0,0,0,.08),0 0 0 1px var(--pp-line)}}' +
      '.dsh-pp .dsh-pp-tab:focus-visible{{outline:2px solid var(--pp-accent);' +
      'outline-offset:1px}}' +
      '.dsh-pp-count{{font-weight:500;font-size:11px;color:var(--pp-muted);' +
      'font-variant-numeric:tabular-nums}}' +
      '.dsh-pp-tab.dsh-pp-has .dsh-pp-count{{color:var(--pp-accent)}}' +
      '.dsh-pp-list{{flex:1 1 auto;min-height:0;overflow:auto;' +
      'display:flex;flex-direction:column;gap:2px;margin:0 -4px;padding:0 4px 2px}}' +
      // The line that stands in for an empty list is a sentence, not a card.
      '.dsh-pp-list>.dsh-pp-lede{{margin:2px 0 0}}' +
      '.dsh-pp-list>.dsh-pp-none{{padding:18px 0;text-align:center}}' +
      '.dsh-pp-list>[hidden]{{display:none}}' +
      // One plugin to a line, the way an extension list draws them: the
      // picture, what it is called and what it does, then what can be done to
      // it and the tick at the far end. No border apiece: the island is the
      // box, a row is lit under the pointer, and a ticked one keeps an accent
      // wash, so a selection is visible without reading the box.
      //
      // `content-visibility` is what keeps a long list cheap: a card scrolled
      // out of view is not laid out or painted, which is the part of a virtual
      // list worth having without the part that rebuilds the DOM under the
      // pointer. The intrinsic size is about a two-line card, so the scrollbar
      // is near enough right before each card has been seen once. An engine
      // that has neither property draws every card, as it did before.
      '.dsh-pp-row{{position:relative;display:flex;flex:none;gap:12px;' +
      'align-items:center;padding:9px 10px;' +
      'border-radius:10px;background:transparent;cursor:pointer;' +
      'content-visibility:auto;contain-intrinsic-size:auto 60px;' +
      'transition:background .15s}}' +
      '.dsh-pp-row[hidden]{{display:none}}' +
      '.dsh-pp-row:hover{{background:var(--pp-soft)}}' +
      '.dsh-pp-row.dsh-pp-on,.dsh-pp-row.dsh-pp-on:hover{{background:var(--pp-tint)}}' +
      // The plugin's own picture, drawn whole at the tile's size: most of them
      // are a tile already, corners and all.
      '.dsh-pp-icon{{flex:none;display:flex;align-items:center;justify-content:center;' +
      'width:34px;height:34px;border-radius:9px;overflow:hidden}}' +
      '.dsh-pp-icon img{{display:block;width:100%;height:100%;object-fit:contain}}' +
      // The stand-in for a plugin that ships no picture: a puzzle piece, on a
      // colour of its own. Pale on a light panel and deep on a dark one, so
      // no tile is ever louder than a real icon beside it.
      '.dsh-pp-blank{{background:hsl(var(--pp-hue),70%,93%);' +
      'color:hsl(var(--pp-hue),45%,40%)}}' +
      '.dsh-pp-dark .dsh-pp-blank{{background:hsl(var(--pp-hue),28%,22%);' +
      'color:hsl(var(--pp-hue),70%,76%)}}' +
      '.dsh-pp-blank svg{{display:block}}' +
      // The real checkbox: still focusable and still what `ticked()` reads,
      // but out of the layout so the drawn tick can take its place.
      '.dsh-pp-row input{{position:absolute;opacity:0;width:1px;height:1px;' +
      'margin:0;pointer-events:none}}' +
      '.dsh-pp-tick{{flex:none;display:flex;align-items:center;' +
      'justify-content:center;width:16px;height:16px;' +
      'border-radius:5px;border:1.5px solid var(--pp-line);' +
      'background:var(--pp-bg);transition:border-color .15s,background .15s}}' +
      '.dsh-pp-row:hover .dsh-pp-tick{{border-color:var(--pp-hover)}}' +
      '.dsh-pp-row.dsh-pp-on .dsh-pp-tick{{border-color:var(--pp-accent);' +
      'background:var(--pp-accent)}}' +
      // The check itself: centred by the box's own flexbox, which is the one
      // way it is certain to be. See `ICONS.tick`.
      // 3 rather than 2 on a 24-unit grid, which is what it takes to come out
      // the weight it was on the 16-unit one this icon used to be drawn on: a
      // tick 11 pixels across inside a filled box wants the heavier stroke.
      '.dsh-pp-tick svg{{width:11px;height:11px;color:#fff;stroke-width:3;' +
      'transform:scale(.4);opacity:0;' +
      'transition:transform .15s,opacity .15s}}' +
      '.dsh-pp-row.dsh-pp-on .dsh-pp-tick svg{{transform:scale(1);opacity:1}}' +
      // Keyboard focus has to land somewhere visible, and the box it lands on
      // is invisible by now.
      '.dsh-pp-row input:focus-visible ~ .dsh-pp-tick{{outline:2px solid var(--pp-accent);' +
      'outline-offset:2px}}' +
      '.dsh-pp-body{{min-width:0;flex:1}}' +
      '.dsh-pp-name{{font-weight:600;font-size:13.5px;line-height:1.35;' +
      'display:flex;align-items:center;gap:6px}}' +
      // The name is what gives way, and the tag at the end of it never does.
      // This line used to wrap instead, which put "installed" on a line of its
      // own under any name long enough -- a second line of card spent saying
      // what a tag beside the name had already said. So the name is ellipsised
      // here the way the package under it already is, and the tag keeps its
      // place at the end of the one line.
      '.dsh-pp-title{{min-width:0;overflow:hidden;text-overflow:ellipsis;' +
      'white-space:nowrap}}' +
      // One line, cut short with an ellipsis.
      '.dsh-pp-desc{{margin-top:1px;font-size:12px;line-height:1.5;' +
      'color:var(--pp-muted);white-space:nowrap;overflow:hidden;' +
      'text-overflow:ellipsis}}' +
      // What can be done to it, between the text and the tick.
      '.dsh-pp-tools{{flex:none;display:flex;align-items:center;gap:2px}}' +
      '.dsh-pp-tools:empty{{display:none}}' +
      '.dsh-pp-tools svg{{display:block;width:14px;height:14px}}' +
      // Written with the class twice over to outrank `.dsh-pp button` below,
      // which is the panel's ordinary button and nothing like this one.
      //
      // No box around them. Three outlined squares in the corner of a card
      // this small were three more rectangles than the card needed, and they
      // sat there whether or not anyone was going to press one. The glyph is
      // the button; the tint arrives under the pointer, which is the only
      // moment anything has to say "this can be pressed".
      '.dsh-pp .dsh-pp-act{{display:inline-flex;align-items:center;' +
      'justify-content:center;width:24px;height:24px;padding:0;' +
      'border:0;border-radius:7px;background:none;' +
      'color:var(--pp-muted);cursor:pointer;text-decoration:none;' +
      'transition:color .15s,background .15s}}' +
      '.dsh-pp .dsh-pp-act:hover{{color:var(--pp-accent);' +
      'background:var(--pp-tint)}}' +
      '.dsh-pp .dsh-pp-act:focus-visible{{outline:2px solid var(--pp-accent);' +
      'outline-offset:1px}}' +
      // The update, though, is a pill with the version on it: the one action
      // on a card worth seeing before the pointer gets there.
      '.dsh-pp .dsh-pp-act.dsh-pp-bump{{width:auto;padding:0 8px 0 6px;gap:4px;' +
      'color:var(--pp-accent);background:var(--pp-tint);font-size:11.5px;' +
      'font-weight:500;font-variant-numeric:tabular-nums}}' +
      '.dsh-pp .dsh-pp-act.dsh-pp-bump:hover{{background:var(--pp-accent);color:#fff}}' +
      '.dsh-pp .dsh-pp-act.dsh-pp-bump svg{{width:12px;height:12px}}' +
      // The repository link only under the pointer, or under keyboard focus:
      // a row at rest shows what the plugin is and nothing it can do.
      '.dsh-pp-row a.dsh-pp-act{{opacity:0}}' +
      '.dsh-pp-row:hover a.dsh-pp-act,.dsh-pp-row a.dsh-pp-act:focus-visible{{opacity:1}}' +
      // Nothing here is load-bearing; a user who asked for less movement can
      // have the same panel without any of it.
      '@media (prefers-reduced-motion:reduce){{.dsh-pp-row,.dsh-pp-tick,' +
      '.dsh-pp-tick svg,.dsh-pp .dsh-pp-act{{transition:none}}}}' +
      '.dsh-pp-chip{{flex:none;font-size:11px;font-weight:500;line-height:1.6;' +
      'padding:0 7px;border-radius:999px}}' +
      '.dsh-pp-chip.dsh-pp-fix{{color:var(--pp-fix);background:rgba(217,119,6,.12)}}' +
      // The same red the removal button wears, because it is the same verb:
      // this one is here to be taken away.
      '.dsh-pp-chip.dsh-pp-stale{{color:var(--pp-danger);background:rgba(220,50,40,.12)}}' +
      // The fold: a quiet line of text on the canvas, not a button.
      '.dsh-pp .dsh-pp-fold{{align-self:flex-start;height:auto;padding:2px 4px;' +
      'gap:4px;border:0;font-size:12.5px;color:var(--pp-muted)}}' +
      '.dsh-pp .dsh-pp-fold:hover{{background:none;color:var(--pp-fg)}}' +
      '.dsh-pp-fold svg{{display:block;width:14px;height:14px;transition:transform .15s}}' +
      '.dsh-pp-opened .dsh-pp-fold svg{{transform:rotate(90deg)}}' +
      '.dsh-pp-add{{display:none}}' +
      '.dsh-pp-opened .dsh-pp-add{{display:block}}' +
      '.dsh-pp:not(.dsh-pp-offering) .dsh-pp-fold,' +
      '.dsh-pp:not(.dsh-pp-offering) .dsh-pp-add{{display:none}}' +
      // The box and the two local buttons share a line while the card is wide
      // enough for the box to keep a useful width beside them, and the buttons
      // drop below it when it is not.
      '.dsh-pp-entry{{display:flex;flex-wrap:wrap;align-items:center;gap:8px}}' +
      '.dsh-pp-picks{{display:flex;flex:none;gap:8px}}' +
      '.dsh-pp-spec{{flex:1 1 220px;min-width:0;height:32px;padding:0 11px;' +
      'border:1px solid transparent;border-radius:9px;background:var(--pp-soft);' +
      'color:var(--pp-fg);font:13px ui-monospace,Consolas,monospace;' +
      'user-select:text;-webkit-user-select:text}}' +
      '.dsh-pp-spec:focus{{outline:2px solid var(--pp-accent);outline-offset:-1px}}' +
      '.dsh-pp-local{{display:flex;align-items:center;gap:10px;margin-top:8px}}' +
      '.dsh-pp-local[hidden]{{display:none}}' +
      '.dsh-pp-picked{{flex:1 1 auto;min-width:0;overflow:hidden;' +
      'text-overflow:ellipsis;white-space:nowrap;' +
      'font:12px ui-monospace,Consolas,monospace;color:var(--pp-fg)}}' +
      '.dsh-pp-log{{display:none;flex:1 1 auto;min-height:120px;margin:0;' +
      'overflow:auto;padding:12px 14px;box-shadow:0 0 0 .5px var(--pp-line);' +
      'border-radius:14px;background:var(--pp-bg);' +
      'font:12px/1.5 ui-monospace,Consolas,monospace;color:var(--pp-muted);' +
      'white-space:pre-wrap;word-break:break-word;' +
      'user-select:text;-webkit-user-select:text}}' +
      // While it runs the log has the card to itself; once it is over the list
      // comes back above it and the log keeps what it printed, because on a
      // failure that output is the whole of what the user has to go on.
      '.dsh-pp-running .dsh-pp-main,' +
      '.dsh-pp-running .dsh-pp-fold,' +
      '.dsh-pp-running .dsh-pp-add{{display:none}}' +
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
      'justify-content:flex-end;gap:10px;padding:2px 4px}}' +
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
    // Kept, because `discard` takes it away again.
    sheet = style;
    document.head.appendChild(sheet);

    root = make('div', 'dsh-pp');
    var card = make('div', 'dsh-pp-card', root);
    // On the card's canvas: the heading, the list's island, the fold with the
    // free-text island behind it, then the log and the buttons.
    var head = make('div', 'dsh-pp-head', card);
    make('h1', '', head).textContent = TEXT.title;
    var trust = make('span', 'dsh-pp-trust', head);
    trust.innerHTML = svg(ICONS.trust);
    make('span', '', trust).textContent = TEXT.trustLead;
    trust.title = TEXT.trust;
    lede = make('p', 'dsh-pp-lede', head);
    var main = make('div', 'dsh-pp-island dsh-pp-main', card);
    bar = make('div', 'dsh-pp-bar', main);
    var find = make('div', 'dsh-pp-find', bar);
    find.innerHTML = svg(ICONS.search);
    search = make('input', '', find);
    search.type = 'text';
    search.spellcheck = false;
    search.placeholder = TEXT.search;
    search.setAttribute('aria-label', TEXT.search);
    search.addEventListener('input', function () {{
      query = search.value.trim();
      narrow();
      list.scrollTop = 0;
    }});
    var strip = make('div', 'dsh-pp-tabs', bar);
    strip.setAttribute('role', 'tablist');
    tabs = {{}};
    [
      ['installed', TEXT.tabInstalled],
      ['offered', TEXT.tabOffered],
      ['updates', TEXT.tabUpdates]
    ].forEach(function (tab) {{
      var node = button(strip, '', function () {{
        pickTab(tab[0]);
      }});
      node.className = 'dsh-pp-tab';
      node.setAttribute('role', 'tab');
      node.setAttribute('data-tab', tab[0]);
      make('span', '', node).textContent = tab[1];
      tabs[tab[0]] = {{ node: node, count: make('span', 'dsh-pp-count', node) }};
    }});

    ups = make('div', 'dsh-pp-ups', main);
    ups.hidden = true;
    upsText = make('span', '', ups);
    var every = button(ups, '', updateAll);
    every.className = 'dsh-pp-primary';
    every.innerHTML = svg(ICONS.update);
    make('span', '', every).textContent = TEXT.updateAll;

    list = make('div', 'dsh-pp-list', main);

    // Folded away until asked for: most openings install off the list, and
    // a box and two buttons nobody is using were half of what the card said.
    var fold = button(card, '', function () {{
      root.classList.toggle('dsh-pp-opened');
      if (root.classList.contains('dsh-pp-opened')) spec.focus();
    }});
    fold.className = 'dsh-pp-fold';
    fold.innerHTML = svg(ICONS.more);
    make('span', '', fold).textContent = TEXT.other;

    var add = make('div', 'dsh-pp-island dsh-pp-add', card);
    var entry = make('div', 'dsh-pp-entry', add);
    spec = make('input', 'dsh-pp-spec', entry);
    spec.type = 'text';
    spec.spellcheck = false;
    spec.placeholder = TEXT.specHint;
    spec.setAttribute('aria-label', TEXT.specHint);
    // One unit, so a row too narrow for all three moves both buttons below
    // the box rather than leaving one of them beside it.
    var picks = make('div', 'dsh-pp-picks', entry);
    button(picks, TEXT.local, function () {{
      signal('plugins-pick');
    }});
    button(picks, TEXT.tarball, function () {{
      signal('plugins-pick?what=tgz');
    }});

    // What was chosen, on a line of its own under the row, and only while
    // something is.
    local = make('div', 'dsh-pp-local', add);
    picked = make('span', 'dsh-pp-picked', local);
    button(local, TEXT.unpick, function () {{
      choose(null);
    }});
    local.hidden = true;

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

    // Painted before it is in the document, so it is never the wrong colour
    // for a frame.
    paint(root);
    document.body.appendChild(root);
  }}

  /** Take the built card down, stylesheet and all, so the next opening builds
   *  it again. What a language switch leaves behind; see `__dshPluginText`. */
  function discard() {{
    if (sheet && sheet.parentNode) sheet.parentNode.removeChild(sheet);
    if (root && root.parentNode) root.parentNode.removeChild(root);
    sheet = null;
    root = null;
    // They were the old card's; the next one builds its own.
    rows = [];
    notice = null;
    // The card showing it has gone; the next one starts with nothing chosen.
    chosen = false;
    stale = false;
  }}

  // The way out that is not at the far end of the card. Ignored while an
  // install runs, which is exactly when the button it stands in for is
  // disabled: pnpm is mid-write, and there is nothing to go back to yet.
  //
  // Registered once, out here rather than in `build`: the card is built again
  // after a language switch, and a listener per build is a second handler
  // holding a card the user cannot see. `shown()` is false while there is no
  // card, which is what keeps the rest of the line from being read then.
  document.addEventListener('keydown', function (event) {{
    if (event.key !== 'Escape' || !shown()) return;
    // The search first: it is the smaller thing to be rid of, and the key a
    // user typing into a box reaches for to empty it.
    if (query && !root.classList.contains('dsh-pp-running')) {{
      search.value = '';
      query = '';
      narrow();
      return;
    }}
    if (!leave.disabled) done();
  }});

  function ready(then) {{
    if (document.body) then();
    else document.addEventListener('DOMContentLoaded', then, {{ once: true }});
  }}

  // ------------------------------------------------- what Rust calls in --

  /** The listing, and which of the two ways this was opened. */
  window.__dshPlugins = function (listing, how) {{
    ready(function () {{
      if (stale) discard();
      if (!root) build();

      var data;
      try {{
        data = JSON.parse(listing);
      }} catch (error) {{
        data = {{ presets: [] }};
      }}

      var first = how === 'first';
      // A first launch says what this is; coming back from the menu, the
      // user already knows.
      lede.textContent = TEXT.ledeFirst;
      lede.hidden = !first;
      // Each opening starts unfiltered, on what is already here -- or on what
      // could be, when that is nothing or this is the first launch.
      search.value = '';
      query = '';
      filter = first || !(data.installed || []).length ? 'offered' : 'installed';
      root.classList.toggle('dsh-pp-opened', chosen);
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

  /** The list again, after a run changed what it says. */
  window.__dshPluginLists = function (listing) {{
    if (!root) return;
    try {{
      fill(JSON.parse(listing), true);
    }} catch (error) {{
      // Leaving the list as it was is the better of the two wrong answers.
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

  /** What the folder picker came back with: a path to show, or why the folder
   *  it came back with will not do. */
  window.__dshPluginPicked = function (path, error) {{
    if (!root) return;
    if (path) {{
      choose(path);
      say('', '');
    }} else if (error) {{
      say('bad', error);
    }}
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

  /** The labels again, after dsh changed language; see `relabel` in panel.rs.
   *
   *  The card is built once and kept, so the words in one already built are the
   *  language it was built in. Rewriting them node by node would be a second
   *  list of which element holds which label, to keep in step with the first;
   *  the card is thrown away instead and the next opening builds it out of the
   *  new TEXT. Not thrown away here: this can land while the panel is on
   *  screen, and a card that vanishes under the user is worse than a card in
   *  the language they just left. */
  window.{relabel} = function (next) {{
    TEXT = next;
    stale = !!root;
  }};
}})();"#
    )
}

#[cfg(test)]
mod tests {
    /// The update button puts the version on the wire, not just the name.
    ///
    /// `dsh plugin add <name>` on a dependency whose recorded range already
    /// admits what the lockfile pinned is a no-op: pnpm says "Already up to
    /// date" and rewrites nothing, while `pnpm outdated` goes on reporting the
    /// newer release. So the button ran, reported success, drew itself again,
    /// and the plugin stayed where it was — silent in every direction. See
    /// [`crate::plugins::update`], which is the half that receives this.
    #[test]
    fn the_update_button_names_the_version_it_offers() {
        let script = super::script();

        assert!(
            script.contains("item.value + '@' + item.update.to"),
            "the update signal has to carry the version, or pnpm does nothing"
        );
    }
}
