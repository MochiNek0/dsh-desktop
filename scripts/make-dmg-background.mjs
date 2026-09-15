// Draw the .dmg window's background: the drag-to-Applications arrow, and the
// one line that gets the app past Gatekeeper.
//
// macOS refuses to open this app the first time — "Apple 无法验证 dsh-desktop
// 是否包含恶意软件" — because it is not signed with an Apple developer ID, and
// nothing in the app can say so: it never runs. The disk image is the last
// surface the user sees before that refusal, so the sentence goes there.
//
// A background image is the whole of what a .dmg window can carry. Tauri builds
// it from the `.app` and an `Applications` symlink and nothing else — there is
// no config for a third file, so a `请先看我.txt` beside the app is not an
// option. `bundle.macOS.dmg.background` is; see `tauri.macos.conf.json`, whose
// `windowSize` is the size this draws at.
//
// ## Rendered here, committed as a PNG
//
// Unlike the installer bitmaps next door, this one is not generated during the
// build. It is text, and text needs a font and a shaper — which in Node means a
// dependency, and on the macOS runner means a second renderer in a second
// language that no one can look at from a Windows machine. So the browser that
// is already on the machine draws it, by hand, and the result is committed:
// CI gains nothing to go wrong, and what ships is what was looked at.
//
//   npm run art:dmg
//
// Re-run it when the copy below changes. Running it on a Mac is worth it if the
// machine is to hand — the type is whatever fonts the rendering machine has,
// and on macOS that is the same San Francisco and PingFang the rest of the
// system draws.

import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
/** Tracked, unlike `src-tauri/installer/`: this one ships as a source file. */
const out = join(root, "src-tauri", "dmg");
/** Gitignored scratch, the way every other rendered-for-a-human file is. */
const scratch = join(root, "target", "art");

/**
 * Has to match `windowSize` in `tauri.macos.conf.json`. Finder draws the
 * background at its own pixel size and does not scale it, so a mismatch is a
 * tiled or cropped image rather than a stretched one.
 */
const WIDTH = 660;
const HEIGHT = 460;

/** The whole of what the image says. */
const COPY = {
  drag: "把 dsh-desktop 拖到 Applications",
  dragEn: "Drag dsh-desktop into Applications",
  title: "第一次打开被 macOS 拦下了？",
  body: "这个应用没有 Apple 开发者签名，所以 macOS 默认不放行。拖进去之后，在「终端」里执行这一行：",
  command: "xattr -dr com.apple.quarantine /Applications/dsh-desktop.app",
  en: "Blocked on first launch? This app is not signed with an Apple developer ID — run that line in Terminal once, and macOS will open it.",
};

/** The loading page's accent; see `dist/index.html`. */
const ACCENT = "#4d6bfe";

/**
 * The page, at exactly the window's size.
 *
 * The top third is left empty on purpose: Finder draws the app icon at x=180
 * and the Applications symlink at x=480, both centred on y=140, and anything
 * drawn under them is something they sit on top of. A 128px icon with its name
 * under it reaches about y=222 from there, which is where the caption starts.
 * The arrow goes in the gap between the two icons; everything with words in it
 * goes below them. The three positions are `appPosition` and
 * `applicationFolderPosition` in `tauri.macos.conf.json` — change one, change
 * both.
 */
function page() {
  return `<!doctype html>
<html lang="zh">
<head>
<meta charset="utf-8">
<style>
  @page { size: ${WIDTH}px ${HEIGHT}px; margin: 0 }
  html, body { margin: 0; padding: 0 }
  body {
    width: ${WIDTH}px; height: ${HEIGHT}px; overflow: hidden;
    background: linear-gradient(180deg, #fbfbfd 0%, #f2f3f7 100%);
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC",
      "Microsoft YaHei", system-ui, sans-serif;
    color: #1b1c1f;
    -webkit-font-smoothing: antialiased;
  }
  .arrow {
    position: absolute; left: 258px; top: 128px; width: 144px; height: 24px;
    color: #b6b9c4;
  }
  .drag {
    position: absolute; left: 0; right: 0; top: 238px;
    text-align: center; font-size: 14px; font-weight: 600; letter-spacing: .2px;
  }
  .drag small {
    display: block; margin-top: 5px;
    font-size: 11px; font-weight: 400; color: #7c8090; letter-spacing: 0;
  }
  .note {
    position: absolute; left: 34px; right: 34px; bottom: 26px;
    padding: 14px 16px 15px; box-sizing: border-box;
    background: rgba(255, 255, 255, .82);
    border: 1px solid rgba(0, 0, 0, .08);
    border-radius: 12px;
  }
  .note h1 { margin: 0; font-size: 12.5px; font-weight: 600 }
  .note p { margin: 6px 0 0; font-size: 11.5px; line-height: 1.5; color: #4a4d57 }
  code {
    display: block; margin: 9px 0 0; padding: 8px 10px;
    background: #14161c; border-radius: 8px;
    color: #e9ecf5; font-size: 11.5px; line-height: 1.2;
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    white-space: nowrap; overflow: hidden; text-overflow: clip;
  }
  code b { color: ${ACCENT}; font-weight: 400 }
  .en { margin-top: 8px; font-size: 10.5px; line-height: 1.45; color: #85889a }
</style>
</head>
<body>
  <svg class="arrow" viewBox="0 0 144 24" fill="none" stroke="currentColor"
       stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
    <path d="M2 12h132" stroke-dasharray="6 7"/>
    <path d="M130 5l10 7-10 7"/>
  </svg>

  <div class="drag">${COPY.drag}<small>${COPY.dragEn}</small></div>

  <div class="note">
    <h1>${COPY.title}</h1>
    <p>${COPY.body}</p>
    <code><b>$</b> ${COPY.command}</code>
    <p class="en">${COPY.en}</p>
  </div>
</body>
</html>`;
}

/**
 * A Chrome or Edge on this machine, whichever turns up first.
 *
 * Any of them draws the same boxes; what differs is the fonts they find, which
 * is the one thing about this image that is the rendering machine's rather than
 * the repository's. See the note at the top.
 */
function browser() {
  const candidates = {
    darwin: [
      "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
      "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
      "/Applications/Chromium.app/Contents/MacOS/Chromium",
    ],
    win32: [
      join(
        process.env["ProgramFiles"] ?? "C:/Program Files",
        "Google/Chrome/Application/chrome.exe",
      ),
      join(
        process.env["ProgramFiles(x86)"] ?? "C:/Program Files (x86)",
        "Google/Chrome/Application/chrome.exe",
      ),
      join(
        process.env["LOCALAPPDATA"] ?? "",
        "Google/Chrome/Application/chrome.exe",
      ),
      join(
        process.env["ProgramFiles(x86)"] ?? "C:/Program Files (x86)",
        "Microsoft/Edge/Application/msedge.exe",
      ),
    ],
  };

  const found = (candidates[process.platform] ?? [
    "/usr/bin/google-chrome",
    "/usr/bin/chromium",
    "/usr/bin/chromium-browser",
  ]).find((path) => path && existsSync(path));

  if (!found) {
    throw new Error(
      "no Chrome, Chromium or Edge found — this draws the image with the " +
        "browser already on the machine, and there is none to use",
    );
  }
  return found;
}

mkdirSync(out, { recursive: true });
mkdirSync(scratch, { recursive: true });

const html = join(scratch, "dmg-background.html");
const png = join(out, "background.png");
writeFileSync(html, page(), "utf8");

// `--force-device-scale-factor=1`, deliberately: Finder puts the background
// into the window at one image pixel per point, so a 2x render would be a
// quarter of the window with the rest tiled. Soft on a Retina display is the
// price, and it is the price every unsigned .dmg pays.
execFileSync(browser(), [
  "--headless=new",
  "--disable-gpu",
  "--hide-scrollbars",
  "--force-device-scale-factor=1",
  `--window-size=${WIDTH},${HEIGHT}`,
  "--virtual-time-budget=2000",
  `--screenshot=${png}`,
  `file:///${html.replace(/\\/g, "/")}`,
]);

console.log(`wrote ${png}`);
