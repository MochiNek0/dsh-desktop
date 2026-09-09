/**
 * dsh desktop signal plugin, node half.
 *
 * Deliberately empty. Everything this plugin does is browser-side: it reads
 * what the client already knows about each session and tells the desktop shell
 * over the shell's own channel. The host has nothing to contribute to that and
 * nothing to expose to the model.
 */

/** Host plugin body — the whole plugin is its `./client` half. */
export function apply() {}
