//! The renderer's only door to the desktop shell.
//!
//! Before this module existed, every feature asked `globalThis.ompStudioChrome`
//! for a method and branched on whether it came back. That is what made deleting
//! Electron cascade: each missing member took a whole component with it, because
//! "the method is not there" and "this machine cannot do that" were the same
//! check, and the only available response to the first was to render nothing.
//!
//! The rule now is one line: **no file outside this module reads
//! `globalThis.ompStudioChrome`.** A feature asks for a capability bit and
//! renders a degraded state when it is false, or calls a method it already knows
//! is present.
//!
//! The four groups are the traits the shell-side split is converging on. They are
//! types here and separate implementations there, so moving a capability between
//! them — or off the desktop shell entirely, as the browser group is — changes
//! this file and nothing else in the renderer.

/**
 * The surface, as the preload declares it. Absent on a plain browser build.
 *
 * Derived rather than imported: `vite-env.d.ts` declares `ompStudioChrome`
 * inline and exports no name for it, and exporting one would mean editing that
 * declaration every time a member changes.
 */
export type ChromeSurface = NonNullable<typeof globalThis.ompStudioChrome>;

/**
 * One bit per capability, not per method.
 *
 * A method going missing is a refactor; a capability going missing is a fact
 * about the machine, and that is the thing worth branching on. The bits are
 * derived from what is actually wired, so an older installed shell reports the
 * capabilities it has rather than the ones this build knows the names of.
 */
export interface ChromeCapabilities {
  // --- HostApi: git, files, storage, artifacts. Pure Rust on the shell side. ---
  readonly artifacts: boolean;
  readonly serviceDefinitions: boolean;
  readonly projectIntegration: boolean;
  // --- MediaApi: audio, upload, permissions. Rust plus on-demand processes. ---
  readonly mediaUpload: boolean;
  readonly liveAudio: boolean;
  readonly microphonePermission: boolean;
  // --- BrowserApi: browser observation. External process, attached on demand. ---
  readonly browserObservation: boolean;
  // --- The prediction channel: a transport to a socket the runtime owns. ---
  readonly prediction: boolean;
  // --- PlatformApi: menu, logs, dialogs. Tauri plugins plus an ACL. ---
  readonly logs: boolean;
  readonly menu: boolean;
  readonly platform: boolean;
  // Present on every surface that has a window at all, and reported so a caller
  // can tell "no shell" from "a shell without this".
  readonly window: boolean;
}

/** The shell's own surface, or `undefined` where there is no shell. */
export function shellSurface(): ChromeSurface | undefined {
  return globalThis.ompStudioChrome;
}

/** Whether a member is wired at all.
 *
 * Kept private: the whole point is that callers ask for a *capability*, not for
 * a member. Deriving the bits from member existence is the current mechanism,
 * and it is the one being replaced by a real manifest — at which point this
 * function goes away and the bits come off the wire.
 */
function wired(surface: ChromeSurface | undefined, name: keyof ChromeSurface): boolean {
  return surface !== undefined && surface[name] !== undefined;
}

/**
 * What this machine can do, as bits.
 *
 * A missing member means the capability is absent, which is the honest reading:
 * a shell that does not implement `beginMediaUpload` cannot start a media
 * upload, whatever the reason.
 */
export function shellCapabilities(): ChromeCapabilities {
  const shell = shellSurface();
  return {
    artifacts:
      wired(shell, "chooseArtifactDirectory") && wired(shell, "importArtifact") && wired(shell, "exportArtifact"),
    serviceDefinitions:
      wired(shell, "listServiceDefinitions") && wired(shell, "saveServiceDefinition") && wired(shell, "removeServiceDefinition"),
    projectIntegration: wired(shell, "openProjectInEditor") && wired(shell, "openProjectDirectory"),
    mediaUpload:
      wired(shell, "beginMediaUpload") && wired(shell, "appendMediaUpload") && wired(shell, "finishMediaUpload") && wired(shell, "abortMediaUpload"),
    liveAudio: wired(shell, "attachLiveAudio") && wired(shell, "appendLiveAudio") && wired(shell, "detachLiveAudio"),
    microphonePermission: wired(shell, "requestMicrophoneAccess") && wired(shell, "openMicrophoneSettings"),
    browserObservation:
      wired(shell, "attachBrowserObservation") && wired(shell, "readComputerCapture") && wired(shell, "controlBrowserObservation") && wired(shell, "detachBrowserObservation"),
    // `importPredictionHistory` is deliberately *not* in the group: it needs the
    // file picker, so a shell without it can still attach and steer. Including it
    // would report the whole channel unavailable for a missing import.
    prediction:
      wired(shell, "attachPrediction") && wired(shell, "predictionInput") && wired(shell, "detachPrediction") && wired(shell, "onPrediction"),
    logs: wired(shell, "openLogDir") && wired(shell, "exportLogs"),
    menu: wired(shell, "onMenuCommand"),
    platform: wired(shell, "platform"),
    window: shell !== undefined,
  };
}

/**
 * What to show where a capability is missing.
 *
 * The string is the whole point of this module: it is what a user reads instead
 * of a component that silently is not there. Written here so the wording is the
 * same everywhere, and so a capability that starts out degraded has one place to
 * become real.
 */
export function shellDegradedReason(capability: keyof ChromeCapabilities): string {
  switch (capability) {
    case "artifacts":
      return "当前外壳不支持产物管理";
    case "serviceDefinitions":
      return "当前外壳不支持服务定义";
    case "projectIntegration":
      return "当前外壳不支持在编辑器中打开项目";
    case "mediaUpload":
      return "当前外壳不支持媒体上传";
    case "liveAudio":
      return "当前外壳不支持实时音频";
    case "microphonePermission":
      return "当前外壳不支持麦克风授权管理";
    case "browserObservation":
      return "当前外壳不支持浏览器观测";
    case "prediction":
      return "当前外壳不支持输入预测";
    case "logs":
      return "当前外壳不支持日志目录";
    case "menu":
      return "当前外壳不支持菜单命令";
    case "platform":
      return "无法识别当前平台";
    case "window":
      return "不在桌面外壳中运行";
  }
}
