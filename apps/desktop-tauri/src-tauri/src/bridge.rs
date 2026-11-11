//! The Desktop IPC bridge, wired to a real Studio Bridge session.
//!
//! M1.2 replaces the M1.1 stubs. The session wiring is ported from
//! `crates/step-studio-shell/src/bridge_client.rs`, which already drove the
//! adapter; the difference is only where the event stream goes — that shell
//! sent over an mpsc channel to its own view, this one emits on the Tauri event
//! channel the renderer already listens to.
//!
//! **M1.2a lands `bootstrap` and `subscribe`.** `query` and `command` stay
//! stubs until M1.2b, which is where the renderer's `{name, input}` envelope
//! gets mapped onto Studio operations and the turn's event pump is built —
//! together, because a turn's events have to be emitted from the same place
//! that awaits its receipt.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use step_studio_bridge::{AdapterLaunch, BridgeSession};

/// Rejection payload. Tauri hands the deserialized error to JS, where the
/// sender re-wraps it as an `Error` so the UI sees a transport failure rather
/// than a typed client error.
#[derive(Serialize)]
pub struct IpcError {
    pub code: String,
    pub message: String,
}

impl IpcError {
    fn not_migrated(operation: &str) -> Self {
        Self {
            code: "DESKTOP_BRIDGE_NOT_MIGRATED".to_string(),
            message: format!("desktop bridge: {operation} is not migrated yet (M1.2b)"),
        }
    }
}

// --- the unavailable bootstrap, kept as the fallback -----------------------
//
// Mirrors `unavailableBootstrap()` in `apps/renderer/src/App.tsx:385-394`.
// When the session cannot be established — no Step CLI, no socket — the shell
// reports this instead of failing, and the renderer draws its honest empty
// state.

#[derive(Serialize)]
pub struct UnavailableBootstrap {
    #[serde(rename = "contractVersion")]
    pub contract_version: u32,
    pub authority: UnavailableAuthority,
    pub runtime: UnavailableRuntime,
    pub surface: UnavailableSurface,
    #[serde(rename = "capabilityManifest")]
    pub capability_manifest: UnavailableCapabilityManifest,
    #[serde(rename = "commandManifestHash")]
    pub command_manifest_hash: String,
    pub selected: serde_json::Value,
}

#[derive(Serialize)]
pub struct UnavailableAuthority {
    #[serde(rename = "authorityId")]
    pub authority_id: String,
    #[serde(rename = "authorityEpoch")]
    pub authority_epoch: u32,
}

#[derive(Serialize)]
pub struct UnavailableRuntime {
    pub status: String,
    pub classification: String,
}

#[derive(Serialize)]
pub struct UnavailableSurface {
    #[serde(rename = "terminalAttach")]
    pub terminal_attach: bool,
    #[serde(rename = "fileReveal")]
    pub file_reveal: bool,
    #[serde(rename = "previewInput")]
    pub preview_input: bool,
    #[serde(rename = "openExternal")]
    pub open_external: bool,
}

#[derive(Serialize)]
pub struct UnavailableCapabilityManifest {
    pub profile: String,
    #[serde(rename = "generatedAt")]
    pub generated_at: String,
    pub hash: String,
    pub capabilities: Vec<serde_json::Value>,
}

/// `CLIENT_CONTRACT_VERSION` from `packages/client-contract/src/index.ts:55`.
const CLIENT_CONTRACT_VERSION: u32 = 2;

fn unavailable_bootstrap() -> UnavailableBootstrap {
    UnavailableBootstrap {
        contract_version: CLIENT_CONTRACT_VERSION,
        authority: UnavailableAuthority {
            authority_id: "desktop-unavailable".to_string(),
            authority_epoch: 1,
        },
        runtime: UnavailableRuntime {
            status: "unavailable".to_string(),
            classification: "unavailable".to_string(),
        },
        surface: UnavailableSurface {
            terminal_attach: false,
            file_reveal: false,
            preview_input: false,
            open_external: false,
        },
        capability_manifest: UnavailableCapabilityManifest {
            profile: "limited".to_string(),
            generated_at: "1970-01-01T00:00:00.000Z".to_string(),
            hash: "unavailable".to_string(),
            capabilities: Vec::new(),
        },
        command_manifest_hash: "unavailable".to_string(),
        selected: serde_json::json!({}),
    }
}

// --- the session ----------------------------------------------------------

/// State shared with the Tauri commands.
pub struct SessionState {
    /// The real snapshot once a session is established. Cached because the
    /// webview's `bootstrap` must not block on a socket round-trip.
    pub bootstrap: Mutex<Option<serde_json::Value>>,
    /// Where events go, set by `subscribe` — the renderer telling us which
    /// window is listening.
    pub window: Mutex<Option<tauri::WebviewWindow>>,
}

/// Where the adapter lives. Packaged builds ship the native runtime beside the
/// shell; a checkout run finds it under `target/`.
///
/// Returns at most ONE command: `AdapterLaunch.command` is `argv[0]` plus its
/// arguments, so a second element would be passed to the adapter as an
/// argument. The candidates deliberately overlap — a shell in `target/release`
/// finds the same binary through `beside_exe` and through `repo_root` — so this
/// must take the first hit rather than collect them all.
fn native_runtime_command(repo_root: &Path) -> Vec<String> {
    let beside_exe = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("step-studio-runtime")));
    let candidates = beside_exe
        .into_iter()
        .chain(["release", "debug"].map(|profile| {
            repo_root.join(format!("target/{profile}/step-studio-runtime"))
        }));
    for candidate in candidates {
        if candidate.is_file() {
            return vec![candidate.to_string_lossy().to_string()];
        }
    }
    Vec::new()
}

/// Walk up looking for the workspace root, so a shell run from a subdirectory
/// still finds the adapter.
fn find_repo_root(start: &Path) -> PathBuf {
    let mut current = Some(start);
    while let Some(dir) = current {
        if dir.join("crates/studio-bridge").is_dir() {
            return dir.to_path_buf();
        }
        current = dir.parent();
    }
    start.to_path_buf()
}

fn env_or(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.to_string())
}

/// Connect a session and take its first snapshot.
///
/// Returns `None` when the session cannot be established, in which case the
/// shell keeps reporting the unavailable shape. Every failure is printed: a
/// silent fallback would look identical to a working shell with nothing to
/// show.
pub fn connect_session() -> Option<serde_json::Value> {
    let cwd = PathBuf::from(env_or("STEP_STUDIO_CWD", "."));
    let repo_root = find_repo_root(
        &std::env::var_os("STEP_STUDIO_REPO_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| cwd.clone())),
    );
    let step_command = env_or("STEP_STUDIO_STEP_CMD", "step");

    // A path that does not exist is a configuration mistake; the adapter would
    // only report it as a closed socket after the handshake.
    if step_command.contains('/') && !Path::new(&step_command).is_file() {
        eprintln!("[bridge] Step CLI not found at {step_command}");
        return None;
    }

    let session_dir = cwd.join(".step-studio");
    let _ = std::fs::create_dir_all(&session_dir);
    let launch = AdapterLaunch {
        node: "node".to_string(),
        adapter_script: repo_root
            .join("packages/runtime-stepcode/dist/src/main.js")
            .to_string_lossy()
            .to_string(),
        socket_path: session_dir
            .join("bridge.sock")
            .to_string_lossy()
            .to_string(),
        token_file: session_dir.join("token").to_string_lossy().to_string(),
        cwd: cwd.to_string_lossy().to_string(),
        runtime_epoch: 1,
        // Both names: the Rust adapter reads STEP_STUDIO_STEP_CMD, the
        // TypeScript fallback reads STEP_STUDIO_RUNTIME_CMD.
        env: vec![
            ("STEP_STUDIO_STEP_CMD".to_string(), step_command.clone()),
            ("STEP_STUDIO_RUNTIME_CMD".to_string(), step_command),
            // Resume the previous session.
            ("STEP_STUDIO_SESSION".to_string(), "continue".to_string()),
        ],
        // The shell launches the adapter, so it owns the token file.
        token: None,
        command: native_runtime_command(&repo_root),
    };

    let session = match BridgeSession::connect(&launch) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("[bridge] connect failed: {error}");
            return None;
        }
    };

    match session.snapshot() {
        Ok(snapshot) => {
            // The session is dropped here on purpose in M1.2a: keeping it would
            // need the command pump, which lands with M1.2b. Reconnecting per
            // bootstrap is wasteful but honest about where the boundary is.
            drop(session);
            Some(bootstrap_from_snapshot(&snapshot))
        }
        Err(error) => {
            eprintln!("[bridge] snapshot failed: {error}");
            None
        }
    }
}

/// Translate a Studio snapshot into the client contract's `ClientBootstrap`.
///
/// The two are different shapes and the renderer validates the contract
/// version, so returning the snapshot verbatim fails with "Client contract
/// mismatch". `session.snapshot()` wraps the runtime's snapshot under a
/// `snapshot` key; the runtime side is built in
/// `crates/step-studio-runtime/src/snapshot.rs`.
///
/// `surface` and `capabilityManifest` are deliberately minimal in M1.2a: the
/// terminal and chrome bridges that would justify them land in M1.2c, so
/// claiming them now would make the renderer offer actions that fail.
fn bootstrap_from_snapshot(response: &serde_json::Value) -> serde_json::Value {
    let snapshot = response.get("snapshot").unwrap_or(response);
    let runtime_id = snapshot
        .get("runtimeId")
        .and_then(|v| v.as_str())
        .unwrap_or("step-runtime");
    let runtime_epoch = snapshot.get("runtimeEpoch").and_then(|v| v.as_u64()).unwrap_or(1);
    let session_id = snapshot.get("sessionId").and_then(|v| v.as_str()).unwrap_or_default();

    let mut selected = serde_json::json!({});
    if let Some(model) = snapshot.get("model") {
        if let Some(selector) = model.get("selector").and_then(|v| v.as_str()) {
            selected["model"] = serde_json::json!(selector);
        }
        if let Some(thinking) = model.get("thinking").and_then(|v| v.as_str()) {
            selected["thinking"] = serde_json::json!(thinking);
        }
    }

    serde_json::json!({
        "contractVersion": CLIENT_CONTRACT_VERSION,
        "authority": {
            "authorityId": runtime_id,
            "authorityEpoch": runtime_epoch,
        },
        "runtime": {
            // The shell spawned the adapter, which is what "managed" means.
            "status": "connected",
            "classification": "managed",
            "runtimeId": runtime_id,
            "runtimeEpoch": runtime_epoch,
        },
        "surface": {
            "terminalAttach": false,
            "fileReveal": false,
            "previewInput": false,
            "openExternal": false,
        },
        // Empty until M1.2b reads the adapter's declared capability list; the
        // renderer hides what is not declared, which is the honest state.
        "capabilityManifest": {
            "profile": "limited",
            "generatedAt": "1970-01-01T00:00:00.000Z",
            "hash": format!("step-studio-{runtime_epoch}"),
            "capabilities": [],
        },
        "commandManifestHash": format!("step-studio-{runtime_epoch}"),
        "selected": selected,
        "sessionId": session_id,
    })
}

// --- the Tauri commands ---------------------------------------------------

#[tauri::command]
pub fn bootstrap(state: tauri::State<'_, SessionState>) -> serde_json::Value {
    // A real snapshot when the session is up, the honest unavailable shape
    // when it is not.
    let cached = state
        .bootstrap
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match cached.as_ref() {
        Some(snapshot) => snapshot.clone(),
        None => serde_json::to_value(unavailable_bootstrap())
            .unwrap_or_else(|_| serde_json::json!({})),
    }
}

#[tauri::command]
pub fn query(request: serde_json::Value) -> Result<serde_json::Value, IpcError> {
    let _ = request;
    Err(IpcError::not_migrated("query"))
}

#[tauri::command]
pub fn command(request: serde_json::Value) -> Result<serde_json::Value, IpcError> {
    let _ = request;
    Err(IpcError::not_migrated("command"))
}

#[tauri::command]
pub fn subscribe(
    state: tauri::State<'_, SessionState>,
    window: tauri::WebviewWindow,
) -> Result<(), IpcError> {
    let mut slot = state
        .window
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *slot = Some(window);
    Ok(())
}

#[tauri::command]
pub fn close() -> Result<(), IpcError> {
    Ok(())
}
