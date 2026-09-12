//! One reload, when the page cannot fetch the script its own boot manifest
//! told it to fetch.
//!
//! dsh serves every plugin's browser half as *one* combined script — a combo
//! URL naming all of them at once, `/plugins/??a/client.js,b/client.js,…&rev=`,
//! whose `rev` is a content hash over the bytes of every bundle in it. The
//! address is minted when `index.html` is rendered, inlined into the page as
//! `window.__DSH_BOOT__`, and answered by an exact-match table that keeps the
//! current composition and one generation before it. Everything else is a 404.
//!
//! So the page and the server can disagree, and the way they come to disagree
//! is installing or removing a plugin: that changes which bundles are in the
//! batch and therefore the hash naming it. A document holding an address from
//! before the change asks for a script that no longer exists — and because the
//! batch is all of them, the one 404 takes dsh's own interface down with the
//! plugin that was added or removed. What the user gets is a card reading
//! "Failed to load plugins" over a list of forty-odd packages they never
//! installed.
//!
//! It does not recover on its own, and that is the part worth fixing. dsh's
//! boot card has no retry. This app deliberately does not reload the page when
//! `dsh web` comes back on the port it was on — see [`crate::resume`], where
//! staying put is how a draft in the composer survives a restart — which is
//! right for a page that booted and wrong for one that never did. Between them
//! the window sits on a dead page for as long as the user leaves it there.
//!
//! ## The seam
//!
//! dsh's web boot reads an optional `globalThis.__DSH_TRANSPORT__`, and takes
//! `loadBundle` off it in place of its own script-tag loader. This installs one
//! that does exactly what the default does and, when a fetch fails, reloads the
//! document instead of rejecting: the reload re-renders `index.html`, which
//! mints the addresses again from the composition the server actually has.
//!
//! Two things it deliberately does not cover. The bootstrap batch is a plain
//! `<script src>` in the served markup rather than something `loadBundle` is
//! asked for, so a 404 there is still dsh's card — but that batch is one
//! package, `@deepseek-ai/dsh-client-modules`, and its address moves only when
//! dsh itself is updated. And a server that is actually down fails the retry
//! too, which is what [`BUDGET`] is for: a stale address costs one reload, and
//! a dsh that is gone stops reloading and lets dsh say so.

/// How many reloads one document may spend, and how long apart.
///
/// The recovery this is for costs exactly one: the reload re-reads the manifest
/// and the second attempt is against addresses that exist. More than one is
/// therefore not this failure, and the budget stops a broken server turning the
/// window into a reload loop. It is not one, because a window is long-lived —
/// `sessionStorage` outlives a reload and is only cleared when the window is —
/// and a user who changes plugins twice in an afternoon should get the repair
/// both times.
const BUDGET: u32 = 3;

/// Seconds between reloads, so a failure that answers instantly cannot spend
/// the whole budget before the user sees anything.
const APART: u32 = 10;

/// The script, for the window's `initialization_script`.
///
/// Runs at document start on every page this window loads, ours included, where
/// it does nothing: the global is read by dsh's boot and by nothing else.
///
/// Guarded against replacing a transport that is already there. Nothing sets
/// one today — the seam exists for an embedder, which is what this app is — but
/// silently taking it over would be this app deciding it is the only embedder
/// for a page it does not own.
pub fn script() -> String {
    format!(
        r#"(function () {{
  if (window.__DSH_TRANSPORT__) return;

  var KEY = 'dsh-desktop:bundle-reload';

  // Whether this document may spend a reload. `sessionStorage` throws in
  // enough situations to be worth not trusting; a window that cannot keep
  // the count gets one reload rather than none, since the failure this
  // recovers from is the common one and a loop needs a broken server.
  function mayReload() {{
    var now = Date.now();
    try {{
      var seen = JSON.parse(sessionStorage.getItem(KEY) || '{{"n":0,"at":0}}');
      if (seen.n >= {budget}) return false;
      if (now - seen.at < {apart} * 1000) return false;
      sessionStorage.setItem(KEY, JSON.stringify({{ n: seen.n + 1, at: now }}));
      return true;
    }} catch (e) {{
      return true;
    }}
  }}

  // What dsh's own loader does, kept the same on the way in: an async classic
  // script, removed once it has run, resolving on load. A bundle registers its
  // factory as a side effect of executing, so nothing is read off the element.
  function fetchBundle(url) {{
    return new Promise(function (resolve, reject) {{
      var el = document.createElement('script');
      el.async = true;
      el.src = url;
      el.addEventListener('load', function () {{ el.remove(); resolve(); }}, {{ once: true }});
      el.addEventListener('error', function () {{
        el.remove();
        reject(new Error('dsh-desktop: bundle script ' + url + ' failed to load'));
      }}, {{ once: true }});
      document.head.append(el);
    }});
  }}

  window.__DSH_TRANSPORT__ = {{
    loadBundle: function (url) {{
      return fetchBundle(url).catch(function (error) {{
        if (!mayReload()) throw error;
        // The document is going away, so this promise is never settled: dsh is
        // mid-boot and resolving it would have it carry on against a page that
        // is being replaced. The reload re-renders the manifest.
        location.reload();
        return new Promise(function () {{}});
      }});
    }}
  }};
}})();"#,
        budget = BUDGET,
        apart = APART,
    )
}

#[cfg(test)]
mod tests {
    use super::{script, APART, BUDGET};

    /// The two numbers are the whole of the loop protection, and they are
    /// written into the script by `format!` rather than read from it. A
    /// placeholder that stopped being substituted would leave a script that
    /// parses and never stops reloading.
    #[test]
    fn the_budget_reaches_the_script() {
        let script = script();
        assert!(
            script.contains(&format!("seen.n >= {BUDGET}")),
            "the reload budget must be substituted: {script}"
        );
        assert!(
            script.contains(&format!("< {APART} * 1000")),
            "the interval must be substituted: {script}"
        );
    }

    /// Every brace in the script body is doubled for `format!`, and one that is
    /// not is a syntax error this app would ship without noticing: the script
    /// is evaluated by the webview, not by the compiler. Balance is the cheapest
    /// check that catches an unescaped `{` having eaten the rest of a line.
    #[test]
    fn the_script_is_balanced() {
        let script = script();
        let mut depth = 0i32;
        for character in script.chars() {
            match character {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            assert!(depth >= 0, "a closing brace with nothing open: {script}");
        }
        assert_eq!(depth, 0, "unbalanced braces: {script}");
    }

    /// The seam is dsh's, spelled its way. A typo here is a transport nothing
    /// ever reads and a recovery that silently does not happen.
    #[test]
    fn it_installs_on_the_seam_dsh_reads() {
        let script = script();
        assert!(script.contains("window.__DSH_TRANSPORT__ = {"));
        assert!(
            script.contains("if (window.__DSH_TRANSPORT__) return;"),
            "a transport already installed is left alone: {script}"
        );
    }
}
