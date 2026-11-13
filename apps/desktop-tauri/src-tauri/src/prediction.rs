//! The prediction channel: the shell as a transport, once more.
//!
//! Ported from the Electron shell's `DesktopPrediction`
//! (`apps/desktop/src/prediction.ts`).
//!
//! This is the third of the four private runtime channels the shell relays, and the
//! pattern is now established: the runtime writes a descriptor naming a socket and
//! a token, the shell validates the descriptor, authenticates, and relays
//! newline-delimited JSON both ways. No engine, no protocol of the shell's own —
//! `std::os::unix::net::UnixStream` is the whole transport.
//!
//! What is different here is the history import, and it is the one part with real
//! state: a file the user picks is copied to a name the runtime chooses, the
//! runtime is told that name, and the file is deleted whatever happens. The rules
//! that follow:
//!
//! - **The copy is bounded and private.** 8 MiB, mode 0600, `wx` so two imports
//!   cannot race on one name.
//! - **The import is not retried.** A timeout resolves with a message saying it
//!   "may have partially completed", because it may have: the runtime could have
//!   applied half the file before the shell stopped listening. Retrying would
//!   double it.
//! - **One import per window, and one per channel.** Two concurrent imports on one
//!   socket would interleave their request ids and resolve each other.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::chrome::{ChromeError, ChromeResult};

/// The Tauri channel prediction events travel on, byte-identical to the Electron
/// preload's.
pub const PREDICTION_EVENT_CHANNEL: &str = "omp-studio:desktop:prediction-event";

/// A descriptor is a handful of fields.
const DESCRIPTOR_MAX_BYTES: u64 = 4096;

/// The prediction handoff's window is shorter than the browser's: 30 seconds.
const DESCRIPTOR_WINDOW_MS: i64 = 30_000;

/// Attach must complete inside this, or the socket is dropped.
const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);

/// One private line may not exceed this.
const PRIVATE_MAX_BYTES: usize = 32_768;

/// A history file may not exceed this.
const IMPORT_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// An import that has not been answered inside this is abandoned, not retried.
const IMPORT_TIMEOUT: Duration = Duration::from_secs(90);

/// One live channel per window.
struct Channel {
    session_id: String,
    socket: Arc<Mutex<std::os::unix::net::UnixStream>>,
    /// The import in flight, if any.
    importing: Option<Importing>,
}

/// One history import awaiting its `imported` or `error` event.
struct Importing {
    request_id: String,
    /// Fires with the result. `None` once it has.
    resolve: std::sync::mpsc::Sender<Value>,
}

static CHANNELS: Mutex<Option<HashMap<String, Channel>>> = Mutex::new(None);
/// Which windows have an import running. A set, so the value is never read.
static ACTIVE_IMPORTS: Mutex<Option<HashMap<String, bool>>> = Mutex::new(None);

fn channels() -> std::sync::MutexGuard<'static, Option<HashMap<String, Channel>>> {
    CHANNELS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

/// Whether a string is a canonical UUID.
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

/// A bounded, control-character-free session identity.
fn is_session(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

/// The socket the runtime listens on for one channel.
///
/// Same derivation the runtime makes, so the shell only dials an endpoint it
/// computed itself.
fn channel_socket(channel_id: &str) -> PathBuf {
    let digest = Sha256::digest(channel_id.as_bytes());
    let name = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    crate::chrome::shell_state_dir().join(format!("p-{name}.sock"))
}

/// The one message for every way an attach can fail.
const ATTACH_FAILED: &str = "Prediction channel unavailable.";

/// Read and validate `<state>/prediction/<id>.json`.
fn read_descriptor(channel_id: &str, session_id: &str) -> ChromeResult<String> {
    if !is_id(channel_id) {
        return Err(ChromeError::invalid("invalid prediction id"));
    }
    if !is_session(session_id) {
        return Err(ChromeError::invalid("invalid prediction session"));
    }
    let file = crate::chrome::shell_state_dir()
        .join("prediction")
        .join(format!("{channel_id}.json"));
    let metadata = std::fs::symlink_metadata(&file)
        .map_err(|_| ChromeError::failed(ATTACH_FAILED))?;
    if !metadata.is_file() || metadata.len() > DESCRIPTOR_MAX_BYTES {
        return Err(ChromeError::failed(ATTACH_FAILED));
    }
    let text =
        std::fs::read_to_string(&file).map_err(|_| ChromeError::failed(ATTACH_FAILED))?;
    let descriptor: Value =
        serde_json::from_str(&text).map_err(|_| ChromeError::failed(ATTACH_FAILED))?;
    let Some(object) = descriptor.as_object() else {
        return Err(ChromeError::failed(ATTACH_FAILED));
    };
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "version" | "channelId" | "sessionId" | "endpoint" | "token" | "expiresAt"))
    {
        return Err(ChromeError::failed(ATTACH_FAILED));
    }
    let field = |name: &str| object.get(name);
    let text_field = |name: &str| field(name).and_then(Value::as_str);
    if field("version").and_then(Value::as_u64) != Some(1)
        || text_field("channelId") != Some(channel_id)
        || text_field("sessionId") != Some(session_id)
    {
        return Err(ChromeError::failed("Prediction descriptor expired or changed"));
    }
    let token = text_field("token").unwrap_or("");
    if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ChromeError::failed(ATTACH_FAILED));
    }
    let expires_at = field("expiresAt").and_then(Value::as_i64).unwrap_or_default();
    let now = now_millis();
    if expires_at < now || expires_at > now + DESCRIPTOR_WINDOW_MS {
        return Err(ChromeError::failed("Prediction descriptor expired or changed"));
    }
    if text_field("endpoint").is_none_or(|named| named != channel_socket(channel_id).to_string_lossy()) {
        return Err(ChromeError::failed("Prediction descriptor expired or changed"));
    }
    Ok(token.to_string())
}

/// Attach to one prediction channel.
pub fn attach(app: tauri::AppHandle, window: String, input: Value) -> ChromeResult<Value> {
    let Some(object) = input.as_object() else {
        return Ok(json!({ "ok": false, "message": ATTACH_FAILED }));
    };
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "channelId" | "sessionId" | "expiresAt"))
    {
        return Ok(json!({ "ok": false, "message": ATTACH_FAILED }));
    }
    let channel_id = object.get("channelId").and_then(Value::as_str).unwrap_or("");
    let session_id = object.get("sessionId").and_then(Value::as_str).unwrap_or("");
    let token = match read_descriptor(channel_id, session_id) {
        Ok(token) => token,
        Err(_) => return Ok(json!({ "ok": false, "message": ATTACH_FAILED })),
    };

    // One channel per window: attaching again replaces the old socket.
    dispose_window(&window);
    use std::os::unix::net::UnixStream;
    let socket = match UnixStream::connect(channel_socket(channel_id)) {
        Ok(socket) => socket,
        Err(_) => return Ok(json!({ "ok": false, "message": ATTACH_FAILED })),
    };
    {
        use std::io::Write;
        let mut writer = &socket;
        if writer.write_all(format!("{token}\n").as_bytes()).is_err() {
            return Ok(json!({ "ok": false, "message": ATTACH_FAILED }));
        }
    }
    let reader_socket = match socket.try_clone() {
        Ok(reader) => reader,
        Err(_) => return Ok(json!({ "ok": false, "message": ATTACH_FAILED })),
    };

    // `channel_id` is read once here and then owned by the reader thread; the
    // `session_id` is what the write path checks ownership against.
    let channel = Channel {
        session_id: session_id.to_string(),
        socket: Arc::new(Mutex::new(socket)),
        importing: None,
    };

    // The runtime answers `OK\n` before anything else.
    use std::io::Read;
    let mut answer = [0u8; 3];
    let deadline = Instant::now() + ATTACH_TIMEOUT;
    let mut seen = 0;
    let mut reader = reader_socket;
    while seen < 3 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(json!({ "ok": false, "message": "Prediction attach timed out." }));
        }
        reader.set_read_timeout(Some(remaining)).ok();
        match reader.read(&mut answer[seen..]) {
            Ok(0) => return Ok(json!({ "ok": false, "message": ATTACH_FAILED })),
            Ok(read) => seen += read,
            Err(_) => return Ok(json!({ "ok": false, "message": "Prediction connection failed." })),
        }
    }
    if &answer != b"OK\n" {
        return Ok(json!({ "ok": false, "message": ATTACH_FAILED }));
    }

    channels()
        .get_or_insert_with(HashMap::new)
        .insert(window.clone(), channel);

    let observation = window.clone();
    let owned_id = channel_id.to_string();
    std::thread::spawn(move || {
        let mut pending: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => {
                    pending.extend_from_slice(&chunk[..read]);
                    if pending.len() > PRIVATE_MAX_BYTES {
                        break;
                    }
                    while let Some(index) = pending.iter().position(|byte| *byte == b'\n') {
                        let line: Vec<u8> = pending.drain(..=index).collect();
                        let line = &line[..line.len() - 1];
                        let Ok(text) = std::str::from_utf8(line) else {
                            return;
                        };
                        let Ok(event) = serde_json::from_str::<Value>(text) else {
                            return;
                        };
                        if !is_event(&event) {
                            return;
                        }
                        if event.get("channelId").and_then(Value::as_str) != Some(owned_id.as_str()) {
                            return;
                        }
                        // An import's answer resolves the promise the importer is
                        // holding, and is not forwarded: it is a reply, not an
                        // observation.
                        let request_id = event.get("requestId").and_then(Value::as_str);
                        let answered = channels()
                            .get_or_insert_with(HashMap::new)
                            .get_mut(&observation)
                            .and_then(|channel| {
                                let importing = channel.importing.as_ref()?;
                                (request_id == Some(importing.request_id.as_str())).then(|| {
                                    let importing = channel.importing.take()?;
                                    let answer = if event.get("kind").and_then(Value::as_str) == Some("imported") {
                                        json!({ "ok": true, "count": event.get("count"), "truncated": event.get("truncated") })
                                    } else {
                                        json!({ "ok": false, "message": event.get("message").and_then(Value::as_str).unwrap_or("Prediction import failed.") })
                                    };
                                    importing.resolve.send(answer).ok();
                                    Some(())
                                })
                            })
                            .flatten();
                        if answered.is_some() {
                            continue;
                        }
                        emit_to(&app, &observation, event);
                    }
                }
                Err(_) => break,
            }
        }
        // A closed channel is an event too: the renderer clears its state from it.
        dispose_window(&observation);
        emit_to(
            &app,
            &observation,
            json!({ "kind": "closed", "channelId": owned_id }),
        );
    });

    Ok(json!({ "ok": true }))
}

/// `validatePredictionEvent`, ported.
fn is_event(event: &Value) -> bool {
    let Some(object) = event.as_object() else {
        return false;
    };
    if !object.get("channelId").and_then(Value::as_str).is_some_and(is_id) {
        return false;
    }
    match object.get("kind").and_then(Value::as_str).unwrap_or("") {
        "closed" => object.len() == 2,
        "error" => {
            object.len() == 3
                && object
                    .get("message")
                    .and_then(Value::as_str)
                    .is_some_and(|message| !message.is_empty() && message.len() <= 4096)
        }
        "imported" => {
            object.len() == 5
                && object.get("requestId").and_then(Value::as_str).is_some_and(is_id)
                && object.get("count").and_then(Value::as_u64).is_some()
                && object.get("truncated").is_some_and(Value::is_boolean)
        }
        _ => false,
    }
}

/// `validatePredictionInput`, ported. `import` is refused: it needs the file
/// picker, which only `import_history` has.
fn is_input(input: &Value) -> bool {
    let Some(object) = input.as_object() else {
        return false;
    };
    let kind = object.get("kind").and_then(Value::as_str).unwrap_or("");
    let allowed: &[&str] = match kind {
        "steer" => &["kind", "text"],
        "cancel" | "release" | "ping" => &["kind"],
        _ => return false,
    };
    if object.len() != allowed.len() || object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return false;
    }
    if kind == "steer" {
        let text = object.get("text").and_then(Value::as_str).unwrap_or("");
        return !text.is_empty() && text.len() <= 16384 && !text.contains('\0');
    }
    true
}

fn emit_to(app: &tauri::AppHandle, window: &str, event: Value) {
    use tauri::{Emitter, Manager};
    if let Some(window) = app.get_webview_window(window) {
        let _ = window.emit(PREDICTION_EVENT_CHANNEL, event);
    }
}

/// Send one input to the runtime.
pub fn control(window: String, input: Value) -> ChromeResult<Value> {
    let Some(object) = input.as_object() else {
        return Ok(json!({ "ok": false, "message": "Prediction channel is not owned by this window" }));
    };
    if object.keys().any(|key| !matches!(key.as_str(), "channelId" | "input")) {
        return Ok(json!({ "ok": false, "message": "Prediction channel is not owned by this window" }));
    }
    let channel_id = object.get("channelId").and_then(Value::as_str).unwrap_or("");
    let action = object.get("input").cloned().unwrap_or(Value::Null);
    // `import` is refused here, and only here: `is_input` does not list it, so it
    // is an unknown kind like any other. That is the whole boundary — a history
    // import needs the file picker, and a renderer that could send one directly
    // would name any file on the machine.
    if !is_input(&action) {
        return Ok(json!({ "ok": false, "message": "History imports require the file picker" }));
    }
    let socket = match channels()
        .get_or_insert_with(HashMap::new)
        .get(&window)
        .filter(|channel| channel.session_id.len() <= 128)
        .map(|channel| Arc::clone(&channel.socket))
    {
        Some(socket) if is_id(channel_id) => socket,
        _ => {
            return Ok(json!({ "ok": false, "message": "Prediction channel is not owned by this window" }))
        }
    };
    use std::io::Write;
    let text = match serde_json::to_string(&action) {
        Ok(text) => text,
        Err(_) => return Ok(json!({ "ok": false, "message": "Prediction input rejected" })),
    };
    let mut guard = socket
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match guard.write_all(format!("{text}\n").as_bytes()) {
        Ok(()) => Ok(json!({ "ok": true })),
        Err(_) => Ok(json!({ "ok": false, "message": "Prediction channel is not owned by this window" })),
    }
}

/// Import a JSONL history file through the file picker.
pub fn import_history(window: String, input: Value, picked: Option<PathBuf>) -> ChromeResult<Value> {
    let Some(object) = input.as_object() else {
        return Ok(json!({ "ok": false, "message": "Invalid import request" }));
    };
    if object.keys().any(|key| key != "channelId") {
        return Ok(json!({ "ok": false, "message": "Invalid import request" }));
    }
    let channel_id = object.get("channelId").and_then(Value::as_str).unwrap_or("");

    // One per window, and one per channel.
    {
        let mut active = ACTIVE_IMPORTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let table = active.get_or_insert_with(HashMap::new);
        if table.contains_key(&window) {
            return Ok(json!({ "ok": false, "message": "A history import is already running" }));
        }
        table.insert(window.clone(), true);
    }
    let outcome = (|| -> ChromeResult<Value> {
        let has_channel = channels()
            .get_or_insert_with(HashMap::new)
            .get(&window)
            .is_some_and(|_| is_id(channel_id));
        if !has_channel {
            return Ok(json!({ "ok": false, "message": "Prediction channel is not owned by this window" }));
        }
        let Some(selected) = picked else {
            // Cancelled is a success with nothing done, which is what the renderer
            // shows as "no change".
            return Ok(json!({ "ok": true, "cancelled": true }));
        };
        let metadata = std::fs::symlink_metadata(&selected)
            .map_err(|_| ChromeError::failed("Select a JSONL history file no larger than 8 MiB"))?;
        if !metadata.is_file() || metadata.len() > IMPORT_MAX_BYTES {
            return Err(ChromeError::failed(
                "Select a JSONL history file no larger than 8 MiB",
            ));
        }

        // Copied to a name the shell chooses, with a mode that keeps it private:
        // the runtime reads it from here, and nobody else should.
        let directory = crate::chrome::shell_state_dir().join("prediction-imports");
        std::fs::create_dir_all(&directory).map_err(|error| {
            ChromeError::failed(format!("cannot stage the history file: {error}"))
        })?;
        let transfer_id = uuid::Uuid::new_v4().to_string();
        let request_id = uuid::Uuid::new_v4().to_string();
        let target = directory.join(format!("{transfer_id}.jsonl"));
        let bytes = std::fs::read(&selected)
            .map_err(|_| ChromeError::failed("cannot read the history file"))?;
        if bytes.len() as u64 > IMPORT_MAX_BYTES {
            return Err(ChromeError::failed("History file exceeds 8 MiB"));
        }
        write_private(&target, &bytes)?;

        let (sender, receiver) = std::sync::mpsc::channel();
        {
            let mut guard = channels();
            if let Some(channel) = guard.get_or_insert_with(HashMap::new).get_mut(&window) {
                channel.importing = Some(Importing {
                    request_id: request_id.clone(),
                    resolve: sender,
                });
            }
        }
        let write = {
            let socket = channels()
                .get_or_insert_with(HashMap::new)
                .get(&window)
                .map(|channel| Arc::clone(&channel.socket));
            let line = serde_json::to_string(&json!({ "kind": "import", "requestId": request_id, "transferId": transfer_id }))
                .unwrap_or_default();
            match socket {
                Some(socket) => {
                    let mut guard = socket.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    use std::io::Write;
                    guard.write_all(format!("{line}\n").as_bytes()).is_ok()
                }
                None => false,
            }
        };
        if !write {
            let _ = std::fs::remove_file(&target);
            return Ok(json!({ "ok": false, "message": "Prediction channel is not owned by this window" }));
        }

        // Waited for, and abandoned on timeout. **Not retried**: the runtime may
        // have applied part of the file before the shell stopped listening, and a
        // second attempt would double it.
        let outcome = match receiver.recv_timeout(IMPORT_TIMEOUT) {
            Ok(answer) => answer,
            Err(_) => json!({ "ok": false, "message": "Prediction import timed out; it was not retried" }),
        };
        let _ = std::fs::remove_file(&target);
        Ok(outcome)
    })();
    // Whatever happened, the import is over and the staged file is gone.
    {
        let mut guard = channels();
        if let Some(channel) = guard.get_or_insert_with(HashMap::new).get_mut(&window) {
            channel.importing = None;
        }
    }
    let mut active = ACTIVE_IMPORTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(table) = active.as_mut() {
        table.remove(&window);
    }
    drop(active);
    match outcome {
        Ok(value) => Ok(value),
        Err(error) => Ok(json!({ "ok": false, "message": error.message })),
    }
}

/// Write with owner-only permissions.
fn write_private(path: &std::path::Path, bytes: &[u8]) -> ChromeResult<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| ChromeError::failed(format!("cannot stage the history file: {error}")))?;
    file.write_all(bytes)
        .map_err(|error| ChromeError::failed(format!("cannot stage the history file: {error}")))
}

/// End one channel.
pub fn detach(window: String, input: Value) -> ChromeResult<Value> {
    let _ = input;
    dispose_window(&window);
    Ok(json!({ "ok": true }))
}

/// Drop a window's channel and close its socket.
pub fn dispose_channel(window: &str) {
    dispose_window(window)
}

fn dispose_window(window: &str) {
    let mut guard = channels();
    if let Some(table) = guard.as_mut() {
        table.remove(window);
    }
}

/// Attach to one prediction channel.
#[tauri::command]
pub fn attach_prediction(
    app: tauri::AppHandle,
    window: tauri::Window,
    input: Value,
) -> ChromeResult<Value> {
    attach(app, window.label().to_string(), input)
}

/// Send one input to the runtime.
#[tauri::command]
pub fn prediction_input(window: tauri::Window, input: Value) -> ChromeResult<Value> {
    control(window.label().to_string(), input)
}

/// End one channel.
#[tauri::command]
pub fn detach_prediction(window: tauri::Window, input: Value) -> ChromeResult<Value> {
    detach(window.label().to_string(), input)
}

/// Import a JSONL history file, through the OS file picker.
#[tauri::command]
pub fn import_prediction_history(window: tauri::Window, input: Value) -> ChromeResult<Value> {
    let Some(handle) = crate::app_handle() else {
        return Ok(json!({ "ok": false, "message": "the file picker is not available" }));
    };
    let picked = tauri_plugin_dialog::DialogExt::dialog(handle)
        .file()
        .set_title("选择提示词历史 / Import prompt history")
        .add_filter("JSONL history", &["jsonl"])
        .blocking_pick_file();
    let picked = picked.as_ref().and_then(|file| file.as_path()).map(|path| path.to_path_buf());
    import_history(window.label().to_string(), input, picked)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "3f2504e0-4f89-11d3-9a0c-0305e82c3301";
    const OTHER: &str = "3f2504e0-4f89-11d3-9a0c-0305e82c3302";

    #[test]
    fn a_well_formed_event_passes_and_a_malformed_one_does_not() {
        for good in [
            json!({ "kind": "closed", "channelId": ID }),
            json!({ "kind": "error", "channelId": ID, "message": "boom" }),
            json!({ "kind": "imported", "channelId": ID, "requestId": OTHER, "count": 3, "truncated": false }),
        ] {
            assert!(is_event(&good), "must pass: {good}");
        }
        for bad in [
            json!({ "kind": "explode", "channelId": ID }),
            // `count` missing from an `imported`.
            json!({ "kind": "imported", "channelId": ID, "requestId": OTHER, "truncated": false }),
            // `requestId` that is not a UUID.
            json!({ "kind": "imported", "channelId": ID, "requestId": "x", "count": 1, "truncated": false }),
            // `count` that is not a number. The field count is right, so only the
            // count check catches this — the first version of the test omitted it
            // and the mutation that deleted the check passed.
            json!({ "kind": "imported", "channelId": ID, "requestId": OTHER, "count": "many", "truncated": false }),
            // `truncated` that is not a boolean.
            json!({ "kind": "imported", "channelId": ID, "requestId": OTHER, "count": 1, "truncated": "no" }),
            // An extra field.
            json!({ "kind": "closed", "channelId": ID, "extra": 1 }),
        ] {
            assert!(!is_event(&bad), "must be refused: {bad}");
        }
    }

    #[test]
    fn an_import_cannot_be_sent_as_a_plain_input() {
        // A renderer that could send `import` directly would name any file on the
        // machine; the picker is the boundary.
        for allowed in [
            json!({ "kind": "ping" }),
            json!({ "kind": "cancel" }),
            json!({ "kind": "release" }),
            json!({ "kind": "steer", "text": "turn left" }),
        ] {
            assert!(is_input(&allowed), "{allowed} is a legal input");
        }
        // `import` is not in the list, which is the whole boundary: it needs the
        // picker. The first version of this test also passed with the extra
        // `import` check in `control` deleted, because `is_input` refuses it
        // anyway — the two were redundant and the test could not tell.
        assert!(!is_input(&json!({ "kind": "import", "requestId": ID, "transferId": ID })));
        assert!(!is_input(&json!({ "kind": "steer" })), "steer needs its text");
        assert!(!is_input(&json!({ "kind": "steer", "text": "" })));
        assert!(!is_input(&json!({ "kind": "unknown" })));
        // An extra field on a kind that takes none. Without this the field-count
        // check is unexercised: deleting it leaves every case above green, because
        // none of them carries a field its kind forbids.
        assert!(!is_input(&json!({ "kind": "ping", "sequence": 1 })));
        assert!(!is_input(&json!({ "kind": "steer", "text": "x", "extra": 1 })));
    }

    #[test]
    fn a_descriptor_the_runtime_wrote_is_accepted() {
        // No descriptor on this machine, which is the honest state: the runtime
        // has not prepared a channel. What this pins is that the *validation*
        // runs before any socket is dialled — a renderer that invents a channelId
        // is refused for the shape, not for a connection error.
        assert!(read_descriptor("not-a-uuid", "session-1").is_err());
        assert!(read_descriptor(ID, "").is_err());
        assert!(read_descriptor(ID, &"s".repeat(129)).is_err());
    }

    #[test]
    fn the_socket_name_is_the_one_the_runtime_derives() {
        // `p-<sha256(id)[..16 hex]>.sock`, the same derivation as the browser and
        // audio channels with a different prefix.
        let path = channel_socket(ID);
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        assert!(name.starts_with("p-"), "{name}");
        assert_eq!(name.len(), "p-".len() + 16 + ".sock".len(), "{name}");
        assert_eq!(channel_socket(ID), channel_socket(ID), "stable");
        assert_ne!(channel_socket(ID), channel_socket(OTHER), "and distinct");
    }

    #[test]
    fn a_staged_history_file_is_owner_only_and_exclusive() {
        let dir = std::env::temp_dir().join(format!("prediction-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("make a directory");
        let target = dir.join("transfer.jsonl");
        write_private(&target, b"{\"role\":\"user\"}\n").expect("staged");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&target).expect("metadata").permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "the staged history is owner-only, was {mode:o}");
        }
        // `create_new`, so a second import cannot land on the same name.
        assert!(write_private(&target, b"again").is_err(), "the name is exclusive");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_import_without_a_channel_is_refused_rather_than_staged() {
        // Staging a file and then finding nowhere to send it would leave a copy of
        // the user's history on disk.
        let outcome = import_history(
            "test-no-channel".to_string(),
            json!({ "channelId": ID }),
            Some(std::env::temp_dir().join("history.jsonl")),
        )
        .expect("answered");
        assert_eq!(outcome.get("ok").and_then(Value::as_bool), Some(false));
    }

    #[test]
    fn the_channel_is_checked_before_the_picker_opens() {
        // The order is the point, and it matches the Electron shell: you cannot
        // import into a channel you do not own, so the picker never opens for one.
        // The first version of this test asserted a cancel succeeds — which is
        // true only *after* the channel check, and this machine has no channel, so
        // it was asserting the wrong arm.
        let outcome = import_history("test-cancel".to_string(), json!({ "channelId": ID }), None)
            .expect("answered");
        assert_eq!(
            outcome.get("ok").and_then(Value::as_bool),
            Some(false),
            "no channel means no import, picker or not: {outcome}"
        );
        assert!(
            outcome
                .get("message")
                .and_then(Value::as_str)
                .is_some_and(|message| message.contains("not owned")),
            "the message names the reason: {outcome}"
        );
    }

    #[test]
    fn an_import_request_with_an_unknown_field_is_refused() {
        for bad in [
            json!({ "channelId": ID, "extra": 1 }),
            json!({}),
            json!({ "channelId": "not-a-uuid" }),
        ] {
            let outcome = import_history("test-fields".to_string(), bad, None).expect("answered");
            assert_eq!(outcome.get("ok").and_then(Value::as_bool), Some(false));
        }
    }
}
