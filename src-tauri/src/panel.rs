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
        // The icon that opens a card's description. The cards are laid out
        // several to a row now, and one has no room for the text itself.
        "about": t!("看介绍", "What it does"),
        // On the card, at the end of its name: what used to be a heading
        // over a list of its own. Both halves are one list now.
        "have": t!("已安装", "Installed"),
        // The third thing a card can be, and the one nobody wants to see: a
        // name dsh still loads that nothing installed. See `plugins::holdings`.
        "residue": t!("残留", "Residue"),
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
    let watcher = crate::controls::theme_watcher("dsh-pp-dark");
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

  var root = null, sheet, lede, list, hint, spec, log, note;
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

  // A path apiece, wrapped by `svg` below -- the same shape `controls` keeps
  // its titlebar glyphs in, and for the same reason: the page underneath has
  // an icon set of its own and none of it is reachable from here.
  var ICONS = {{
    about: '<circle cx="8" cy="8" r="6.2"/><path d="M8 7.4v3.7"/>' +
      '<path d="M8 5.1h.01"/>',
    // The arrow leaving the box: this one goes out to the user's browser.
    repo: '<path d="M9.7 3.2h3.1v3.1"/><path d="M12.8 3.2 8 8"/>' +
      '<path d="M11.2 9.6v2c0 .7-.5 1.2-1.2 1.2H4.4c-.7 0-1.2-.5-1.2-1.2V6' +
      'c0-.7.5-1.2 1.2-1.2h2"/>',
    // Filled, so a star this size reads as a star rather than as a scribble.
    recommended: '<path fill="currentColor" stroke="none" d="M8 2.4l1.75 3.54' +
      ' 3.85.55-2.8 2.7.66 3.85L8 11.27l-3.46 1.77.66-3.85-2.8-2.7 3.85-.55z"/>',
    authored: '<circle cx="8" cy="5.6" r="2.3"/>' +
      '<path d="M3.7 13a4.4 4.4 0 0 1 8.6 0"/>',
    other: '<path d="M8 2.7l5 2.6v5.4L8 13.3 3 10.7V5.3z"/>',
    // The one inside a ticked box. Drawn rather than typed, because a glyph
    // would inherit whatever the page underneath does to fonts -- and drawn
    // rather than built out of a rotated corner, which is what this replaces:
    // a corner has to be nudged into the middle of the box by hand, and it
    // was a pixel and a half high of it.
    tick: '<path d="M3.2 8.5l3.1 3.1 6.5-6.8"/>'
  }};

  function svg(shape) {{
    return '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" ' +
      'stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" ' +
      'aria-hidden="true">' + shape + '</svg>';
  }}

  /** Which list a preset came off: the mark it wears, and what that says.
   *
   *  This is what the group headings used to say. Three headings over four
   *  cards was most of the panel, and a card can carry the distinction
   *  itself. Anything else falls back to `other`, which keeps a preset
   *  visible even when the shipped list names a section this panel predates
   *  -- the same fallback the headings had. */
  function kindOf(section) {{
    if (section === 'recommended') {{
      return {{ icon: ICONS.recommended, label: TEXT.groupRecommended }};
    }}
    if (section === 'authored') {{
      return {{ icon: ICONS.authored, label: TEXT.groupAuthored }};
    }}
    return {{ icon: ICONS.other, label: TEXT.groupOther }};
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

  /** The one description on screen, and the icon that asked for it. */
  var pop = null, popFor = null;

  function forget() {{
    if (pop) pop.hidden = true;
    popFor = null;
  }}

  /** Say what a plugin does, beside the card that was asked.
   *
   *  Parented to the panel rather than to the card: a card takes a transform
   *  on hover, and `position:fixed` inside a transformed element is fixed to
   *  that element rather than to the window -- so a popover built into the
   *  card would be clipped by the scrolling list the card sits in.
   *
   *  One at a time, and a second press on the same icon puts it away. */
  function explain(from, text) {{
    if (popFor === from) {{
      forget();
      return;
    }}
    if (!pop) pop = make('div', 'dsh-pp-pop', root);
    pop.textContent = text;
    pop.hidden = false;
    popFor = from;

    // Measured from a corner, not from where it happens to be: with no `left`
    // of its own it takes its static position -- off to one side of a card
    // that is centred in the window -- and a shrink-to-fit box squeezed
    // against the window's edge is not the size it is about to be.
    pop.style.left = '0px';
    pop.style.top = '0px';

    var at = from.getBoundingClientRect();
    var size = pop.getBoundingClientRect();
    var edge = 10;
    var left = Math.min(at.left - 8, window.innerWidth - size.width - edge);
    // Under the icon where there is room for it, over it where there is not.
    var under = at.bottom + 8;
    var top = under + size.height > window.innerHeight - edge
      ? at.top - size.height - 8
      : under;
    pop.style.left = Math.round(Math.max(edge, left)) + 'px';
    pop.style.top = Math.round(Math.max(edge, top)) + 'px';
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

    // The tick the user actually sees. The real checkbox stays in the DOM --
    // it is what `ticked()` reads and what keyboard focus lands on -- but it is
    // taken out of the layout by the stylesheet and drawn as this instead.
    var mock = make('span', 'dsh-pp-tick', line);
    mock.innerHTML = svg(ICONS.tick);
    mock.setAttribute('aria-hidden', 'true');

    var body = make('div', 'dsh-pp-body', line);
    var name = make('div', 'dsh-pp-name', body);
    name.appendChild(document.createTextNode(item.name));
    if (item.fix) name.appendChild(chip('fix', TEXT.fix));
    // Last on the line, after any chip a preset came with. Residue instead of
    // "installed", never both: the tick does the same thing to either, but
    // calling a leftover an installed plugin is the thing that hid it.
    if (item.stale) name.appendChild(chip('stale', TEXT.residue));
    else if (item.installed) name.appendChild(chip('installed', TEXT.have));

    // What is actually going on the machine, where the name does not already
    // say it. The names are translated, so `插件市场` on its own names nothing
    // pnpm has heard of. Under the name rather than beside it -- a card three
    // to a row has not the width for both -- and ellipsised by the stylesheet
    // rather than wrapped.
    if (item.detail) make('div', 'dsh-pp-pkg', body).textContent = item.detail;

    var bar = make('div', 'dsh-pp-tools', body);

    // The description is nowhere on the card -- it is what made every card
    // four lines tall -- so this icon is the whole of the way to it.
    if (item.description) {{
      var about = act(bar, ICONS.about, TEXT.about);
      about.addEventListener('click', function () {{
        explain(about, item.description);
      }});
    }}

    if (item.url) {{
      // A link rather than a button: the shell sends every navigation out of
      // this app's own origins to the user's browser, which is the whole of
      // what this has to do. See `is_ours` in main.rs.
      act(bar, ICONS.repo, TEXT.repo, 'a').href = item.url;
    }}

    // Which list it came off. A mark rather than a button -- there is nothing
    // to press -- and what it means is on the hover.
    var kind = kindOf(item.section);
    var flag = make('span', 'dsh-pp-kind dsh-pp-' + (item.section || 'other'), bar);
    flag.innerHTML = svg(kind.icon);
    flag.title = kind.label;

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
   *  above it stay put. */
  function fill(data) {{
    var presets = data.presets || [];
    var installed = data.installed || [];

    // Whatever it was pointing at is about to be thrown away.
    forget();
    list.textContent = '';

    // Recommended first, then the ones this app's author wrote, then a section
    // this panel has no order for. The headings that used to say which was
    // which are gone -- every card wears its own mark now; see `kindOf` --
    // so this order is what is left grouping them. `sort` is stable in both
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
        // The package name, where that is not already the label, and the
        // range pnpm recorded. Between them they say which thing this is.
        detail: [item.label === item.name ? '' : item.name, item.version]
          .filter(Boolean)
          .join(' '),
        description: item.description,
        url: item.url,
        section: item.section,
        installed: true,
        stale: !!item.stale
      }};
    }}));

    // At the end, which is where the installed ones are: nothing is offered
    // after the first thing that is already here.
    if (!presets.length) {{
      make('p', 'dsh-pp-lede', list).textContent = installed.length ? TEXT.allIn : TEXT.empty;
    }}

    cards.forEach(function (item) {{
      list.appendChild(card(item));
    }});

    offerRemoval();
  }}

  function start() {{
    var ids = ticked(OFFERED);
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
      '--pp-soft:#f7f8fa;--pp-fix:#b54708;--pp-star:#e0a30c;' +
      '--pp-hover:#c3cbe6;--pp-tint:rgba(77,107,254,.07)}}' +
      '.dsh-pp.dsh-pp-dark{{background:rgba(0,0,0,.5);' +
      '--pp-bg:#17171d;--pp-fg:#ececf1;--pp-muted:#9aa0ac;--pp-line:#2b2b34;' +
      '--pp-danger:#f97066;--pp-ok:#3ccb9a;' +
      '--pp-soft:rgba(255,255,255,.04);--pp-fix:#f0a35e;--pp-star:#f5c344;' +
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
      // Padded, and pulled back out again by the same margin, so a card that
      // lifts on hover has somewhere to lift into. Without it the scroller's
      // own edge cut the top off the whole first row.
      '.dsh-pp-list{{flex:1 1 auto;min-height:0;overflow:auto;' +
      'margin:-6px -4px;padding:6px 4px}}' +
      // Several cards to a row, and as many of them as the width takes: three
      // at the card's full width, two at the narrowest window the app opens
      // at. `align-content` keeps a short list at the top of the scroller
      // rather than stretched down it.
      '.dsh-pp-grid{{display:grid;gap:8px;align-content:start;' +
      'grid-template-columns:repeat(auto-fill,minmax(196px,1fr))}}' +
      // The line that stands in for an empty list is a sentence, not a card.
      '.dsh-pp-list>.dsh-pp-lede{{grid-column:1/-1;margin:2px 0 0}}' +
      // A card rather than a row: it lifts on hover and takes an accent border
      // when it is ticked, so a selection is visible without reading the box.
      // No margin -- the grid's gap is what separates them.
      '.dsh-pp-row{{position:relative;display:flex;gap:9px;align-items:flex-start;' +
      'padding:10px 11px;border:1px solid var(--pp-line);border-radius:10px;' +
      'background:var(--pp-bg);cursor:pointer;' +
      'transition:border-color .15s,background .15s,box-shadow .15s,transform .15s}}' +
      '.dsh-pp-row:hover{{border-color:var(--pp-hover);background:var(--pp-soft);' +
      'transform:translateY(-1px);box-shadow:0 4px 14px rgba(0,0,0,.07)}}' +
      '.dsh-pp-row.dsh-pp-on{{border-color:var(--pp-accent);background:var(--pp-tint)}}' +
      '.dsh-pp-row.dsh-pp-on:hover{{border-color:var(--pp-accent)}}' +
      // The real checkbox: still focusable and still what `ticked()` reads,
      // but out of the layout so the drawn tick can take its place.
      '.dsh-pp-row input{{position:absolute;opacity:0;width:1px;height:1px;' +
      'margin:0;pointer-events:none}}' +
      '.dsh-pp-tick{{flex:none;display:flex;align-items:center;' +
      'justify-content:center;width:16px;height:16px;margin-top:1px;' +
      'border-radius:5px;border:1.5px solid var(--pp-line);' +
      'background:var(--pp-bg);transition:border-color .15s,background .15s}}' +
      '.dsh-pp-row:hover .dsh-pp-tick{{border-color:var(--pp-hover)}}' +
      '.dsh-pp-row.dsh-pp-on .dsh-pp-tick{{border-color:var(--pp-accent);' +
      'background:var(--pp-accent)}}' +
      // The check itself: centred by the box's own flexbox, which is the one
      // way it is certain to be. See `ICONS.tick`.
      '.dsh-pp-tick svg{{width:11px;height:11px;color:#fff;stroke-width:2;' +
      'transform:scale(.4);opacity:0;' +
      'transition:transform .15s,opacity .15s}}' +
      '.dsh-pp-row.dsh-pp-on .dsh-pp-tick svg{{transform:scale(1);opacity:1}}' +
      // Keyboard focus has to land somewhere visible, and the box it lands on
      // is invisible by now.
      '.dsh-pp-row input:focus-visible ~ .dsh-pp-tick{{outline:2px solid var(--pp-accent);' +
      'outline-offset:2px}}' +
      '.dsh-pp-body{{min-width:0;flex:1}}' +
      '.dsh-pp-name{{font-weight:600;font-size:13.5px;line-height:1.35;' +
      'display:flex;align-items:center;gap:6px;flex-wrap:wrap}}' +
      // The package name, said quietly: it is what the row installs, not what
      // the row is called. Monospace because it is a thing to be typed —
      // `dsh plugin add` takes this exact string.
      // Ellipsised rather than wrapped: it is one token, and a card this
      // narrow would give a long one a line and a half of its own.
      '.dsh-pp-pkg{{margin-top:1px;font-weight:400;font-size:11.5px;' +
      'color:var(--pp-muted);font-family:ui-monospace,Consolas,monospace;' +
      'white-space:nowrap;overflow:hidden;text-overflow:ellipsis}}' +
      // The bottom line of a card: what it can be asked, then the mark for
      // which list it came off, at the far end.
      '.dsh-pp-tools{{display:flex;align-items:center;gap:6px;margin-top:8px}}' +
      '.dsh-pp-tools svg{{display:block;width:14px;height:14px}}' +
      // Written with the class twice over to outrank `.dsh-pp button` below,
      // which is the panel's ordinary button and nothing like this one.
      '.dsh-pp .dsh-pp-act{{display:inline-flex;align-items:center;' +
      'justify-content:center;width:24px;height:24px;padding:0;' +
      'border:1px solid var(--pp-line);border-radius:7px;background:none;' +
      'color:var(--pp-muted);cursor:pointer;text-decoration:none;' +
      'transition:color .15s,border-color .15s,background .15s}}' +
      '.dsh-pp .dsh-pp-act:hover{{color:var(--pp-accent);' +
      'border-color:var(--pp-accent);background:var(--pp-tint)}}' +
      '.dsh-pp .dsh-pp-act:focus-visible{{outline:2px solid var(--pp-accent);' +
      'outline-offset:1px}}' +
      // Not a button: nothing to press, and no box around it, so the two that
      // are pressable still read as the only two.
      '.dsh-pp-kind{{display:inline-flex;align-items:center;margin-left:auto;' +
      'color:var(--pp-muted)}}' +
      // The one mark worth picking out of a grid at a glance, and the only
      // thing on the panel that is yellow. Its own colour rather than the
      // amber `--pp-fix` lends a chip: a star is a star.
      '.dsh-pp-kind.dsh-pp-recommended{{color:var(--pp-star)}}' +
      // The description, on the card that was asked for it. Fixed to the
      // window, so the scrolling list it is raised over cannot clip it.
      '.dsh-pp-pop{{position:fixed;z-index:1;max-width:300px;' +
      'padding:9px 11px;border-radius:10px;border:1px solid var(--pp-line);' +
      'background:var(--pp-bg);color:var(--pp-fg);font-size:12.5px;' +
      'line-height:1.55;box-shadow:0 12px 32px rgba(0,0,0,.24)}}' +
      '.dsh-pp-pop[hidden]{{display:none}}' +
      // Nothing here is load-bearing; a user who asked for less movement can
      // have the same panel without any of it.
      '@media (prefers-reduced-motion:reduce){{.dsh-pp-row,.dsh-pp-tick,' +
      '.dsh-pp-tick svg,.dsh-pp .dsh-pp-act{{transition:none}}' +
      '.dsh-pp-row:hover{{transform:none}}}}' +
      '.dsh-pp-chip{{font-size:11px;font-weight:500;line-height:1.5;padding:0 7px;' +
      'border-radius:999px;border:1px solid currentColor}}' +
      '.dsh-pp-chip.dsh-pp-fix{{color:var(--pp-fix)}}' +
      '.dsh-pp-chip.dsh-pp-installed{{color:var(--pp-ok)}}' +
      // The same red the removal button wears, because it is the same verb:
      // this one is here to be taken away.
      '.dsh-pp-chip.dsh-pp-stale{{color:var(--pp-danger)}}' +
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
      '.dsh-pp-running .dsh-pp-list,' +
      '.dsh-pp-running .dsh-pp-hint{{display:none}}' +
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
    // Kept, because `discard` takes it away again.
    sheet = style;
    document.head.appendChild(sheet);

    root = make('div', 'dsh-pp');
    var card = make('div', 'dsh-pp-card', root);
    make('h1', '', card).textContent = TEXT.title;
    lede = make('p', 'dsh-pp-lede', card);
    list = make('div', 'dsh-pp-list dsh-pp-grid', card);
    // A description is raised over the list; the icon it was raised from goes
    // out from under it as soon as the list moves.
    list.addEventListener('scroll', forget);

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
    // It hung off the root that has just gone; the next one builds another.
    pop = null;
    popFor = null;
    stale = false;
  }}

  // A press anywhere else puts an open description away. The icons stop their
  // own clicks where they are raised (see `act`), so this only ever sees the
  // other kind -- including a press on the card behind the popover, which is
  // a tick and should not also have to be a dismissal.
  document.addEventListener('click', function () {{
    if (popFor) forget();
  }});

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
    // The description first: it is the smaller thing to be rid of, and a user
    // who wanted the panel gone has not been asked twice for nothing.
    if (popFor) {{
      forget();
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

  /** The list again, after a run changed what it says. */
  window.__dshPluginLists = function (listing) {{
    if (!root) return;
    try {{
      fill(JSON.parse(listing));
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
    forget();
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
