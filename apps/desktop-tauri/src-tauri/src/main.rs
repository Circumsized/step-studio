//! Step Studio Tauri shell (M1.2: the bridge is wired to a real session).
//!
//! The window is built here rather than in `tauri.conf.json` because
//! `initialization_script` has no conf field: the bridge bundle must be
//! attached at construction time, before the renderer bundle runs. That is why
//! `app.windows` is empty in the conf.

mod bridge;
mod chrome;
mod pump;

use std::sync::{Arc, Mutex};

use serde_json::Value;
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

/// Print every translated event to stderr, so a turn can be verified on a
/// machine with no display and no way to drive the composer.
///
/// Opt-in rather than always on: a turn is hundreds of deltas and nobody wants
/// that in a normal log. `STEP_STUDIO_SELF_CHECK=1` is how the shell proves
/// the bridge works after packaging.
fn trace_sink(payload: &Value) {
    let kind = payload.get("kind").and_then(Value::as_str).unwrap_or("?");
    match kind {
        "conversation.changed" => {
            let update = payload.get("update");
            let delta = update
                .and_then(|u| u.get("delta"))
                .and_then(Value::as_str)
                .unwrap_or("");
            eprintln!(
                "[self-check] #{} {} {}",
                payload.get("streamSeq").and_then(Value::as_u64).unwrap_or(0),
                update.and_then(|u| u.get("kind")).and_then(Value::as_str).unwrap_or("?"),
                delta
            );
        }
        "command.receipt" => {
            let receipt = payload.get("receipt");
            eprintln!(
                "[self-check] receipt {} -> {}",
                receipt.and_then(|r| r.get("commandName")).and_then(Value::as_str).unwrap_or("?"),
                receipt.and_then(|r| r.get("status")).and_then(Value::as_str).unwrap_or("?")
            );
        }
        _ => eprintln!("[self-check] {kind}"),
    }
}

/// Built by `npm run build:bridge` (esbuild, IIFE). Embedded at compile time so
/// a missing or stale bundle fails the build rather than the runtime.
const BRIDGE_INIT_SCRIPT: &str = include_str!("../../dist/bridge-init.js");

fn main() {
    // Where the pump emits. Created before the session so the pump can be
    // handed it, and filled in by `subscribe` once the renderer says which
    // window is listening — until then events have nowhere to go and are
    // dropped, which is correct because nothing is subscribed.
    let window: Arc<Mutex<Option<tauri::WebviewWindow>>> = Arc::new(Mutex::new(None));

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            bridge::bootstrap,
            bridge::query,
            bridge::command,
            bridge::subscribe,
            bridge::close,
            chrome::save_avatar,
            chrome::load_avatar,
            chrome::clear_avatar,
            chrome::open_url,
            chrome::notify,
            chrome::set_theme,
            chrome::set_conversation_view_state,
            chrome::report_payload_health,
            chrome::report_performance_sample,
        ])
        // `move`: the closure hands the window handle to the pump, which
        // outlives this function.
        .setup(move |app| {
            // Establish the session before the first `bootstrap` can arrive, so
            // the renderer never sees a snapshot flip from unavailable to real.
            // The pump starts here too: it owns the session from now on, and
            // commands only ever talk to it over `jobs`.
            let self_check = std::env::var_os("STEP_STUDIO_SELF_CHECK").is_some();
            let (bootstrap, jobs) = match bridge::connect_session() {
                Some(connected) => {
                    // One sink, two jobs: the window gets every event, and in
                    // self-check mode stderr gets the same stream.
                    let window = Arc::clone(&window);
                    let sink: pump::EventSink = Box::new(move |payload| {
                        if let Some(target) = window
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .as_ref()
                        {
                            // A window that went away is not worth failing a turn over.
                            let _ = target.emit(pump::EVENT_CHANNEL, payload.clone());
                        }
                        if self_check {
                            trace_sink(&payload);
                        }
                    });
                    let jobs = pump::spawn_pump(
                        connected.session,
                        sink,
                        connected.runtime_epoch,
                        connected.authority_epoch,
                        connected.state_version,
                    );
                    (Some(connected.bootstrap), Some(jobs))
                }
                None => (None, None),
            };
            let state = bridge::SessionState {
                bootstrap: Mutex::new(bootstrap),
                window: Arc::clone(&window),
                // Cloned, not moved: the self-check turn below needs a sender
                // too, and a second sender to the same pump is free.
                jobs: Mutex::new(jobs.clone()),
            };
            // Managed by type: the commands take `State<'_, SessionState>`, so
            // wrapping it here would store a different type and every lookup
            // would miss. The clone is for the self-check turn below.
            app.manage(state);

            // The self-check turn. Sent after the state is managed so the reply
            // has somewhere to land, and on its own thread so `setup` returns.
            if let Some(sender) = jobs {
                let prompt = std::env::var("STEP_STUDIO_SELF_CHECK_PROMPT")
                    .unwrap_or_else(|_| "reply with exactly: PONG".to_string());
                std::thread::spawn(move || {
                    let (reply, inbox) = std::sync::mpsc::channel();
                    if sender
                        .send(pump::Job {
                            request_id: "self-check".to_string(),
                            command_name: "core.prompt".to_string(),
                            operation: serde_json::json!({ "kind": "core.prompt", "text": prompt }),
                            dispatch: pump::Dispatch::Turn,
                            want: pump::Want::Ack,
                            reply,
                        })
                        .is_err()
                    {
                        return;
                    }
                    match inbox.recv() {
                        Ok(Ok(_)) => eprintln!("[self-check] prompt accepted"),
                        Ok(Err(error)) => eprintln!("[self-check] prompt failed: {error}"),
                        Err(_) => eprintln!("[self-check] prompt never answered"),
                    }
                });
            }

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
