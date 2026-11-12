/**
 * The `ompStudioChrome` bridge: what it exposes, and that every command it
 * reaches actually exists on the Rust side.
 *
 * The second half is the point. A renamed `#[tauri::command]` or a dropped
 * argument key compiles, typechecks, and then fails only at runtime with an
 * opaque Tauri error — the same class of drift the `command-table` test guards
 * for the Host bridge. Both sources are read, so the assertion spans the
 * language boundary.
 */

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, test } from "node:test";

import { createTauriChromeBridge, detectPlatform } from "../src/chrome-bridge.js";

// The suite runs from `dist/test`, so the sources are two levels up — the same
// resolution `command-table.test.ts` uses for the Rust side.
const packageRoot = join(import.meta.dirname, "..", "..");
const bridgeSource = readFileSync(join(packageRoot, "src", "chrome-bridge.ts"), "utf8");
const rustSource = readFileSync(join(packageRoot, "src-tauri", "src", "chrome.rs"), "utf8");

/** Every `#[tauri::command] pub fn NAME` the chrome module declares. */
function rustCommands(): Set<string> {
  const names = [...rustSource.matchAll(/#\[tauri::command\]\s*\npub fn (\w+)/g)].map(m => m[1] ?? "");
  assert.ok(names.length > 0, "no #[tauri::command] found in chrome.rs");
  return new Set(names);
}

/** Every `tauriInvoke<...>("name"` the bridge issues. */
function invokedCommands(): Set<string> {
  const names = [...bridgeSource.matchAll(/tauriInvoke<[^>]*>\(\s*"(\w+)"/g)].map(m => m[1] ?? "");
  assert.ok(names.length > 0, "no tauriInvoke calls found in chrome-bridge.ts");
  return new Set(names);
}

describe("the chrome bridge's surface", () => {
  test("it exposes exactly the shell-level subset", () => {
    const keys = Object.keys(createTauriChromeBridge()).sort();
    assert.deepEqual(keys, [
      "clearAvatar",
      "loadAvatar",
      "notify",
      "openUrl",
      "platform",
      "reportPayloadHealth",
      "reportPerformanceSample",
      "saveAvatar",
      "setConversationViewState",
      "setTheme",
    ]);
  });

  test("the Host-backed and updater methods stay absent", () => {
    const bridge = createTauriChromeBridge() as unknown as Record<string, unknown>;
    // The renderer reads their absence as "unavailable" and degrades, which is
    // the honest state for a shell that runs no Host.
    for (const absent of ["beginMediaUpload", "importArtifact", "getUpdateSnapshot", "attachPrediction", "requestMicrophoneAccess"]) {
      assert.equal(bridge[absent], undefined, `${absent} must not be exposed`);
    }
  });
});

describe("platform detection", () => {
  test("each WebView engine reports its own OS", () => {
    assert.equal(detectPlatform("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/120 Safari/537.36 Edg/120"), "win32");
    assert.equal(detectPlatform("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15"), "darwin");
    assert.equal(detectPlatform("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15"), "linux");
  });

  test("the live platform is one the contract allows", () => {
    assert.ok(["win32", "darwin", "linux"].includes(createTauriChromeBridge().platform));
  });
});

describe("command-name drift across the language boundary", () => {
  test("every command the bridge invokes is declared in Rust", () => {
    const declared = rustCommands();
    for (const name of invokedCommands()) {
      assert.ok(declared.has(name), `chrome-bridge.ts invokes "${name}" but chrome.rs declares no such command`);
    }
  });

  test("every command chrome.rs declares is reached by the bridge", () => {
    // The other direction matters too: a command nobody calls is dead weight
    // that still has to be kept in sync with the ACL.
    const invoked = invokedCommands();
    for (const name of rustCommands()) {
      assert.ok(invoked.has(name), `chrome.rs declares "${name}" but chrome-bridge.ts never invokes it`);
    }
  });
});
