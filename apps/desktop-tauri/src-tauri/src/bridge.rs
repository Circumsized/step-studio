//! M1.1 stub surface for the Desktop IPC bridge (design/plan.md §5.2).
//!
//! Every command here is a stub. M1.2 replaces these bodies with the
//! in-process `studio-bridge` facade; the signatures are that seam and must
//! not change shape in M1.1.
//!
//! The stubs answer with the outcome the Electron Host already produces for
//! unmigrated work, so the renderer renders its honest empty state instead of
//! an error boundary (plan §16.4 acceptance ⑤: no white screen, no crash):
//!
//! - `bootstrap` resolves with the same "runtime unavailable" shape the
//!   renderer itself falls back to (`apps/renderer/src/App.tsx:385`), copied
//!   verbatim rather than invented, so the workbench shell mounts and reports
//!   the runtime as unavailable.
//! - `query` / `command` reject with an `IpcError`. The JS sender re-wraps any
//!   rejection as an `Error`, so the UI shows a transport failure, exactly as
//!   it does when the Electron Host has no facade yet.
//! - `subscribe` registers nothing; no events are emitted in M1.1.
//! - `close` resolves.

use serde::Serialize;

/// Rejection payload. Tauri rejects with the deserialized error, so this
/// becomes a plain `{code, message}` object on the JS side; the sender reads
/// `.message` and re-wraps it as an `Error` (see `apps/desktop-tauri/src/
/// tauri-sender.ts`).
#[derive(Serialize)]
pub struct IpcError {
    pub code: String,
    pub message: String,
}

impl IpcError {
    fn not_migrated(operation: &str) -> Self {
        Self {
            code: "DESKTOP_BRIDGE_NOT_MIGRATED".to_string(),
            message: format!(
                "desktop bridge: {operation} is not migrated to the Tauri shell yet (M1.2)"
            ),
        }
    }
}

/// Mirrors `unavailableBootstrap()` in `apps/renderer/src/App.tsx:385-394`.
/// Field names and values are copied from there on purpose: this is the shape
/// the renderer already knows how to render.
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

#[tauri::command]
pub fn bootstrap() -> Result<UnavailableBootstrap, IpcError> {
    Ok(unavailable_bootstrap())
}

#[tauri::command]
pub fn query(request: serde_json::Value) -> Result<serde_json::Value, IpcError> {
    // The request is echoed nowhere: it is only read to keep the signature
    // honest about the envelope shape M1.2 will parse.
    let _ = request;
    Err(IpcError::not_migrated("query"))
}

#[tauri::command]
pub fn command(request: serde_json::Value) -> Result<serde_json::Value, IpcError> {
    let _ = request;
    Err(IpcError::not_migrated("command"))
}

#[tauri::command]
pub fn subscribe(scope: serde_json::Value) -> Result<(), IpcError> {
    // M1.1 registers no subscription: there is no event source to fan out.
    // The JS sender has already resolved its listener before this resolves.
    let _ = scope;
    Ok(())
}

#[tauri::command]
pub fn close() -> Result<(), IpcError> {
    Ok(())
}
