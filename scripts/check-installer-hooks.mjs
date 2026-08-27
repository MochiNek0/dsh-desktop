// Compile `src-tauri/installer-hooks.nsh` without building the whole app.
//
// The hooks file is not a script on its own — it is `!include`d into the
// installer Tauri generates — so nothing checks it until `tauri build` runs
// NSIS, minutes into a bundle, long after the Rust has compiled. Worse, a hook
// can compile and still be wrong in a way only a user sees.
//
// So `hooks-syntax-check.nsi` stands in for the generated installer: the same
// defines, the same includes in the same order, and every hook macro inserted
// once so its body is compiled rather than merely parsed. This script points
// makensis at it.
//
// Two mistakes it has already caught, both of which would have shipped:
//
//   - `${DriveGetType}` does not exist. `${GetDrives}` filters by type itself
//     and hands the callback the drive in `$9`.
//   - `.onGUIInit` cannot be defined by the hooks file, because MUI2 defines it.
//     The supported seam is `MUI_CUSTOMFUNCTION_GUIINIT`.
//
// Windows only: makensis is the NSIS compiler, and the copy this uses is the one
// the Tauri CLI downloads for bundling. On any other platform, and on a Windows
// machine that has never bundled, this exits 0 with a note rather than failing —
// it is a check that could not run, not a check that failed.

import { spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const script = join(root, "src-tauri", "hooks-syntax-check.nsi");

function makensis() {
  if (process.platform !== "win32") return null;
  const local = process.env.LOCALAPPDATA;
  const bundled = local ? join(local, "tauri", "NSIS", "makensis.exe") : null;
  if (bundled && existsSync(bundled)) return bundled;
  // A machine-wide NSIS, if the developer has one. Looked for on disk rather
  // than with `where`, which would need a pipe to read — see the note on the
  // compile below.
  for (const base of [process.env["ProgramFiles(x86)"], process.env.ProgramFiles]) {
    if (!base) continue;
    const candidate = join(base, "NSIS", "makensis.exe");
    if (existsSync(candidate)) return candidate;
  }
  return null;
}

const compiler = makensis();
if (!compiler) {
  console.log(
    "check:installer - skipped: no makensis.\n" +
      "  It ships with the NSIS toolchain the Tauri CLI downloads on the first\n" +
      "  Windows bundle, so `npm run build` once, or install NSIS, to enable it."
  );
  process.exit(0);
}

if (!existsSync(script)) {
  console.error(`check:installer - missing harness: ${script}`);
  process.exit(1);
}

// makensis writes its report to a file rather than to a pipe we read, and the
// child's stdio is inherited.
//
// Not a preference: some sandboxed environments refuse to let a child process
// open the named pipes `stdio: "pipe"` needs, and `spawnSync` then fails with
// EPERM and a null status — which reads exactly like a compile failure with an
// empty error, and sent a first version of this script chasing one. A log file
// works everywhere and has the same content.
const log = join(tmpdir(), "dsh-installer-hooks.log");
// `/O` is makensis's own "write the output here"; `/V2` keeps warnings and
// errors while dropping the per-line script echo.
const built = spawnSync(compiler, [`/O${log}`, "/V2", script], {
  cwd: join(root, "src-tauri"),
  stdio: "inherit",
});

if (built.error) {
  console.error(`check:installer - could not run makensis: ${built.error.message}`);
  process.exit(1);
}

const output = (existsSync(log) ? readFileSync(log, "utf8") : "").trim();

if (built.status !== 0) {
  console.error("check:installer - installer-hooks.nsh does not compile:\n");
  console.error(output);
  process.exit(1);
}

// Warnings the harness itself causes, rather than the hooks: it declares no
// uninstaller and never sets the template's `$UpdateMode`, and it does not use
// every StrFunc the hooks file registers.
const expected = [/6020: Uninstaller script code/, /6001: Variable "UpdateMode"/, /6010: install function "\w+" not referenced/];
const unexpected = output
  .split(/\r?\n/)
  .filter((line) => /warning/i.test(line))
  .filter((line) => !expected.some((pattern) => pattern.test(line)))
  // The trailing "N warnings:" summary repeats what was already reported.
  .filter((line) => !/^\d+ warnings?:/.test(line));

if (unexpected.length > 0) {
  console.error("check:installer - unexpected NSIS warnings:\n");
  console.error(unexpected.join("\n"));
  process.exit(1);
}

console.log("check:installer - installer-hooks.nsh compiles.");
