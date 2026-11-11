/**
 * Tauri sender for the Desktop IPC bridge.
 *
 * The bridge itself — the frozen five-method surface, the fixed channel
 * constants, the per-scope event filtering — is NOT reimplemented here. It
 * comes from `createDesktopIpcBridge` in `@omp-studio/transport-desktop`,
 * the same module the Electron preload uses. This file only adapts Tauri's
 * `invoke`/`listen` to the `DesktopIpcBridge` sender shape that module
 * expects, so the renderer and `transport-desktop` keep a zero diff.
 *
 * Three adaptations, each forced by a real difference from Electron:
 *
 * 1. **Command names drop the channel family prefix.** Tauri namespaces
 *    plugin commands as `plugin:<name>|<cmd>`, so a colon-bearing command
 *    name would need an explicit rename on every `#[tauri::command]`. The
 *    table below is the single routing source and a test enforces that it
 *    stays complete in both directions.
 *
 * 2. **One `listen` per event channel, fanned out to a Set.** Electron's
 *    `ipcRenderer.on` registers a listener synchronously and
 *    `removeListener` takes it back synchronously. Tauri's `listen` returns
 *    a promise, so a per-subscription registration would leave a window
 *    where `removeListener` finds nothing to remove and the listener lives
 *    for the lifetime of the window. Registering once and fanning out keeps
 *    `on`/`removeListener` synchronous and identity-exact, with no map.
 *
 * 3. **Rejections are re-wrapped as `Error`.** Electron rejects with an
 *    `Error`; Tauri rejects with the deserialized Rust error, which for a
 *    struct `Err` is a plain `{code, message}` object. The renderer's
 *    `toClientError` treats a `{code, message}` shape as an already-typed
 *    client error, so passing Tauri's rejection through would surface the
 *    Host's real error code to the UI where Electron surfaced
 *    `TRANSPORT_ERROR`. Re-wrapping restores byte-identical behaviour.
 */

import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";

import {
  DESKTOP_BRIDGE_GLOBAL,
  DESKTOP_IPC_CHANNELS,
  type DesktopIpcBridge,
} from "@omp-studio/transport-desktop";

/** Tauri delivers `Event<T>`; the bridge's listener wants the bare payload. */
interface TauriEventLike<T> {
  readonly event: string;
  readonly payload: T;
}

export interface TauriInvokeLike {
  <T>(command: string, args?: Record<string, unknown>): Promise<T>;
}

export interface TauriListenLike {
  <T>(event: string, handler: (event: TauriEventLike<T>) => void): Promise<() => void>;
}

interface CommandRow {
  /** The `#[tauri::command]` name registered in `src-tauri/src/bridge.rs`. */
  readonly command: string;
  /** Argument names, in the order `invoke(channel, ...args)` supplies them. */
  readonly args: readonly string[];
}

/** The family prefix every Host channel carries; the command is the remainder. */
export const DESKTOP_COMMAND_PREFIX = "omp-studio:desktop:";

const COMMANDS: Readonly<Record<string, CommandRow>> = Object.freeze({
  [DESKTOP_IPC_CHANNELS.bootstrap]: { command: "bootstrap", args: [] },
  [DESKTOP_IPC_CHANNELS.query]: { command: "query", args: ["request"] },
  [DESKTOP_IPC_CHANNELS.command]: { command: "command", args: ["request"] },
  [DESKTOP_IPC_CHANNELS.subscribe]: { command: "subscribe", args: ["scope"] },
  [DESKTOP_IPC_CHANNELS.close]: { command: "close", args: [] },
});

function rowFor(channel: string): CommandRow {
  const row = COMMANDS[channel];
  if (row === undefined) {
    throw new Error(`tauri desktop ipc: unknown channel ${channel}`);
  }
  return row;
}

function toNamedArgs(names: readonly string[], args: readonly unknown[]): Record<string, unknown> {
  // Silently dropping an unnamed argument would change a call's meaning.
  if (args.length > names.length) {
    throw new Error(`tauri desktop ipc: ${args.length} arguments for ${names.length} names`);
  }
  const payload: Record<string, unknown> = {};
  names.forEach((name, index) => {
    if (index < args.length) payload[name] = args[index];
  });
  return payload;
}

function messageOf(error: unknown): string {
  if (typeof error === "string") return error;
  if (typeof error === "object" && error !== null) {
    const message = (error as { message?: unknown }).message;
    if (typeof message === "string") return message;
  }
  return "desktop ipc: host rejected the request";
}

/**
 * Build the `DesktopIpcBridge` sender over Tauri IPC. `invoke` and `listen`
 * are injectable so the unit tests never touch `window.__TAURI_INTERNALS__`.
 */
export function createTauriDesktopSender(
  invoke: TauriInvokeLike = tauriInvoke,
  listen: TauriListenLike = tauriListen,
): DesktopIpcBridge {
  const listeners = new Set<(event: unknown, ...args: readonly unknown[]) => void>();
  // One registration for the whole channel. Resolving it before the first
  // `subscribe` command closes the window where an event emitted between
  // registration and resolution would be dropped. The catch keeps a missing
  // `__TAURI_INTERNALS__` (plain-browser dev) from becoming an unhandled
  // rejection; such a window simply never receives events.
  const listening: Promise<() => void> = listen<unknown>(DESKTOP_IPC_CHANNELS.event, event => {
    for (const listener of [...listeners]) listener(event, event.payload);
  }).catch(() => () => {});

  return {
    invoke: async (channel, ...args) => {
      const row = rowFor(channel);
      if (channel === DESKTOP_IPC_CHANNELS.subscribe) await listening;
      try {
        return await invoke<unknown>(row.command, toNamedArgs(row.args, args));
      } catch (error) {
        throw error instanceof Error ? error : new Error(messageOf(error));
      }
    },

    on: (channel, listener) => {
      if (channel !== DESKTOP_IPC_CHANNELS.event) {
        throw new Error(`tauri desktop ipc: unknown event channel ${channel}`);
      }
      listeners.add(listener);
    },

    removeListener: (channel, listener) => {
      if (channel !== DESKTOP_IPC_CHANNELS.event) {
        throw new Error(`tauri desktop ipc: unknown event channel ${channel}`);
      }
      listeners.delete(listener);
    },
  };
}

