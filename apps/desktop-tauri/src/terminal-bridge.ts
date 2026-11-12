/**
 * The `ompStudioTerminal` global for the Tauri shell (M1.2c).
 *
 * Mirrors the shape in `apps/renderer/src/vite-env.d.ts:285-298` exactly, because
 * `terminalSessions.tsx` and `TerminalPane.tsx` consume it structurally.
 *
 * Two adaptations, both forced by a real difference from Electron:
 *
 * 1. **Events fan out from one `listen` per channel.** Same reason as the desktop
 *    sender: Tauri's `listen` returns a promise, so a per-subscription
 *    registration would leave a window where `removeListener` finds nothing to
 *    remove and the listener lives for the window's lifetime. Registering once
 *    and fanning out to a `Set` keeps `onData`/`onExit` synchronous and
 *    identity-exact.
 *
 * 2. **`recordingStart`/`Stop`/`Status` are absent.** They mint an `artifactId`
 *    from the Host's artifact library, which this shell does not run. The
 *    renderer reads their absence as "unavailable" and disables the record
 *    button, which is the honest state.
 *
 * `create` accepts and ignores `workspaceId`: mapping a workspace id to a
 * directory needs the Host's workspace registry. The terminal starts in the
 * shell's own working directory instead — a real gap against omp-studio, not a
 * simplification to be proud of.
 */

import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";

/** Must stay byte-identical to the Rust constants in `terminal.rs`. */
export const TERMINAL_DATA_CHANNEL = "omp-studio:terminal:data";
export const TERMINAL_EXIT_CHANNEL = "omp-studio:terminal:exit";

/** Tauri delivers `Event<T>`; the bridge's listener wants the bare payload. */
interface TauriEventLike<T> {
  readonly event: string;
  readonly payload: T;
}

export interface TauriListenLike {
  <T>(event: string, handler: (event: TauriEventLike<T>) => void): Promise<() => void>;
}

export interface TerminalSize {
  readonly cols?: number;
  readonly rows?: number;
  readonly targetId?: string;
  readonly workspaceId?: string;
}

export interface TauriTerminalBridge {
  create(size?: TerminalSize): Promise<{ id: string; name: string; cwd: string }>;
  write(id: string, data: string): Promise<void>;
  resize(id: string, cols: number, rows: number): Promise<void>;
  dispose(id: string): Promise<void>;
  onData(listener: (event: { id: string; data: string }) => void): () => void;
  onExit(listener: (event: { id: string }) => void): () => void;
}

/** One registration per channel, fanned out to a Set of listeners. */
function fanOut<T>(
  channel: string,
  listen: TauriListenLike,
): { add(listener: (event: T) => void): void; remove(listener: (event: T) => void): void } {
  const listeners = new Set<(event: T) => void>();
  // Resolving before the first subscribe closes the window where an event
  // emitted between registration and resolution would be dropped. The catch
  // keeps a missing Tauri runtime (plain-browser dev) from becoming an
  // unhandled rejection.
  const listening: Promise<() => void> = listen<T>(channel, event => {
    for (const listener of [...listeners]) listener(event.payload);
  }).catch(() => () => {});
  void listening;
  return {
    add: listener => listeners.add(listener),
    remove: listener => listeners.delete(listener),
  };
}

export function createTauriTerminalBridge(
  listen: TauriListenLike = tauriListen,
): TauriTerminalBridge {
  const data = fanOut<{ id: string; data: string }>(TERMINAL_DATA_CHANNEL, listen);
  const exit = fanOut<{ id: string }>(TERMINAL_EXIT_CHANNEL, listen);

  return {
    create: size => tauriInvoke<{ id: string; name: string; cwd: string }>("create", { size }),
    write: (id, payload) => tauriInvoke<void>("write", { id, data: payload }),
    resize: (id, cols, rows) => tauriInvoke<void>("resize", { id, cols, rows }),
    dispose: id => tauriInvoke<void>("dispose", { id }),

    onData: listener => {
      data.add(listener);
      return () => data.remove(listener);
    },
    onExit: listener => {
      exit.add(listener);
      return () => exit.remove(listener);
    },
  };
}
