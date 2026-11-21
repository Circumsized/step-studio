/**
 * Tauri v2 bridge init script (design/plan.md §5.2).
 *
 * Injected as the window's `initialization_script`, i.e. before the renderer
 * bundle runs. Tauri has no `contextBridge` and no isolated world: this runs
 * in the MAIN world, the same one as the bundle. So the exposed object is
 * made non-writable and non-configurable explicitly, which Electron's
 * contextBridge used to do for us. That is hardening, not a trust boundary —
 * page script can still reach `window.__TAURI_INTERNALS__` directly, which
 * is why plan §8 keeps the real invariant in the Rust facade and treats the
 * Tauri ACL as no security boundary at all.
 *
 * The frozen five-method surface, the fixed channel constants and the
 * per-scope event filtering are NOT reimplemented here: they come from
 * `createDesktopIpcBridge`, the same Electron-free module the Electron
 * preload uses, already unit-tested against a fake sender. This file only
 * supplies the Tauri sender and installs the result on the one global name
 * the renderer reads (`apps/renderer/src/main.tsx:74`).
 *
 * M1.1 exposed `stepStudio` only. M1.2c adds `stepStudioChrome` (the shell-level
 * subset — see `chrome-bridge.ts`) and `stepStudioTerminal` (a real PTY — see
 * `terminal-bridge.ts`). `stepStudioRemote` is still absent; the renderer treats
 * all three as optional, so their absence degrades instead of breaking.
 */

import { createDesktopIpcBridge, DESKTOP_BRIDGE_GLOBAL } from "@step-studio/transport-desktop";

import { createTauriChromeBridge } from "./chrome-bridge.js";
import { createTauriTerminalBridge } from "./terminal-bridge.js";
import { createTauriDesktopSender } from "./tauri-sender.js";

const api = createDesktopIpcBridge(createTauriDesktopSender());

Object.defineProperty(window, DESKTOP_BRIDGE_GLOBAL, {
  value: api,
  writable: false,
  configurable: false,
  enumerable: true,
});

Object.defineProperty(window, "stepStudioChrome", {
  value: createTauriChromeBridge(),
  writable: false,
  configurable: false,
  enumerable: true,
});

Object.defineProperty(window, "stepStudioTerminal", {
  value: createTauriTerminalBridge(),
  writable: false,
  configurable: false,
  enumerable: true,
});
