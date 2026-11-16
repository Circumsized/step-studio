//! The adapter the daemon owns.
//!
//! The shell used to do three jobs at once through `BridgeSession`: launch the
//! adapter, talk to it as a client, and kill it on the way out. The daemon
//! takes the first and the last; the shell keeps only the second, as a client
//! of the daemon.
//!
//! Splitting the connection into a writer the supervisor holds and a reader
//! that moves into the pump thread is what makes the relay possible at all:
//! one socket, two owners, one direction each. A Unix socket duplicates, which
//! is why this is two handles rather than two connections.

use std::fs;
#[cfg(unix)]
use std::io::BufReader;
use std::io::Write;
use std::path::Path;
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
#[cfg(unix)]
use step_studio_bridge::client::wait_for_socket;
use step_studio_bridge::client::{spawn_adapter, AdapterLaunch};
#[cfg(unix)]
use step_studio_bridge::frame::read_frame;
use step_studio_bridge::frame::write_frame;
#[cfg(unix)]
use step_studio_bridge::handshake::verify_hello;
use step_studio_bridge::handshake::challenge_proof;
// `PROTOCOL_VERSION` is only read inside `connect_and_handshake`, which is
// `#[cfg(unix)]`: on Windows the adapter has no duplex transport to handshake
// over, so importing it unconditionally makes the Windows clippy job — the one
// whose whole purpose is keeping the pipe path compiling — fail on an unused
// import.
#[cfg(unix)]
use step_studio_bridge::handshake::PROTOCOL_VERSION;
#[cfg(unix)]
use step_studio_bridge::transport;
use step_studio_bridge::transport::Stream;

/// How long to wait for the adapter's socket to appear.
#[cfg(unix)]
const SOCKET_TIMEOUT: Duration = Duration::from_secs(20);

/// What went wrong bringing the adapter up.
#[derive(Debug)]
pub enum AdapterError {
    Spawn(String),
    Socket(String),
    Handshake(String),
}

impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(d) => write!(f, "spawn adapter: {d}"),
            Self::Socket(d) => write!(f, "adapter socket: {d}"),
            Self::Handshake(d) => write!(f, "handshake: {d}"),
        }
    }
}

impl std::error::Error for AdapterError {}

/// The adapter's write half, its child, and what the handshake established.
pub struct AdapterHandle {
    pub child: Child,
    /// A mutex, because a turn and a control command can both be in flight on
    /// one connection — that is what makes aborting a turn possible. Behind an
    /// `Arc` so a surface's relay can borrow it without holding the adapter
    /// slot, which would block the supervisor and the drain path.
    pub writer: Arc<Mutex<Stream>>,
    pub epoch: u64,
    pub instance_id: String,
    /// The adapter's hello, verbatim. A surface's hello is answered from this
    /// rather than relayed, because the adapter handshakes exactly once per
    /// connection and would ignore a second one.
    pub identity: Value,
    /// The bridge token, kept so a surface's proof can be signed.
    pub token: String,
    /// When this adapter started, which is what the supervisor's grace period
    /// measures uptime from.
    pub started: Instant,
    /// Cleared when this adapter's read half ends.
    ///
    /// Part of the handle rather than a separate slot, so a surface reads the
    /// adapter and its liveness in one atomic step. Two fields would let a
    /// surface see a new adapter paired with the previous one's death, and
    /// release a connection that is fine.
    pub alive: Arc<AtomicBool>,
}

/// Spawn the adapter, wait for its socket, and complete the handshake.
///
/// Returns the handle plus the read half, which the caller moves into the pump
/// thread. The two halves are one socket: closing one does not close the other,
/// so the connection ends only when both are gone.
pub fn launch(
    launch: &AdapterLaunch,
    challenge: &str,
) -> Result<(AdapterHandle, Stream), AdapterError> {
    // The daemon launches the adapter, so the daemon owns the token file — and
    // therefore the directory it lives in, which the shell used to create.
    if let Some(parent) = Path::new(&launch.token_file).parent() {
        fs::create_dir_all(parent).map_err(|e| AdapterError::Spawn(format!("token dir: {e}")))?;
    }
    // The daemon launches the adapter, so the daemon owns the token file.
    let token = match launch.token.clone() {
        Some(token) => token,
        None => {
            let token = step_studio_bridge::token::generate_token();
            fs::write(&launch.token_file, &token)
                .map_err(|e| AdapterError::Spawn(format!("token file: {e}")))?;
            token
        }
    };

    let mut child =
        spawn_adapter(launch).map_err(|error| AdapterError::Spawn(error.to_string()))?;
    let started = Instant::now();

    // Everything after the spawn can fail, and a `Child` that is simply dropped
    // is not killed: Rust closes the handle and leaves the process running. A
    // failed handshake would therefore leak one adapter per attempt, each
    // holding the session's socket — which is the orphan the whole daemon
    // exists to prevent.
    match connect_and_handshake(launch, &token, challenge) {
        Ok(parts) => Ok((
            AdapterHandle {
                child,
                started,
                writer: parts.writer,
                epoch: parts.epoch,
                instance_id: parts.instance_id,
                identity: parts.identity,
                token,
                alive: Arc::new(AtomicBool::new(true)),
            },
            parts.reader,
        )),
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(error)
        }
    }
}

/// What a surface's relay needs from the adapter, without holding the slot.
///
/// A snapshot rather than a borrow: the slot has to stay free while a surface
/// is attached, because the supervisor's wait loop and the drain path both need
/// it.
#[derive(Clone)]
pub struct AdapterView {
    pub writer: Arc<Mutex<Stream>>,
    pub epoch: u64,
    pub instance_id: String,
    pub identity: Value,
    pub token: String,
    pub alive: Arc<AtomicBool>,
}

/// What the handshake established, before the child is attached to it.
struct Identity {
    writer: Arc<Mutex<Stream>>,
    epoch: u64,
    instance_id: String,
    /// The adapter's hello, verbatim.
    identity: Value,
    reader: Stream,
}

/// Wait for the adapter's socket and complete the handshake.
///
/// Split from [`launch`] so the caller can kill the child on the way out: the
/// socket is the adapter's, and an adapter that never finishes its handshake
/// has nothing to offer.
/// Windows: not implemented, and said so rather than faked.
///
/// The blocker used to be the crate: `named_pipe::PipeClient` neither duplicated
/// nor offered synchronous duplex I/O, so one connection could not have a writer
/// and a reader. That is no longer true — the transport is on `miow` now, whose
/// `NamedPipe` implements `Read` and `Write` on `&self` and is shared behind an
/// `Arc`, which is exactly this shape (see `transport::connection_duplicate`).
///
/// What is left is wiring, and it is narrower than it looks: the roles are
/// already fixed — the adapter binds and accepts (`bridge_server.rs`), this side
/// connects as a client (`miow::pipe::connect`, a `File`). Two concrete
/// obstacles remain, neither of them a design question: splitting the single
/// `File` the client gets into a writer the supervisor holds and a reader that
/// moves into the pump, and the fact that `transport::accept` blocks with no
/// timeout, so a Windows daemon parked there never observes `stopping`. Both
/// need a Windows machine to validate, which is why this is M5.1 and not a stub. What it must not be is a path that compiles and then
/// deadlocks: a pump blocked in `read` while holding the only handle would hang
/// the daemon with nothing in the log. Refusing here is recoverable and visible;
/// a hang is neither.
#[cfg(not(unix))]
fn connect_and_handshake(
    _launch: &AdapterLaunch,
    _token: &str,
    _challenge: &str,
) -> Result<Identity, AdapterError> {
    Err(AdapterError::Socket(
        "the adapter transport needs a duplex handle, and a Windows named pipe \
         does not provide one; this is M5.1 work and needs a Windows machine to \
         validate"
            .to_string(),
    ))
}

#[cfg(unix)]
fn connect_and_handshake(
    launch: &AdapterLaunch,
    token: &str,
    challenge: &str,
) -> Result<Identity, AdapterError> {
    let stream = wait_for_socket(&launch.socket_path, SOCKET_TIMEOUT)
        .map_err(|error| AdapterError::Socket(error.to_string()))?;
    transport::set_read_timeout(&stream, Some(Duration::from_secs(300)))
        .map_err(|e| AdapterError::Socket(e.to_string()))?;

    // The challenge is the daemon's; the adapter must prove it holds the token
    // before anything else is trusted. Read directly, before the pump thread
    // exists, so exactly one owner touches the socket at a time — the same
    // ordering `BridgeSession::connect` uses.
    let hello = json!({
        "type": "studio.hello",
        "requestId": "daemon-hello",
        "supportedProtocolVersions": [PROTOCOL_VERSION],
        "requiredProfile": "limited",
        "challenge": challenge,
    });
    write_frame(&mut transport::handshake_writer(&stream), 1, &hello)
        .map_err(|e| AdapterError::Handshake(e.to_string()))?;
    let mut hello_reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|e| AdapterError::Socket(e.to_string()))?,
    );
    let frame = read_frame(&mut hello_reader)
        .map_err(|e| AdapterError::Handshake(e.to_string()))?
        .ok_or_else(|| AdapterError::Handshake("adapter closed before hello".to_string()))?;
    let verified = verify_hello(&frame.body, token, challenge).map_err(AdapterError::Handshake)?;

    let writer = Arc::new(Mutex::new(
        stream
            .try_clone()
            .map_err(|e| AdapterError::Socket(e.to_string()))?,
    ));
    Ok(Identity {
        writer,
        epoch: verified.runtime_epoch,
        instance_id: verified.runtime_instance_id,
        identity: frame.body,
        reader: stream,
    })
}

/// Answer a surface's hello from what the adapter reported.
///
/// The proof is re-signed for the surface's own challenge: a proof computed for
/// the daemon's challenge would not verify against the surface's, and the
/// surface would refuse the connection.
///
/// Every other field is the adapter's own, passed through verbatim. The first
/// version re-listed them by hand and dropped `upstreamCommit` doing it — which
/// the TypeScript client requires, so every surface handshake failed with
/// `malformed response` and the cross-language test that would have caught it
/// documented that same failure as its proof of being non-vacuous. Copying a
/// known set of keys is a list that goes stale the moment the adapter grows one;
/// overriding four keys is a rule that does not.
pub fn hello_result(handle: &AdapterView, challenge: &str, request_id: &Value) -> Value {
    let identity = &handle.identity;
    let proof = challenge_proof(&handle.token, challenge, &handle.instance_id);
    let mut body = identity.clone();
    if let Some(map) = body.as_object_mut() {
        map.insert("requestId".to_string(), request_id.clone());
        map.insert("runtimeInstanceId".to_string(), json!(handle.instance_id));
        map.insert("runtimeEpoch".to_string(), json!(handle.epoch));
        map.insert("challengeProof".to_string(), json!(proof));
    }
    body
}

impl AdapterHandle {
    /// A snapshot of what a relay needs. See [`AdapterView`].
    pub fn view(&self) -> AdapterView {
        AdapterView {
            writer: Arc::clone(&self.writer),
            epoch: self.epoch,
            instance_id: self.instance_id.clone(),
            identity: self.identity.clone(),
            token: self.token.clone(),
            alive: Arc::clone(&self.alive),
        }
    }
}

/// Whether an adapter is still the one a surface is talking to.
pub fn is_alive(flag: &AtomicBool) -> bool {
    flag.load(Ordering::Relaxed)
}

/// Mark an adapter's read half as ended.
pub fn mark_dead(flag: &AtomicBool) {
    flag.store(false, Ordering::Relaxed);
}

/// The challenge the daemon sends. Carries the daemon epoch so a proof minted
/// for a previous daemon cannot be replayed against this one.
pub fn challenge_for(daemon_epoch: u64) -> String {
    format!("daemon-{daemon_epoch}")
}

/// Ask the adapter to shut down, then make sure it is gone.
///
/// The drain is bounded: a runtime that has not exited by the deadline is
/// killed. An unbounded wait here would hang the daemon's exit path on a
/// runtime stuck mid-turn, which is the one moment nobody is watching.
pub fn drain(handle: &mut AdapterHandle, deadline: Duration) {
    let request = json!({
        "type": "studio.request",
        "requestId": "daemon-shutdown",
        "runtimeEpoch": handle.epoch,
        "operation": { "kind": "runtime.shutdown", "drain": true },
    });
    {
        let mut writer = handle
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = write_frame(&mut *writer, handle.epoch, &request);
        let _ = writer.flush();
    }
    let started = Instant::now();
    while started.elapsed() < deadline {
        match handle.child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => break,
        }
    }
    let _ = handle.child.kill();
    let _ = handle.child.wait();
}
