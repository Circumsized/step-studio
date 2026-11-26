// Shared by `packaged-csp.test.mjs` (the source constant) and
// `packaged-csp-artifact.test.mjs` (the built index.html). A second copy of this
// parser is how two policies end up agreeing with each other and neither with
// what ships.

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));

/** `directive -> [sources]`, parsed from one CSP string. */
export function parseCsp(csp) {
  const directives = new Map();
  for (const part of csp.split(";").map(entry => entry.trim()).filter(Boolean)) {
    const [name, ...sources] = part.split(/\s+/u);
    directives.set(name, sources);
  }
  return directives;
}

/** Every CSP meta tag in an HTML document, in document order. */
export function cspMetas(html) {
  return [...html.matchAll(/<meta http-equiv="Content-Security-Policy" content="([^"]*)"/gu)].map(match => match[1]);
}

/** The one policy the document is allowed to carry. */
export function soleCsp(html) {
  const found = cspMetas(html);
  assert.equal(found.length, 1, `expected exactly one Content-Security-Policy meta tag, found ${found.length}`);
  return found[0];
}

/**
 * The policy string, straight out of `vite.config.ts`.
 *
 * The artifact test demands the built HTML carry this exact string rather than a
 * second copy of it: `vite.config.ts`'s own header notes nothing asserts against
 * a second copy, and a hand-written meta in the template would be exactly that.
 */
export async function packagedCsp() {
  const source = await readFile(`${root}apps/renderer/vite.config.ts`, "utf8");
  const match = source.match(/const PACKAGED_RENDERER_CSP =\s*\n?\s*"([^"]+)"/u);
  assert.ok(match, "PACKAGED_RENDERER_CSP is missing from apps/renderer/vite.config.ts");
  return match[1];
}
