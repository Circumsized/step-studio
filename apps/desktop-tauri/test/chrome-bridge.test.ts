/**
 * The `stepStudioChrome` bridge: what it exposes, and that every command it
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
// Every module that declares chrome commands, not just `chrome.rs`: the artifact
// and service-definition members live in their own files, and reading one file
// would have reported them as unbacked invocations — a false alarm loud enough to
// be believed.
const RUST_COMMAND_MODULES = [
  "chrome.rs",
  "artifacts.rs",
  "service_definitions.rs",
  "browser_observation.rs",
  "media.rs",
  "prediction.rs",
];
const rustSources = RUST_COMMAND_MODULES.map(name =>
  readFileSync(join(packageRoot, "src-tauri", "src", name), "utf8"),
);

/** Every `#[tauri::command] pub fn NAME` those modules declare. */
function rustCommands(): Set<string> {
  const names = rustSources.flatMap(source =>
    [...source.matchAll(/#\[tauri::command\]\s*\npub fn (\w+)/g)].map(m => m[1] ?? ""),
  );
  assert.ok(names.length > 0, "no #[tauri::command] found in the shell's command modules");
  return new Set(names);
}

/**
 * Every `tauriInvoke("name"` the bridge issues, with or without a type argument.
 *
 * The type argument is optional and the later commands all omit it — TypeScript
 * infers it from the command's return type. The first version of this pattern
 * required `<...>`, so it silently stopped matching the moment someone wrote the
 * shorter form, and the drift guard went blind on exactly the commands added
 * after it. `export_logs`, `open_log_dir` and `reveal_skillshare_token` were all
 * invisible to it for two commits. A guard that stops matching is worse than no
 * guard: it reports green while checking nothing.
 */
function invokedCommands(): Set<string> {
  const names = [...bridgeSource.matchAll(/tauriInvoke(?:<[^>]*>)?\(\s*"(\w+)"/g)].map(m => m[1] ?? "");
  assert.ok(names.length > 0, "no tauriInvoke calls found in chrome-bridge.ts");
  return new Set(names);
}

describe("the chrome bridge's surface", () => {
  test("it exposes exactly the shell-level subset", () => {
    const keys = Object.keys(createTauriChromeBridge()).sort();
    assert.deepEqual(keys, [
      "abortMediaUpload",
      "appendLiveAudio",
      "appendMediaUpload",
      "attachBrowserObservation",
      "attachLiveAudio",
      "attachPrediction",
      "beginMediaUpload",
      "chooseArtifactDirectory",
      "clearAvatar",
      "controlBrowserObservation",
      "detachBrowserObservation",
      "detachLiveAudio",
      "detachPrediction",
      "exportArtifact",
      "exportLogs",
      "finishMediaUpload",
      "importArtifact",
      "importPredictionHistory",
      "listServiceDefinitions",
      "loadAvatar",
      "notify",
      "onBrowserObservation",
      "onMenuCommand",
      "onPrediction",
      "openLogDir",
      "openMicrophoneSettings",
      "openProjectDirectory",
      "openProjectInEditor",
      "openUrl",
      "platform",
      "predictionInput",
      "readComputerCapture",
      "removeServiceDefinition",
      "reportPayloadHealth",
      "reportPerformanceSample",
      "requestMicrophoneAccess",
      "revealSkillshareToken",
      "saveAvatar",
      "saveServiceDefinition",
      "setConversationViewState",
      "setTheme",
    ]);
  });

  test("the Host-backed and updater methods stay absent", () => {
    const bridge = createTauriChromeBridge() as unknown as Record<string, unknown>;
    // The renderer reads their absence as "unavailable" and degrades, which is
    // the honest state for a shell that runs no Host.
    //
    // The three artifact members are deliberately *not* in this list. They were
    // when the shell ran no Host, but the shell now owns the artifact library
    // itself, so they are shell-level operations like any other.
    for (const absent of ["getUpdateSnapshot"]) {
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
