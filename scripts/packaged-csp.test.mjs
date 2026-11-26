// The packaged renderer's CSP, and the two properties that must never regress.
//
// It lives in `apps/renderer/vite.config.ts` as a meta tag, because a `file://`
// load does not reliably get webRequest CSP headers and `tauri.conf.json`'s
// `security.csp` is deliberately `null` — letting Tauri inject its own nonce and
// hash sources would, per CSP3, make `'unsafe-inline'` in `style-src` stop
// applying, and React's and mermaid's inline styles would all break. So the
// renderer owns the policy and nothing else may quietly redefine it.
//
// The origins below are read out of the Tauri crate this app pins (2.12.5, per
// Cargo.lock), not guessed: `src/protocol/mod.rs:32-39` for the page origin,
// `scripts/core.js:13-20` for `convertFileSrc`, `scripts/ipc-protocol.js:37`
// for the fetch and `:59-68` for the postMessage fallback.

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { packagedCsp, parseCsp } from "./packaged-csp-lib.mjs";

const parse = parseCsp;

test("connect-src covers the origins Tauri's IPC actually uses", async () => {
  const connect = parse(await packagedCsp()).get("connect-src");
  assert.ok(connect, "connect-src is missing");
  // `tauri-2.12.5/src/protocol/mod.rs:32-39` — page origin. `'self'` covers this.
  // `scripts/ipc-protocol.js:37` — `fetch(convertFileSrc(cmd, 'ipc'))`, which is
  // `http(s)://ipc.localhost` on Windows and `ipc://localhost` elsewhere. A
  // different host from the page, so `'self'` does not reach it and every
  // `invoke` in a packaged build was refused by the browser.
  for (const origin of ["ipc:", "http://ipc.localhost", "https://ipc.localhost"]) {
    assert.ok(connect.includes(origin), `connect-src is missing ${origin}; invoke() would be blocked`);
  }
  // `.localhost` is RFC 6761 loopback, so widening to both schemes costs nothing
  // and survives a Tauri config that switches to https.
  assert.ok(!connect.includes("*"), "connect-src must not carry a wildcard");
});

test("the packaged policy blocks inline script and plugin objects", async () => {
  const directives = parse(await packagedCsp());
  const script = directives.get("script-src");
  assert.ok(script, "script-src is missing");
  // The renderer shows model-produced markdown and mermaid SVG through
  // `dangerouslySetInnerHTML`, so inline script is the one thing that must stay
  // refused; the nonce/hash sources Tauri would add are exactly what `csp: null`
  // avoids, and they are not needed once `'unsafe-inline'` is gone.
  assert.ok(!script.includes("'unsafe-inline'"), "script-src must not allow inline script");
  assert.ok(script.includes("'self'"), "script-src must still allow the bundle");
  assert.deepEqual(directives.get("object-src"), ["'none'"], "object-src must be 'none'");
  assert.ok(directives.get("base-uri")?.includes("'self'"), "base-uri must be 'self'");
  assert.ok(directives.get("form-action")?.includes("'self'"), "form-action must be 'self'");
});

test("style-src keeps 'unsafe-inline', which is why tauri.conf.json leaves csp null", async () => {
  const style = parse(await packagedCsp()).get("style-src");
  assert.ok(style?.includes("'unsafe-inline'"), "React and mermaid write inline style attributes");
});
