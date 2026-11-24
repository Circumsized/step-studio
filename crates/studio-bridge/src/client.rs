//! A connected Studio Bridge session: handshake done, commands and events
//! flowing. The shell drives this from a background thread.

use crate::transport::{self, Stream};
use std::collections::VecDeque;
use std::io::{BufReader, Write};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::frame::{read_frame, write_frame, FrameError};
use crate::handshake::{verify_hello, PROTOCOL_VERSION};

/// Receipt statuses that settle a command.
pub const TERMINAL_STATUSES: [&str; 4] = ["completed", "failed", "rejected", "outcome_unknown"];

#[derive(Debug)]
pub enum BridgeError {
    Io(String),
    Protocol(String),
    Rejected { code: String, message: String },
    TimedOut(String),
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(d) => write!(f, "io: {d}"),
            Self::Protocol(d) => write!(f, "protocol: {d}"),
            Self::Rejected { code, message } => write!(f, "{code}: {message}"),
            Self::TimedOut(what) => write!(f, "timed out waiting for {what}"),
        }
    }
}

impl std::error::Error for BridgeError {}

impl From<FrameError> for BridgeError {
    fn from(error: FrameError) -> Self {
        Self::Protocol(error.to_string())
    }
}

/// Frames the reader thread has taken off the socket, waiting for whoever
/// asked for them.
#[derive(Default)]
struct Inbox {
    /// `studio.event` payloads, in arrival order. Events carry no requestId —
    /// the protocol broadcasts them on the connection — so this is one stream
    /// shared by every in-flight command. See [`BridgeSession::invoke_with`].
    events: VecDeque<Value>,
    /// `studio.snapshot` and terminal `studio.receipt` bodies, by requestId.
    replies: Vec<(String, Value)>,
    /// Why the connection ended, once it does.
    closed: Option<String>,
}

/// The reader thread's output plus the condvar waiters park on.
#[derive(Default)]
struct Shared {
    inbox: Mutex<Inbox>,
    ready: Condvar,
}

impl Shared {
    /// A poisoned mutex still holds valid state, and a stalled reader must not
    /// take the whole session down with it.
    fn lock_inbox(&self) -> std::sync::MutexGuard<'_, Inbox> {
        self.inbox
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn close(&self, reason: String) {
        self.lock_inbox().closed = Some(reason);
        self.ready.notify_all();
    }
}

/// One live session. Owns the socket and the adapter child process.
///
/// Every method takes `&self`, so a session behind an `Arc` can serve a
/// streaming command and a control command at once — which is what makes
/// aborting a turn possible.
pub struct BridgeSession {
    child: Option<Child>,
    /// Only the write half is used here; the read half belongs to the reader
    /// thread. A mutex, because two commands may write concurrently.
    stream: Mutex<Stream>,
    shared: Arc<Shared>,
    epoch: u64,
    instance_id: String,
}

/// Where the adapter lives and how to launch it.
pub struct AdapterLaunch {
    pub node: String,
    pub adapter_script: String,
    pub socket_path: String,
    pub token_file: String,
    pub cwd: String,
    pub runtime_epoch: u64,
    /// Extra environment for the adapter, e.g. `STEP_STUDIO_RUNTIME_CMD`.
    pub env: Vec<(String, String)>,
    /// The bridge token, when the peer already holds it. Set this and
    /// `token_file` may be empty: the session writes the file only when it is
    /// the one launching the runtime.
    pub token: Option<String>,
    /// Full command to run instead of `node <adapter_script>`. Used when the
    /// runtime is a native binary rather than a Node script.
    pub command: Vec<String>,
}

impl BridgeSession {
    /// Launch the adapter, wait for its socket, and complete the handshake.
    pub fn connect(launch: &AdapterLaunch) -> Result<Self, BridgeError> {
        // Nothing to spawn means the peer is already running and this session is
        // only a client — used when the runtime under test is not one we spawn.
        //
        // The test is "is there a command at all", not "is `node` non-empty".
        // Gating on `node` made the `command` field unreachable from here: a
        // launch carrying only a native binary in `command` left `node` empty, so
        // the session decided the peer was already up, never spawned anything, and
        // then waited twenty seconds for a socket nobody was going to create.
        let spawnable = !launch.node.is_empty() || !launch.command.is_empty();

        // Minting belongs to whoever launches the runtime: that session chooses
        // the secret and the spawned process reads it back from the file. A
        // client-only session has nothing to choose — the peer published its
        // token already, and minting here would write a file nobody reads and
        // then fail the handshake as a proof mismatch, which says nothing about
        // the real cause (the daemon never published a token).
        let token = match launch.token.clone() {
            Some(token) => token,
            None if spawnable => {
                let token = crate::token::generate_token();
                crate::token::write_token_file(std::path::Path::new(&launch.token_file), &token)
                    .map_err(|e| BridgeError::Io(format!("token file: {e}")))?;
                token
            }
            None => {
                return Err(BridgeError::Protocol(
                    "no bridge token: the peer published none".to_string(),
                ));
            }
        };

        let child = if !spawnable {
            None
        } else {
            let _ = std::fs::remove_file(&launch.socket_path);
            Some(Self::spawn(launch)?)
        };
        #[allow(unused_mut)]
        let mut stream = wait_for_socket(&launch.socket_path, Duration::from_secs(20))?;
        transport::set_read_timeout(&stream, Some(Duration::from_secs(300)))
            .map_err(|e| BridgeError::Io(e.to_string()))?;

        // Handshake. The challenge is ours; the runtime must prove it holds the
        // token before anything else is trusted. This frame is read directly,
        // before the reader thread exists, so exactly one owner touches the
        // socket at a time.
        let reader = BufReader::new(stream_clone(&stream)?);
        let challenge = crate::handshake::fresh_challenge("shell", launch.runtime_epoch);
        let hello = json!({
            "type": "studio.hello",
            "requestId": "shell-hello",
            "supportedProtocolVersions": [PROTOCOL_VERSION],
            "requiredProfile": "limited",
            "challenge": challenge,
        });
        // The shape differs per platform: a Unix socket is duplicable, so this
        // is a second handle; on Windows the client side is a `File` and the one
        // already held is lent, because this is the only frame written before
        // the reader thread takes over.
        #[cfg(unix)]
        write_frame(&mut transport::handshake_writer(&stream), 1, &hello)?;
        #[cfg(windows)]
        write_frame(&mut transport::handshake_writer(&mut stream), 1, &hello)?;
        // `set_read_timeout` above bounds this on Unix and is accepted-and-
        // ignored on Windows, so on Windows a peer that connects and says
        // nothing — a squatted endpoint, or a daemon that wedged before its
        // hello — blocked here forever, with no error and no way back. The same
        // shape and the same primitive the daemon uses for its own first frame.
        let frame = transport::first_frame_within(reader, HELLO_BUDGET)
            .ok_or_else(|| BridgeError::TimedOut(format!("no hello within {HELLO_BUDGET:?}")))?
            .map_err(|error| BridgeError::Io(error.to_string()))?
            .ok_or_else(|| BridgeError::Protocol("adapter closed before hello".to_string()))?;
        let verified =
            verify_hello(&frame.body, &token, &challenge).map_err(BridgeError::Protocol)?;

        // Answering the peer's challenge only proved the peer could verify. This
        // is the half that proves *we* hold the token: the daemon puts its own
        // challenge in the hello result it just sent, and we sign it. Without
        // this the daemon served any peer that could open the endpoint.
        let peer_challenge = frame
            .body
            .get("challenge")
            .and_then(|c| c.as_str())
            .ok_or_else(|| {
                BridgeError::Protocol("the peer issued no challenge to answer".to_string())
            })?
            .to_string();
        let proof =
            crate::handshake::hello_proof(&token, &peer_challenge, &verified.runtime_instance_id);
        // Same ownership rule as the hello above: exactly one owner touches the
        // socket until the reader thread exists.
        #[cfg(unix)]
        write_frame(&mut transport::handshake_writer(&stream), 1, &proof)?;
        #[cfg(windows)]
        write_frame(&mut transport::handshake_writer(&mut stream), 1, &proof)?;

        // From here on a reader thread owns the socket: it demultiplexes
        // frames into the shared inbox and wakes whoever is waiting.
        let shared = Arc::new(Shared::default());
        spawn_reader(stream_clone(&stream)?, Arc::clone(&shared));

        Ok(Self {
            child,
            stream: Mutex::new(stream),
            shared,
            epoch: verified.runtime_epoch,
            instance_id: verified.runtime_instance_id,
        })
    }

    /// Launch the adapter process. Separated so a client-only session can skip
    /// it entirely.
    fn spawn(launch: &AdapterLaunch) -> Result<Child, BridgeError> {
        spawn_adapter(launch)
    }

    /// Ask the runtime to shut down, then make sure it is gone.
    ///
    /// Explicit rather than in `Drop`, because who owns the child decides
    /// whether a disconnect is allowed to end the session. The shell's session
    /// is client-only — it launched nothing — so dropping it must only close
    /// the socket; sending `runtime.shutdown` there is what made a shell
    /// restart kill the conversation. The daemon, which does own the child,
    /// calls this itself.
    ///
    /// Idempotent: the child is taken, so a second call is a no-op.
    pub fn shutdown(&mut self) {
        let Some(child) = self.child.take() else {
            // Not ours to stop. Close the socket and nothing else.
            return;
        };
        Self::drain_child(&self.stream, child);
    }

    /// Send the shutdown request and wait out the child.
    fn drain_child(stream: &Mutex<Stream>, mut child: Child) {
        let mut stream = stream
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let request = json!({
            "type": "studio.request",
            "requestId": "shell-shutdown",
            "runtimeEpoch": 0,
            "operation": { "kind": "runtime.shutdown", "drain": true },
        });
        let _ = write_frame(&mut *stream, 0, &request);
        let _ = stream.flush();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(_) => break,
            }
        }
        let _ = child.kill();
        let _ = child.wait();
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// The snapshot is a query: it answers with a `studio.snapshot` frame.
    pub fn snapshot(&self) -> Result<Value, BridgeError> {
        let request_id = self.next_request_id();
        self.send(&json!({
            "type": "studio.request",
            "requestId": request_id,
            "runtimeEpoch": self.epoch,
            "operation": { "kind": "runtime.snapshot" },
        }))?;
        self.await_reply(&request_id, |_| {})
    }

    /// Why the connection ended, once it has.
    ///
    /// A pump that keeps issuing commands against a dead session produces a
    /// stream of transport errors that all look like operation failures. Asking
    /// the session directly is what tells "this command failed" from "there is
    /// nobody left to ask".
    pub fn close_reason(&self) -> Option<String> {
        self.shared.lock_inbox().closed.clone()
    }

    /// Take every event that arrived while no command was waiting for them.
    ///
    /// Events are broadcast, not addressed: they queue in the inbox and are
    /// drained by whichever command is awaiting a reply. Between commands
    /// nothing drains, so a long-lived consumer that also issues commands has
    /// to empty the queue itself — otherwise a turn's events sit in the inbox
    /// until the next command happens to collect them.
    pub fn drain_events(&self) -> Vec<Value> {
        let mut inbox = self.shared.lock_inbox();
        inbox.events.drain(..).collect()
    }

    /// Run a command and collect events until its terminal receipt.
    ///
    /// Every event is buffered, so the caller sees the turn only once it is
    /// over. Use [`BridgeSession::invoke_with`] when the consumer can render as
    /// events arrive.
    pub fn invoke(&self, operation: Value) -> Result<(Value, Vec<Value>), BridgeError> {
        let mut events = Vec::new();
        let receipt = self.invoke_with(operation, |event| events.push(event.clone()))?;
        Ok((receipt, events))
    }

    /// Send a command without waiting for its receipt. A streaming turn is
    /// already consuming the event stream, so an answer sent mid-turn must not
    /// compete for it: this writes the frame and returns.
    pub fn send_command(&self, operation: Value) -> Result<(), BridgeError> {
        let request_id = self.next_request_id();
        self.send(&json!({
            "type": "studio.request",
            "requestId": request_id,
            "runtimeEpoch": self.epoch,
            "operation": operation,
        }))
    }

    /// Run a command and collect the events it saw on the way.
    ///
    /// Same as [`BridgeSession::invoke_with`], but for a caller that wants the
    /// events rather than only the receipt. A turn's deltas that land while a
    /// query waits are real output, and dropping them loses the turn.
    pub fn invoke_collecting(
        &self,
        operation: Value,
        mut on_event: impl FnMut(&Value),
    ) -> Result<(Value, Vec<Value>), BridgeError> {
        let mut events = Vec::new();
        let receipt = self.invoke_with(operation, |event| {
            events.push(event.clone());
            on_event(event);
        })?;
        Ok((receipt, events))
    }

    /// Run a command, handing each event to `on_event` the moment it arrives,
    /// and return the terminal receipt.
    ///
    /// Streaming is not cosmetic: a turn takes seconds, and buffering means the
    /// UI shows nothing for all of them and then the whole answer at once.
    ///
    /// Events are broadcast, not addressed, so **at most one event-consuming
    /// command may be in flight** on a session — otherwise two waiters split
    /// one stream between them. Control commands (`core.abort`,
    /// `session.model.set`) emit no events and are exempt.
    pub fn invoke_with(
        &self,
        operation: Value,
        on_event: impl FnMut(&Value),
    ) -> Result<Value, BridgeError> {
        let request_id = self.next_request_id();
        self.send(&json!({
            "type": "studio.request",
            "requestId": request_id,
            "runtimeEpoch": self.epoch,
            "operation": operation,
        }))?;
        self.await_reply(&request_id, on_event)
    }

    /// Write one frame. Locked, because a control command and a turn can be in
    /// flight together.
    fn send(&self, body: &Value) -> Result<(), BridgeError> {
        let mut stream = self
            .stream
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        write_frame(&mut *stream, self.epoch, body).map_err(BridgeError::from)
    }

    /// Block until the reply to `request_id` lands, handing over every event
    /// that arrives on the way. Events are drained before the reply is taken,
    /// so nothing that preceded it is dropped.
    fn await_reply(
        &self,
        request_id: &str,
        mut on_event: impl FnMut(&Value),
    ) -> Result<Value, BridgeError> {
        let mut inbox = self.shared.lock_inbox();
        loop {
            // Deliver outside the lock: a slow consumer must not stall the
            // reader thread for every other waiter.
            let batch: Vec<Value> = inbox.events.drain(..).collect();
            if !batch.is_empty() {
                drop(inbox);
                for event in &batch {
                    on_event(event);
                }
                inbox = self.shared.lock_inbox();
            }
            if let Some(index) = inbox.replies.iter().position(|(id, _)| id == request_id) {
                return Ok(inbox.replies.remove(index).1);
            }
            // The reply is checked first: a connection can close with the
            // answer still sitting in the inbox.
            if let Some(reason) = inbox.closed.clone() {
                return Err(BridgeError::Protocol(reason));
            }
            inbox = self
                .shared
                .ready
                .wait(inbox)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    fn next_request_id(&self) -> String {
        // Unique across every session in this process *and* across processes,
        // because the daemon multiplexes surfaces over its one adapter
        // connection and correlates a reply by this id. A per-session counter
        // made two surfaces mint the same id, and the daemon — which now routes
        // by id rather than broadcasting — handed one surface's reply to the
        // other, or to neither.
        //
        // The process id is what separates two processes (a shell and a TUI
        // attached to one daemon); the counter separates the sessions inside
        // one process. A pid is reused only after the process that held it is
        // gone, and a gone process's surface is gone with it, so its entries
        // have already been dropped from the daemon's map.
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
        format!("shell-{}-{seq}", std::process::id())
    }
}

/// How long the handshake's first read may take.
///
/// Matches the daemon's own first-frame budget, and sits under the 20 s
/// `wait_for_socket` already spent getting the socket to exist.
const HELLO_BUDGET: Duration = Duration::from_secs(30);

impl Drop for BridgeSession {
    fn drop(&mut self) {
        // Only ends the runtime when this session launched it. See
        // [`BridgeSession::shutdown`].
        self.shutdown();
        // Half-close, so the peer sees the disconnect now rather than whenever
        // this process happens to exit. The reader thread holds a clone of the
        // socket, so without this the write side stays open on our end and the
        // peer never gets an EOF — which means a daemon watching for surfaces
        // to leave would wait forever.
        announce_departure(&self.stream);
    }
}

/// Tell the peer this side is done writing.
///
/// Unix only for now: a named pipe handle has no equivalent, and the Windows
/// path is unverified regardless.
#[cfg(unix)]
fn announce_departure(stream: &Mutex<Stream>) {
    let stream = stream
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _ = stream.shutdown(std::net::Shutdown::Write);
}

#[cfg(not(unix))]
fn announce_departure(_stream: &Mutex<Stream>) {}

/// Own the read half of the socket and file every frame into the shared inbox.
fn spawn_reader(stream: Stream, shared: Arc<Shared>) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        loop {
            match read_frame(&mut reader) {
                Ok(Some(frame)) => {
                    let body = frame.body;
                    let kind = body.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    let request_id = body
                        .get("requestId")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let mut inbox = shared.lock_inbox();
                    match kind {
                        "studio.event" => inbox
                            .events
                            .push_back(body.get("event").cloned().unwrap_or(Value::Null)),
                        // `accepted` is not terminal, so it is dropped here
                        // rather than settling a waiter early.
                        "studio.snapshot" if !request_id.is_empty() => {
                            inbox.replies.push((request_id, body));
                        }
                        "studio.receipt"
                            if !request_id.is_empty()
                                && is_terminal(body.get("status").and_then(|s| s.as_str())) =>
                        {
                            inbox.replies.push((request_id, body));
                        }
                        _ => {}
                    }
                    drop(inbox);
                    shared.ready.notify_all();
                }
                Ok(None) => {
                    shared.close("adapter closed the connection".to_string());
                    return;
                }
                Err(error) => {
                    shared.close(error.to_string());
                    return;
                }
            }
        }
    });
}

/// Poll for the socket file; the adapter creates it after binding.
///
/// Public because the daemon spawns the adapter through [`spawn_adapter`] and
/// has to wait for the socket the same way a session does.
pub fn wait_for_socket(path: &str, timeout: Duration) -> Result<Stream, BridgeError> {
    let deadline = Instant::now() + timeout;
    loop {
        // On Windows the pipe is named, not pathed, so it has no file to wait
        // for: the connect itself is the wait, and a missing pipe fails fast
        // rather than hanging until the deadline.
        let target = transport::endpoint_for_bind(path);
        match connect(&target) {
            Ok(stream) => return Ok(stream),
            Err(error) if Instant::now() >= deadline => {
                return Err(BridgeError::TimedOut(format!("adapter socket: {error}")));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(40)),
        }
    }
}

/// Connect to the bridge endpoint.
///
/// Delegates to [`transport::connect`] rather than naming either platform's
/// type: on Unix that is `UnixStream::connect`, on Windows it is
/// `miow::pipe::connect`, which blocks until the server is reachable and hands
/// back the read+write `File` the client talks over.
fn connect(endpoint: &str) -> Result<Stream, BridgeError> {
    transport::connect(endpoint).map_err(|e| BridgeError::Io(e.to_string()))
}

/// A second handle on the same socket, so the reader thread can own one while
/// the session writes through the other.
///
/// The same call on both platforms: a Unix socket with `try_clone`, a Windows
/// pipe because the client side is a `File` and `File::try_clone` is
/// `DuplicateHandle`. This used to open a *second connection* to the same pipe
/// name on Windows, on the premise that a pipe handle could not be duplicated —
/// which was wrong, and cost an environment variable to plumb the endpoint
/// through.
fn stream_clone(stream: &Stream) -> Result<Stream, BridgeError> {
    transport::duplicate(stream).map_err(|e| BridgeError::Io(e.to_string()))
}

/// Build and launch the adapter process.
///
/// Public because the daemon owns the adapter in M2 while the shell is a
/// client: both need the same argv, and a second copy of it would drift. The
/// daemon spawns through here and keeps the `Child` itself.
pub fn spawn_adapter(launch: &AdapterLaunch) -> Result<Child, BridgeError> {
    let mut command = if launch.command.is_empty() {
        let mut command = Command::new(&launch.node);
        command.arg(&launch.adapter_script);
        command
    } else {
        let mut parts = launch.command.iter();
        let mut command = Command::new(parts.next().expect("command is non-empty"));
        for arg in parts {
            command.arg(arg);
        }
        command
    };
    command
        .arg("--mode")
        .arg("studio-host")
        .arg("--bridge-endpoint")
        .arg(&launch.socket_path)
        .arg("--bridge-token-file")
        .arg(&launch.token_file)
        .arg("--bridge-runtime-epoch")
        .arg(launch.runtime_epoch.to_string())
        .arg("--cwd")
        .arg(&launch.cwd);
    for (key, value) in &launch.env {
        command.env(key, value);
    }
    command
        .stdout(std::process::Stdio::null())
        .spawn()
        .map_err(|e| BridgeError::Io(format!("spawn adapter: {e}")))
}

fn is_terminal(status: Option<&str>) -> bool {
    matches!(status, Some(s) if TERMINAL_STATUSES.contains(&s))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A peer that connects and says nothing, and stays connected.
    ///
    /// Dropping the client handle would be EOF, which `read_frame` already
    /// reports as `Ok(None)`; the defect is a peer that is open and silent. On
    /// Windows `set_read_timeout` is ignored, so before the bound this read
    /// never returned at all — no error, no way back. The daemon-side test for
    /// the same shape hangs under sabotage; this one fails instead.
    #[test]
    fn a_silent_peer_is_given_up_on_instead_of_blocking_forever() {
        let dir = std::env::temp_dir().join(format!("ss-hello-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let endpoint = dir.join("bridge.sock");

        let for_client = transport::endpoint_for_bind(&endpoint.to_string_lossy());
        // Bind the pipe endpoint, not the socket path: on Windows `bind` hands
        // its argument straight to `CreateNamedPipeW`, so a filesystem path is
        // not a pipe name. `serve` converts before binding for the same reason.
        let listener = transport::bind(&for_client).expect("bind");
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let silent = std::thread::spawn(move || {
            let client = transport::connect(&for_client)?;
            let _ = held_tx.send(client);
            std::io::Result::<()>::Ok(())
        });
        let _held = held_rx.recv().expect("the peer connected");

        let accepted = transport::accept(listener).expect("accept");
        let reader = BufReader::new(accepted.into_connection());
        let started = std::time::Instant::now();
        let outcome = transport::first_frame_within(reader, Duration::from_millis(300));

        assert!(outcome.is_none(), "a silent peer was not given up on");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the bound took {:?}",
            started.elapsed()
        );
        let _ = silent.join();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The bound must not cost the talking peer its frame: the reader is
    /// consumed by the wait, and the frame has to come back regardless.
    #[test]
    fn a_peer_that_speaks_still_gets_its_frame_through_the_bound() {
        let dir = std::env::temp_dir().join(format!("ss-hello-talk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let endpoint = dir.join("bridge.sock");

        let for_client = transport::endpoint_for_bind(&endpoint.to_string_lossy());
        // Bind the pipe endpoint, not the socket path: on Windows `bind` hands
        // its argument straight to `CreateNamedPipeW`, so a filesystem path is
        // not a pipe name. `serve` converts before binding for the same reason.
        let listener = transport::bind(&for_client).expect("bind");
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let talker = std::thread::spawn(move || {
            let mut client = transport::connect(&for_client)?;
            let frame = crate::frame::FrameEncoder::encode(1, &serde_json::json!({"type": "studio.hello"}))
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            client.write_all(&frame)?;
            let _ = held_tx.send(client);
            std::io::Result::<()>::Ok(())
        });
        let _held = held_rx.recv().expect("the peer connected");

        let accepted = transport::accept(listener).expect("accept");
        let reader = BufReader::new(accepted.into_connection());
        let first = transport::first_frame_within(reader, Duration::from_secs(5))
            .expect("the wait gave up on a talking peer")
            .expect("the frame failed to decode")
            .expect("the peer closed instead of sending");
        assert_eq!(
            first.body.get("type").and_then(Value::as_str),
            Some("studio.hello"),
            "the frame survived the bound"
        );
        let _ = talker.join();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
