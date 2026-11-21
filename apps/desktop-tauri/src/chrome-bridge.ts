/**
 * The `stepStudioChrome` global for the Tauri shell (M1.2c).
 *
 * Same shape and same reason for existing as the Electron preload's
 * `apps/desktop/src/chrome-api.ts`: a narrow, named surface of shell
 * operations. There is no generic `invoke` escape hatch, so the renderer can
 * only address the operations declared here.
 *
 * Only the shell-level subset is present. The auto-updater half and the
 * Host-backed methods (media upload, prediction, browser observation, service
 * definitions) are absent on purpose: the first is packaging rather than a
 * product surface, the second needs a Host this shell does not run. The
 * renderer treats every one of them as optional and draws its honest
 * unavailable state, exactly as it does when the preload is missing.
 *
 * The three artifact members are the exception, and they are here because the
 * shell owns the artifact library itself: a catalog plus content-addressed
 * payloads under `.step-studio/artifact-library/v1`, ported from the Electron
 * host's `ArtifactLibrary`. Reading and deleting reach it through the bridge's
 * `artifacts.list` / `artifacts.storage.get` / `artifacts.delete`; these three
 * are the parts that need an OS dialog.
 *
 * `platform` is derived from the renderer's own user agent rather than a
 * command, because it is a synchronous readonly field and the WebView engine
 * is what fixes the answer: WebView2 → Windows, WKWebView → macOS,
 * WebKitGTK → Linux. The Electron preload pins the same value from
 * `process.platform`.
 */

import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";

/**
 * The channel the Rust menu emits on. Kept in step with
 * `src-tauri/src/menu.rs`; a drift there is a menu that appears to do nothing.
 */
const MENU_COMMAND_EVENT = "step-studio:chrome:menu-command";

/**
 * The channel browser-observation events arrive on. Byte-identical to the
 * Electron preload's (`browser-observation-shared.ts`), so the renderer's
 * subscription is the same on both shells.
 */
const BROWSER_EVENT_CHANNEL = "step-studio:desktop:browser-event";

/**
 * The channel prediction events arrive on. Byte-identical to the Electron
 * preload's (`prediction-shared.ts`).
 */
const PREDICTION_EVENT_CHANNEL = "step-studio:desktop:prediction-event";

/** Mirrors `ArtifactRecord` from `@step-studio/studio-protocol`. */
export interface ArtifactRecord {
  readonly artifactId: string;
  readonly kind: string;
  readonly name: string;
  readonly mimeType: string;
  readonly bytes: number;
  readonly createdAt: string;
  readonly workspaceId?: string;
  readonly sessionId?: string;
  readonly sha256: string;
}

/** Mirrors `PredictionResult` in `apps/desktop/src/prediction-shared.ts`. */
export type PredictionResult = {
  readonly ok: boolean;
  readonly cancelled?: boolean;
  readonly message?: string;
  readonly count?: number;
  readonly truncated?: boolean;
};

/** Mirrors `PredictionAttach` in the same file. */
export interface PredictionAttach {
  readonly channelId: string;
  readonly sessionId: string;
  readonly expiresAt: number;
}

/** Mirrors `PredictionControl` in the same file. */
export interface PredictionControl {
  readonly channelId: string;
  readonly input:
    | { readonly kind: "steer"; readonly text: string }
    | { readonly kind: "cancel" }
    | { readonly kind: "release" }
    | { readonly kind: "ping" };
}

/** Mirrors `PredictionEvent` from `@step-studio/studio-protocol`. */
export type PredictionEvent =
  | { readonly kind: "closed"; readonly channelId: string }
  | { readonly kind: "error"; readonly channelId: string; readonly message: string }
  | { readonly kind: "imported"; readonly channelId: string; readonly requestId: string; readonly count: number; readonly truncated: boolean };

/** Mirrors `MediaUploadResult` in `apps/desktop/src/media-upload-shared.ts`. */
export type MediaUploadResult =
  | {
      readonly ok: true;
      readonly uploadId?: string;
      readonly artifact?: ArtifactRecord;
    }
  | { readonly ok: false; readonly message: string };

/** Mirrors `MediaUploadInput` in the same file. */
export interface MediaUploadInput {
  readonly kind: "image" | "video" | "audio";
  readonly name: string;
  readonly mimeType: string;
  readonly workspaceId?: string;
  readonly sessionId?: string;
}

/** Mirrors `LiveAudioResult` in `apps/desktop/src/live-audio-shared.ts`. */
export type LiveAudioResult = { readonly ok: boolean; readonly message?: string };

/** Mirrors `LiveAudioAttach` in the same file. */
export interface LiveAudioAttach {
  readonly audioId: string;
  readonly sessionId: string;
}

/** Mirrors `LiveAudioChunk` in the same file. */
export interface LiveAudioChunk {
  readonly audioId: string;
  readonly sequence: number;
  readonly bytes: ArrayBuffer;
}

/** Mirrors `BrowserObservationResult` in `apps/desktop/src/browser-observation-shared.ts`. */
export type BrowserObservationResult = { readonly ok: boolean; readonly message?: string };

/** Mirrors `BrowserObservationAttach` in the same file. */
export interface BrowserObservationAttach {
  readonly observationId: string;
  readonly sessionId: string;
  readonly tabId: string;
}

/** Mirrors `BrowserObservationControl` in the same file. */
export interface BrowserObservationControl {
  readonly observationId: string;
  readonly input: BrowserObservationInput;
}

/** Mirrors `BrowserObservationInput` from `@step-studio/studio-protocol`. */
export type BrowserObservationInput =
  | { readonly kind: "ping" }
  | { readonly kind: "take" }
  | { readonly kind: "release" }
  | { readonly kind: "ack"; readonly sequence: number }
  | { readonly kind: "navigate"; readonly url: string }
  | { readonly kind: "click"; readonly sequence: number; readonly x: number; readonly y: number; readonly button: "left" | "right" }
  | { readonly kind: "wheel"; readonly sequence: number; readonly x: number; readonly y: number; readonly deltaX: number; readonly deltaY: number }
  | { readonly kind: "key"; readonly sequence: number; readonly key: string; readonly code: string; readonly modifiers: number }
  | { readonly kind: "text"; readonly sequence: number; readonly text: string };

/** Mirrors `BrowserObservationEvent` from `@step-studio/studio-protocol`. */
export type BrowserObservationEvent =
  | { readonly kind: "state"; readonly observationId: string; readonly tabId: string; readonly title: string; readonly url: string; readonly frozen: boolean; readonly busy: boolean; readonly control: "agent" | "waiting" | "human" | "other-window" }
  | { readonly kind: "frame"; readonly observationId: string; readonly tabId: string; readonly sequence: number; readonly data: string; readonly width: number; readonly height: number; readonly timestamp: number }
  | { readonly kind: "error"; readonly observationId: string; readonly message: string }
  | { readonly kind: "closed"; readonly observationId: string };

/** Mirrors `ComputerCaptureRequest` in `apps/desktop/src/computer-observation-shared.ts`. */
export interface ComputerCaptureRequest {
  readonly captureId: string;
  readonly sessionId: string;
  readonly targetId: string;
}

/** Mirrors `ComputerCaptureResult` in the same file. */
export type ComputerCaptureResult =
  | { readonly ok: true; readonly data: ArrayBuffer; readonly width: number; readonly height: number }
  | { readonly ok: false; readonly message: string };

/** Mirrors `ServiceDefinitionResult` in `apps/desktop/src/chrome-services-shared.ts`. */
export type ServiceDefinitionResult =
  | { readonly ok: true; readonly definitions: readonly ServiceDefinition[] }
  | { readonly ok: false; readonly message: string };

/** Mirrors `ServiceDefinition` in `apps/desktop/src/service-definitions.ts`. */
export interface ServiceDefinition {
  readonly id: string;
  readonly revision: number;
  readonly workspaceId: string;
  readonly updatedAt: string;
  readonly spec: {
    readonly name: string;
    readonly command: string;
    readonly cwd?: string | undefined;
    readonly env?: Record<string, string> | undefined;
    readonly pty?: boolean | undefined;
    readonly ready?:
      | { readonly log?: string | undefined; readonly port?: number | undefined; readonly host?: string | undefined; readonly timeoutMs: number }
      | undefined;
    readonly restart?: "no" | "on-failure" | "always" | undefined;
    readonly mode?: "session" | "persist" | "detached" | undefined;
  };
}

/** Mirrors `ServiceDefinitionsInput` in the same file. */
export interface ServiceDefinitionsInput {
  readonly workspaceId: string;
  readonly id?: string;
  readonly revision?: number;
  readonly spec?: ServiceDefinition["spec"];
}

/** Mirrors `ArtifactFileResult` in `apps/desktop/src/chrome-artifacts-shared.ts`. */
export type ArtifactFileResult =
  | {
      readonly ok: true;
      readonly cancelled: boolean;
      readonly artifact?: {
        readonly artifactId: string;
        readonly kind: string;
        readonly name: string;
        readonly mimeType: string;
        readonly bytes: number;
        readonly createdAt: string;
        readonly workspaceId?: string;
        readonly sessionId?: string;
        readonly sha256: string;
      };
      readonly storage?: {
        readonly locationName: string;
        readonly writable: boolean;
        readonly total: number;
        readonly totalBytes: number;
      };
    }
  | { readonly ok: false; readonly message: string };

/** Mirrors `ArtifactImportInput` in the same file. */
export interface ArtifactImportInput {
  readonly kind: string;
  readonly sessionId?: string;
  readonly workspaceId?: string;
}

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
  openLogDir(): Promise<{ readonly ok: true } | { readonly ok: false; readonly message: string }>;

  /**
   * Native menu commands. The listener gets an allowlisted id and returns an
   * unsubscribe function, matching the Electron preload's shape.
   */
  onMenuCommand(listener: (command: string) => void): () => void;
  revealSkillshareToken(input: {
    secretId: string;
    sessionId: string;
  }): Promise<{ ok: true; token: string; name: string } | { ok: false; message: string }>;
  /**
   * The prediction channel: a transport to a socket the runtime owns, the same
   * shape as browser observation. `predictionInput` refuses `import` — a history
   * import needs the file picker, which only `importPredictionHistory` has, so a
   * renderer cannot name a file on the machine.
   */
  attachPrediction(input: PredictionAttach): Promise<PredictionResult>;
  predictionInput(input: PredictionControl): Promise<PredictionResult>;
  detachPrediction(input: { channelId: string }): Promise<PredictionResult>;
  importPredictionHistory(input: { channelId: string }): Promise<PredictionResult>;
  onPrediction(listener: (event: PredictionEvent) => void): () => void;
  /**
   * Media upload: a chunked stream that lands in the artifact library. Four
   * members, and the ids the renderer sends chunks against come from `begin`, so
   * a renderer cannot finish or abort an upload it did not start.
   */
  beginMediaUpload(input: MediaUploadInput): Promise<MediaUploadResult>;
  appendMediaUpload(input: { uploadId: string; sequence: number; bytes: ArrayBuffer }): Promise<MediaUploadResult>;
  finishMediaUpload(input: { uploadId: string }): Promise<MediaUploadResult>;
  abortMediaUpload(input: { uploadId: string }): Promise<MediaUploadResult>;
  /**
   * Live audio: a PCM transport to a socket the runtime owns. The capture is the
   * WebView's `getUserMedia`; these members relay validated frames to it.
   */
  attachLiveAudio(input: LiveAudioAttach): Promise<LiveAudioResult>;
  appendLiveAudio(input: LiveAudioChunk): Promise<LiveAudioResult>;
  detachLiveAudio(input: { audioId: string }): Promise<LiveAudioResult>;
  /**
   * Microphone permission. `granted` everywhere, because the prompt belongs to
   * the WebView engine and there is no shell-level state to report.
   */
  requestMicrophoneAccess(): Promise<"granted" | "denied">;
  openMicrophoneSettings(): Promise<boolean>;
  /**
   * Browser observation. The shell is a transport here, not a browser: the
   * runtime owns the tab and hands over a private socket, and these four members
   * relay to it. `attach` names the observation the runtime prepared, so a
   * renderer cannot invent one.
   */
  attachBrowserObservation(input: BrowserObservationAttach): Promise<BrowserObservationResult>;
  controlBrowserObservation(input: BrowserObservationControl): Promise<BrowserObservationResult>;
  detachBrowserObservation(input: { observationId: string }): Promise<BrowserObservationResult>;
  onBrowserObservation(listener: (event: BrowserObservationEvent) => void): () => void;
  /**
   * One computer capture, read once. The bytes are the PNG itself.
   */
  readComputerCapture(input: ComputerCaptureRequest): Promise<ComputerCaptureResult>;
  /**
   * Saved service configurations. Each answers with the Electron preload's shape
   * (`ServiceDefinitionResult` in `apps/desktop/src/chrome-services-shared.ts`),
   * so the renderer's handling is identical on both shells. `ok: false` carries a
   * message that already says what to do — the shell distinguished "no keystore
   * here" from "the keystore is locked" from "the store will not open" on purpose,
   * because only the middle one is fixed by the user granting access.
   */
  listServiceDefinitions(input: ServiceDefinitionsInput): Promise<ServiceDefinitionResult>;
  saveServiceDefinition(input: ServiceDefinitionsInput): Promise<ServiceDefinitionResult>;
  removeServiceDefinition(input: ServiceDefinitionsInput): Promise<ServiceDefinitionResult>;
  /**
   * The artifact library. Each answers with the Electron preload's shape
   * (`ArtifactFileResult` in `apps/desktop/src/chrome-artifacts-shared.ts`), so
   * the renderer's handling is identical on both shells.
   */
  chooseArtifactDirectory(): Promise<ArtifactFileResult>;
  importArtifact(input: ArtifactImportInput): Promise<ArtifactFileResult>;
  exportArtifact(input: { artifactId: string }): Promise<ArtifactFileResult>;
  exportLogs(): Promise<
    | { readonly ok: true }
    | { readonly ok: false; readonly cancelled: true }
    | { readonly ok: false; readonly message: string }
  >;
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

    // The log directory is `<cwd>/.step-studio`, derived the same way the bridge
    // derives it — the bridge is the one that writes the file, so it decides.
    openLogDir: () => tauriInvoke("open_log_dir"),
    chooseArtifactDirectory: () => tauriInvoke("choose_artifact_directory"),
    // Spread field by field, as the other argument-taking commands do: an
    // `interface` has no index signature, so it is not an `InvokeArgs`.
    importArtifact: input =>
      tauriInvoke("import_artifact", {
        kind: input.kind,
        ...(input.sessionId === undefined ? {} : { sessionId: input.sessionId }),
        ...(input.workspaceId === undefined ? {} : { workspaceId: input.workspaceId }),
      }),
    exportArtifact: input => tauriInvoke("export_artifact", input),
    exportLogs: () => tauriInvoke("export_logs"),

    // The shell builds a native menu and emits the allowlisted id; this only
    // subscribes. The id is not narrowed here because the shell already enforces
    // the closed set — an id outside it never reaches the channel.
    // A one-time secret handoff. The token comes back over this channel and is
    // never routed through Host receipts or snapshots: those are logged, and a
    // token in a log is a leaked token.
    revealSkillshareToken: input =>
      tauriInvoke("reveal_skillshare_token", {
        secretId: input.secretId,
        sessionId: input.sessionId,
      }),
    // Spread field by field: a `readonly` interface has no index signature, so it
    // is not an `InvokeArgs`. Only the keys the caller actually set are sent,
    // because the shell refuses an unexpected one — `{...input, spec: undefined}`
    // would be "unknown field" on a remove.
    listServiceDefinitions: ({ workspaceId }) =>
      tauriInvoke("list_service_definitions", { workspaceId }),
    saveServiceDefinition: input =>
      tauriInvoke("save_service_definition", {
        workspaceId: input.workspaceId,
        ...(input.id === undefined ? {} : { id: input.id }),
        ...(input.revision === undefined ? {} : { revision: input.revision }),
        ...(input.spec === undefined ? {} : { spec: input.spec }),
      }),
    removeServiceDefinition: ({ workspaceId, id, revision }) =>
      tauriInvoke("remove_service_definition", { workspaceId, id, revision }),
    attachPrediction: input =>
      tauriInvoke("attach_prediction", {
        channelId: input.channelId,
        sessionId: input.sessionId,
        expiresAt: input.expiresAt,
      }),
    predictionInput: ({ channelId, input: action }) =>
      tauriInvoke("prediction_input", { channelId, input: action }),
    detachPrediction: ({ channelId }) => tauriInvoke("detach_prediction", { channelId }),
    importPredictionHistory: ({ channelId }) =>
      tauriInvoke("import_prediction_history", { channelId }),

    beginMediaUpload: input =>
      tauriInvoke("begin_media_upload", {
        kind: input.kind,
        name: input.name,
        mimeType: input.mimeType,
        ...(input.sessionId === undefined ? {} : { sessionId: input.sessionId }),
        ...(input.workspaceId === undefined ? {} : { workspaceId: input.workspaceId }),
      }),
    appendMediaUpload: input =>
      tauriInvoke("append_media_upload", {
        uploadId: input.uploadId,
        sequence: input.sequence,
        bytes: input.bytes,
      }),
    finishMediaUpload: ({ uploadId }) => tauriInvoke("finish_media_upload", { uploadId }),
    abortMediaUpload: ({ uploadId }) => tauriInvoke("abort_media_upload", { uploadId }),
    attachLiveAudio: ({ audioId, sessionId }) =>
      tauriInvoke("attach_live_audio", { audioId, sessionId }),
    appendLiveAudio: ({ audioId, sequence, bytes }) =>
      tauriInvoke("append_live_audio", { audioId, sequence, bytes }),
    detachLiveAudio: ({ audioId }) => tauriInvoke("detach_live_audio", { audioId }),
    requestMicrophoneAccess: () => tauriInvoke("request_microphone_access"),
    openMicrophoneSettings: () => tauriInvoke("open_microphone_settings_command"),

    attachBrowserObservation: input =>
      tauriInvoke("attach_browser_observation", {
        observationId: input.observationId,
        sessionId: input.sessionId,
        tabId: input.tabId,
      }),
    controlBrowserObservation: ({ observationId, input: action }) =>
      tauriInvoke("control_browser_observation", { observationId, input: action }),
    detachBrowserObservation: ({ observationId }) =>
      tauriInvoke("detach_browser_observation", { observationId }),
    readComputerCapture: ({ captureId, sessionId, targetId }) =>
      tauriInvoke("read_computer_capture_command", { captureId, sessionId, targetId }),

    // The shell relays to the owning window only, so another window's input
    // cannot move this one's page. The renderer filters by `observationId` on
    // top, which is what the pane's own ref check does.
    onBrowserObservation: listener => {
      // Same shape as :  resolves with the unlisten
      // function, and the contract's unsubscribe is synchronous, so the returned
      // closure captures whatever has resolved so far.
      let unsubscribe = () => {};
      void tauriListen<BrowserObservationEvent>(BROWSER_EVENT_CHANNEL, event => {
        listener(event.payload);
      }).then(unlisten => {
        unsubscribe = unlisten;
      });
      return () => unsubscribe();
    },

    // Same shape as `onMenuCommand`: `listen` resolves with the unlisten
    // function, and the contract's unsubscribe is synchronous.
    onPrediction: listener => {
      let unsubscribe = () => {};
      void tauriListen<PredictionEvent>(PREDICTION_EVENT_CHANNEL, event => {
        listener(event.payload);
      }).then(unlisten => {
        unsubscribe = unlisten;
      });
      return () => unsubscribe();
    },

    onMenuCommand: listener => {
      let unsubscribe = () => {};
      void tauriListen<string>(MENU_COMMAND_EVENT, event => {
        listener(event.payload);
      }).then(unlisten => {
        unsubscribe = unlisten;
      });
      return () => unsubscribe();
    },

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
