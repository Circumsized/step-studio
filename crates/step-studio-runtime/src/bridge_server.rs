//! The Studio Bridge server side of the adapter.
//!
//! One connection at a time: a second connection means the previous Host is
//! stale, and a single event slot pointing at a closed socket would orphan
//! whichever client it belonged to.

use std::io::BufReader;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use step_studio_bridge::transport::{self, Accepted};

use serde_json::{json, Value};

use step_studio_bridge::frame::{read_frame, write_frame, FrameError};
use step_studio_bridge::handshake::{challenge_proof, verify_hello, PROTOCOL_VERSION};

use crate::capabilities::capability_manifest;
use crate::conversation::{map_event, MapContext};
use crate::snapshot::build_snapshot;
use crate::step_rpc::{StepError, StepRpc};

/// Receipt statuses that settle a command.
const TERMINAL_STATUSES: [&str; 4] = ["completed", "failed", "rejected", "outcome_unknown"];

/// Terminal outcomes a reconnect may have missed.
const TERMINAL_HISTORY: usize = 64;

/// How long a shutdown waits for the turn in flight before exiting anyway.
///
/// Five seconds, the same bound the desktop Host already uses for a graceful
/// shutdown once the OS is powering off (`apps/desktop/src/types.ts`). One
/// number for one idea.
const SHUTDOWN_DRAIN: Duration = Duration::from_secs(5);

/// How long `usage.panel` waits for the CLI's panel before giving up.
///
/// The panel arrives on the fire-and-forget channel rather than as the prompt's
/// response, and the CLI scans activity history first — measured at a few
/// seconds against the real CLI.
const USAGE_PANEL_TIMEOUT: Duration = Duration::from_secs(30);

/// Outstanding dialog requests, by the id the CLI issued.
///
/// The agent side resolves a timed-out dialog on its own and never says so,
/// so this set is an upper bound: an id in it may already be answered
/// upstream. What it does catch is answering an id this connection never
/// issued, or issued before a reconnect, which must fail closed rather than
/// push a stray frame into the CLI.
#[derive(Default)]
struct InteractionRegistry {
    /// (interaction id, method), so an answer can be translated into the
    /// shape that method expects.
    outstanding: Vec<(String, String)>,
}

impl InteractionRegistry {
    fn open(&mut self, id: &str, method: &str) {
        if !id.is_empty() {
            self.outstanding.push((id.to_string(), method.to_string()));
        }
    }

    /// Forget an id, reporting the method it was issued for. The method has to
    /// come back with the id: the answer is translated per method, and an empty
    /// one silently sends the wrong shape to the CLI.
    fn close(&mut self, id: &str) -> Option<String> {
        let index = self.outstanding.iter().position(|open| open.0 == id)?;
        Some(self.outstanding.remove(index).1)
    }

    /// Everything still open, for an abort or a shutdown: each one owes the
    /// client a resolution, and each one is a frame the CLI will never answer.
    fn drain(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.outstanding)
    }
}

pub struct AdapterConfig {
    pub endpoint: String,
    pub token: String,
    pub runtime_epoch: u64,
    pub cwd: String,
    pub step_command: String,
    pub step_env: Vec<(String, String)>,
    /// Resume the previous session instead of starting a fresh one.
    pub continue_session: bool,
    /// Exit when the one connection ends, rather than waiting for the next.
    ///
    /// The daemon sets this. It is the adapter's only client, so the adapter's
    /// lifetime is the daemon's: without it the adapter would outlive a daemon
    /// that was killed, still holding `step` and with it the user's session.
    pub exit_on_disconnect: bool,
}

/// Everything one connection needs: the socket, the CLI, and the shared state
/// that must outlive a reconnect.
pub struct Runtime {
    config: AdapterConfig,
    terminal: Arc<Mutex<Vec<Value>>>,
}

impl Runtime {
    pub fn new(config: AdapterConfig) -> Self {
        Self {
            config,
            terminal: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Serve until the process is asked to shut down.
    pub fn serve(&self) -> Result<(), String> {
        let _ = std::fs::remove_file(&self.config.endpoint);
        // A Unix socket is a file, so a stale one has to go first; a Windows
        // pipe is named, and there is nothing to remove.
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.config.endpoint);
        let bind_at = transport::endpoint_for_bind(&self.config.endpoint);
        let listener = transport::bind(&bind_at)
            .map_err(|e| format!("could not bind {}: {e}", self.config.endpoint))?;
        eprintln!("step-studio-runtime listening on {}", self.config.endpoint);

        // One connection at a time.
        //
        // On Unix the listener persists and `accept` blocks on it, so this loops
        // for as long as the process runs. On Windows a single-instance pipe
        // serves exactly one client, so `bind` has already waited by the time it
        // returns: the first connection is served, and each one after that is a
        // fresh `bind`.
        // `accept` blocks on a Unix listener, so the unix arm loops forever and
        // never reaches the tail; on Windows each connection needs a fresh bind,
        // so that arm returns when it can no longer make one.
        #[cfg(unix)]
        {
            // The listener is borrowed: it outlives every connection it accepts.
            loop {
                match transport::accept(listener.try_clone().expect("the listener duplicates")) {
                    Ok(connection) => {
                        // Joined, not just spawned: whether this connection was
                        // the adapter's last is decided by what it returns.
                        let worker = serve_one(connection, &self.config, &self.terminal);
                        let _ = worker.join();
                        if self.config.exit_on_disconnect {
                            return Ok(());
                        }
                    }
                    Err(error) => {
                        eprintln!("step-studio-runtime: accept failed: {error}");
                    }
                }
            }
            // Never reached: the loop above runs for the life of the process.
            #[allow(unreachable_code)]
            Ok(())
        }
        #[cfg(windows)]
        {
            // The listener is not consumed on Windows: `NamedPipe::connect`
            // takes `&self`, so the same pipe is re-accepted for the next
            // client. The rebind below still needs
            // `FILE_FLAG_FIRST_PIPE_INSTANCE` cleared — see `transport::bind`.
            let mut listener = listener;
            loop {
                let connection = match transport::accept(listener) {
                    Ok(connection) => connection,
                    Err(error) => {
                        eprintln!("step-studio-runtime: accept failed: {error}");
                        return Ok(());
                    }
                };
                listener = match transport::bind(&bind_at) {
                    Ok(next) => next,
                    Err(error) => {
                        eprintln!("step-studio-runtime: rebind failed: {error}");
                        return Ok(());
                    }
                };
                serve_one(connection, &self.config, &self.terminal);
            }
        }
    }
}

/// Hand one accepted connection to its own thread, and return the handle so
/// the caller can wait for it.
fn serve_one(
    connection: transport::Accepted,
    config: &AdapterConfig,
    terminal: &Arc<Mutex<Vec<Value>>>,
) -> std::thread::JoinHandle<()> {
    let config = AdapterConfig {
        endpoint: config.endpoint.clone(),
        token: config.token.clone(),
        runtime_epoch: config.runtime_epoch,
        cwd: config.cwd.clone(),
        step_command: config.step_command.clone(),
        step_env: config.step_env.clone(),
        continue_session: config.continue_session,
        exit_on_disconnect: config.exit_on_disconnect,
    };
    let terminal = Arc::clone(terminal);
    std::thread::spawn(move || {
        if let Err(error) = serve_connection(connection, config, terminal) {
            eprintln!("step-studio-runtime: connection ended: {error}");
        }
    })
}

/// The tail, not the head: a UUIDv7's first characters are its timestamp, so
/// two sessions minted seconds apart look identical there.
fn short(id: &str) -> &str {
    let start = id.len().saturating_sub(8);
    &id[start..]
}

/// A live session: the Host socket plus the CLI it drives.
struct Session {
    stream: transport::Accepted,
    step: StepRpc,
    epoch: u64,
    seq: u64,
    context: MapContext,
    session_id: String,
    terminal: Arc<Mutex<Vec<Value>>>,
    shutting_down: bool,
    /// When a shutdown request stops waiting for the turn in flight. `None`
    /// until a shutdown is asked for.
    shutdown_deadline: Option<Instant>,    state_version: u64,
    command_manifest_hash: String,
    pending_prompt: Option<String>,
    /// The id the in-flight `prompt` was sent under; empty when none is.
    prompt_request_id: String,
    /// A `usage.panel` request waiting for its panel, and when it gives up.
    ///
    /// Parked rather than waited for: holding the connection handler until the
    /// panel arrives would stop every other command for those seconds, and
    /// `core.abort` is the one that must never be blocked.
    pending_usage: Option<(String, Instant)>,
    interactions: InteractionRegistry,
}

impl Session {
    /// Send one CLI event where it belongs: dialogs are tracked, everything
    /// else is mapped and forwarded.
    ///
    /// Shared with the turn pump so a query that has to read the event stream
    /// (`usage.panel`) routes what it passes exactly as the pump would, instead
    /// of growing a second copy of this that drifts.
    fn route_event(&mut self, event: &Value) {
        // A parked usage request is settled from here, so the panel is
        // intercepted before the general routing sees it.
        if self.pending_usage.is_some() {
            self.settle_usage(event);
            if self.pending_usage.is_none() {
                return;
            }
        }
        let kind = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if kind == "extension_ui_request" {
            let method = event.get("method").and_then(|v| v.as_str()).unwrap_or("");
            if method == "cancel" {
                // The CLI resolved a dialog on its own — abort or timeout — and
                // is telling the host to close the card. Dropping this silently
                // left the card up and let a late answer be accepted for a
                // dialog that no longer existed upstream.
                let target = event.get("targetId").and_then(|v| v.as_str()).unwrap_or("");
                if self.interactions.close(target).is_some() {
                    let command_id = format!("cmd-{target}");
                    self.resolved(target, &command_id, "expired");
                }
            } else if crate::conversation::is_dialog_method(method) {
                if let Some(id) = event.get("id").and_then(|v| v.as_str()) {
                    self.interactions.open(id, method);
                }
            }
        }
        if let Some(mapped) = map_event(event, &self.context) {
            self.event(mapped.event, mapped.advances_state);
        }
    }

    /// Ask the CLI for its `/usage` panel and answer with what came back.
    ///
    /// The panel is not the prompt's response: `/usage` is dispatched as a slash
    /// command and the answer arrives on the fire-and-forget `notify` channel,
    /// as ANSI text. So this sends the prompt and then waits for a notify.
    ///
    /// stdin must stay open while it waits — closing it cancels the CLI's
    /// activity scan and all that comes back is `extension_error`. `StepRpc`
    /// owns the process for the session's lifetime, so it does.
    ///
    /// Every other event keeps flowing through the normal mapping: a query must
    /// not swallow a turn's events on its way past.
    fn usage_panel(&mut self, request_id: &str) {
        if let Err(error) = self.step.send("prompt", json!({ "message": "/usage" })) {
            self.failed(request_id, error.to_string());
            return;
        }
        // Parked, not waited for. The first version held the connection handler
        // until the panel arrived, which stopped every other command for those
        // seconds — `core.abort` included, and an abort that cannot be sent is
        // the worst thing to block.
        self.pending_usage = Some((request_id.to_string(), Instant::now() + USAGE_PANEL_TIMEOUT));
    }

    /// Complete a parked `usage.panel` request if its panel has arrived.
    ///
    /// The deadline is not checked here — `expire_usage` owns it. Checking it
    /// only when an event happened to arrive meant a CLI that went quiet hung
    /// the request forever, which is the only state a timeout can fire in.
    fn settle_usage(&mut self, event: &Value) {
        let Some((request_id, _)) = self.pending_usage.clone() else {
            return;
        };
        // `/usage` is a slash command, not a message: the CLI dispatches it and
        // writes the panel on the fire-and-forget channel as a `command_output`
        // frame carrying `text` — `rpc-mode.ts` wires
        // `output: commandOutput => output({ type: "command_output", text })`.
        // This used to wait for an `extension_ui_request` with `method:"notify"`
        // and `extensionPath:"command:usage"` — a frame the pinned Runtime never
        // emits — so every `usage.panel` ran out its whole timeout.
        // `command_output` carries no id and no command name; the connection is
        // serial and nothing else sends a slash command while this request is
        // parked, so the first one is the panel.
        let is_panel = event.get("type").and_then(|v| v.as_str()) == Some("command_output");
        if is_panel {
            self.pending_usage = None;
            let panel = event.get("text").and_then(|v| v.as_str()).unwrap_or("");
            self.receipt(&request_id, "completed", Some(json!({ "panel": panel })));
        }
    }

    /// Fail a parked `usage.panel` whose deadline has passed.
    ///
    /// Called from the main loop, which ticks every 50 ms, rather than from
    /// `route_event`: the deadline has to be evaluated when the CLI has gone
    /// quiet, and quiet is the only state the timeout can fire in.
    fn expire_usage(&mut self) {
        let Some((request_id, deadline)) = self.pending_usage.clone() else {
            return;
        };
        if Instant::now() >= deadline {
            self.pending_usage = None;
            self.failed(
                &request_id,
                "the CLI did not return a usage panel".to_string(),
            );
        }
    }

    /// Whether a turn is still running inside the shutdown deadline.
    fn draining_a_turn(&self) -> bool {
        self.pending_prompt.is_some()
            && self
                .shutdown_deadline
                .is_some_and(|deadline| Instant::now() < deadline)
    }

    fn send(&mut self, body: &Value) -> Result<(), FrameError> {
        write_frame(&mut self.stream, self.epoch, body)
    }

    /// Record a terminal outcome so a reconnect can recover it.
    fn terminal(&mut self, receipt: Value) {
        if let Ok(mut guard) = self.terminal.lock() {
            guard.push(receipt.clone());
            if guard.len() > TERMINAL_HISTORY {
                guard.remove(0);
            }
        }
        let _ = self.send(&receipt);
    }

    fn receipt(&mut self, request_id: &str, status: &str, result: Option<Value>) {
        self.receipt_with_error(request_id, status, result, None);
    }

    fn receipt_with_error(
        &mut self,
        request_id: &str,
        status: &str,
        result: Option<Value>,
        error: Option<(&str, String)>,
    ) {
        // Built as a map because `json!` has no spread syntax and `result` is
        // only present on some statuses.
        let mut receipt = serde_json::Map::new();
        receipt.insert("type".to_string(), json!("studio.receipt"));
        receipt.insert("requestId".to_string(), json!(request_id));
        receipt.insert("commandId".to_string(), json!(format!("cmd-{request_id}")));
        receipt.insert("runtimeEpoch".to_string(), json!(self.epoch));
        receipt.insert("stateVersion".to_string(), json!(0));
        receipt.insert("status".to_string(), json!(status));
        if let Some(value) = result {
            receipt.insert("result".to_string(), value);
        }
        if let Some((code, message)) = error {
            // The Host and the shell both read the reason off the receipt; a
            // bare `failed` tells the user nothing.
            receipt.insert(
                "error".to_string(),
                json!({ "code": code, "message": message, "retryable": false }),
            );
        }
        let receipt = Value::Object(receipt);
        if TERMINAL_STATUSES.contains(&status) {
            self.terminal(receipt);
        } else {
            let _ = self.send(&receipt);
        }
    }

    fn failed(&mut self, request_id: &str, message: String) {
        // The error rides on the receipt, not the result.
        self.receipt_with_error(
            request_id,
            "failed",
            None,
            Some(("INTERNAL_ERROR", message)),
        );
    }

    /// A failure with a code the client can branch on. The generic `failed`
    /// reports INTERNAL_ERROR, which is wrong for a stale answer.
    fn failed_with_code(&mut self, request_id: &str, code: &str, message: String) {
        self.receipt_with_error(request_id, "failed", None, Some((code, message)));
    }

    /// Tell the client a dialog is over, whichever way it ended. The upstream
    /// resolves a timeout without a word, so the only clear signals this
    /// adapter can send are its own; without this a card outlives its dialog.
    fn resolved(&mut self, interaction_id: &str, command_id: &str, outcome: &str) {
        self.event(
            json!({
                "kind": "interaction.resolved",
                "interactionId": interaction_id,
                "commandId": command_id,
                "leaseGeneration": 0,
                "outcome": outcome,
            }),
            true,
        );
    }

    /// Close out every dialog a dead turn was holding open.
    fn resolve_outstanding(&mut self, outcome: &str) {
        for (id, _method) in self.interactions.drain() {
            let command_id = format!("cmd-{id}");
            self.resolved(&id, &command_id, outcome);
        }
    }

    fn event(&mut self, event: Value, advances_state: bool) {
        self.seq += 1;
        let state_version = if advances_state {
            self.state_version += 1;
            self.state_version
        } else {
            self.state_version
        };
        let envelope = json!({
            "type": "studio.event",
            "runtimeEpoch": self.epoch,
            "eventSeq": self.seq,
            "stateVersion": state_version,
            "occurredAt": crate::conversation::now_iso(),
            "event": event,
        });
        let _ = self.send(&envelope);
    }

    fn dispatch(&mut self, body: &Value) {
        let Some(operation) = body.get("operation") else {
            return;
        };
        let request_id = body
            .get("requestId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let kind = operation.get("kind").and_then(|v| v.as_str()).unwrap_or("");

        match kind {
            "core.prompt" => {
                let text = operation
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if std::env::var_os("STEP_STUDIO_TRACE").is_some() {
                    eprintln!("[runtime] prompt accepted, sending to cli");
                }
                self.receipt(&request_id, "accepted", None);
                self.pending_prompt = Some(request_id);
                // The id the CLI will echo back on `prompt_result`. Kept so the
                // pump can tell this turn's result from any other frame that
                // happens to describe an ending.
                self.prompt_request_id = match self.step.send("prompt", json!({ "message": text })) {
                    Ok(id) => id,
                    Err(error) => {
                        self.settle_prompt(Some(format!("step runtime could not start: {error}")));
                        String::new()
                    }
                };
            }
            "core.steer" | "core.followUp" => {
                let command = if kind == "core.steer" {
                    "steer"
                } else {
                    "follow_up"
                };
                let text = operation
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // Queueing is synchronous upstream, so this answers `completed`;
                // only `core.prompt` outlives its reply.
                match self.step.request(
                    command,
                    json!({ "message": text }),
                    std::time::Duration::from_secs(10),
                ) {
                    Ok(_) => {
                        self.receipt(&request_id, "completed", Some(json!({ "queued": true })))
                    }
                    Err(error) => self.failed(&request_id, error.to_string()),
                }
            }
            "core.abort" => {
                let _ = self.step.send("abort", json!({}));
                // The turn is over, so every dialog it was holding is over:
                // tell the client, and stop tracking ids the CLI will not answer.
                self.resolve_outstanding("aborted");
                if let Some(previous) = self.pending_prompt.take() {
                    let turn = self.context.turn_id();
                    self.event(json!({ "kind": "conversation.turn.aborted", "sessionId": self.session_id, "turnId": turn }), true);
                    self.receipt(&previous, "completed", Some(json!({ "ok": true })));
                }
                self.receipt(&request_id, "completed", Some(json!({ "aborted": true })));
            }
            "runtime.snapshot" => {
                let snapshot = build_snapshot(&mut self.step, self.epoch, &self.session_id);
                let _ = self.send(&json!({
                    "type": "studio.snapshot",
                    "requestId": request_id,
                    "snapshot": snapshot,
                    "commandManifestHash": self.command_manifest_hash,
                    "capabilityHash": capability_manifest().hash,
                    // Reporting 0 here resets the Host's watermark and turns the
                    // next event into a gap, which drops every later event.
                    "lastEventSeq": self.seq,
                    "terminalReceipts": self.terminal.lock().map(|g| g.clone()).unwrap_or_default(),
                }));
            }
            "usage.panel" => self.usage_panel(&request_id),
            "runtime.shutdown" => {
                // Draining means letting the turn in flight finish. The old
                // code aborted it and called that a drain, so any caller that
                // passed `drain: true` and waited got a silently truncated
                // turn instead of the turn.
                self.shutting_down = true;
                self.shutdown_deadline = Some(Instant::now() + SHUTDOWN_DRAIN);
                self.receipt(&request_id, "completed", None);
                let _ = self.send(&json!({ "kind": "runtime.shutdownComplete" }));
                // Nothing in flight, so there is nothing to wait for.
                if self.pending_prompt.is_none() {
                    self.resolve_outstanding("aborted");
                }
            }
            "session.model.set" => {
                if std::env::var_os("STEP_STUDIO_TRACE").is_some() {
                    eprintln!(
                        "[runtime] model.set {}",
                        operation
                            .get("selector")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?")
                    );
                }
                let selector = operation
                    .get("selector")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                match resolve_and_set_model(&mut self.step, &selector) {
                    Ok(()) => {
                        self.refresh_state();
                        self.receipt(&request_id, "completed", Some(json!({ "ok": true })));
                    }
                    Err(error) => self.failed(&request_id, error),
                }
            }
            "session.thinking.set" => {
                let level = operation
                    .get("level")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                match set_thinking(&mut self.step, &level) {
                    Ok(()) => {
                        self.refresh_state();
                        self.receipt(&request_id, "completed", Some(json!({ "ok": true })));
                    }
                    Err(error) => self.failed(&request_id, error),
                }
            }
            "session.new" => {
                // Step exposes this as `new_session`; it mints a fresh session
                // id and drops the transcript. The mapping state is reset with
                // it, so entry ids from the old session cannot leak into the
                // new one.
                if std::env::var_os("STEP_STUDIO_TRACE").is_some() {
                    eprintln!("[runtime] session.new requested");
                }
                match self.step.request(
                    "new_session",
                    json!({}),
                    std::time::Duration::from_secs(15),
                ) {
                    Ok(_) => {
                        self.context = MapContext::new(String::new());
                        self.refresh_state();
                        self.receipt(&request_id, "completed", Some(json!({ "ok": true })));
                    }
                    Err(error) => self.failed(&request_id, error.to_string()),
                }
            }
            "session.fork" => match fork_session(&mut self.step) {
                Ok(()) => {
                    self.refresh_state();
                    self.receipt(&request_id, "completed", Some(json!({ "ok": true })));
                }
                Err(error) => self.failed(&request_id, error),
            },
            "session.models.list" => {
                match self.step.request(
                    "get_available_models",
                    json!({}),
                    std::time::Duration::from_secs(10),
                ) {
                    Ok(catalog) => {
                        let models = catalog
                            .get("models")
                            .and_then(|m| m.as_array())
                            .map(|list| {
                                list.iter()
                                    .map(|entry| {
                                        let provider = entry.get("provider").and_then(|v| v.as_str()).unwrap_or("");
                                        let id = entry.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                        json!({
                                            "provider": provider,
                                            "id": id,
                                            "selector": format!("{provider}/{id}"),
                                            "name": entry.get("name").and_then(|v| v.as_str()),
                                            "reasoning": entry.get("reasoning").and_then(|v| v.as_bool()),
                                            "contextWindow": entry.get("contextWindow").and_then(|v| v.as_i64()),
                                            "maxTokens": entry.get("maxTokens").and_then(|v| v.as_i64()),
                                            "api": entry.get("api").and_then(|v| v.as_str()),
                                        })
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();
                        self.receipt(&request_id, "completed", Some(json!({ "models": models })));
                    }
                    Err(error) => self.failed(&request_id, error.to_string()),
                }
            }
            "session.transcript.read" => {
                let cursor = operation.get("cursor").and_then(|v| v.as_str());
                match read_transcript(&mut self.step, cursor) {
                    Ok(page) => self.receipt(&request_id, "completed", Some(page)),
                    Err(error) => self.failed(&request_id, error),
                }
            }
            // `session.history` is the paged form of the transcript: Studio's
            // Renderer asks for it with a cursor, and this adapter has no
            // pagination upstream, so the whole branch is the page.
            "session.history" => {
                let cursor = operation.get("cursor").and_then(|v| v.as_str());
                match read_transcript(&mut self.step, cursor) {
                    Ok(page) => self.receipt(&request_id, "completed", Some(page)),
                    Err(error) => self.failed(&request_id, error),
                }
            }
            // The same branch, as one object rather than a page: what the shell
            // restores a resumed session from.
            "session.state" => {
                self.refresh_state();
                let snapshot = build_snapshot(&mut self.step, self.epoch, &self.session_id);
                self.receipt(
                    &request_id,
                    "completed",
                    Some(json!({ "snapshot": snapshot })),
                );
            }
            // Extensions and their MCP servers are facts on disk; the command
            // catalog is one RPC away. Neither needs a bespoke adapter.
            "session.extensions.list" => {
                let root = crate::extensions::config_root();
                let extensions = crate::extensions::list_extensions(root.as_deref());
                self.receipt(
                    &request_id,
                    "completed",
                    Some(json!({ "extensions": extensions })),
                );
            }
            "session.commands.list" => match crate::extensions::list_commands(&mut self.step) {
                Ok(commands) => self.receipt(
                    &request_id,
                    "completed",
                    Some(json!({ "commands": commands })),
                ),
                Err(error) => self.failed(&request_id, error),
            },
            // The client's answer to a dialog the CLI is holding open. The
            // wire shape is the Studio contract's: decision submit/cancel plus
            // an optional value, which is translated per method into what the
            // CLI expects (confirmed for confirm, value for the rest).
            "interaction.respond" => {
                let interaction_id = operation
                    .get("interactionId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let command_id = operation
                    .get("commandId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let decision = operation
                    .get("decision")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let value = operation.get("value");
                let Some(method) = self.interactions.close(&interaction_id) else {
                    // The client is holding a card for a dialog that is already
                    // over (the upstream resolved it, or a reconnect lost it).
                    // Resolve first: a client that stops reading at the receipt
                    // would otherwise never see the card clear.
                    let command_id = if command_id.is_empty() {
                        format!("cmd-{interaction_id}")
                    } else {
                        command_id
                    };
                    self.resolved(&interaction_id, &command_id, "aborted");
                    self.failed_with_code(
                        &request_id,
                        "INTERACTION_STALE",
                        format!("no outstanding interaction {interaction_id}"),
                    );
                    return;
                };
                let answer = match decision {
                    "cancel" => json!({ "cancelled": true }),
                    "submit" => match method.as_str() {
                        "confirm" => json!({ "confirmed": is_affirmative(value) }),
                        // `ask` holds several questions in one dialog, so it
                        // cannot reuse the single-value shape. The CLI validates
                        // the answer against the questions it sent: one entry per
                        // question, in order, each naming its question id.
                        "ask" => json!({ "answers": value.cloned().unwrap_or(json!([])) }),
                        _ => json!({ "value": value.cloned().unwrap_or(Value::Null) }),
                    },
                    _ => {
                        self.failed(&request_id, format!("unknown decision: {decision}"));
                        return;
                    }
                };
                match self
                    .step
                    .reply("extension_ui_response", &interaction_id, answer)
                {
                    Ok(()) => {
                        let outcome = if decision == "cancel" {
                            "cancelled"
                        } else {
                            "submitted"
                        };
                        self.resolved(&interaction_id, &command_id, outcome);
                        self.receipt(&request_id, "completed", Some(json!({ "ok": true })));
                    }
                    Err(error) => self.failed(&request_id, error.to_string()),
                }
            }
            _ => self.receipt(&request_id, "rejected", None),
        }
    }

    fn refresh_state(&mut self) {
        if let Ok(state) =
            self.step
                .request("get_state", json!({}), std::time::Duration::from_secs(10))
        {
            if let Some(id) = state.get("sessionId").and_then(|v| v.as_str()) {
                if !id.is_empty() && id != self.session_id {
                    if std::env::var_os("STEP_STUDIO_TRACE").is_some() {
                        eprintln!(
                            "[runtime] session {} -> {}",
                            short(&self.session_id),
                            short(id)
                        );
                    }
                    self.session_id = id.to_string();
                }
            }
        }
    }

    /// Pump CLI events into Studio events until the prompt settles.
    fn pump_events(&mut self, timeout: std::time::Duration) -> bool {
        let mut settled = false;
        while let Some(event) = self.step.wait_event(timeout) {
            let kind = event
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if std::env::var_os("STEP_STUDIO_TRACE").is_some() {
                eprintln!(
                    "[runtime] cli event {kind} {}",
                    serde_json::to_string(&event)
                        .unwrap_or_default()
                        .chars()
                        .take(300)
                        .collect::<String>()
                );
            }
            self.route_event(&event);
            // `prompt_result`, correlated on the id the prompt was sent under —
            // upstream calls it the "terminal frame emitted exactly once per
            // accepted prompt/abort_and_prompt". `session_settled` is *not* a
            // substitute for it: that frame is session-scoped and carries no id,
            // so it fires whenever the session goes quiet — including when a
            // queued prompt's work finishes while this turn is still live.
            // Matching it unconditionally ended live turns early.
            //
            // (`agent_settled` was the first thing matched here. It appears
            // nowhere in the Runtime's source; no prompt ever settled, the Host
            // waited out its whole timeout, and the shell sat on "streaming"
            // with a Stop button wired to a finished turn.)
            if kind == "prompt_result"
                && event.get("id").and_then(|v| v.as_str()) == Some(&self.prompt_request_id)
            {
                // The turn's own verdict comes from the same frame
                // (`RpcPromptResultFrame`: `status` plus
                // `error{message, provider, model, httpStatus, retryable}`), and
                // its `sessionSettled` flag says whether more work will follow.
                //
                // This used to read a `turn_error` field that was declared,
                // initialised to `None` and never written anywhere in the crate —
                // so a turn whose `prompt_result` was `status:"error"` settled the
                // Studio prompt as `completed {ok:true}`. The field's own comment
                // described exactly the behaviour that did not exist. It is gone;
                // the frame is the source of truth.
                let error = prompt_failure(&event);
                self.settle_prompt(error);
                settled = true;
                break;
            }
        }
        // The CLI can die mid-turn. Nothing will ever settle the prompt then,
        // so the Host would wait forever and the shell would sit on "streaming"
        // with a Stop button that talks to a dead process. The TypeScript
        // adapter settles on exit; this is the same guarantee.
        if !settled && self.step.closed() {
            self.settle_prompt(Some(
                "the Step CLI exited before the turn finished".to_string(),
            ));
            settled = true;
        }
        settled
    }

    fn settle_prompt(&mut self, error: Option<String>) {
        let Some(request_id) = self.pending_prompt.take() else {
            return;
        };
        match error {
            Some(message) => self.failed(&request_id, message),
            None => self.receipt(&request_id, "completed", Some(json!({ "ok": true }))),
        }
    }
}

/// Resolve a selector against the live catalog.
///
/// Step model ids routinely contain a slash of their own
/// (`zed/claude-haiku-5-5` under provider `local-gateway`), so splitting on
/// the first slash reads the provider as `zed`. Matching the whole selector
/// as an id first, then falling back to `provider/id`, accepts both forms.
fn resolve_and_set_model(step: &mut StepRpc, selector: &str) -> Result<(), String> {
    let catalog = step
        .request(
            "get_available_models",
            json!({}),
            std::time::Duration::from_secs(10),
        )
        .map_err(|e| e.to_string())?;
    let models = catalog.get("models").and_then(|m| m.as_array());

    if let Some(list) = models {
        if let Some(match_by_id) = list
            .iter()
            .find(|m| m.get("id").and_then(|v| v.as_str()) == Some(selector))
        {
            let provider = match_by_id
                .get("provider")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            return set_model(step, provider, selector);
        }
    }
    if let Some(slash) = selector.find('/') {
        let provider = &selector[..slash];
        let id = &selector[slash + 1..];
        if let Some(list) = models {
            let found = list.iter().any(|m| {
                m.get("provider").and_then(|v| v.as_str()) == Some(provider)
                    && m.get("id").and_then(|v| v.as_str()) == Some(id)
            });
            if found {
                return set_model(step, provider, id);
            }
        }
    }
    Err(format!("model not found: {selector}"))
}

fn set_model(step: &mut StepRpc, provider: &str, model_id: &str) -> Result<(), String> {
    step.request(
        "set_model",
        json!({ "provider": provider, "modelId": model_id }),
        std::time::Duration::from_secs(20),
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

fn set_thinking(step: &mut StepRpc, level: &str) -> Result<(), String> {
    const LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];
    if !LEVELS.contains(&level) {
        if level != "auto" {
            return Err(format!("unsupported thinking level: {level}"));
        }
        // `auto` has no Step equivalent: take the model's first supported level.
        let catalog = step
            .request(
                "get_available_thinking_levels",
                json!({}),
                std::time::Duration::from_secs(10),
            )
            .map_err(|e| e.to_string())?;
        let levels = catalog.get("levels").and_then(|l| l.as_array());
        let first = levels
            .and_then(|list| list.first())
            .and_then(|v| v.as_str())
            .ok_or("model reports no thinking levels")?;
        return apply_thinking(step, first);
    }
    apply_thinking(step, level)
}

fn apply_thinking(step: &mut StepRpc, level: &str) -> Result<(), String> {
    step.request(
        "set_thinking_level",
        json!({ "level": level }),
        std::time::Duration::from_secs(10),
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

fn fork_session(step: &mut StepRpc) -> Result<(), String> {
    let entries = step
        .request("get_entries", json!({}), std::time::Duration::from_secs(10))
        .map_err(|e| e.to_string())?;
    let leaf = entries.get("leafId").and_then(|v| v.as_str()).unwrap_or("");
    if leaf.is_empty() {
        return Err("session has no entry to fork from".to_string());
    }
    step.request(
        "fork",
        json!({ "entryId": leaf }),
        std::time::Duration::from_secs(20),
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// One page of the active branch. Step exposes no pagination cursor, so the
/// head cursor is a generation token and any other cursor is stale.
fn read_transcript(step: &mut StepRpc, cursor: Option<&str>) -> Result<Value, String> {
    let entries = step
        .request("get_entries", json!({}), std::time::Duration::from_secs(15))
        .map_err(|e| e.to_string())?;
    let list = entries
        .get("entries")
        .and_then(|e| e.as_array())
        .cloned()
        .unwrap_or_default();
    let leaf = entries
        .get("leafId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let head = format!("step-1:{}", if leaf.is_empty() { "empty" } else { &leaf });
    if let Some(cursor) = cursor {
        if !cursor.is_empty() && cursor != head {
            return Err(
                "CURSOR_STALE: transcript cursor does not match the active session".to_string(),
            );
        }
    }
    let items: Vec<Value> = list
        .iter()
        .filter(|entry| entry.get("type").and_then(|v| v.as_str()) == Some("message"))
        .filter_map(|entry| {
            let message = entry.get("message")?;
            let role = message.get("role").and_then(|v| v.as_str())?;
            // `toolResult` is a message too, and dropping it left a restored
            // conversation showing tool calls with no output.
            if !matches!(role, "user" | "assistant" | "toolResult") {
                return None;
            }
            let mut item = json!({
                "kind": "message",
                "itemId": entry.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                "parentId": entry.get("parentId").and_then(|v| v.as_str()),
                "createdAt": crate::conversation::to_iso(entry.get("timestamp").unwrap_or(&Value::Null)),
                "role": role,
                "content": crate::conversation::to_studio_blocks(Some(message.get("content").unwrap_or(&Value::Null))),
            });
            if role == "toolResult" {
                // Step pairs a result to its call by id, so the shell can match
                // it exactly rather than by position.
                if let Some(object) = item.as_object_mut() {
                    object.insert(
                        "toolCallId".to_string(),
                        json!(message.get("toolCallId").and_then(|v| v.as_str()).unwrap_or("")),
                    );
                    object.insert(
                        "toolName".to_string(),
                        json!(message.get("toolName").and_then(|v| v.as_str()).unwrap_or("")),
                    );
                    object.insert(
                        "isError".to_string(),
                        json!(message.get("isError").and_then(|v| v.as_bool()).unwrap_or(false)),
                    );
                }
            }
            Some(item)
        })
        .collect();
    Ok(json!({
        "runtimeEpoch": step_epoch_placeholder(),
        "sessionId": "",
        "branchLeafId": if leaf.is_empty() { Value::Null } else { json!(leaf) },
        "items": items,
        "headCursor": head,
        "hasMoreBefore": false,
    }))
}

fn step_epoch_placeholder() -> Value {
    json!(0)
}

/// Whether a confirm answer approves. The Studio contract's approval values
/// are `true`, `"once"`, `"session"` or a grant object; anything else denies.
/// Why a turn failed, read off the `prompt_result` frame that says so.
///
/// `None` for every other frame and for every non-error status — including
/// `aborted`, which `e2e.rs` deliberately pins as settling `completed`, because an
/// abort is something the user asked for rather than something that went wrong.
fn prompt_failure(event: &Value) -> Option<String> {
    if event.get("type").and_then(|v| v.as_str()) != Some("prompt_result") {
        return None;
    }
    if event.get("status").and_then(|v| v.as_str()) != Some("error") {
        return None;
    }
    let message = event
        .pointer("/error/message")
        .and_then(|m| m.as_str())
        .filter(|m| !m.is_empty())
        .unwrap_or("the turn failed");
    // Provider and model are what make the message actionable: the same text
    // means different things from different providers, and the shell shows this
    // string to a user deciding whether to retry.
    let context = ["provider", "model"]
        .iter()
        .filter_map(|key| event.pointer(&format!("/error/{key}")).and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join(" ");
    Some(if context.is_empty() { message.to_string() } else { format!("{message} ({context})") })
}

fn is_affirmative(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(true)) => true,
        Some(Value::String(choice)) => choice == "once" || choice == "session",
        Some(Value::Object(_)) => true,
        _ => false,
    }
}

/// One decoded frame from the Host, or the reason the socket went away.
enum HostFrame {
    Body(Value),
    Closed,
}

fn serve_connection(
    stream: transport::Accepted,
    config: AdapterConfig,
    terminal: Arc<Mutex<Vec<Value>>>,
) -> Result<(), String> {
    let step = StepRpc::spawn(&crate::step_rpc::StepLaunch {
        command: config.step_command.clone(),
        cwd: config.cwd.clone(),
        env: config.step_env.clone(),
        continue_session: config.continue_session,
    })
    .map_err(|e| e.to_string())?;

    let manifest = capability_manifest();
    let command_hash = manifest.command_hash.clone();
    let mut session = Session {
        stream,
        step,
        epoch: config.runtime_epoch,
        seq: 0,
        context: MapContext::new(String::new()),
        session_id: String::new(),
        terminal,
        shutting_down: false,
        shutdown_deadline: None,
        state_version: 0,
        command_manifest_hash: command_hash,
        pending_prompt: None,
        prompt_request_id: String::new(),
        pending_usage: None,
        interactions: InteractionRegistry::default(),
    };

    // Handshake: the runtime must prove it holds the token before anything else
    // is trusted. This one frame is read directly, before the reader thread
    // exists, so there is exactly one owner of the socket.
    let frame = {
        let mut hello_reader = BufReader::new(transport::handshake_reader(&mut session.stream));
        read_frame(&mut hello_reader)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "host closed before hello".to_string())?
    };
    let body = frame.body;
    if body.get("type").and_then(|v| v.as_str()) != Some("studio.hello") {
        return Err("first frame was not a studio.hello".to_string());
    }
    let challenge = body.get("challenge").and_then(|v| v.as_str()).unwrap_or("");
    let instance_id = format!("step-runtime-{}", std::process::id());
    let proof = challenge_proof(&config.token, challenge, &instance_id);
    let _ = verify_hello(&body, &config.token, challenge); // validation only
    session
        .send(&json!({
            "type": "studio.hello.result",
            "requestId": body.get("requestId"),
            "selectedProtocolVersion": PROTOCOL_VERSION,
            "runtimeVersion": manifest.runtime_version,
            "upstreamVersion": "step-code",
            "upstreamCommit": "0.1.7",
            "runtimeInstanceId": instance_id,
            "runtimeEpoch": session.epoch,
            "capabilityManifest": manifest.value,
            "commandManifestHash": manifest.command_hash,
            "stateVersion": 0,
            "challengeProof": proof,
        }))
        .map_err(|e| e.to_string())?;

    // The session id only exists once the CLI has created its session.
    session.refresh_state();
    if !session.session_id.is_empty() {
        session.context = MapContext::new(session.session_id.clone());
    }

    // A reader thread owns the Host socket for the same reason the CLI has one:
    // a blocking read here would stall the event pump while a turn streams.
    let (host_tx, host_rx) = std::sync::mpsc::channel::<HostFrame>();
    let reader_stream = stream_for_reader(&session)?;
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader_stream);
        loop {
            match read_frame(&mut reader) {
                Ok(Some(frame)) => {
                    if host_tx.send(HostFrame::Body(frame.body)).is_err() {
                        return;
                    }
                }
                Ok(None) => {
                    let _ = host_tx.send(HostFrame::Closed);
                    return;
                }
                Err(_) => {
                    let _ = host_tx.send(HostFrame::Closed);
                    return;
                }
            }
        }
    });

    loop {
        // A shutdown waits for the turn in flight, but only until its deadline.
        // Past it the loop returns and the process exits, which is what makes
        // the wait bounded — and the turn that missed it settles as
        // `outcome_unknown` on the client rather than as a completed lie.
        if session.shutting_down && !session.draining_a_turn() {
            return Ok(());
        }
        // A streaming turn must keep flowing while the Host is idle, so the CLI
        // is polled first with a short budget rather than blocking on either.
        // Pumped for a parked `usage.panel` as well as for a turn. Without the
        // second condition an idle connection never drains the CLI's events, so
        // the panel a query is waiting for would never be read.
        // A parked `usage.panel` has to time out even when the CLI has gone
        // quiet — that is the only state its deadline can fire in.
        session.expire_usage();
        if (session.pending_prompt.is_some() || session.pending_usage.is_some())
            && session.pump_events(std::time::Duration::from_millis(50))
        {
            continue;
        }
        match host_rx.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(HostFrame::Body(body)) => {
                if body.get("type").and_then(|v| v.as_str()) == Some("studio.request") {
                    session.dispatch(&body);
                }
            }
            Ok(HostFrame::Closed) => return Ok(()),
            // A timeout is not a disconnect: the Host is simply idle while a
            // turn streams. Treating it as an ending dropped the session, which
            // killed the CLI mid-turn.
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => return Ok(()),
        }
        if session.shutting_down {
            return Ok(());
        }
    }
}

/// A second handle on the Host socket for the reader thread.
#[cfg(unix)]
fn stream_for_reader(session: &Session) -> Result<Accepted, String> {
    // A UnixStream is duplicable, which is the only way to read and write one
    // concurrently.
    // The session holds an `Accepted`, whose stream arm is the Unix socket.
    match &session.stream {
        Accepted::Stream(stream) => transport::duplicate(stream).map(Accepted::Stream),
    }
    .map_err(|e| format!("could not duplicate the bridge socket: {e}"))
}

/// On Windows the accepted connection is a pipe behind an `Arc`, so the reader
/// gets a clone of the same handle rather than a second connection to the same
/// name.
#[cfg(windows)]
fn stream_for_reader(session: &Session) -> Result<Accepted, String> {
    // `Read` and `Write` are both on `&self` for `miow::pipe::NamedPipe`, and
    // the listener is an `Arc`, so one connection serves both threads. This
    // used to open a second pipe to the same name on the premise that a pipe
    // handle could not be shared — which was wrong, and needed
    // `STEP_STUDIO_TRANSPORT_ENDPOINT` plumbed through the environment to find
    // the name again.
    match &session.stream {
        Accepted::Server(server) => Ok(Accepted::Server(std::sync::Arc::clone(server))),
        Accepted::Stream(stream) => transport::duplicate(stream).map(Accepted::Stream),
    }
    .map_err(|e| format!("could not duplicate the bridge socket: {e}"))
}

/// Errors from the CLI that must not crash the adapter.
pub fn describe_step_error(error: &StepError) -> String {
    error.to_string()
}
