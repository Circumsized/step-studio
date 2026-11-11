//! Step Studio Tauri shell (M1.1: scaffold + renderer mount + bridge injection).
//!
//! The window is built here rather than in `tauri.conf.json` because
//! `initialization_script` has no conf field: the bridge bundle must be
//! attached to the window at construction time, before the renderer bundle
//! runs. That is why `app.windows` is empty in the conf.
//!
//! M1.2 replaces the stub bodies in `bridge.rs` with the in-process
//! `studio-bridge` facade; nothing in this file changes for that.

mod bridge;

use tauri::{WebviewUrl, WebviewWindowBuilder};

/// Built by `npm run build:bridge` (esbuild, IIFE). Embedded at compile time so
/// a missing or stale bundle fails the build rather than the runtime.
const BRIDGE_INIT_SCRIPT: &str = include_str!("../../dist/bridge-init.js");

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            bridge::bootstrap,
            bridge::query,
            bridge::command,
            bridge::subscribe,
            bridge::close,
        ])
        .setup(|app| {
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
            .title("Step Studio")
            .inner_size(1280.0, 800.0)
            .initialization_script(BRIDGE_INIT_SCRIPT)
            .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("step-studio-desktop-tauri: tauri run failed");
}
