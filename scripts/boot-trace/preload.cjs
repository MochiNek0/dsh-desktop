// Record every file a dsh boot reads, so the app can pull that exact set past
// the virus scanner in parallel before dsh gets to it one file at a time.
// See `traceBootSet` in scripts/bundle-runtime.mjs and src-tauri/src/warm.rs.
//
// Loaded with `--require`, which runs before the ESM entry point is imported:
// the patches below catch `require` and native bindings, and the loader hook
// registered at the end catches the ESM graph, which is most of it.

const { appendFileSync } = require('node:fs');
const Module = require('node:module');
const { pathToFileURL } = require('node:url');

const trace = process.env.DSH_BOOT_TRACE;

/** Append one read to the trace, timestamped so the order survives two writers. */
function note(file) {
  try {
    appendFileSync(trace, `${Date.now()}\t${file}\n`);
  } catch {
    // A trace that loses a line is a warm-up that misses a file. Never fatal.
  }
}

const load = Module._load;
Module._load = function (request, parent, isMain) {
  try {
    note(Module._resolveFilename(request, parent, isMain));
  } catch {
    // An unresolvable request is about to throw on its own terms.
  }
  return load.apply(this, arguments);
};

const dlopen = process.dlopen;
process.dlopen = function (module, filename, flags) {
  note(filename);
  return dlopen.apply(this, arguments);
};

Module.register('./esm-hook.mjs', pathToFileURL(__filename));
