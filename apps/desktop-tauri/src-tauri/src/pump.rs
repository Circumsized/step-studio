//! The session pump: the one thread that owns the Studio Bridge session.
//!
//! Why a thread and not a `Mutex<BridgeSession>` shared with the commands:
//! events are broadcast, not addressed, so **at most one event-consuming
//! command may be in flight** on a session. A mutex would let two Tauri
//! commands await two replies at once and split one stream between them.
//! Routing every operation through one thread makes that invariant structural
//! rather than something a counter has to enforce — and it is what lets
//! `core.abort` reach the session while a turn is still streaming.
//!
//! The second job is translation. The renderer speaks the Host facade's
//! vocabulary (`conversation.open`, `session.model.set`); the adapter speaks
//! its own (`session.transcript.read`, `core.prompt`). Only the difference is
//! mapped here — the adapter's event payloads are already
//! `ConversationRuntimeEvent` and ride through verbatim, which is why
//! streaming costs nothing but an envelope.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use step_studio_bridge::BridgeSession;

/// The channel `transport-desktop` subscribes to. Must stay byte-identical to
/// `DESKTOP_IPC_CHANNELS.event` in `packages/transport-desktop/src/channels.ts`.
pub const EVENT_CHANNEL: &str = "omp-studio:desktop:event";

/// Operation kinds the adapter accepts verbatim as `{kind, ...input}`, with the
/// grade the adapter reports for each (`packages/runtime-stepcode/src/main.ts:29-84`).
///
/// The list is the adapter's own dispatch, not a design of ours: anything absent
/// here is not something this shell can do, and the renderer is told so rather
/// than being handed a guess.
///
/// The grade is load-bearing, not decoration. The renderer's `can()`
/// (`apps/renderer/src/App.tsx:3831`) refuses anything marked unavailable, so a
/// capability declared here is one the composer will let the user press.
const ADAPTER_NATIVE: &[(&str, &str)] = &[
    ("core.steer", "stable"),
    ("core.followUp", "stable"),
    ("core.abort", "stable"),
    ("session.new", "stable"),
    ("session.fork", "limited"),
    ("session.model.set", "stable"),
    ("session.thinking.set", "limited"),
    ("session.models.list", "stable"),
    ("session.commands.list", "stable"),
    ("session.extensions.list", "stable"),
    ("session.transcript.read", "stable"),
    ("session.state", "stable"),
];

/// `core.prompt` sits apart because it is the one `Dispatch::Turn` operation.
const PROMPT_CAPABILITY: (&str, &str) = ("core.prompt", "stable");

/// Facade names the shell answers itself, with the grade the data earns.
///
/// Separate from `ADAPTER_NATIVE` because these are not adapter operations:
/// they are answered in the pump's local arm, and listing them among the
/// adapter's ids routes them to the runtime — which rejects them, and does so
/// with `result: null` rather than an error, so the renderer's `.catch` never
/// fires and a panel silently empties. That is exactly what the first version
/// of this table did.
const SHELL_LOCAL: &[(&str, &str)] = &[
    // Answered by the shell from its own dispatch table.
    ("capabilities.get", "stable"),
    // Facts about the workspace, not about the runtime: the shell reads them from
    // the `git` CLI. The adapter has no git capability to offer.
    ("git.repository.get", "limited"),
    // The write side. `limited` because a third of the contract's ~30 operation
    // kinds are wired; the rest are refused with a message that says so.
    ("git.execute", "limited"),
    // The workspace's files, read from disk by the shell. The adapter has no
    // filesystem capability to offer.
    ("workspace.fileTree", "limited"),
    // `limited` because the history page is read but not written: commits are
    // still made elsewhere.
    ("git.log.list", "limited"),
    ("git.branches.list", "limited"),
    ("git.diff.get", "limited"),
    // Answered by the shell from Step's own session store, which is a fact
    // about this machine rather than about the runtime.
    ("history.list", "limited"),
    // The projects Step has sessions for, from the same store.
    ("projects.list", "limited"),
    // Assembled from what the shell already holds, so it is local: no new data
    // source, and no round trip for bytes the renderer already has.
    ("home.get", "limited"),
    // The sub-agent sessions Step keeps on disk. Step excludes them from every
    // RPC list, so the store is the source.
    ("session.agents.list", "limited"),
    ("workspace.open", "limited"),
    // Both are facts about this machine and this shell, so the shell answers
    // them: no adapter round trip for information it already has.
    ("environment.get", "limited"),
    ("diagnostics.get", "limited"),
];

const RENAMED: &[(&str, &str)] = &[
    // `session.create` is a Host-level concept (workspace, archive entry); the
    // nearest thing a single-session adapter has is a fresh session.
    ("session.create", "session.new"),
    // The renderer opens a conversation; the adapter reads a transcript page.
    ("conversation.open", "session.transcript.read"),
    // The facade's usage query; the adapter asks the CLI for its `/usage` panel.
    ("usage.get", "usage.panel"),
    // The facade's model config; the adapter has a catalog and nothing else.
    ("models.get", "session.models.list"),
    // The capabilities page's two lists. The adapter serves both and declares
    // them stable; the facade spellings and the reshape were what was missing.
    ("mcp.get", "session.extensions.list"),
    ("skills.get", "session.commands.list"),
];

/// How the adapter answers an operation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dispatch {
    /// The reply outlives the turn: the adapter answers `accepted` now and the
    /// terminal receipt only when the turn ends (`main.ts:603-611`). Fired
    /// without waiting, so the pump stays free to run `core.abort`.
    Turn,
    /// Answers with its own terminal receipt.
    Control,
    /// Answered by the shell itself, from what it already knows.
    ///
    /// The capability manifest is derived from this dispatch table, so asking
    /// the adapter for it would be a round trip to learn something the shell
    /// computed before the session existed.
    Local,
}

/// `{...input, kind}`. The spread order is the one the Electron facade uses
/// (`packages/host-client-api/src/facade.ts:435-438`), so a caller cannot
/// override `kind` by shipping one inside `input`.
fn spread(input: &Value, kind: &str) -> Value {
    let mut object = match input {
        Value::Object(map) => map.clone(),
        _ => serde_json::Map::new(),
    };
    object.insert("kind".to_string(), Value::String(kind.to_string()));
    Value::Object(object)
}

/// Map a facade name onto an adapter operation. `None` means this shell cannot
/// serve it, which the renderer renders as an honest unavailable.
pub fn resolve_operation(name: &str, input: &Value) -> Option<(Value, Dispatch)> {
    if name == "core.prompt" {
        return Some((spread(input, "core.prompt"), Dispatch::Turn));
    }
    if name == "capabilities.get" {
        return Some((json!({}), Dispatch::Local));
    }
    if name == "git.repository.get"
        || name == "git.log.list"
        || name == "git.branches.list"
        || name == "git.diff.get"
        || name == "git.execute"
        || name == "workspace.fileTree"
    {
        // The whole input, spread. Hand-picking `workspaceId`/`limit`/`skip`
        // dropped `operation`, `path`, `target` and `paths`, so every
        // `git.execute` was refused for a missing operation, every
        // `workspace.fileTree` listed the repository root regardless of the
        // directory asked for, and every `git.diff.get` returned an empty patch.
        // None of the module's tests caught it: they call the module directly.
        return Some((spread(input, ""), Dispatch::Local));
    }
    if let Some((_, renamed)) = RENAMED.iter().find(|(from, _)| *from == name) {
        return Some((spread(input, renamed), Dispatch::Control));
    }
    if SHELL_LOCAL.iter().any(|(id, _)| *id == name) {
        return Some((spread(input, ""), Dispatch::Local));
    }
    if let Some((id, _)) = ADAPTER_NATIVE.iter().find(|(id, _)| *id == name) {
        return Some((spread(input, id), Dispatch::Control));
    }
    None
}

/// The capability manifest, derived from the dispatch tables so it cannot claim
/// something [`resolve_operation`] refuses.
///
/// Deriving it is the point: a hand-written list would drift the moment an
/// operation is added or removed, and the failure mode is silent — the renderer
/// offers an action the shell then rejects.
pub fn capability_manifest() -> Value {
    let (prompt_id, prompt_grade) = PROMPT_CAPABILITY;
    let mut capabilities = vec![json!({
        "id": prompt_id,
        "grade": prompt_grade,
        "version": 1,
        "evidence": "step-studio-tauri",
    })];
    for (id, grade) in ADAPTER_NATIVE {
        capabilities.push(json!({
            "id": id,
            "grade": grade,
            "version": 1,
            "evidence": "step-studio-tauri",
        }));
    }
    // Every facade spelling the renderer actually uses, with the grade of
    // whatever it resolves to.
    //
    // Derived rather than listed: a second list of names is a second thing to
    // keep in step with the tables, and the first version of this claimed only
    // `ADAPTER_NATIVE` — which left `can()` false for `usage.get`, `models.get`,
    // `capabilities.get`, `session.create` and `conversation.open`, five names
    // this shell serves.
    for (id, grade) in SHELL_LOCAL {
        capabilities.push(json!({
            "id": id,
            "grade": grade,
            "version": 1,
            "evidence": "step-studio-tauri",
        }));
    }
    for (from, to) in RENAMED {
        let grade = ADAPTER_NATIVE
            .iter()
            .find(|(id, _)| id == to)
            .map(|(_, grade)| *grade)
            .unwrap_or("limited");
        capabilities.push(json!({
            "id": from,
            "grade": grade,
            "version": 1,
            "evidence": "step-studio-tauri",
        }));
    }
    // Hashed over the capability list only, exactly as the adapter does
    // (`main.ts:88-93`): `generatedAt` changes every boot and must not change
    // the identity, or the renderer would refetch the manifest every launch.
    let mut hasher = Sha256::new();
    hasher.update(Value::Array(capabilities.clone()).to_string().as_bytes());
    let hash = hasher.finalize();
    json!({
        "profile": "limited",
        "generatedAt": iso_utc(now_millis()),
        "hash": to_hex(&hash),
        "capabilities": capabilities,
    })
}

fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        // A hex conversion cannot fail.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// What the caller wants back.
///
/// A command is answered with its ack and learns the outcome from the receipt
/// event, exactly as the Electron facade does. A query wants the payload.
#[derive(Clone, Copy, PartialEq)]
pub enum Want {
    Ack,
    Result,
}

/// One operation handed to the pump, with where to send its answer.
pub struct Job {
    pub request_id: String,
    pub command_name: String,
    pub operation: Value,
    pub dispatch: Dispatch,
    pub want: Want,
    pub reply: Sender<Result<Value, String>>,
}

/// Per-stream sequence state.
///
/// `state_version` deliberately does not advance on a delta: the adapter only
/// bumps it for state-changing events (`main.ts:162-167`), and
/// `runtime-projection.ts:38-50` records why republishing an unchanged
/// read model per token is a cost, not a feature. Mirroring that keeps the
/// renderer's max-tracking `advanceConnection` honest.
struct Stream {
    /// The snapshot this session started with. Replaced whenever a fresher one
    /// arrives, so a later `home.get` answers with the current state rather than
    /// the one the shell booted with.
    snapshot: Value,
    cursor: u64,
    /// Which adapter this session is talking to. It changes when the adapter
    /// restarts, which is the signal a reconnecting shell needs: the daemon's
    /// epoch does not move when only the adapter did.
    runtime_id: String,
    stream_seq: u64,
    state_version: u64,
    runtime_epoch: u64,
    authority_epoch: u64,
    /// The prompt whose turn is running. The adapter never names the requestId
    /// in `conversation.turn.completed`, so the pairing is kept here instead.
    pending_turn: Option<(String, String)>,
}

/// What [`Stream::checkpoint`] captures.
struct DeliveryState {
    cursor: u64,
    pending_turn: Option<(String, String)>,
}

impl Stream {
    /// Everything a delivery can change, so an undelivered batch can be undone.
    ///
    /// Two fields, not one: `translate` consumes `pending_turn` when it builds a
    /// turn-end receipt, so rolling back only the cursor would leave a prompt
    /// whose receipt was built, dropped, and can never be built again.
    fn checkpoint(&self) -> DeliveryState {
        DeliveryState {
            cursor: self.cursor,
            pending_turn: self.pending_turn.clone(),
        }
    }

    fn rollback(&mut self, state: DeliveryState) {
        self.cursor = state.cursor;
        self.pending_turn = state.pending_turn;
    }

    /// A fresh `ClientEventBase`. Every event increments `cursor` by one, so
    /// the sequence never shows the gap the contract reads as a resync.
    fn base(&mut self, kind: &str) -> Value {
        self.cursor += 1;
        json!({
            "kind": kind,
            "authorityEpoch": self.authority_epoch,
            "runtimeEpoch": self.runtime_epoch,
            "stateVersion": self.state_version,
            "cursor": self.cursor,
            "occurredAt": iso_utc(now_millis()),
        })
    }

    fn receipt(&mut self, request_id: &str, command_name: &str, status: &str, extra: Value) -> Value {
        let mut event = self.base("command.receipt");
        let mut receipt = json!({
            "requestId": request_id,
            "commandName": command_name,
            "status": status,
            "observedAt": iso_utc(now_millis()),
        });
        if let (Some(target), Some(source)) = (receipt.as_object_mut(), extra.as_object()) {
            for (key, value) in source {
                target.insert(key.clone(), value.clone());
            }
        }
        event["receipt"] = receipt;
        event
    }

    /// Translate one adapter event into the ClientEvents the renderer consumes.
    fn translate(&mut self, event: &Value) -> Vec<Value> {
        let kind = event.get("kind").and_then(Value::as_str).unwrap_or_default();
        if !kind.starts_with("conversation.") {
            // Anything the conversation engine does not want still has to move
            // the transport's cursor and version forward.
            return vec![self.base("state.changed")];
        }

        let mut out = Vec::new();
        if kind == "conversation.turn.completed" || kind == "conversation.turn.aborted" {
            if let Some((request_id, command_name)) = self.pending_turn.take() {
                let (status, extra) = if kind == "conversation.turn.completed" {
                    ("completed", json!({ "result": {} }))
                } else {
                    (
                        "failed",
                        json!({ "error": { "code": "TURN_ABORTED", "message": "the turn was aborted" } }),
                    )
                };
                out.push(self.receipt(&request_id, &command_name, status, extra));
            }
        }

        self.stream_seq += 1;
        let mut changed = self.base("conversation.changed");
        changed["sessionId"] = event
            .get("sessionId")
            .cloned()
            .unwrap_or_else(|| Value::String(String::new()));
        changed["streamSeq"] = json!(self.stream_seq);
        changed["eventSeq"] = json!(self.cursor);
        // The adapter event is already a ConversationRuntimeEvent.
        changed["update"] = event.clone();
        out.push(changed);
        out
    }
}

/// Where translated events go. Returns whether anyone actually received it.
///
/// A boxed closure rather than a `WebviewWindow` so the pump can be driven
/// without a window at all — which is how the shell proves a turn end to end
/// on a headless machine (see `STEP_STUDIO_SELF_CHECK` in `main.rs`).
///
/// The return value is load-bearing: see [`deliver`].
pub type EventSink = Box<dyn Fn(Value) -> bool + Send + Sync + 'static>;

/// Start the pump. Returns the channel commands and queries send through.
/// Start the pump, and hand back both the job sender and the thread's handle.
///
/// The handle is the point: the pump ends when the session does, and whoever
/// owns the shell's lifetime needs to know that happened. Returning only the
/// sender left a dead session looking like a session whose commands all fail.
pub fn spawn_pump(
    session: BridgeSession,
    sink: EventSink,
    runtime_epoch: u64,
    authority_epoch: u64,
    state_version: u64,
    runtime_id: String,
    // The snapshot the session started with, so `home.get` can assemble a read
    // model around it. Seeded from the bootstrap rather than fetched: the
    // renderer already has it, and a second round trip for the same bytes is a
    // round trip spent learning nothing.
    snapshot: Value,
) -> (Sender<Job>, std::thread::JoinHandle<Option<String>>) {
    let (sender, receiver) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        pump(
            session,
            &sink,
            receiver,
            Stream {
                snapshot,
                cursor: 0,
                stream_seq: 0,
                state_version,
                runtime_epoch,
                authority_epoch,
                pending_turn: None,
                runtime_id,
            },
        )
    });
    (sender, handle)
}

/// The event that tells the renderer which authority this session belongs to.
///
/// A reconnect produces no command and no adapter event until the user does
/// something, so without this the renderer has no way to learn the authority
/// changed — it would keep the state the dead session left behind. Both halves
/// of the identity are carried: the epoch moves when the daemon restarts, the
/// runtime id when the adapter does, and the second is the common case.
fn connection_announcement(stream: &mut Stream) -> Value {
    let mut event = stream.base("runtime.changed");
    event["connection"] = json!({
        "status": "connected",
        // The daemon manages the adapter's lifetime, which is what "managed"
        // means here.
        "classification": "managed",
        "runtimeId": stream.runtime_id,
        "runtimeEpoch": stream.runtime_epoch,
    });
    event
}

/// Hand one adapter event to the sink, and only advances the stream if it landed.
///
/// The bootstrap advertises `cursor: 0` as the resume point, so the counter has
/// to describe what the renderer has actually received. An event that moves the
/// counter without reaching anyone turns the first real event into a gap, and a
/// gap forces `resync_required` (`packages/client/src/reducer.ts:737`) — the
/// whole shell then sits behind "restoring latest Runtime state" until a fresh
/// snapshot arrives, which this shell never sends.
///
/// Events arriving before `subscribe` are exactly that case: the self-check
/// turn and the adapter's own startup chatter land while no window exists yet.
///
/// A partially delivered batch (the window vanishing between two calls) would
/// still leave the counter ahead of what was received, but that case has no
/// client left to observe a gap.
fn deliver(sink: &EventSink, stream: &mut Stream, event: &Value) {
    let before = stream.checkpoint();
    let events = stream.translate(event);
    let mut received = false;
    for payload in &events {
        if sink(payload.clone()) {
            received = true;
        }
    }
    if !received {
        stream.rollback(before);
    }
}

/// The same rule for a payload this shell builds itself — a command receipt.
///
/// Kept separate from [`deliver`] because the payload is built here rather than
/// translated, but the rewind rule is identical: a receipt that never reaches
/// the renderer must not consume a cursor either.
fn emit(sink: &EventSink, stream: &mut Stream, build: impl FnOnce(&mut Stream) -> Value) {
    let before = stream.checkpoint();
    if !sink(build(stream)) {
        stream.rollback(before);
    }
}

/// Drive one session until it ends. Returns why it ended, when it ended on its
/// own rather than because the shell closed the job channel.
fn pump(
    session: BridgeSession,
    sink: &EventSink,
    jobs: Receiver<Job>,
    mut stream: Stream,
) -> Option<String> {
    // Announce this session before anything else.
    //
    // A reconnect produces no command and no adapter event until the user does
    // something, so without this the renderer has no way to learn the authority
    // changed — it would keep the state the dead session left behind. The
    // cursor comes from the stream, so this cannot collide with the first real
    // event the way an event emitted from outside the pump would.
    emit(sink, &mut stream, connection_announcement);
    loop {
        // The connection can die while the shell is idle, with no command to
        // fail and so nothing to notice. Checking here is what turns "every
        // command errors" into "the session is gone, reconnect".
        if let Some(reason) = session.close_reason() {
            return Some(reason);
        }
        match jobs.recv_timeout(Duration::from_millis(50)) {
            Ok(job) => {
                let ack = json!({
                    "commandName": job.command_name,
                    "requestId": job.request_id,
                    "status": "accepted",
                    "acceptedAt": iso_utc(now_millis()),
                });
                match job.dispatch {
                    Dispatch::Local => {
                        // Shell-local answers, all for the same reason: each is
                        // something the shell already knows, so asking the adapter
                        // for it would be a round trip to learn it. The manifest
                        // is derived from these very dispatch tables; the git
                        // state is a fact about the workspace; the history is a
                        // fact about this machine's session store.
                        let answer = if job.command_name == "git.repository.get" {
                            let workspace_id = job
                                .operation
                                .get("workspaceId")
                                .and_then(Value::as_str)
                                .unwrap_or("");
                            crate::git::repository_get(workspace_id)
                        } else if job.command_name == "git.branches.list" {
                            let workspace_id = job
                                .operation
                                .get("workspaceId")
                                .and_then(Value::as_str)
                                .unwrap_or("");
                            crate::git::branches_list(workspace_id)
                        } else if job.command_name == "git.execute" {
                            let operation = job
                                .operation
                                .get("operation")
                                .cloned()
                                .unwrap_or(Value::Null);
                            crate::git::execute(&operation)
                        } else if job.command_name == "session.agents.list" {
                            crate::history::agents(job.operation.get("sessionId").and_then(Value::as_str))
                        } else if job.command_name == "environment.get" {
                            crate::diagnostics::environment(stream.runtime_id.clone(), stream.authority_epoch, stream.runtime_epoch)
                        } else if job.command_name == "diagnostics.get" {
                            crate::diagnostics::report(stream.runtime_id.clone(), stream.authority_epoch)
                        } else if job.command_name == "workspace.open" {
                            let workspace_id = job
                                .operation
                                .get("workspaceId")
                                .and_then(Value::as_str)
                                .unwrap_or("");
                            crate::history::open(workspace_id)
                        } else if job.command_name == "home.get" {
                            // An assembly, not a fetch: authority and runtime
                            // come from this session's epochs, the snapshot from
                            // the stream, and the two lists from the session
                            // store. Nothing here asks the adapter.
                            let history = crate::history::list(Some(10), None);
                            let recent_threads: Vec<Value> = history
                                .get("entries")
                                .and_then(Value::as_array)
                                .map(|entries| {
                                    entries
                                        .iter()
                                        .map(|entry| {
                                            json!({
                                                "threadId": entry.get("threadId").cloned().unwrap_or(Value::Null),
                                                "title": entry.get("title").cloned().unwrap_or(Value::Null),
                                                "lastActiveAt": entry.get("lastActiveAt").cloned().unwrap_or(Value::Null),
                                            })
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();
                            json!({
                                "authority": {
                                    "authorityId": stream.runtime_id,
                                    "authorityEpoch": stream.authority_epoch,
                                },
                                "runtime": {
                                    "status": "connected",
                                    "classification": "managed",
                                    "runtimeId": stream.runtime_id,
                                    "runtimeEpoch": stream.runtime_epoch,
                                },
                                "snapshot": stream.snapshot.clone(),
                                "recentThreads": recent_threads,
                                "recentWorkspaces": crate::history::projects()
                                    .get("workspaces")
                                    .cloned()
                                    .unwrap_or(Value::Array(Vec::new())),
                            })
                        } else if job.command_name == "projects.list" {
                            crate::history::projects()
                        } else if job.command_name == "history.list" {
                            let limit = job.operation.get("limit").and_then(Value::as_u64);
                            let workspace_id =
                                job.operation.get("workspaceId").and_then(Value::as_str);
                            crate::history::list(limit, workspace_id)
                        } else if job.command_name == "workspace.fileTree" {
                            let workspace_id = job
                                .operation
                                .get("workspaceId")
                                .and_then(Value::as_str)
                                .unwrap_or("");
                            let path = job.operation.get("path").and_then(Value::as_str);
                            crate::git::file_tree(workspace_id, path)
                        } else if job.command_name == "git.diff.get" {
                            let workspace_id = job
                                .operation
                                .get("workspaceId")
                                .and_then(Value::as_str)
                                .unwrap_or("");
                            let path = job.operation.get("path").and_then(Value::as_str).unwrap_or("");
                            let target = job
                                .operation
                                .get("target")
                                .and_then(Value::as_str)
                                .unwrap_or("working");
                            crate::git::diff_get(workspace_id, path, target)
                        } else if job.command_name == "git.log.list" {
                            let workspace_id = job
                                .operation
                                .get("workspaceId")
                                .and_then(Value::as_str)
                                .unwrap_or("");
                            crate::git::log_list(
                                workspace_id,
                                job.operation.get("limit").and_then(Value::as_u64),
                                job.operation.get("skip").and_then(Value::as_u64),
                            )
                        } else {
                            capability_manifest()
                        };
                        emit(sink, &mut stream, |stream| {
                            stream.receipt(
                                &job.request_id,
                                &job.command_name,
                                "completed",
                                json!({ "result": answer }),
                            )
                        });
                        // The caller asked for the result, so it gets the result.
                        // Replying with the ack here hands the renderer a receipt
                        // where it expects a read model, and it reads fields off
                        // it — a TypeError on the first render after load.
                        let _ = job.reply.send(match job.want {
                            Want::Ack => Ok(ack),
                            Want::Result => Ok(answer),
                        });
                    }
                    Dispatch::Turn => {
                        stream.pending_turn = Some((job.request_id.clone(), job.command_name.clone()));
                        // Fire and forget: waiting here would block the pump
                        // for the whole turn, and `core.abort` could never
                        // reach the session.
                        if let Err(error) = session.send_command(job.operation) {
                            emit(sink, &mut stream, |stream| {
                                stream.receipt(
                                    &job.request_id,
                                    &job.command_name,
                                    "failed",
                                    json!({ "error": { "code": "TRANSPORT_ERROR", "message": error.to_string() } }),
                                )
                            });
                            let _ = job.reply.send(Err(format!("desktop bridge: {error}")));
                            continue;
                        }
                        let _ = job.reply.send(Ok(ack));
                    }
                    Dispatch::Control if job.command_name == "session.state" => {
                        // The reply carries `{snapshot}`; keep it so a later
                        // `home.get` answers with the current state.
                        let answer = session.invoke_with(job.operation.clone(), |_| {});
                        if let Ok(ref payload) = answer {
                            if let Some(fresh) = payload.get("snapshot") {
                                stream.snapshot = fresh.clone();
                            }
                        }
                        let _ = job.reply.send(match answer {
                            Ok(payload) => {
                                emit(sink, &mut stream, |stream| {
                                    stream.receipt(&job.request_id, &job.command_name, "completed", json!({ "result": payload }))
                                });
                                Ok(ack)
                            }
                            Err(error) => Err(format!("desktop bridge: {error}")),
                        });
                    }
                    Dispatch::Control => {
                        // Events that arrive while this command runs are
                        // forwarded, not buffered and dropped: the turn keeps
                        // streaming through a model change.
                        let outcome = session.invoke_with(job.operation, |event| {
                            deliver(sink, &mut stream, event);
                        });
                        let _ = job.reply.send(match outcome {
                            Ok(receipt) => {
                                let status = receipt.get("status").and_then(Value::as_str).unwrap_or("completed");
                                let payload = receipt.get("result").cloned().unwrap_or(Value::Null);
                                let extra = match status {
                                    "completed" => json!({ "result": payload.clone() }),
                                    // `rejected` and `outcome_unknown` carry a
                                    // reason in the contract, not an error.
                                    "rejected" | "outcome_unknown" => json!({
                                        "reason": receipt.get("reason").and_then(Value::as_str)
                                            .unwrap_or("the adapter did not complete the operation"),
                                    }),
                                    _ => json!({
                                        "error": receipt.get("error").cloned().unwrap_or_else(|| json!({
                                            "code": "INTERNAL_ERROR",
                                            "message": "the adapter reported a failure",
                                        })),
                                    }),
                                };
                                emit(sink, &mut stream, |stream| {
                                    stream.receipt(&job.request_id, &job.command_name, status, extra)
                                });
                                match job.want {
                                    Want::Ack => Ok(ack),
                                    Want::Result => Ok(payload),
                                }
                            }
                            Err(error) => {
                                emit(sink, &mut stream, |stream| {
                                    stream.receipt(
                                        &job.request_id,
                                        &job.command_name,
                                        "failed",
                                        json!({ "error": { "code": "TRANSPORT_ERROR", "message": error.to_string() } }),
                                    )
                                });
                                Err(format!("desktop bridge: {error}"))
                            }
                        });
                    }
                }
            }
            // Idle: the turn's events are queueing up with nobody to collect
            // them. Without this they would sit until the next command.
            Err(RecvTimeoutError::Timeout) => {
                for event in session.drain_events() {
                    deliver(sink, &mut stream, &event);
                }
            }
            Err(RecvTimeoutError::Disconnected) => return None,
        }
    }
}

pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// Epoch milliseconds as an ISO-8601 UTC instant.
///
/// `std` has no civil-date formatter and the workspace carries no time crate,
/// so this is Howard Hinnant's `civil_from_days` inlined — the same algorithm
/// every date library uses, kept small enough to read.
pub fn iso_utc(millis: u64) -> String {
    let day = (millis / 86_400_000) as i64;
    let time = millis % 86_400_000;
    let (hour, minute, second, milli) = (
        time / 3_600_000,
        time / 60_000 % 60,
        time / 1_000 % 60,
        time % 1_000,
    );

    // Shift the origin to 1970-03-01 so the leap day lands at the end of the
    // year, then split into 400-year eras.
    let shifted = day + 719_468;
    let era = shifted / 146_097;
    let day_of_era = (shifted - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day_of_month = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    let year = if month <= 2 { year + 1 } else { year };

    format!("{year:04}-{month:02}-{day_of_month:02}T{hour:02}:{minute:02}:{second:02}.{milli:03}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_utc_formats_known_instants() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_utc(1_000_000_000_123), "2001-09-09T01:46:40.123Z");
        // A non-leap century boundary, and a leap year (2024 has a Feb 29).
        assert_eq!(iso_utc(946_684_800_000), "2000-01-01T00:00:00.000Z");
        assert_eq!(iso_utc(1_709_164_800_000), "2024-02-29T00:00:00.000Z");
        assert_eq!(iso_utc(1_767_225_600_000), "2026-01-01T00:00:00.000Z");
    }

    #[test]
    fn spread_lets_kind_win_over_input() {
        let operation = spread(&json!({ "kind": "caller-supplied", "text": "hi" }), "core.prompt");
        assert_eq!(operation["kind"], "core.prompt");
        assert_eq!(operation["text"], "hi");
    }

    #[test]
    fn spread_ignores_a_non_object_input() {
        assert_eq!(spread(&json!("nope"), "session.new"), json!({ "kind": "session.new" }));
    }

    #[test]
    fn resolve_maps_the_adapter_native_names_through_unchanged() {
        for (name, _) in ADAPTER_NATIVE {
            let (operation, dispatch) = resolve_operation(name, &json!({ "limit": 5 })).expect("native name");
            assert_eq!(operation["kind"], *name, "{name}");
            assert_eq!(dispatch, Dispatch::Control, "{name}");
        }
    }

    #[test]
    fn resolve_marks_a_prompt_as_a_turn() {
        let (operation, dispatch) = resolve_operation("core.prompt", &json!({ "text": "hi" })).expect("prompt");
        assert_eq!(operation["kind"], "core.prompt");
        assert_eq!(operation["text"], "hi");
        assert_eq!(dispatch, Dispatch::Turn);
    }

    #[test]
    fn resolve_renames_the_facade_spellings() {
        assert_eq!(resolve_operation("session.create", &json!({})).expect("create").0["kind"], "session.new");
        assert_eq!(resolve_operation("conversation.open", &json!({})).expect("open").0["kind"], "session.transcript.read");
    }

    #[test]
    fn the_capability_manifest_is_answered_locally_and_matches_the_table() {
        // It is derived from the dispatch tables, so asking the adapter for it
        // would be a round trip to learn something already computed.
        let (operation, dispatch) = resolve_operation("capabilities.get", &json!({})).expect("resolves");
        assert_eq!(dispatch, Dispatch::Local);
        assert!(operation.as_object().is_some_and(|m| m.is_empty()), "nothing to send anywhere");

        // And the manifest claims exactly what resolves: the bidirectional
        // invariant the renderer's `can()` depends on.
        let manifest = capability_manifest();
        let claimed: Vec<&str> = manifest
            .get("capabilities")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|entry| entry.get("id").and_then(Value::as_str))
                    .collect()
            })
            .unwrap_or_default();
        for id in ["core.prompt", "usage.get", "models.get", "capabilities.get"] {
            assert!(claimed.contains(&id), "{id} is resolvable but not claimed");
        }
        assert!(!claimed.contains(&"github.pr.list"), "a refused name must not be claimed");
    }

    #[test]
    fn the_git_queries_carry_their_whole_input_through_resolve() {
        // The bug this pins: the git arm hand-picked `workspaceId`/`limit`/`skip`
        // instead of spreading the input, so `operation`, `path`, `target` and
        // `paths` were all dropped. Every `git.execute` was then refused for a
        // missing operation, every `workspace.fileTree` listed the repository
        // root regardless of the directory asked for, and every `git.diff.get`
        // returned an empty patch — and none of the module's tests caught it,
        // because they call the module directly rather than through here.
        let (operation, dispatch) =
            resolve_operation("git.execute", &json!({ "workspaceId": "ws-1", "operation": { "kind": "stage", "paths": ["a.rs"] } }))
                .expect("resolves");
        assert_eq!(dispatch, Dispatch::Local);
        assert_eq!(operation.get("workspaceId").and_then(Value::as_str), Some("ws-1"));
        let nested = operation.get("operation").expect("the operation survives");
        assert_eq!(nested.get("kind").and_then(Value::as_str), Some("stage"));
        assert_eq!(
            nested.pointer("/paths/0").and_then(Value::as_str),
            Some("a.rs"),
            "the paths survive too"
        );

        let (operation, _) = resolve_operation(
            "workspace.fileTree",
            &json!({ "workspaceId": "ws-1", "path": "crates" }),
        )
        .expect("resolves");
        assert_eq!(
            operation.get("path").and_then(Value::as_str),
            Some("crates"),
            "the directory asked for must survive, or the tree always lists the root"
        );

        let (operation, _) = resolve_operation(
            "git.diff.get",
            &json!({ "workspaceId": "ws-1", "path": "a.rs", "target": "staged" }),
        )
        .expect("resolves");
        assert_eq!(operation.get("path").and_then(Value::as_str), Some("a.rs"));
        assert_eq!(operation.get("target").and_then(Value::as_str), Some("staged"));
    }

    #[test]
    fn resolve_refuses_what_the_adapter_cannot_do() {
        assert!(resolve_operation("github.pr.list", &json!({})).is_none());
        assert!(resolve_operation("stats.dashboard", &json!({})).is_none());
        assert!(resolve_operation("", &json!({})).is_none());
    }

    #[test]
    fn the_manifest_declares_exactly_what_resolve_accepts() {
        let manifest = capability_manifest();
        let declared: Vec<&str> = manifest["capabilities"]
            .as_array()
            .expect("capabilities is a list")
            .iter()
            .map(|entry| entry["id"].as_str().expect("id is text"))
            .collect();
        // Every declared capability resolves, and nothing resolves that is not
        // declared. The renamed facade spellings are declared in their own
        // right: the renderer calls `usage.get`, not `usage.panel`, so claiming
        // only the adapter's ids would leave `can()` false for the names it uses.
        for id in &declared {
            assert!(resolve_operation(id, &json!({})).is_some(), "{id} is declared but does not resolve");
        }
        // Both tables of facade spellings, not just one: `session.create` and
        // `conversation.open` resolve through RENAMED and were missing, so
        // `can()` returned false for two names this shell serves.
        for id in SHELL_LOCAL.iter().map(|(id, _)| *id) {
            assert!(declared.contains(&id), "{id} resolves but is not declared");
        }
        for (from, _) in RENAMED.iter() {
            assert!(declared.contains(from), "{from} resolves but is not declared");
        }
        // Counted, and de-duplicated. The first version of this assertion baked
        // in the length of both tables, which pinned a duplicate: `usage.get`
        // and `models.get` were claimed once from RENAMED and once from
        // SHELL_LOCAL, with conflicting grades, and the count still matched.
        let declared_count = declared.len();
        let mut sorted: Vec<&str> = declared.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            declared_count,
            "an id is declared twice: {:?}",
            sorted
        );
        assert!(declared.contains(&"core.prompt"));
        // The other direction, and the one that matters: everything this shell
        // can resolve must be declared, so `can()` is never false for a name the
        // pump will happily answer. A count assertion cannot check this — the
        // previous version asserted a total, which is why a name could be
        // routed to the adapter and still leave the suite green.
        for name in [
            "core.prompt",
            "session.create",
            "conversation.open",
            "usage.get",
            "models.get",
            "mcp.get",
            "skills.get",
            "capabilities.get",
            "history.list",
            "projects.list",
            "home.get",
            "session.agents.list",
            "workspace.open",
            "workspace.fileTree",
            "git.repository.get",
            "git.log.list",
            "git.branches.list",
            "git.diff.get",
            "git.execute",
            "environment.get",
            "diagnostics.get",
        ] {
            assert!(
                declared.contains(&name),
                "{name} resolves but is not declared, so can() is false for it"
            );
        }
        // `conversation.open` used to be excluded on the grounds that its target
        // was declared instead — which left `can()` false for a name the
        // renderer calls. The invariant is the other way round: every name the
        // renderer can call must be declared under that name.
        assert!(declared.contains(&"conversation.open"));
        assert!(declared.contains(&"session.create"));
        // The hash is over the capability list, so it is stable across boots.
        assert_eq!(manifest["hash"], capability_manifest()["hash"]);
        assert_eq!(manifest["hash"].as_str().expect("hash is text").len(), 64);
    }

    #[test]
    fn the_manifest_marks_limited_capabilities_limited() {
        let manifest = capability_manifest();
        let grades: Vec<(&str, &str)> = manifest["capabilities"]
            .as_array()
            .expect("capabilities is a list")
            .iter()
            .map(|entry| {
                (
                    entry["id"].as_str().expect("id is text"),
                    entry["grade"].as_str().expect("grade is text"),
                )
            })
            .collect();
        assert!(grades.contains(&("session.fork", "limited")));
        assert!(grades.contains(&("session.thinking.set", "limited")));
        assert!(grades.contains(&("core.prompt", "stable")));
        // Nothing is declared unavailable: an unavailable entry would make the
        // renderer hide the action, which is the same as not declaring it.
        assert!(!grades.iter().any(|(_, grade)| *grade == "unavailable"));
    }

    fn stream() -> Stream {
        Stream {
            snapshot: serde_json::json!({ "sessionId": "s1" }),
            cursor: 0,
            stream_seq: 0,
            state_version: 7,
            runtime_epoch: 3,
            authority_epoch: 3,
            pending_turn: None,
            runtime_id: "step-runtime-test".to_string(),
        }
    }

    #[test]
    fn the_first_event_announces_the_authority_it_belongs_to() {
        let mut stream = stream();
        let event = connection_announcement(&mut stream);
        assert_eq!(event.get("kind").and_then(Value::as_str), Some("runtime.changed"));
        // The renderer keys its reload on both halves: the epoch moves when the
        // daemon restarts, the runtime id when the adapter does.
        let connection = event.get("connection").expect("a connection payload");
        assert_eq!(
            connection.get("runtimeId").and_then(Value::as_str),
            Some("step-runtime-test")
        );
        assert_eq!(connection.get("runtimeEpoch").and_then(Value::as_u64), Some(3));
        assert_eq!(connection.get("status").and_then(Value::as_str), Some("connected"));
        // The cursor comes from the stream, so this cannot collide with the
        // first real event the way an event emitted from outside would.
        assert_eq!(event.get("cursor").and_then(Value::as_u64), Some(1));
    }

    #[test]
    fn a_second_announcement_moves_the_cursor_rather_than_reusing_it() {
        let mut stream = stream();
        let first = connection_announcement(&mut stream);
        let second = connection_announcement(&mut stream);
        assert_eq!(first.get("cursor").and_then(Value::as_u64), Some(1));
        assert_eq!(second.get("cursor").and_then(Value::as_u64), Some(2));
    }

    #[test]
    fn a_delta_becomes_a_conversation_changed_with_a_fresh_stream_seq() {
        let mut stream = stream();
        let delta = json!({ "kind": "conversation.message.delta", "sessionId": "s1", "turnId": "t1", "delta": "hi" });
        let events = stream.translate(&delta);
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event["kind"], "conversation.changed");
        assert_eq!(event["sessionId"], "s1");
        assert_eq!(event["streamSeq"], 1);
        assert_eq!(event["update"], delta);
        // A delta must not move the state version.
        assert_eq!(event["stateVersion"], 7);
    }

    #[test]
    fn stream_seq_advances_by_one_so_the_engine_sees_no_gap() {
        let mut stream = stream();
        for expected in 1..=3 {
            let events = stream.translate(&json!({ "kind": "conversation.message.delta", "sessionId": "s" }));
            assert_eq!(events[0]["streamSeq"], expected);
        }
    }

    #[test]
    fn turn_completion_settles_the_prompt_that_started_it() {
        let mut stream = stream();
        stream.pending_turn = Some(("req-1".to_string(), "core.prompt".to_string()));
        let events = stream.translate(&json!({ "kind": "conversation.turn.completed", "sessionId": "s1", "turnId": "t1" }));
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["kind"], "command.receipt");
        assert_eq!(events[0]["receipt"]["requestId"], "req-1");
        assert_eq!(events[0]["receipt"]["status"], "completed");
        // The turn event still reaches the conversation engine.
        assert_eq!(events[1]["kind"], "conversation.changed");
        assert!(stream.pending_turn.is_none());
    }

    #[test]
    fn an_aborted_turn_fails_the_prompt() {
        let mut stream = stream();
        stream.pending_turn = Some(("req-1".to_string(), "core.prompt".to_string()));
        let events = stream.translate(&json!({ "kind": "conversation.turn.aborted", "sessionId": "s1" }));
        assert_eq!(events[0]["receipt"]["status"], "failed");
        assert_eq!(events[0]["receipt"]["error"]["code"], "TURN_ABORTED");
    }

    #[test]
    fn a_turn_event_without_a_pending_prompt_emits_no_receipt() {
        let mut stream = stream();
        let events = stream.translate(&json!({ "kind": "conversation.turn.completed", "sessionId": "s1" }));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["kind"], "conversation.changed");
    }

    /// A sink that reports whether it took the payload, as the real one does.
    fn sink_that(takes: bool) -> EventSink {
        Box::new(move |_| takes)
    }

    const DELTA: &str = "conversation.message.delta";

    #[test]
    fn an_event_nobody_receives_does_not_advance_the_cursor() {
        // The bootstrap advertises cursor 0, so a counter that moves without
        // anyone receiving turns the first real event into a gap — and a gap
        // forces resync_required for the rest of the session.
        let mut stream = stream();
        deliver(&sink_that(false), &mut stream, &json!({ "kind": DELTA, "sessionId": "s" }));
        assert_eq!(stream.cursor, 0);

        // The next event that IS received must be cursor 1, not 3.
        deliver(&sink_that(true), &mut stream, &json!({ "kind": DELTA, "sessionId": "s" }));
        assert_eq!(stream.cursor, 1);
    }

    #[test]
    fn a_receipt_nobody_receives_does_not_advance_the_cursor_either() {
        let mut stream = stream();
        emit(&sink_that(false), &mut stream, |stream| {
            stream.receipt("req-1", "core.prompt", "completed", json!({ "result": {} }))
        });
        assert_eq!(stream.cursor, 0);

        emit(&sink_that(true), &mut stream, |stream| {
            stream.receipt("req-1", "core.prompt", "completed", json!({ "result": {} }))
        });
        assert_eq!(stream.cursor, 1);
    }

    #[test]
    fn a_turn_end_delivers_both_payloads_or_neither() {
        let mut stream = stream();
        stream.pending_turn = Some(("req-1".to_string(), "core.prompt".to_string()));
        deliver(&sink_that(false), &mut stream, &json!({ "kind": "conversation.turn.completed", "sessionId": "s" }));
        // Nothing was received, so the pairing must survive for a later attempt.
        assert!(stream.pending_turn.is_some(), "the pending turn must not be consumed by an undelivered event");
        assert_eq!(stream.cursor, 0);

        deliver(&sink_that(true), &mut stream, &json!({ "kind": "conversation.turn.completed", "sessionId": "s" }));
        assert!(stream.pending_turn.is_none());
        // Two payloads: the receipt and the conversation.changed.
        assert_eq!(stream.cursor, 2);
    }

    #[test]
    fn a_non_conversation_event_only_advances_the_transport() {
        let mut stream = stream();
        let events = stream.translate(&json!({ "kind": "runtime.shutdownComplete" }));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["kind"], "state.changed");
        assert_eq!(events[0]["cursor"], 1);
    }
}

#[cfg(test)]
mod routing_tests {
    use super::*;

    #[test]
    fn a_shell_local_name_is_never_sent_to_the_adapter() {
        // The bug this pins: the shell-answered names sat in `ADAPTER_NATIVE`,
        // which routes to `Dispatch::Control` — so they were sent to the
        // runtime, which rejects them. Worse, it rejects them with
        // `result: null` rather than an error, so the renderer's `.catch` never
        // fired and a panel silently emptied instead of failing loudly.
        for name in [
            "history.list",
            "projects.list",
            "home.get",
            "session.agents.list",
            "workspace.open",
            "environment.get",
            "diagnostics.get",
            "git.repository.get",
            "git.execute",
            "workspace.fileTree",
            "git.log.list",
            "git.branches.list",
            "git.diff.get",
            "capabilities.get",
        ] {
            let (_, dispatch) = resolve_operation(name, &json!({})).expect("resolves");
            assert_eq!(
                dispatch,
                Dispatch::Local,
                "{name} must be answered here, not sent to the runtime"
            );
        }
    }

    #[test]
    fn a_capabilities_page_query_is_a_rename_not_a_local_answer() {
        // Both underlying lists are adapter operations the runtime implements,
        // so these travel and are reshaped on the way back.
        for (name, target) in [
            ("mcp.get", "session.extensions.list"),
            ("skills.get", "session.commands.list"),
        ] {
            let (operation, dispatch) = resolve_operation(name, &json!({})).expect("resolves");
            assert_eq!(dispatch, Dispatch::Control);
            assert_eq!(
                operation.get("kind").and_then(Value::as_str),
                Some(target),
                "{name} must resolve to the operation the runtime serves"
            );
        }
    }
}
