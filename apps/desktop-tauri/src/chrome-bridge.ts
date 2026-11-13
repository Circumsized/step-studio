/**
 * The `ompStudioChrome` global for the Tauri shell (M1.2c).
 *
 * Same shape and same reason for existing as the Electron preload's
 * `apps/desktop/src/chrome-api.ts`: a narrow, named surface of shell
 * operations. There is no generic `invoke` escape hatch, so the renderer can
 * only address the operations declared here.
 *
 * Only the shell-level subset is present. The auto-updater half and the
 * Host-backed methods (artifacts, media upload, prediction, browser
 * observation, service definitions) are absent on purpose: the first is
 * packaging rather than a product surface, the second needs a Host this shell
 * does not run. The renderer treats every one of them as optional and draws
 * its honest unavailable state, exactly as it does when the preload is missing.
 *
 * `platform` is derived from the renderer's own user agent rather than a
 * command, because it is a synchronous readonly field and the WebView engine
 * is what fixes the answer: WebView2 → Windows, WKWebView → macOS,
 * WebKitGTK → Linux. The Electron preload pins the same value from
 * `process.platform`.
 */

import { invoke as tauriInvoke } from "@tauri-apps/api/core";

/** Mirrors `ChromeAvatarBytes` in `apps/desktop/src/chrome-profile-shared.ts`. */
export interface AvatarBytes {
  readonly mime: "image/jpeg" | "image/webp" | "image/png";
  readonly bytes: Uint8Array;
}

export interface ConversationViewState {
  readonly runtimeEpoch: number;
  readonly visibleSessionIds: readonly string[];
  readonly blockedSessionIds?: readonly string[];
}

export interface TauriChromeBridge {
  readonly platform: "win32" | "darwin" | "linux";
  openUrl(payload: { url: string }): Promise<void>;
  notify(payload: { title: string; body?: string }): Promise<void>;
  setTheme(theme: "light" | "dark", tint?: { color: string; symbolColor: string }): Promise<void>;
  setConversationViewState(state: ConversationViewState): Promise<boolean>;
  reportPayloadHealth(status: "ready" | "failed"): Promise<boolean>;
  reportPerformanceSample(sample: unknown): Promise<boolean>;
  saveAvatar(input: AvatarBytes): Promise<{ ok: true }>;
  loadAvatar(): Promise<AvatarBytes | null>;
  clearAvatar(): Promise<void>;
  openProjectDirectory(workspaceId: string): Promise<void>;
  openProjectInEditor(workspaceId: string): Promise<{ mode: "editor" | "reveal" }>;
}

/**
 * Exported for the anti-drift test, which feeds it the three WebView user
 * agents directly rather than reaching for a stubbed `navigator`.
 */
export function detectPlatform(agent: string): "win32" | "darwin" | "linux" {
  if (agent.includes("Windows")) return "win32";
  if (agent.includes("Macintosh") || agent.includes("Mac OS")) return "darwin";
  return "linux";
}

/**
 * Tauri maps an `invoke` argument object onto the Rust command's parameter
 * names, so each method passes exactly the names its command declares.
 *
 * Bytes cross as a plain array in both directions. Tauri's IPC does carry
 * `ArrayBuffer`, but relying on that makes the bridge's contract depend on
 * which transport is in play; a JSON array of numbers is what `Vec<u8>` always
 * means, and the renderer's `Uint8Array` is rebuilt explicitly here.
 */
export function createTauriChromeBridge(): TauriChromeBridge {
  return {
    platform: detectPlatform(navigator.userAgent),

    openUrl: payload => tauriInvoke<void>("open_url", { url: payload.url }),

    // The `workspaceId` is the project's absolute path, and the shell validates
    // it as an existing directory before handing it to the OS opener — this
    // string arrives from the renderer, so it is not trusted on the way out.
    openProjectDirectory: workspaceId =>
      tauriInvoke<void>("open_project_directory", { workspaceId }),
    openProjectInEditor: workspaceId =>
      tauriInvoke<{ mode: "editor" | "reveal" }>("open_project_in_editor", { workspaceId }),

    notify: payload =>
      tauriInvoke<void>(
        "notify",
        payload.body === undefined
          ? { title: payload.title }
          : { title: payload.title, body: payload.body },
      ),

    // The tint is accepted and dropped: under a pure-Tauri shell there is no
    // native titlebar overlay to colour.
    setTheme: theme => tauriInvoke<void>("set_theme", { theme }),

    setConversationViewState: state =>
      tauriInvoke<boolean>("set_conversation_view_state", { state }),

    reportPayloadHealth: status => tauriInvoke<boolean>("report_payload_health", { status }),

    reportPerformanceSample: sample =>
      tauriInvoke<boolean>("report_performance_sample", { sample }),

    saveAvatar: input =>
      tauriInvoke<{ ok: true }>("save_avatar", {
        mime: input.mime,
        bytes: Array.from(input.bytes),
      }),

    loadAvatar: async () => {
      const loaded = await tauriInvoke<{ mime: AvatarBytes["mime"]; bytes: number[] } | null>(
        "load_avatar",
      );
      if (loaded === null) return null;
      return { mime: loaded.mime, bytes: new Uint8Array(loaded.bytes) };
    },

    clearAvatar: () => tauriInvoke<void>("clear_avatar"),
  };
}
