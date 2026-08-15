// The ESM half of the boot trace; see preload.cjs, which registers it.
//
// Hooks run on their own thread, so this reads the trace path from the
// environment rather than being handed it.

import { appendFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const trace = process.env.DSH_BOOT_TRACE;

export async function load(url, context, next) {
  const result = await next(url, context);

  if (url.startsWith('file:')) {
    try {
      appendFileSync(trace, `${Date.now()}\t${fileURLToPath(url)}\n`);
    } catch {
      // As in preload.cjs: a dropped line only costs a warm-up miss.
    }
  }

  return result;
}
