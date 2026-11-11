//! Step Studio Tauri shell (M1.2: the bridge is wired to a real session).
//!
//! The window is built here rather than in `tauri.conf.json` because
//! `initialization_script` has no conf field: the bridge bundle must be
//! attached at construction time, before the renderer bundle runs. That is why
//! `app.windows` is empty in the conf.

mod bridge;

use std::sync::Mutex;

use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

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
            let state = bridge::SessionState {
                bootstrap: Mutex::new(None),
                window: Mutex::new(None),
            };
            // Establish the session before the first `bootstrap` can arrive, so
            // the renderer never sees a snapshot flip from unavailable to real.
            if let Some(snapshot) = bridge::connect_session() {
                let mut slot = state
                    .bootstrap
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                *slot = Some(snapshot);
            }
            // Managed by type: the commands take `State<'_, SessionState>`, so
            // wrapping it here would store a different type and every lookup
            // would miss.
            app.manage(state);

            WebviewWindowBuilder::new(
                app,
                "main",
                WebviewUrl::App("index.html".into()),
            )
            .title("Step Studio")
            .inner_size(1280.0, 800.0)
            .initialization_script(BRIDGE_INIT_SCRIPT)
            .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("step-studio-desktop-tauri: tauri run failed");
}
