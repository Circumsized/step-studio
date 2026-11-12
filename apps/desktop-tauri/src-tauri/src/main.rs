//! Step Studio Tauri shell (M1.2: the bridge is wired to a real session).
//!
//! The window is built here rather than in `tauri.conf.json` because
//! `initialization_script` has no conf field: the bridge bundle must be
//! attached at construction time, before the renderer bundle runs. That is why
//! `app.windows` is empty in the conf.

mod bridge;
mod chrome;
mod git;
mod pump;
mod terminal;

use std::sync::atomic::AtomicU64;
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
            terminal::create,
            terminal::write,
            terminal::resize,
            terminal::dispose,
        ])
        // `move`: the closure hands the window handle to the pump, which
        // outlives this function.
        .setup(move |app| {
            let self_check = std::env::var_os("STEP_STUDIO_SELF_CHECK").is_some();
            // The window is where events go. It lives in the shared inner state
            // rather than in the sink closure, so a reconnect gets the same
            // window without anybody having to rebuild the closure.
            let inner = Arc::new(bridge::SessionInner {
                bootstrap: Mutex::new(None),
                window: Arc::clone(&window),
                jobs: Mutex::new(None),
                generation: AtomicU64::new(0),
            });
            app.manage(bridge::SessionState {
                inner: Arc::clone(&inner),
            });
            app.manage(terminal::TerminalState::default());

            // The supervisor owns the connection from here on: it connects, and
            // connects again whenever the session ends. Establishing it before
            // the window exists means the first `bootstrap` can arrive at any
            // moment, which is why the renderer treats a missing snapshot as
            // "not yet" rather than "never".
            if let Some(config) = bridge::connection_config() {
                let sink_state = Arc::clone(&inner);
                let trace = self_check;
                bridge::supervise(config, Arc::clone(&inner), move || {
                    let window = Arc::clone(&sink_state.window);
                    Box::new(move |payload: Value| {
                        let received = match window
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .as_ref()
                        {
                            // A window that went away is not worth failing a turn over.
                            Some(target) => {
                                target.emit(pump::EVENT_CHANNEL, payload.clone()).is_ok()
                            }
                            None => false,
                        };
                        if trace {
                            trace_sink(&payload);
                        }
                        received
                    })
                });
            }

            // The self-check turn. It waits for the first session rather than
            // assuming one, because the supervisor may still be backing off
            // from a daemon that is not up yet.
            if self_check {
                let prompt = std::env::var("STEP_STUDIO_SELF_CHECK_PROMPT")
                    .unwrap_or_else(|_| "reply with exactly: PONG".to_string());
                let inner = Arc::clone(&inner);
                std::thread::spawn(move || {
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
                    let sender = loop {
                        if let Some(sender) = inner
                            .jobs
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .clone()
                        {
                            break sender;
                        }
                        if std::time::Instant::now() >= deadline {
                            eprintln!("[self-check] no session after 60s");
                            return;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    };
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
