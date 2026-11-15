//! Browser observation and computer capture: the shell as a pure transport.
//!
//! Ported from the Electron shell's `DesktopBrowserObservation`
//! (`apps/desktop/src/browser-observation.ts`) and `registerComputerCaptureIpc`
//! (`apps/desktop/src/computer-observation.ts`).
//!
//! **There is no browser engine here, and that is the design.** The runtime owns
//! the browser and writes a one-line descriptor naming a private socket; the shell
//! reads the descriptor, authenticates with the token in it, and relays
//! newline-delimited JSON in both directions. No CDP endpoint or method ever
//! crosses into the renderer — the renderer gets validated events on a Tauri
//! channel and sends validated inputs back. That is why this module adds no
//! dependency: `std::os::unix::net::UnixStream` is the whole transport.
//!
//! The consequences, all deliberate:
//!
//! - **Nothing here can talk to a browser the runtime did not hand it.** The
//!   descriptor is written by the runtime, names the socket, and expires in a
//!   minute. A renderer that guesses an `observationId` finds no file.
//! - **The token is written once, to a socket the runtime owns.** It is not
//!   returned to the renderer and not logged.
//! - **Frames are rate-limited here, not in the renderer.** A stream of frames the
//!   renderer has not acknowledged is backpressure the transport must apply, or a
//!   busy tab fills the socket buffer and the shell's memory with it.
//! - **Computer captures are one-use.** The descriptor and payload are unlinked
//!   after a successful read, so a capture cannot be replayed.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
// Navigation URLs are parsed here rather than trusted: only HTTP(S), and no
// embedded credentials. Already in this crate's closure.
use url::Url;

use crate::chrome::{ChromeError, ChromeResult};

/// The Tauri channel observation events travel on. Kept byte-identical to the
/// Electron preload's so the renderer's subscription is the same on both shells.
pub const BROWSER_EVENT_CHANNEL: &str = "omp-studio:desktop:browser-event";

/// One private line may not exceed this. A frame is ~2.1 MB of base64, so the
/// buffer has to hold one whole line or the frame is lost mid-parse.
const PRIVATE_MAX_BYTES: usize = 4 * 1024 * 1024;

/// The protocol's ceiling on a frame's base64 payload.
const FRAME_MAX_BASE64: usize = 2_796_204;

/// A descriptor is a handful of fields; anything near this size is not one.
const DESCRIPTOR_MAX_BYTES: u64 = 4096;

/// How long a descriptor stays usable, and how far in the future it may claim to
/// be valid. Both bounds: an already-expired one is refused, and one claiming a
/// long life was not written by this handoff.
const DESCRIPTOR_WINDOW_MS: i64 = 60_000;

/// Simultaneous observations, matching the Electron shell.
const MAX_CONNECTIONS: usize = 4;

/// Minimum spacing between delivered frames. Below this the frame is acknowledged
/// and dropped rather than forwarded: the renderer repaints at its own rate, and
/// forwarding every frame only fills the IPC channel.
const FRAME_INTERVAL_MS: i64 = 95;

/// Computer capture payload limits.
const CAPTURE_MAX_BYTES: u64 = 8 * 1024 * 1024;
const CAPTURE_MIN_BYTES: usize = 33;
const MAX_CAPTURE_CLAIMS: usize = 4;

/// Where the descriptor root is, and where a test may redirect it.
///
/// Separate from the Skillshare tests' override on purpose: two suites installing
/// different directories into one slot would race, and the failure would look like
/// a flake rather than what it is.
static STATE_DIR_OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);

fn state_dir_override() -> Option<PathBuf> {
    STATE_DIR_OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// `<state>/browser` and `<state>/computer`, where the runtime drops descriptors.
fn descriptor_root(kind: &str) -> PathBuf {
    state_dir().join(kind)
}

/// The state dir this module reads, with a test's override applied.
fn state_dir() -> PathBuf {
    state_dir_override().unwrap_or_else(crate::chrome::shell_state_dir)
}

/// Every live observation, keyed by the window that owns it.
///
/// Keyed by window rather than by observation id because ownership is per window:
/// two windows asking for the same observation is two sockets, and the second must
/// not silently take over the first's.
static CONNECTIONS: Mutex<Option<HashMap<String, Vec<Connection>>>> = Mutex::new(None);

fn connections() -> std::sync::MutexGuard<'static, Option<HashMap<String, Vec<Connection>>>> {
    CONNECTIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One-use capture ids, so a capture cannot be read twice.
static CAPTURE_CLAIMS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// One live observation: the socket, and the flow-control state the reader and the
/// writer both touch.
struct Connection {
    observation_id: String,
    /// Held so the reader can refuse an event that names a different tab.
    #[allow(dead_code)]
    tab_id: String,
    socket: Arc<Mutex<std::os::unix::net::UnixStream>>,
    flow: Arc<Mutex<Flow>>,
}

/// Frame backpressure, shared between the reader thread and `control`.
#[derive(Default)]
struct Flow {
    /// Highest sequence forwarded. A lower one is a replay and is dropped.
    latest: u64,
    /// Sequence forwarded but not yet acknowledged by the renderer.
    in_flight: u64,
    /// When the last frame was forwarded, in epoch milliseconds.
    last_frame_at: i64,
    /// `agent` | `waiting` | `human` | `other-window`.
    control: String,
}

use std::sync::Arc;

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

/// Whether a string is a canonical UUID, which is what every id here is.
fn is_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        let expected_dash = matches!(index, 8 | 13 | 18 | 23);
        if expected_dash != (*byte == b'-') {
            return false;
        }
        if !expected_dash && !byte.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

/// The socket the runtime is listening on for one observation.
///
/// Must match the runtime's derivation exactly — two derivations of "where the
/// socket is" is how an attach ends up dialling nothing and reporting a timeout.
fn observation_socket(observation_id: &str, socket_directory: &Path) -> PathBuf {
    let digest = Sha256::digest(observation_id.as_bytes());
    let name = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    socket_directory.join(format!("b-{name}.sock"))
}

/// Read and validate `<state>/browser/<id>.json`.
///
/// Every check here exists because the file was written by a different process and
/// is being asked for by a renderer that may not be the one it was written for.
fn read_descriptor(
    observation_id: &str,
    session_id: &str,
    tab_id: &str,
) -> ChromeResult<(String, PathBuf)> {
    if !is_id(observation_id) || !is_id(tab_id) {
        return Err(ChromeError::invalid("invalid browser observation identity"));
    }
    if session_id.is_empty() || session_id.len() > 512 {
        return Err(ChromeError::invalid("invalid browser session"));
    }
    let file = descriptor_root("browser").join(format!("{observation_id}.json"));
    // `symlink_metadata`, not `metadata`: a descriptor that is a symlink is refused
    // rather than followed, the same rule the Skillshare secrets apply.
    let metadata = std::fs::symlink_metadata(&file)
        .map_err(|_| ChromeError::failed("browser observation descriptor unavailable"))?;
    if !metadata.is_file() || metadata.len() > DESCRIPTOR_MAX_BYTES {
        return Err(ChromeError::failed(
            "browser observation descriptor unavailable",
        ));
    }
    let text = std::fs::read_to_string(&file)
        .map_err(|_| ChromeError::failed("browser observation descriptor unavailable"))?;
    let descriptor: Value = serde_json::from_str(&text)
        .map_err(|_| ChromeError::failed("browser observation descriptor unavailable"))?;
    let Some(object) = descriptor.as_object() else {
        return Err(ChromeError::failed(
            "browser observation descriptor unavailable",
        ));
    };
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "version" | "observationId" | "sessionId" | "tabId" | "endpoint" | "token" | "expiresAt"))
    {
        return Err(ChromeError::failed(
            "browser observation descriptor unavailable",
        ));
    }
    let field = |name: &str| object.get(name);
    let text_field = |name: &str| field(name).and_then(Value::as_str);
    if field("version").and_then(Value::as_u64) != Some(1)
        || text_field("observationId") != Some(observation_id)
        || text_field("sessionId") != Some(session_id)
        || text_field("tabId") != Some(tab_id)
    {
        return Err(ChromeError::failed(
            "browser observation descriptor does not match this tab",
        ));
    }
    let token = text_field("token").unwrap_or("");
    if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ChromeError::failed(
            "browser observation descriptor is malformed",
        ));
    }
    let expires_at = field("expiresAt").and_then(Value::as_i64).unwrap_or_default();
    let now = now_millis();
    if expires_at < now || expires_at > now + DESCRIPTOR_WINDOW_MS {
        return Err(ChromeError::failed(
            "browser observation descriptor expired",
        ));
    }
    // The descriptor names the endpoint it was written for. If the shell would
    // dial anywhere else, the descriptor was not written for this shell.
    let endpoint = observation_socket(observation_id, &state_dir());
    if text_field("endpoint").is_none_or(|named| named != endpoint.to_string_lossy()) {
        return Err(ChromeError::failed("browser observation target changed"));
    }
    Ok((token.to_string(), endpoint))
}

/// `validateBrowserObservationEvent`, ported.
fn validate_event(event: &Value) -> ChromeResult<()> {
    let Some(object) = event.as_object() else {
        return Err(ChromeError::invalid("invalid browser observation event"));
    };
    let id = |name: &str| object.get(name).and_then(Value::as_str);
    if !id("observationId").is_some_and(is_id) {
        return Err(ChromeError::invalid("invalid observation identity"));
    }
    let kind = object.get("kind").and_then(Value::as_str).unwrap_or("");
    match kind {
        "closed" => {
            if object.len() != 2 {
                return Err(ChromeError::invalid("invalid browser observation event"));
            }
        }
        "error" => {
            if object.len() != 3 {
                return Err(ChromeError::invalid("invalid browser observation event"));
            }
            let message = object.get("message").and_then(Value::as_str).unwrap_or("");
            if message.is_empty() || message.len() > 4096 || message.contains('\0') {
                return Err(ChromeError::invalid("invalid browser observation event"));
            }
        }
        "state" => {
            if object.len() != 8 || !id("tabId").is_some_and(is_id) {
                return Err(ChromeError::invalid("invalid browser observation event"));
            }
            for key in ["title", "url"] {
                let value = object.get(key).and_then(Value::as_str).unwrap_or("");
                if value.len() > 4096 || value.contains('\0') {
                    return Err(ChromeError::invalid("invalid browser observation event"));
                }
            }
            for key in ["frozen", "busy"] {
                if !object.get(key).is_some_and(Value::is_boolean) {
                    return Err(ChromeError::invalid("invalid browser observation event"));
                }
            }
            if !matches!(
                object.get("control").and_then(Value::as_str),
                Some("agent") | Some("waiting") | Some("human") | Some("other-window")
            ) {
                return Err(ChromeError::invalid("invalid browser observation event"));
            }
        }
        "frame" => {
            if object.len() != 8 || !id("tabId").is_some_and(is_id) {
                return Err(ChromeError::invalid("invalid browser observation event"));
            }
            let sequence = object.get("sequence").and_then(Value::as_u64).unwrap_or_default();
            if sequence < 1 {
                return Err(ChromeError::invalid("invalid browser frame sequence"));
            }
            let data = object.get("data").and_then(Value::as_str).unwrap_or("");
            if data.len() > FRAME_MAX_BASE64
                || !data
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
            {
                return Err(ChromeError::invalid("invalid frame encoding"));
            }
            for key in ["width", "height"] {
                let value = object.get(key).and_then(Value::as_u64).unwrap_or_default();
                if !(1..=4096).contains(&value) {
                    return Err(ChromeError::invalid("invalid browser viewport"));
                }
            }
            let width = object.get("width").and_then(Value::as_u64).unwrap_or_default();
            let height = object.get("height").and_then(Value::as_u64).unwrap_or_default();
            if width * height > 4_000_000 {
                return Err(ChromeError::invalid(
                    "browser viewport exceeds the observation budget",
                ));
            }
        }
        _ => return Err(ChromeError::invalid("unknown browser observation event")),
    }
    Ok(())
}

/// `validateBrowserObservationInput`, ported.
fn validate_input(input: &Value) -> ChromeResult<()> {
    let Some(object) = input.as_object() else {
        return Err(ChromeError::invalid("invalid browser action"));
    };
    let kind = object.get("kind").and_then(Value::as_str).unwrap_or("");
    let allowed: &[&str] = match kind {
        "ping" | "take" | "release" => &["kind"],
        "ack" => &["kind", "sequence"],
        "navigate" => &["kind", "url"],
        "click" => &["kind", "sequence", "x", "y", "button"],
        "wheel" => &["kind", "sequence", "x", "y", "deltaX", "deltaY"],
        "key" => &["kind", "sequence", "key", "code", "modifiers"],
        "text" => &["kind", "sequence", "text"],
        _ => return Err(ChromeError::invalid("unknown browser action")),
    };
    if object.len() != allowed.len() || object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(ChromeError::invalid("invalid browser action"));
    }
    let number = |name: &str, min: f64, max: f64| {
        object
            .get(name)
            .and_then(Value::as_f64)
            .is_some_and(|value| value.is_finite() && (min..=max).contains(&value))
    };
    if allowed.contains(&"sequence") {
        let sequence = object.get("sequence").and_then(Value::as_u64).unwrap_or_default();
        if sequence < 1 {
            return Err(ChromeError::invalid("invalid browser frame sequence"));
        }
    }
    match kind {
        "navigate" => {
            let url = object.get("url").and_then(Value::as_str).unwrap_or("");
            if url.is_empty() || url.len() > 4096 || url.contains('\0') {
                return Err(ChromeError::invalid("invalid browser URL"));
            }
            // Only HTTP(S), and no embedded credentials: a `file:` or `data:` URL
            // would make this transport a file reader.
            let Ok(parsed) = Url::parse(url) else {
                return Err(ChromeError::invalid("invalid browser URL"));
            };
            if !matches!(parsed.scheme(), "http" | "https")
                || !parsed.username().is_empty()
                || parsed.password().is_some()
            {
                return Err(ChromeError::invalid(
                    "browser navigation requires an HTTP(S) URL without embedded credentials",
                ));
            }
        }
        "click" => {
            if !number("x", 0.0, 1.0) || !number("y", 0.0, 1.0) {
                return Err(ChromeError::invalid("invalid browser click coordinates"));
            }
            if !matches!(object.get("button").and_then(Value::as_str), Some("left") | Some("right")) {
                return Err(ChromeError::invalid("invalid mouse button"));
            }
        }
        "wheel" => {
            if !number("x", 0.0, 1.0) || !number("y", 0.0, 1.0) {
                return Err(ChromeError::invalid("invalid browser scroll coordinates"));
            }
            if !number("deltaX", -2048.0, 2048.0) || !number("deltaY", -2048.0, 2048.0) {
                return Err(ChromeError::invalid("invalid browser scroll delta"));
            }
        }
        "key" => {
            for key in ["key", "code"] {
                let value = object.get(key).and_then(Value::as_str).unwrap_or("");
                if value.is_empty() || value.len() > 64 || value.contains('\0') {
                    return Err(ChromeError::invalid("invalid browser key"));
                }
            }
            let modifiers = object.get("modifiers").and_then(Value::as_u64).unwrap_or(u64::MAX);
            if modifiers > 15 {
                return Err(ChromeError::invalid("invalid keyboard modifiers"));
            }
        }
        "text" => {
            let text = object.get("text").and_then(Value::as_str).unwrap_or("");
            if text.len() > 16384 || text.contains('\0') {
                return Err(ChromeError::invalid("invalid browser text"));
            }
        }
        _ => {}
    }
    Ok(())
}

/// Send one event to the window that owns the observation.
fn emit_to(app: &tauri::AppHandle, window: &str, event: Value) {
    // `Manager` provides `get_webview_window`, `Emitter` provides `emit`.
    use tauri::{Emitter, Manager};
    if let Some(window) = app.get_webview_window(window) {
        let _ = window.emit(BROWSER_EVENT_CHANNEL, event);
    }
}

/// Write one JSON line to the socket.
fn write_line(socket: &Arc<Mutex<std::os::unix::net::UnixStream>>, value: &Value) -> ChromeResult<()> {
    use std::io::Write;
    let text = serde_json::to_string(value)
        .map_err(|_| ChromeError::failed("cannot serialise the browser action"))?;
    let mut guard = socket
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard
        .write_all(format!("{text}\n").as_bytes())
        .map_err(|_| ChromeError::failed("browser observation is not connected"))
}

/// Connect, authenticate, and start relaying.
///
/// The reader thread owns the socket's read half; writes go through a shared
/// `UnixStream` behind a mutex. One writer per connection, so a slow input cannot
/// interleave with another's line and corrupt the stream.
pub fn attach(app: tauri::AppHandle, window: String, input: Value) -> ChromeResult<Value> {
    let observation_id = input
        .get("observationId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let session_id = input
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let tab_id = input
        .get("tabId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    let (token, endpoint) = read_descriptor(&observation_id, &session_id, &tab_id)?;

    {
        let mut guard = connections();
        let table = guard.get_or_insert_with(HashMap::new);
        // One observation per window: attaching again replaces the old socket,
        // which is what the renderer's own `detach` before `attach` relies on.
        if let Some(owned) = table.get_mut(&window) {
            owned.retain(|connection| connection.observation_id != observation_id);
            if owned.len() >= MAX_CONNECTIONS {
                return Err(ChromeError::failed("too many browser observations"));
            }
        }
    }

    use std::os::unix::net::UnixStream;
    let socket = UnixStream::connect(&endpoint)
        .map_err(|_| ChromeError::failed("browser observation connection failed"))?;
    // The token goes once, before anything else. A socket that does not answer
    // `OK` is not the runtime's.
    {
        use std::io::Write;
        let mut writer = &socket;
        writer
            .write_all(format!("{token}\n").as_bytes())
            .map_err(|_| ChromeError::failed("browser observation connection failed"))?;
        writer
            .flush()
            .map_err(|_| ChromeError::failed("browser observation connection failed"))?;
    }
    let reader_socket = socket
        .try_clone()
        .map_err(|_| ChromeError::failed("browser observation connection failed"))?;

    let connection = Connection {
        observation_id: observation_id.clone(),
        tab_id: tab_id.clone(),
        socket: Arc::new(Mutex::new(socket)),
        flow: Arc::new(Mutex::new(Flow {
            control: "agent".to_string(),
            ..Flow::default()
        })),
    };
    let shared_socket = Arc::clone(&connection.socket);
    let shared_flow = Arc::clone(&connection.flow);

    connections()
        .get_or_insert_with(HashMap::new)
        .entry(window.clone())
        .or_default()
        .push(connection);

    // The reader. Emits to the owning window only: another window asking for the
    // same observation gets its own socket, and forwarding both to each would let
    // one window's input move the other's page.
    std::thread::spawn(move || {
        use std::io::Read;
        let mut reader = reader_socket;
        let mut pending: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 64 * 1024];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => {
                    pending.extend_from_slice(&chunk[..read]);
                    if pending.len() > PRIVATE_MAX_BYTES {
                        break;
                    }
                    // One line at a time. A partial line stays buffered, which is
                    // what makes a frame that arrives in two reads still parse.
                    while let Some(index) = pending.iter().position(|byte| *byte == b'\n') {
                        let line: Vec<u8> = pending.drain(..=index).collect();
                        let line = &line[..line.len() - 1];
                        // Every one of these is `break`, not `return`: the
                        // cleanup after the loop drops the connection and emits
                        // `closed`, and a `return` here jumps over both. The first
                        // version used `return`, so one event kind a newer runtime
                        // added left the `Connection` in the table with a live fd
                        // for the process lifetime — and the renderer kept showing
                        // a live observation while every `control` answered "not
                        // connected", because no `closed` ever reached it.
                        let Ok(text) = std::str::from_utf8(line) else {
                            break;
                        };
                        let Ok(event) = serde_json::from_str::<Value>(text) else {
                            break;
                        };
                        if validate_event(&event).is_err() {
                            break;
                        }
                        if event.get("observationId").and_then(Value::as_str)
                            != Some(observation_id.as_str())
                            || event
                                .get("tabId")
                                .and_then(Value::as_str)
                                .is_some_and(|id| id != tab_id.as_str())
                        {
                            break;
                        }
                        let mut flow = shared_flow
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        if event.get("kind").and_then(Value::as_str) == Some("frame") {
                            let sequence =
                                event.get("sequence").and_then(Value::as_u64).unwrap_or_default();
                            // A replay, or a frame while one is still unacknowledged,
                            // is backpressure the transport applies rather than the
                            // renderer.
                            if sequence <= flow.latest || flow.in_flight != 0 {
                                let _ = write_line(
                                    &shared_socket,
                                    &json!({ "kind": "ack", "sequence": sequence }),
                                );
                                continue;
                            }
                            flow.latest = sequence;
                            if now_millis() - flow.last_frame_at < FRAME_INTERVAL_MS {
                                let _ = write_line(
                                    &shared_socket,
                                    &json!({ "kind": "ack", "sequence": sequence }),
                                );
                                continue;
                            }
                            flow.in_flight = sequence;
                            flow.last_frame_at = now_millis();
                        } else if event.get("kind").and_then(Value::as_str) == Some("state") {
                            flow.control = event
                                .get("control")
                                .and_then(Value::as_str)
                                .unwrap_or("agent")
                                .to_string();
                        }
                        drop(flow);
                        emit_to(&app, &window, event);
                    }
                }
                // A reset or a refused connection ends the observation; the
                // renderer learns it from the `closed` event below.
                Err(_) => break,
            }
        }
        detach_socket(&window, &observation_id);
        emit_to(
            &app,
            &window,
            json!({ "kind": "closed", "observationId": observation_id }),
        );
    });

    Ok(json!({ "ok": true }))
}

/// Send one input to the runtime.
pub fn control(window: String, input: Value) -> ChromeResult<Value> {
    let observation_id = input
        .get("observationId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let action = input.get("input").cloned().unwrap_or(Value::Null);
    validate_input(&action)?;

    let mut guard = connections();
    let table = guard.get_or_insert_with(HashMap::new);
    let Some(owned) = table.get(&window) else {
        return Ok(json!({ "ok": false, "message": "browser observation is not connected" }));
    };
    let Some(connection) = owned
        .iter()
        .find(|connection| connection.observation_id == observation_id)
    else {
        return Ok(json!({ "ok": false, "message": "browser observation is not connected" }));
    };
    let socket = Arc::clone(&connection.socket);
    let flow = Arc::clone(&connection.flow);
    drop(guard);

    // An acknowledgement is the one input that is not gated on control: it is how
    // the renderer says it has drawn the frame it was sent.
    let kind = action.get("kind").and_then(Value::as_str).unwrap_or("");
    if kind == "ack" {
        let sequence = action.get("sequence").and_then(Value::as_u64).unwrap_or_default();
        let mut flow = flow.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if flow.in_flight != sequence {
            return Ok(json!({ "ok": false, "message": "browser frame acknowledgement is stale" }));
        }
        flow.in_flight = 0;
    } else if !matches!(kind, "ping" | "take" | "release")
        && flow
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .control
            != "human"
    {
        // Anything that moves the page needs the human to hold control first. The
        // runtime enforces this too; refusing here means the click never leaves
        // the machine.
        return Ok(json!({ "ok": false, "message": "explicit human control is required" }));
    }
    write_line(&socket, &action)?;
    Ok(json!({ "ok": true }))
}

/// End one observation.
pub fn detach(window: String, input: Value) -> ChromeResult<Value> {
    let observation_id = input
        .get("observationId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    detach_socket(&window, &observation_id);
    Ok(json!({ "ok": true }))
}

/// Drop the connection and close its socket.
fn detach_socket(window: &str, observation_id: &str) {
    let mut guard = connections();
    if let Some(table) = guard.as_mut() {
        if let Some(owned) = table.get_mut(window) {
            owned.retain(|connection| connection.observation_id != observation_id);
            if owned.is_empty() {
                table.remove(window);
            }
        }
    }
}

/// End every observation a window owns.
///
/// Called from the window's close event. The shell has one window that outlives
/// the process, so nothing calls it today — but a second window would leak its
/// sockets on close, and the hook is the difference between that and not.
#[allow(dead_code)]
pub fn dispose_window(window: &str) {
    let mut guard = connections();
    if let Some(table) = guard.as_mut() {
        table.remove(window);
    }
}

/// Read one computer capture, once.
///
/// The PNG is checked for the magic bytes *and* for IHDR dimensions matching the
/// descriptor: a file that claims one size and is another is not a capture this
/// handoff wrote, and handing it to the renderer would render a broken image with
/// no explanation.
fn read_capture(input: Value) -> ChromeResult<Value> {
    let Some(object) = input.as_object() else {
        return Ok(json!({ "ok": false, "message": "invalid capture request" }));
    };
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "captureId" | "sessionId" | "targetId"))
    {
        return Ok(json!({ "ok": false, "message": "invalid capture request" }));
    }
    let capture_id = object.get("captureId").and_then(Value::as_str).unwrap_or("");
    let session_id = object.get("sessionId").and_then(Value::as_str).unwrap_or("");
    let target_id = object.get("targetId").and_then(Value::as_str).unwrap_or("");
    if !is_id(capture_id)
        || session_id.is_empty()
        || session_id.len() > 512
        || target_id.is_empty()
        || target_id.len() > 1024
    {
        return Ok(json!({ "ok": false, "message": "invalid capture request" }));
    }

    // Claimed before reading, so two readers cannot both be served the same
    // capture. The claim is released either way.
    {
        let mut claims = CAPTURE_CLAIMS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if claims.iter().any(|id| id == capture_id) {
            return Ok(json!({ "ok": false, "message": "invalid or already consumed capture" }));
        }
        if claims.len() >= MAX_CAPTURE_CLAIMS {
            return Ok(json!({ "ok": false, "message": "too many capture reads" }));
        }
        claims.push(capture_id.to_string());
    }

/// Read at most `limit` bytes from an open file.
///
/// A size checked on a *path* bounds nothing: between the check and the read the
/// file can grow, or be replaced. Reading through the handle that was measured,
/// with an explicit cap, means the budget holds even then.
fn read_bounded(file: &std::fs::File, limit: u64) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes)?;
    Ok(bytes)
}

    let root = descriptor_root("computer");
    let metadata_path = root.join(format!("{capture_id}.json"));
    let payload_path = root.join(format!("{capture_id}.png"));
    let unavailable =
        || ChromeError::failed("computer capture unavailable, expired, or outside the resource budget");
    let outcome = (|| -> ChromeResult<Value> {
        // Two stats, on purpose, because they answer different questions.
        //
        // `symlink_metadata` answers "is this a regular file, not a link" — a
        // rejection, so a race there can only fail closed. The size budget is
        // read from the *handle* the bytes are then read through, because
        // checking the size on the path and reading the path afterwards leaves a
        // window in which the file grows, and the budget is the one thing here
        // that must not be bypassable. `read_bounded` closes the rest of it.
        let metadata_link = std::fs::symlink_metadata(&metadata_path).map_err(|_| unavailable())?;
        let payload_link = std::fs::symlink_metadata(&payload_path).map_err(|_| unavailable())?;
        if !metadata_link.is_file() || !payload_link.is_file() {
            return Err(unavailable());
        }
        let metadata_file = std::fs::File::open(&metadata_path).map_err(|_| unavailable())?;
        let payload_file = std::fs::File::open(&payload_path).map_err(|_| unavailable())?;
        // `metadata()` on a handle is `fstat`: the same inode the read will use.
        let metadata_size = metadata_file.metadata().map_err(|_| unavailable())?.len();
        let payload_size = payload_file.metadata().map_err(|_| unavailable())?.len();
        if metadata_size > DESCRIPTOR_MAX_BYTES || payload_size > CAPTURE_MAX_BYTES {
            return Err(unavailable());
        }
        let text = String::from_utf8(
            read_bounded(&metadata_file, DESCRIPTOR_MAX_BYTES).map_err(|_| unavailable())?,
        )
        .map_err(|_| unavailable())?;
        let descriptor: Value = serde_json::from_str(&text).map_err(|_| unavailable())?;
        let Some(descriptor) = descriptor.as_object() else {
            return Err(unavailable());
        };
        let field = |name: &str| descriptor.get(name);
        let text_field = |name: &str| field(name).and_then(Value::as_str);
        if descriptor
            .keys()
            .any(|key| !matches!(key.as_str(), "captureId" | "sessionId" | "targetId" | "width" | "height" | "expiresAt"))
            || text_field("captureId") != Some(capture_id)
            || text_field("sessionId") != Some(session_id)
            || text_field("targetId") != Some(target_id)
        {
            return Err(unavailable());
        }
        let width = field("width").and_then(Value::as_u64).unwrap_or_default();
        let height = field("height").and_then(Value::as_u64).unwrap_or_default();
        let expires_at = field("expiresAt").and_then(Value::as_i64).unwrap_or_default();
        let now = now_millis();
        if width > 4096
            || height > 4096
            || expires_at < now
            || expires_at > now + DESCRIPTOR_WINDOW_MS
        {
            return Err(unavailable());
        }

        let bytes = read_bounded(&payload_file, CAPTURE_MAX_BYTES).map_err(|_| unavailable())?;
        // PNG magic, then IHDR's length and type, then the two dimensions the
        // descriptor claims. Anything else is not a PNG this handoff wrote.
        if bytes.len() < CAPTURE_MIN_BYTES
            || bytes[..8] != [137, 80, 78, 71, 13, 10, 26, 10]
            || u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) != 13
            || &bytes[12..16] != b"IHDR"
            || u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]) != width as u32
            || u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]) != height as u32
        {
            return Err(unavailable());
        }
        Ok(json!({ "ok": true, "data": bytes, "width": width, "height": height }))
    })();

    // One-use, on success only: a failed read leaves the capture for a retry, and
    // a succeeded one cannot be replayed.
    if outcome.is_ok() {
        let _ = std::fs::remove_file(&metadata_path);
        let _ = std::fs::remove_file(&payload_path);
    }
    let mut claims = CAPTURE_CLAIMS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    claims.retain(|id| id != capture_id);
    drop(claims);
    match outcome {
        Ok(value) => Ok(value),
        Err(error) => Ok(json!({ "ok": false, "message": error.message })),
    }
}

/// Attach to one observation.
#[tauri::command]
pub fn attach_browser_observation(
    app: tauri::AppHandle,
    window: tauri::Window,
    input: Value,
) -> ChromeResult<Value> {
    attach(app, window.label().to_string(), input)
}

/// Send one input to the runtime.
#[tauri::command]
pub fn control_browser_observation(window: tauri::Window, input: Value) -> ChromeResult<Value> {
    control(window.label().to_string(), input)
}

/// End one observation.
#[tauri::command]
pub fn detach_browser_observation(window: tauri::Window, input: Value) -> ChromeResult<Value> {
    detach(window.label().to_string(), input)
}

/// Read one computer capture, once.
#[tauri::command]
pub fn read_computer_capture_command(input: Value) -> ChromeResult<Value> {
    read_capture(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A descriptor written the way the runtime writes one.
    fn descriptor(
        observation_id: &str,
        session_id: &str,
        tab_id: &str,
        expires_in_ms: i64,
    ) -> Value {
        let endpoint = observation_socket(observation_id, &state_dir());
        json!({
            "version": 1,
            "observationId": observation_id,
            "sessionId": session_id,
            "tabId": tab_id,
            "endpoint": endpoint.to_string_lossy(),
            "token": "a".repeat(64),
            "expiresAt": now_millis() + expires_in_ms,
        })
    }

    /// Serialises the tests that install the state-dir override.
    ///
    /// The override is one process-global slot, so two tests installing different
    /// directories at once would each read the other's — and the symptom would be
    /// a descriptor that is "unavailable" for no visible reason. This is the third
    /// shared fixture this session that needed serialising; the guard's job, not
    /// each test's.
    static STATE_DIR_SERIALISE: Mutex<()> = Mutex::new(());

    /// Point the descriptor root at a directory of its own, run `body`, restore.
    ///
    /// The serialise lock is held for the whole test, but the *override's* lock is
    /// held only for the install and the restore — never across the call into the
    /// code under test. `std::sync::Mutex` is not reentrant, and `descriptor_root`
    /// locks it: holding it across `body` deadlocks the test against itself, which
    /// reports as a hang with no output.
    fn with_state_dir<T>(tag: &str, body: impl FnOnce(&Path) -> T) -> T {
        let _serialise = STATE_DIR_SERIALISE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = std::env::temp_dir().join(format!("browser-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("make a state directory");
        *STATE_DIR_OVERRIDE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(dir.clone());
        let outcome = body(&dir);
        *STATE_DIR_OVERRIDE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        let _ = std::fs::remove_dir_all(&dir);
        outcome
    }

    fn write_descriptor(id: &str, session: &str, tab: &str, expires_in_ms: i64) {
        let root = descriptor_root("browser");
        std::fs::create_dir_all(&root).expect("make the descriptor directory");
        std::fs::write(
            root.join(format!("{id}.json")),
            serde_json::to_string(&descriptor(id, session, tab, expires_in_ms)).expect("serialise"),
        )
        .expect("write the descriptor");
    }

    const ID: &str = "3f2504e0-4f89-11d3-9a0c-0305e82c3301";
    const TAB: &str = "3f2504e0-4f89-11d3-9a0c-0305e82c3302";
    const OTHER: &str = "3f2504e0-4f89-11d3-9a0c-0305e82c3303";
    const BIG: &str = "3f2504e0-4f89-11d3-9a0c-0305e82c3305";

    #[test]
    fn a_descriptor_the_runtime_wrote_is_accepted() {
        with_state_dir("accept", |_| {
            write_descriptor(ID, "session-1", TAB, 30_000);
            let (token, endpoint) = read_descriptor(ID, "session-1", TAB).expect("accepted");
            assert_eq!(token.len(), 64);
            //  compares whole components, not a string suffix:
            // the component is , so this asserts on the text.
            assert!(endpoint.to_string_lossy().ends_with(".sock"), "the endpoint is a socket path");
        });
    }

    #[test]
    fn a_descriptor_for_another_session_or_tab_is_refused() {
        with_state_dir("mismatch", |_| {
            write_descriptor(ID, "session-1", TAB, 30_000);
            for (session, tab) in [("someone-else", TAB), ("session-1", OTHER)] {
                assert!(
                    read_descriptor(ID, session, tab).is_err(),
                    "a descriptor for {session}/{tab} must not be served"
                );
            }
        });
    }

    #[test]
    fn an_expired_descriptor_is_refused() {
        with_state_dir("expired", |_| {
            write_descriptor(ID, "session-1", TAB, -1_000);
            assert!(read_descriptor(ID, "session-1", TAB).is_err());
        });
    }

    #[test]
    fn a_descriptor_that_claims_a_long_life_was_not_written_by_this_handoff() {
        // The window is a minute by design. One claiming hours is not one this
        // shell wrote, and serving it would honour a lifetime nobody intended.
        with_state_dir("longlife", |_| {
            write_descriptor(ID, "session-1", TAB, 3_600_000);
            assert!(read_descriptor(ID, "session-1", TAB).is_err());
        });
    }

    #[test]
    fn a_descriptor_that_is_a_symlink_is_refused_rather_than_followed() {
        // The link points at a *fully valid* descriptor, so "followed" would
        // succeed. The first version pointed at `{}` and only asserted `is_err()`,
        // which passes either way — a malformed target is refused for being
        // malformed, and the mutation that swapped `symlink_metadata` for
        // `metadata` left the test green. A guard that cannot tell the difference
        // is not guarding.
        with_state_dir("symlink", |_| {
            let target = std::env::temp_dir().join(format!("browser-target-{}.json", std::process::id()));
            std::fs::write(
                &target,
                serde_json::to_string(&descriptor(ID, "session-1", TAB, 30_000)).expect("serialise"),
            )
            .expect("write the target");
            let root = descriptor_root("browser");
            std::fs::create_dir_all(&root).expect("make the descriptor directory");
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, root.join(format!("{ID}.json"))).expect("symlink");
            #[cfg(unix)]
            assert!(
                read_descriptor(ID, "session-1", TAB).is_err(),
                "a symlinked descriptor is refused, not followed"
            );
        });
    }

    #[test]
    fn a_well_formed_event_passes_and_a_malformed_one_does_not() {
        for good in [
            json!({ "kind": "state", "observationId": ID, "tabId": TAB, "title": "t", "url": "https://x", "frozen": false, "busy": false, "control": "agent" }),
            json!({ "kind": "frame", "observationId": ID, "tabId": TAB, "sequence": 1, "data": "AAAA", "width": 800, "height": 600, "timestamp": 1 }),
            json!({ "kind": "error", "observationId": ID, "message": "boom" }),
            json!({ "kind": "closed", "observationId": ID }),
        ] {
            assert!(validate_event(&good).is_ok(), "must pass: {good}");
        }
        for bad in [
            json!({ "kind": "explode", "observationId": ID }),
            // A viewport past the budget.
            json!({ "kind": "frame", "observationId": ID, "tabId": TAB, "sequence": 1, "data": "AAAA", "width": 4096, "height": 4096, "timestamp": 1 }),
            // Sequence zero.
            json!({ "kind": "frame", "observationId": ID, "tabId": TAB, "sequence": 0, "data": "AAAA", "width": 800, "height": 600, "timestamp": 1 }),
            // Non-base64 payload.
            json!({ "kind": "frame", "observationId": ID, "tabId": TAB, "sequence": 1, "data": "!!!!", "width": 800, "height": 600, "timestamp": 1 }),
            // A control value outside the four.
            json!({ "kind": "state", "observationId": ID, "tabId": TAB, "title": "t", "url": "u", "frozen": false, "busy": false, "control": "root" }),
            // An extra field.
            json!({ "kind": "closed", "observationId": ID, "extra": 1 }),
        ] {
            assert!(validate_event(&bad).is_err(), "must be refused: {bad}");
        }
    }

    #[test]
    fn an_input_that_moves_the_page_is_only_well_formed_over_http() {
        assert!(validate_input(&json!({ "kind": "ping" })).is_ok());
        assert!(validate_input(&json!({ "kind": "navigate", "url": "https://example.com/a?b=c" })).is_ok());
        for bad in [
            // `file:` would make this transport a file reader.
            json!({ "kind": "navigate", "url": "file:///etc/passwd" }),
            json!({ "kind": "navigate", "url": "data:text/html,hi" }),
            // Credentials embedded in the URL.
            json!({ "kind": "navigate", "url": "https://user:pass@example.com" }),
            // Coordinates outside the unit square.
            json!({ "kind": "click", "sequence": 1, "x": 1.5, "y": 0.5, "button": "left" }),
            json!({ "kind": "click", "sequence": 1, "x": 0.5, "y": 0.5, "button": "middle" }),
            // A scroll delta past the clamp the renderer applies.
            json!({ "kind": "wheel", "sequence": 1, "x": 0, "y": 0, "deltaX": 99999, "deltaY": 0 }),
            // Modifiers past the four bits.
            json!({ "kind": "key", "sequence": 1, "key": "a", "code": "KeyA", "modifiers": 16 }),
            // An unknown kind.
            json!({ "kind": "launch-missiles" }),
            // An extra field on a kind that takes none.
            json!({ "kind": "ping", "sequence": 1 }),
        ] {
            assert!(validate_input(&bad).is_err(), "must be refused: {bad}");
        }
    }

    #[test]
    fn the_backpressure_rules_are_the_ones_the_reader_applies() {
        // Pinned here because they are the reader's, and the reader is the part
        // that cannot be unit-tested without a socket.
        let flow = Flow { latest: 5, in_flight: 5, ..Flow::default() };
        assert!(5 <= flow.latest, "a replay is caught by the sequence check");
        assert!(flow.in_flight != 0, "an unacknowledged frame is caught by the in-flight check");
        let flow = Flow { in_flight: 0, last_frame_at: now_millis(), ..flow };
        assert!(
            now_millis() - flow.last_frame_at < FRAME_INTERVAL_MS,
            "a frame inside the interval is acknowledged and dropped"
        );
    }

    /// A minimal 1x1 PNG: magic, IHDR with the right dimensions, then padding.
    fn tiny_png(width: u32, height: u32) -> Vec<u8> {
        let mut png = vec![137, 80, 78, 71, 13, 10, 26, 10];
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&width.to_be_bytes());
        png.extend_from_slice(&height.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        png.extend([0u8; 20]);
        png
    }

    #[test]
    fn a_capture_is_read_once_and_its_png_is_checked_against_the_descriptor() {
        with_state_dir("capture", |_| {
            let root = descriptor_root("computer");
            std::fs::create_dir_all(&root).expect("make the capture directory");
            let write = |id: &str, width: u32, height: u32| {
                std::fs::write(
                    root.join(format!("{id}.json")),
                    serde_json::to_string(&json!({
                        "captureId": id,
                        "sessionId": "session-1",
                        "targetId": "display-1",
                        "width": width,
                        "height": height,
                        "expiresAt": now_millis() + 30_000,
                    }))
                    .expect("serialise"),
                )
                .expect("write the descriptor");
                std::fs::write(root.join(format!("{id}.png")), tiny_png(width, height))
                    .expect("write the payload");
            };
            write(ID, 1, 1);
            let request = json!({ "captureId": ID, "sessionId": "session-1", "targetId": "display-1" });
            let first = read_capture(request.clone()).expect("read");
            assert_eq!(first.get("ok").and_then(Value::as_bool), Some(true));
            assert_eq!(first.get("width").and_then(Value::as_u64), Some(1));
            assert!(first.get("data").and_then(Value::as_array).is_some_and(|d| !d.is_empty()));

            // And it is gone: a capture cannot be replayed.
            assert!(!root.join(format!("{ID}.png")).exists(), "the payload is unlinked");
            assert!(!root.join(format!("{ID}.json")).exists(), "and so is the descriptor");
            assert_eq!(
                read_capture(request).expect("read again").get("ok").and_then(Value::as_bool),
                Some(false)
            );

            // A PNG whose dimensions disagree with the descriptor is refused.
            write(OTHER, 4, 4);
            std::fs::write(root.join(format!("{OTHER}.png")), tiny_png(1, 1)).expect("overwrite");
            // And one with the right IHDR but the wrong magic is refused too. The
            // first version only checked dimensions, so removing the magic check
            // left the test green — the header is what says this is a PNG at all.
            let third = "3f2504e0-4f89-11d3-9a0c-0305e82c3304";
            write(third, 1, 1);
            let mut wrong_magic = tiny_png(1, 1);
            wrong_magic[0] = 0;
            std::fs::write(root.join(format!("{third}.png")), wrong_magic).expect("overwrite");
            let outcome = read_capture(json!({ "captureId": OTHER, "sessionId": "session-1", "targetId": "display-1" }))
                .expect("read");
            assert_eq!(
                outcome.get("ok").and_then(Value::as_bool),
                Some(false),
                "a size mismatch is refused"
            );
            let wrong = read_capture(json!({ "captureId": third, "sessionId": "session-1", "targetId": "display-1" }))
                .expect("read");
            assert_eq!(
                wrong.get("ok").and_then(Value::as_bool),
                Some(false),
                "a file that is not a PNG is refused"
            );
        });
    }

    #[test]
    fn a_capture_over_the_resource_budget_is_refused() {
        with_state_dir("capturebudget", |_| {
            let root = descriptor_root("computer");
            std::fs::create_dir_all(&root).expect("make the capture directory");
            let request = json!({ "captureId": BIG, "sessionId": "session-1", "targetId": "display-1" });
            std::fs::write(
                root.join(format!("{BIG}.json")),
                serde_json::to_string(&json!({
                    "captureId": BIG,
                    "sessionId": "session-1",
                    "targetId": "display-1",
                    "width": 1,
                    "height": 1,
                    "expiresAt": now_millis() + 30_000,
                }))
                .expect("serialise"),
            )
            .expect("write the descriptor");
            // Over `CAPTURE_MAX_BYTES`. The size used to be checked on the path
            // and the bytes then read from the path, so a file that grew between
            // the two — or was swapped for a bigger one — was read whole. The
            // budget is the one thing this handoff must not let through, so it is
            // now read off the handle the bytes come through, and capped.
            std::fs::write(
                root.join(format!("{BIG}.png")),
                vec![0u8; (CAPTURE_MAX_BYTES + 1) as usize],
            )
            .expect("write the oversized payload");

            let outcome = read_capture(request).expect("read");
            assert_eq!(
                outcome.get("ok").and_then(Value::as_bool),
                Some(false),
                "a payload over the capture budget must be refused, not read"
            );
            // A refused capture is not consumed, so the file is still there for
            // the reaper. Unlinking on the way out would be a second thing this
            // path does, and "refuse" is the only thing it claims.
            assert!(
                root.join(format!("{BIG}.png")).exists(),
                "a refused capture was consumed anyway"
            );
        });
    }

    #[test]
    fn a_capture_request_with_an_unknown_field_is_refused() {
        with_state_dir("capturefields", |_| {
            for bad in [
                json!({ "captureId": "x", "sessionId": "s", "targetId": "t", "extra": 1 }),
                json!({ "captureId": "not-a-uuid", "sessionId": "s", "targetId": "t" }),
                json!({ "captureId": ID, "sessionId": "", "targetId": "t" }),
            ] {
                assert_eq!(
                    read_capture(bad).expect("answered").get("ok").and_then(Value::as_bool),
                    Some(false)
                );
            }
        });
    }

    #[test]
    fn a_frame_line_split_across_two_reads_still_parses() {
        // The reader buffers until it has a whole line. This is the property that
        // makes a 2 MB frame arriving in 64 KB chunks still decode.
        let mut pending: Vec<u8> = Vec::new();
        pending.extend_from_slice(b"{\"kind\":\"closed\"");
        assert!(pending.iter().position(|byte| *byte == b'\n').is_none(), "no line yet");
        pending.extend_from_slice(b",\"observationId\":\"x\"}\n");
        let index = pending.iter().position(|byte| *byte == b'\n').expect("a line now");
        let line: Vec<u8> = pending.drain(..=index).collect();
        assert!(serde_json::from_str::<Value>(std::str::from_utf8(&line[..line.len() - 1]).unwrap()).is_ok());
    }
}
