// The built index.html must carry the packaged CSP.
//
// `scripts/packaged-csp.test.mjs` reads the source constant, which catches
// policy drift but says nothing about whether the policy reaches the artifact.
// This closes that half: if `packagedCspPlugin` stops injecting — the template
// loses its literal `<head>`, the `html.includes("Content-Security-Policy")`
// early return starts tripping, or the plugin is dropped from the `plugins`
// array — the shipped app runs with no CSP at all and every source-level
// assertion still passes.
//
// Run by `check.mjs`'s production group, after `vite build`.

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { packagedCsp, parseCsp, soleCsp } from "./packaged-csp-lib.mjs";

const root = fileURLToPath(new URL("../", import.meta.url));
const HTML = `${root}apps/renderer/dist/index.html`;

test("the built index.html carries the packaged policy, IPC origins included", async () => {
  const html = await readFile(HTML, "utf8");
  // Byte-for-byte against the source constant, not a re-parse of whatever the
  // template happens to carry: a hand-written meta in `index.html` would make
  // the injection plugin's early return no-op and leave two copies that drift,
  // with the source test green the whole time. `soleCsp` also refuses a second
  // meta rather than silently taking the first.
  assert.equal(soleCsp(html), await packagedCsp(), "the built CSP is not the constant in vite.config.ts");
  const policy = parseCsp(soleCsp(html));
  for (const origin of ["ipc:", "http://ipc.localhost", "https://ipc.localhost"]) {
    assert.ok(policy.get("connect-src")?.includes(origin), `built connect-src is missing ${origin}`);
  }
  assert.ok(!policy.get("script-src")?.includes("'unsafe-inline'"), "built script-src must not allow inline script");
  assert.deepEqual(policy.get("object-src"), ["'none'"], "built object-src must be 'none'");
});
