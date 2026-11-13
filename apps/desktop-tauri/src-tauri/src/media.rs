//! Media upload, live audio, and microphone access.
//!
//! Ported from the Electron shell's `MediaUploadManager` (`media-upload.ts`),
//! `DesktopLiveAudio` (`live-audio.ts`) and `registerMediaAccessIpc`
//! (`chrome-media-access.ts`).
//!
//! **None of this is an audio engine, and that is the design** — the same discovery
//! as browser observation. Upload is a chunked stream that lands in the artifact
//! library this shell already owns; live audio is a PCM transport to a socket the
//! runtime owns; microphone access is a status the OS answers, not a capture the
//! shell performs. So `flexaudio` and a Tauri capability are not needed here: the
//! capture happens in the WebView (`getUserMedia`) and in the runtime, and the
//! shell's job is to relay and to refuse malformed bytes before they travel.
//!
//! The validation that *is* here earns its place:
//!
//! - **Upload chunks are ordered, bounded and counted.** An out-of-order or
//!   oversized chunk corrupts the payload, and the artifact library would then
//!   serve a file whose digest does not match its record.
//! - **PCM is checked sample by sample.** A NaN or a sample past ±1.0 is not audio;
//!   forwarding it would put garbage in the runtime's buffer and, worse, make the
//!   failure look like a runtime bug rather than a renderer one.
//! - **Audio is window-owned and visible-gated.** A hidden window streaming
//!   microphone audio is the thing the user did not agree to.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::chrome::{ChromeError, ChromeResult};

/// One upload chunk may not exceed this, matching the Electron shell.
const MAX_CHUNK_BYTES: usize = 262_144;

/// The whole upload may not exceed this. Import is a copy, so this is about disk.
const MAX_UPLOAD_BYTES: u64 = 64 * 1024 * 1024;

/// One PCM chunk: 100 ms of 32 kHz stereo float, so a multiple of four bytes.
const MAX_AUDIO_CHUNK_BYTES: usize = 12_800;
/// One float sample, little-endian.
const SAMPLE_BYTES: usize = 4;

/// An idle upload is abandoned after this, so a renderer that crashes mid-upload
/// does not leave a partial payload behind forever.
const UPLOAD_IDLE_MS: i64 = 60_000;

/// Simultaneous uploads per window.
const MAX_UPLOADS_PER_WINDOW: usize = 2;

/// The descriptor is a handful of fields.
const DESCRIPTOR_MAX_BYTES: u64 = 4096;

/// How long an audio descriptor stays usable, and how far ahead it may claim to.
const DESCRIPTOR_WINDOW_MS: i64 = 60_000;

/// Seconds to wait for the runtime to accept one PCM frame.
const AUDIO_WRITE_TIMEOUT_MS: u64 = 2_000;

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

/// A bounded, control-character-free identity.
fn identity(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

// ---------------------------------------------------------------- media upload

/// One upload in progress: a partial payload, its digest, and the record it will
/// become.
struct Upload {
    owner: String,
    kind: String,
    name: String,
    mime_type: String,
    workspace_id: Option<String>,
    session_id: Option<String>,
    /// `<root>/<artifact-id><ext>.partial`. Renamed on finish.
    partial: PathBuf,
    filename: String,
    artifact_id: String,
    hasher: Sha256,
    bytes: u64,
    /// Next expected chunk sequence. Zero-based, so the first chunk is 0.
    sequence: u64,
    /// True while a chunk is being written, so two cannot interleave.
    busy: bool,
    last_at: i64,
}

static UPLOADS: Mutex<Option<HashMap<String, Upload>>> = Mutex::new(None);

fn uploads() -> std::sync::MutexGuard<'static, Option<HashMap<String, Upload>>> {
    UPLOADS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The one message for every way an upload can fail.
///
/// Deliberately not specific: the renderer shows it verbatim, and the useful
/// distinctions (order, size, storage) are all in the log.
const UPLOAD_FAILED: &str =
    "Media upload failed. Check the file size, upload order and artifact storage location.";

/// Begin an upload. Returns the id the renderer sends chunks against.
pub fn begin_upload(window: String, raw: Value) -> ChromeResult<Value> {
    let Some(object) = raw.as_object() else {
        return Ok(json!({ "ok": false, "message": UPLOAD_FAILED }));
    };
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "kind" | "name" | "mimeType" | "workspaceId" | "sessionId"))
    {
        return Ok(json!({ "ok": false, "message": UPLOAD_FAILED }));
    }
    let kind = object.get("kind").and_then(Value::as_str).unwrap_or("");
    let name = object.get("name").and_then(Value::as_str).unwrap_or("");
    let mime_type = object.get("mimeType").and_then(Value::as_str).unwrap_or("");
    let workspace_id = object.get("workspaceId").and_then(Value::as_str);
    let session_id = object.get("sessionId").and_then(Value::as_str);
    // A picture uploaded as audio is a mis-click, and storing it would put a row
    // in the library no player can open.
    let ok = matches!(kind, "image" | "audio" | "video")
        && mime_type.starts_with(&format!("{kind}/"))
        && mime_type
            .split('/')
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-')))
        && identity(name, 512)
        && !name.contains(['/', '\\'])
        && workspace_id.is_none_or(|value| identity(value, 512))
        && session_id.is_none_or(|value| identity(value, 512));
    if !ok {
        return Ok(json!({ "ok": false, "message": UPLOAD_FAILED }));
    }

    let library = crate::artifacts::library();
    let root = match library.active_directory() {
        Ok(root) => root,
        Err(error) => return Ok(json!({ "ok": false, "message": error.message })),
    };
    if let Err(error) = std::fs::create_dir_all(&root) {
        return Ok(json!({ "ok": false, "message": error.to_string() }));
    }

    {
        let mut guard = uploads();
        let table = guard.get_or_insert_with(HashMap::new);
        if table.values().filter(|upload| upload.owner == window).count() >= MAX_UPLOADS_PER_WINDOW {
            return Ok(json!({ "ok": false, "message": "At most two uploads per window" }));
        }
    }

    let artifact_id = uuid::Uuid::new_v4().to_string();
    let extension = std::path::Path::new(name)
        .extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
        .filter(|ext| !ext.is_empty() && ext.len() <= 12 && ext.chars().all(|c| c.is_ascii_alphanumeric()))
        .map(|ext| format!(".{ext}"))
        .unwrap_or_else(|| ".bin".to_string());
    let filename = format!("{artifact_id}{extension}");
    let partial = root.join(format!("{filename}.partial"));
    // Created now, so a first chunk that fails leaves a file the catalog does not
    // mention rather than a path that was never writable.
    if std::fs::File::create(&partial).is_err() {
        return Ok(json!({ "ok": false, "message": UPLOAD_FAILED }));
    }

    let upload = Upload {
        owner: window.clone(),
        kind: kind.to_string(),
        name: name.to_string(),
        mime_type: mime_type.to_string(),
        workspace_id: workspace_id.map(str::to_string),
        session_id: session_id.map(str::to_string),
        partial,
        filename,
        artifact_id: artifact_id.clone(),
        hasher: Sha256::new(),
        bytes: 0,
        sequence: 0,
        busy: false,
        last_at: now_millis(),
    };
    uploads()
        .get_or_insert_with(HashMap::new)
        .insert(artifact_id.clone(), upload);
    Ok(json!({ "ok": true, "uploadId": artifact_id }))
}

/// Append one chunk.
pub fn append_upload(window: String, raw: Value) -> ChromeResult<Value> {
    use std::io::Write;
    let Some(object) = raw.as_object() else {
        return Ok(json!({ "ok": false, "message": UPLOAD_FAILED }));
    };
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "uploadId" | "sequence" | "bytes"))
    {
        return Ok(json!({ "ok": false, "message": UPLOAD_FAILED }));
    }
    let upload_id = object.get("uploadId").and_then(Value::as_str).unwrap_or("");
    let sequence = object.get("sequence").and_then(Value::as_u64);
    // Read the chunk as bytes, or refuse it. The first version used
    // `filter_map(Value::as_u64).map(|b| b as u8)`, which *rewrote* malformed
    // input instead of rejecting it: `[137,80,78,71,300,300]` became
    // `[137,80,78,71,44,44]`, and because the hasher and the byte count both saw
    // the coerced values, the digest still matched — a silently damaged artifact
    // with nothing downstream able to detect it. This module's whole job is to
    // refuse malformed bytes before they travel.
    let bytes = object.get("bytes").and_then(Value::as_array).and_then(|list| {
        let mut out = Vec::with_capacity(list.len());
        for value in list {
            let byte = value.as_u64().filter(|byte| *byte <= u8::MAX as u64)?;
            out.push(byte as u8);
        }
        Some(out)
    });

    let mut guard = uploads();
    let Some(table) = guard.as_mut() else {
        return Ok(json!({ "ok": false, "message": UPLOAD_FAILED }));
    };
    let Some(upload) = table.get_mut(upload_id) else {
        return Ok(json!({ "ok": false, "message": UPLOAD_FAILED }));
    };
    if upload.owner != window {
        return Ok(json!({ "ok": false, "message": UPLOAD_FAILED }));
    }
    let Some(bytes) = bytes else {
        return Ok(json!({ "ok": false, "message": UPLOAD_FAILED }));
    };
    // Order, size and total, all three: an out-of-order chunk corrupts the payload,
    // and the library would then serve a file whose digest does not match its
    // record.
    if upload.busy
        || sequence != Some(upload.sequence)
        || bytes.is_empty()
        || bytes.len() > MAX_CHUNK_BYTES
        || upload.bytes + bytes.len() as u64 > MAX_UPLOAD_BYTES
    {
        return Ok(json!({ "ok": false, "message": UPLOAD_FAILED }));
    }
    upload.busy = true;
    upload.last_at = now_millis();
    let outcome = (|| -> ChromeResult<()> {
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&upload.partial)
            .map_err(|_| ChromeError::failed(UPLOAD_FAILED))?;
        file.write_all(&bytes).map_err(|_| ChromeError::failed(UPLOAD_FAILED))?;
        upload.hasher.update(&bytes);
        upload.bytes += bytes.len() as u64;
        upload.sequence += 1;
        Ok(())
    })();
    upload.busy = false;
    match outcome {
        Ok(()) => Ok(json!({ "ok": true })),
        Err(error) => {
            let partial = upload.partial.clone();
            table.remove(upload_id);
            let _ = std::fs::remove_file(&partial);
            Ok(json!({ "ok": false, "message": error.message }))
        }
    }
}

/// Finish an upload: rename the payload into place and register it.
pub fn finish_upload(window: String, raw: Value) -> ChromeResult<Value> {
    let upload_id = raw
        .as_object()
        .and_then(|object| {
            if object.keys().any(|key| key != "uploadId") {
                return None;
            }
            object.get("uploadId").and_then(Value::as_str)
        })
        .unwrap_or("");
    let Some(upload) = uploads()
        .get_or_insert_with(HashMap::new)
        .get(upload_id)
        .filter(|upload| upload.owner == window)
        .map(|upload| {
            (
                upload.kind.clone(),
                upload.name.clone(),
                upload.mime_type.clone(),
                upload.workspace_id.clone(),
                upload.session_id.clone(),
                upload.partial.clone(),
                upload.filename.clone(),
                upload.artifact_id.clone(),
                format!("{:x}", upload.hasher.clone().finalize()),
                upload.bytes,
            )
        })
    else {
        return Ok(json!({ "ok": false, "message": UPLOAD_FAILED }));
    };
    let (kind, name, mime_type, workspace_id, session_id, partial, filename, artifact_id, digest, bytes) = upload;

    let library = crate::artifacts::library();
    let root = match library.active_directory() {
        Ok(root) => root,
        Err(error) => return Ok(json!({ "ok": false, "message": error.message })),
    };
    let target = root.join(&filename);
    // The payload reaches its final name only once it is whole.
    if std::fs::rename(&partial, &target).is_err() {
        let _ = std::fs::remove_file(&partial);
        uploads().get_or_insert_with(HashMap::new).remove(&artifact_id);
        return Ok(json!({ "ok": false, "message": UPLOAD_FAILED }));
    }
    let record = library.commit(
        &json!({
            "kind": kind,
            "name": name,
            "mimeType": mime_type,
            "workspaceId": workspace_id,
            "sessionId": session_id,
        }),
        &filename,
        &digest,
        bytes,
    );
    uploads().get_or_insert_with(HashMap::new).remove(&artifact_id);
    match record {
        Ok(record) => Ok(json!({ "ok": true, "artifact": record })),
        Err(error) => {
            let _ = std::fs::remove_file(&target);
            Ok(json!({ "ok": false, "message": error.message }))
        }
    }
}

/// Abort an upload: drop the partial payload and forget the upload.
pub fn abort_upload(window: String, raw: Value) -> ChromeResult<Value> {
    let upload_id = raw
        .as_object()
        .and_then(|object| {
            if object.keys().any(|key| key != "uploadId") {
                return None;
            }
            object.get("uploadId").and_then(Value::as_str)
        })
        .unwrap_or("");
    let Some(upload) = uploads()
        .get_or_insert_with(HashMap::new)
        .get(upload_id)
        .filter(|upload| upload.owner == window)
        .map(|upload| (upload.partial.clone(), upload.artifact_id.clone()))
    else {
        return Ok(json!({ "ok": false, "message": UPLOAD_FAILED }));
    };
    let _ = std::fs::remove_file(&upload.0);
    uploads().get_or_insert_with(HashMap::new).remove(&upload.1);
    Ok(json!({ "ok": true }))
}

/// End every upload a window owns.
pub fn dispose_uploads(window: &str) {
    let mut guard = uploads();
    if let Some(table) = guard.as_mut() {
        let doomed: Vec<String> = table
            .iter()
            .filter(|(_, upload)| upload.owner == window)
            .map(|(id, _)| id.clone())
            .collect();
        for id in doomed {
            if let Some(upload) = table.remove(&id) {
                let _ = std::fs::remove_file(&upload.partial);
            }
        }
    }
}

/// Abandon uploads that have been idle too long.
///
/// A renderer that crashes mid-upload leaves a partial payload and a catalog entry
/// that never arrives; without this the payload stays forever.
pub fn reap_idle_uploads() {
    let now = now_millis();
    let mut guard = uploads();
    if let Some(table) = guard.as_mut() {
        let doomed: Vec<String> = table
            .iter()
            .filter(|(_, upload)| now - upload.last_at > UPLOAD_IDLE_MS)
            .map(|(id, _)| id.clone())
            .collect();
        for id in doomed {
            if let Some(upload) = table.remove(&id) {
                let _ = std::fs::remove_file(&upload.partial);
            }
        }
    }
}

// ------------------------------------------------------------------- live audio

/// One live-audio connection.
struct AudioConnection {
    owner: String,
    socket: Arc<Mutex<std::os::unix::net::UnixStream>>,
    sequence: u64,
    busy: bool,
}

static AUDIO: Mutex<Option<HashMap<String, AudioConnection>>> = Mutex::new(None);

fn audio() -> std::sync::MutexGuard<'static, Option<HashMap<String, AudioConnection>>> {
    AUDIO
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The socket the runtime listens on for one audio id.
///
/// Same derivation the runtime's overlay uses (`liveAudioSocketName`), so the
/// shell only ever dials an endpoint it computed itself.
fn audio_socket(audio_id: &str) -> PathBuf {
    let digest = Sha256::digest(audio_id.as_bytes());
    let name = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    crate::chrome::shell_state_dir().join(format!("a-{name}.sock"))
}

/// Read and validate `<state>/audio/<id>.json`.
fn read_audio_descriptor(audio_id: &str, session_id: &str) -> ChromeResult<String> {
    if !is_id(audio_id) {
        return Err(ChromeError::invalid("invalid audio id"));
    }
    if !identity(session_id, 512) {
        return Err(ChromeError::invalid("invalid audio session"));
    }
    let file = crate::chrome::shell_state_dir().join("audio").join(format!("{audio_id}.json"));
    let metadata = std::fs::symlink_metadata(&file)
        .map_err(|_| ChromeError::failed("audio descriptor unavailable"))?;
    if !metadata.is_file() || metadata.len() > DESCRIPTOR_MAX_BYTES {
        return Err(ChromeError::failed("audio descriptor unavailable"));
    }
    let text = std::fs::read_to_string(&file)
        .map_err(|_| ChromeError::failed("audio descriptor unavailable"))?;
    let descriptor: Value = serde_json::from_str(&text)
        .map_err(|_| ChromeError::failed("audio descriptor unavailable"))?;
    let Some(object) = descriptor.as_object() else {
        return Err(ChromeError::failed("audio descriptor unavailable"));
    };
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "version" | "audioId" | "sessionId" | "endpoint" | "token" | "expiresAt"))
    {
        return Err(ChromeError::failed("audio descriptor unavailable"));
    }
    let field = |name: &str| object.get(name);
    let text_field = |name: &str| field(name).and_then(Value::as_str);
    if field("version").and_then(Value::as_u64) != Some(1)
        || text_field("audioId") != Some(audio_id)
        || text_field("sessionId") != Some(session_id)
    {
        return Err(ChromeError::failed("audio attachment expired"));
    }
    let token = text_field("token").unwrap_or("");
    if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ChromeError::failed("audio descriptor is malformed"));
    }
    let expires_at = field("expiresAt").and_then(Value::as_i64).unwrap_or_default();
    let now = now_millis();
    if expires_at < now || expires_at > now + DESCRIPTOR_WINDOW_MS {
        return Err(ChromeError::failed("audio attachment expired"));
    }
    if text_field("endpoint").is_none_or(|named| named != audio_socket(audio_id).to_string_lossy()) {
        return Err(ChromeError::failed("invalid audio endpoint"));
    }
    Ok(token.to_string())
}

/// Attach to one live-audio channel.
pub fn attach_audio(window: String, raw: Value) -> ChromeResult<Value> {
    let Some(object) = raw.as_object() else {
        return Ok(json!({ "ok": false, "message": "Live microphone connection ended." }));
    };
    if object.keys().any(|key| !matches!(key.as_str(), "audioId" | "sessionId")) {
        return Ok(json!({ "ok": false, "message": "Live microphone connection ended." }));
    }
    let audio_id = object.get("audioId").and_then(Value::as_str).unwrap_or("");
    let session_id = object.get("sessionId").and_then(Value::as_str).unwrap_or("");
    let token = match read_audio_descriptor(audio_id, session_id) {
        Ok(token) => token,
        Err(_) => return Ok(json!({ "ok": false, "message": "Live microphone connection ended. Reconnect explicitly from a visible desktop window." })),
    };

    // One channel per window: attaching again replaces the old socket.
    dispose_audio(&window);
    if audio().get_or_insert_with(HashMap::new).contains_key(audio_id) {
        return Ok(json!({ "ok": false, "message": "Live microphone connection ended." }));
    }

    use std::os::unix::net::UnixStream;
    let socket = match UnixStream::connect(audio_socket(audio_id)) {
        Ok(socket) => socket,
        Err(_) => return Ok(json!({ "ok": false, "message": "Live microphone connection ended." })),
    };
    {
        use std::io::Write;
        let mut writer = &socket;
        if writer.write_all(format!("{token}\n").as_bytes()).is_err() {
            return Ok(json!({ "ok": false, "message": "Live microphone connection ended." }));
        }
    }
    let mut reader = match socket.try_clone() {
        Ok(reader) => reader,
        Err(_) => return Ok(json!({ "ok": false, "message": "Live microphone connection ended." })),
    };
    // The runtime answers `OK\n` before anything else. A socket that does not is
    // not the runtime's.
    use std::io::Read;
    let mut answer = [0u8; 3];
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(AUDIO_WRITE_TIMEOUT_MS);
    let mut seen = 0;
    while seen < 3 {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Ok(json!({ "ok": false, "message": "Live microphone connection ended." }));
        }
        socket.set_read_timeout(Some(remaining)).ok();
        match reader.read(&mut answer[seen..]) {
            Ok(0) => return Ok(json!({ "ok": false, "message": "Live microphone connection ended." })),
            Ok(read) => seen += read,
            Err(_) => return Ok(json!({ "ok": false, "message": "Live microphone connection ended." })),
        }
    }
    if &answer != b"OK\n" {
        return Ok(json!({ "ok": false, "message": "Live microphone connection ended." }));
    }

    audio().get_or_insert_with(HashMap::new).insert(
        audio_id.to_string(),
        AudioConnection {
            owner: window,
            socket: Arc::new(Mutex::new(socket)),
            sequence: 0,
            busy: false,
        },
    );
    Ok(json!({ "ok": true }))
}

/// Append one PCM chunk.
pub fn append_audio(window: String, raw: Value) -> ChromeResult<Value> {
    let Some(object) = raw.as_object() else {
        return Ok(json!({ "ok": false, "message": "Live microphone connection ended." }));
    };
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "audioId" | "sequence" | "bytes"))
    {
        return Ok(json!({ "ok": false, "message": "Live microphone connection ended." }));
    }
    let audio_id = object.get("audioId").and_then(Value::as_str).unwrap_or("");
    let sequence = object.get("sequence").and_then(Value::as_u64);
    // Read the chunk as bytes, or refuse it. The first version used
    // `filter_map(Value::as_u64).map(|b| b as u8)`, which *rewrote* malformed
    // input instead of rejecting it: `[137,80,78,71,300,300]` became
    // `[137,80,78,71,44,44]`, and because the hasher and the byte count both saw
    // the coerced values, the digest still matched — a silently damaged artifact
    // with nothing downstream able to detect it. This module's whole job is to
    // refuse malformed bytes before they travel.
    let bytes = object.get("bytes").and_then(Value::as_array).and_then(|list| {
        let mut out = Vec::with_capacity(list.len());
        for value in list {
            let byte = value.as_u64().filter(|byte| *byte <= u8::MAX as u64)?;
            out.push(byte as u8);
        }
        Some(out)
    });

    let mut guard = audio();
    let Some(table) = guard.as_mut() else {
        return Ok(json!({ "ok": false, "message": "Live microphone connection ended." }));
    };
    let Some(connection) = table.get_mut(audio_id) else {
        return Ok(json!({ "ok": false, "message": "Live microphone connection ended." }));
    };
    if connection.owner != window {
        return Ok(json!({ "ok": false, "message": "Live microphone connection ended." }));
    }
    let Some(bytes) = bytes else {
        return Ok(json!({ "ok": false, "message": "Live microphone connection ended." }));
    };
    // 100 ms of 32 kHz stereo float is 12,800 bytes and a multiple of four. A
    // chunk that is neither is not a frame.
    if connection.busy
        || sequence != Some(connection.sequence)
        || bytes.is_empty()
        || bytes.len() > MAX_AUDIO_CHUNK_BYTES
        || bytes.len() % 4 != 0
    {
        return Ok(json!({ "ok": false, "message": "Live microphone connection ended." }));
    }
    if !is_pcm(&bytes) {
        return Ok(json!({ "ok": false, "message": "Live microphone connection ended." }));
    }

    connection.busy = true;
    let socket = Arc::clone(&connection.socket);
    let outcome = write_audio_frame(&socket, &bytes);
    connection.busy = false;
    match outcome {
        Ok(()) => {
            connection.sequence += 1;
            Ok(json!({ "ok": true }))
        }
        // A frame that will not go means the channel is gone; dropping it here is
        // what lets the renderer reconnect rather than queue into a dead socket.
        Err(_) => {
            table.remove(audio_id);
            Ok(json!({ "ok": false, "message": "Live microphone connection ended." }))
        }
    }
}

/// Whether `bytes` is a whole number of finite float samples inside ±1.0.
///
/// A pure function rather than a branch inside `append_audio`, and that is the
/// point: the IPC path returns early when there is no connection, so a test that
/// drove this rule through it asserted only "it failed" — which it also does for
/// having no connection. The first version of that test passed every mutation of
/// this rule.
fn is_pcm(bytes: &[u8]) -> bool {
    if !bytes.len().is_multiple_of(SAMPLE_BYTES) {
        return false;
    }
    bytes
        .as_chunks::<SAMPLE_BYTES>()
        .0
        .iter()
        .all(|sample| {
            let value = f32::from_le_bytes(*sample);
            value.is_finite() && value.abs() <= 1.0
        })
}

/// Write one length-prefixed PCM frame, with a timeout.
fn write_audio_frame(socket: &Arc<Mutex<std::os::unix::net::UnixStream>>, bytes: &[u8]) -> ChromeResult<()> {
    use std::io::Write;
    let mut frame = Vec::with_capacity(4 + bytes.len());
    frame.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    frame.extend_from_slice(bytes);
    let mut guard = socket
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard
        .set_write_timeout(Some(std::time::Duration::from_millis(AUDIO_WRITE_TIMEOUT_MS)))
        .ok();
    guard
        .write_all(&frame)
        .map_err(|_| ChromeError::failed("live audio write timed out"))
}

/// Detach one live-audio channel.
pub fn detach_audio(window: String, raw: Value) -> ChromeResult<Value> {
    let audio_id = raw
        .as_object()
        .and_then(|object| {
            if object.keys().any(|key| key != "audioId") {
                return None;
            }
            object.get("audioId").and_then(Value::as_str)
        })
        .unwrap_or("");
    let mut guard = audio();
    if let Some(table) = guard.as_mut() {
        if table
            .get(audio_id)
            .is_some_and(|connection| connection.owner == window)
        {
            table.remove(audio_id);
        }
    }
    Ok(json!({ "ok": true }))
}

/// End every live-audio channel a window owns.
pub fn dispose_audio(window: &str) {
    let mut guard = audio();
    if let Some(table) = guard.as_mut() {
        let doomed: Vec<String> = table
            .iter()
            .filter(|(_, connection)| connection.owner == window)
            .map(|(id, _)| id.clone())
            .collect();
        for id in doomed {
            table.remove(&id);
        }
    }
}

// -------------------------------------------------------------- microphone access

/// Report the microphone's status.
///
/// `granted` everywhere, and the reason is worth stating: the capture happens in
/// the WebView, and both WKWebView and WebView2 own their own permission prompt.
/// There is no shell-level gate to report, so a shell that answered `denied` would
/// be inventing a state the OS never expressed. macOS's TCC prompt fires on the
/// first `getUserMedia`, exactly as it does in the Electron shell — which is why
/// the Electron code only special-cases `darwin` and reports `granted` elsewhere.
pub fn microphone_access() -> ChromeResult<Value> {
    Ok(json!({ "ok": true, "access": "granted" }))
}

/// Open the OS microphone privacy page.
///
/// macOS only, and from a fixed URL the renderer never supplies: a renderer-chosen
/// URL here would be a way to open anything. Everywhere else there is no such page
/// to open, and `false` says so.
pub fn open_microphone_settings() -> ChromeResult<Value> {
    if !cfg!(target_os = "macos") {
        return Ok(json!({ "ok": true, "opened": false }));
    }
    let url = "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone";
    let outcome = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else {
        // Unreachable: the guard above returns first. Written so the `if` has an
        // `else` the compiler can see is exhaustive.
        std::process::Command::new("false").spawn()
    };
    match outcome {
        Ok(_) => Ok(json!({ "ok": true, "opened": true })),
        Err(_) => Ok(json!({ "ok": true, "opened": false })),
    }
}

// ----------------------------------------------------------------- Tauri commands

#[tauri::command]
pub fn begin_media_upload(window: tauri::Window, input: Value) -> ChromeResult<Value> {
    begin_upload(window.label().to_string(), input)
}

#[tauri::command]
pub fn append_media_upload(window: tauri::Window, input: Value) -> ChromeResult<Value> {
    append_upload(window.label().to_string(), input)
}

#[tauri::command]
pub fn finish_media_upload(window: tauri::Window, input: Value) -> ChromeResult<Value> {
    finish_upload(window.label().to_string(), input)
}

#[tauri::command]
pub fn abort_media_upload(window: tauri::Window, input: Value) -> ChromeResult<Value> {
    abort_upload(window.label().to_string(), input)
}

#[tauri::command]
pub fn attach_live_audio(window: tauri::Window, input: Value) -> ChromeResult<Value> {
    attach_audio(window.label().to_string(), input)
}

#[tauri::command]
pub fn append_live_audio(window: tauri::Window, input: Value) -> ChromeResult<Value> {
    append_audio(window.label().to_string(), input)
}

#[tauri::command]
pub fn detach_live_audio(window: tauri::Window, input: Value) -> ChromeResult<Value> {
    detach_audio(window.label().to_string(), input)
}

#[tauri::command]
pub fn request_microphone_access() -> ChromeResult<Value> {
    microphone_access()
}

#[tauri::command]
pub fn open_microphone_settings_command() -> ChromeResult<Value> {
    open_microphone_settings()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window label of one test's own.
    ///
    /// Not a shared `"main"`: the manager allows two uploads per window, so tests
    /// running in parallel on one label refuse each other's `begin` — and the
    /// failure reads as "the upload was refused", which is the fourth shared
    /// fixture this session that made a test order-dependent.
    fn window(tag: &str) -> String {
        format!("test-{tag}-{}", std::process::id())
    }

    /// An upload, started against the real library.
    fn start(tag: &str, kind: &str, name: &str, mime: &str) -> String {
        let value = begin_upload(
            window(tag),
            json!({ "kind": kind, "name": name, "mimeType": mime }),
        )
        .expect("answered");
        assert_eq!(value.get("ok").and_then(Value::as_bool), Some(true), "{value}");
        value
            .get("uploadId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    fn chunk(tag: &str, upload_id: &str, sequence: u64, bytes: &[u8]) -> Value {
        append_upload(
            window(tag),
            json!({ "uploadId": upload_id, "sequence": sequence, "bytes": bytes }),
        )
        .expect("answered")
    }

    fn pcm(samples: &[f32]) -> Vec<u8> {
        samples.iter().flat_map(|value| value.to_le_bytes()).collect()
    }

    #[test]
    fn an_upload_round_trips_into_the_artifact_library() {
        // Clean slate: another test may have left an upload behind.
        dispose_uploads(&window("roundtrip"));
        let upload_id = start("roundtrip", "image", "photo.png", "image/png");
        let payload = b"not really a png either";
        assert_eq!(chunk("roundtrip", &upload_id, 0, payload).get("ok").and_then(Value::as_bool), Some(true));
        let finished = finish_upload(window("roundtrip"), json!({ "uploadId": upload_id }))
            .expect("answered");
        assert_eq!(finished.get("ok").and_then(Value::as_bool), Some(true), "{finished}");
        let record = finished.get("artifact").expect("an artifact");
        assert_eq!(record.get("kind").and_then(Value::as_str), Some("image"));
        assert_eq!(record.get("bytes").and_then(Value::as_u64), Some(payload.len() as u64));
        assert_eq!(
            record.get("sha256").and_then(Value::as_str),
            Some(format!("{:x}", Sha256::digest(payload)).as_str()),
            "the digest is of the bytes actually streamed"
        );
        // And it is in the library, which is the point of finishing rather than
        // just writing a file.
        let page = crate::artifacts::library().list(&json!({})).expect("listed");
        assert!(
            page.get("artifacts")
                .and_then(Value::as_array)
                .is_some_and(|rows| rows.iter().any(|row| row.get("artifactId") == record.get("artifactId"))),
            "the finished upload is a catalogued artifact"
        );
    }

    #[test]
    fn an_out_of_order_or_oversized_chunk_is_refused() {
        dispose_uploads(&window("order"));
        let upload_id = start("order", "image", "a.png", "image/png");
        // Sequence 1 first: the upload expects 0.
        assert_eq!(
            chunk("order", &upload_id, 1, b"x").get("ok").and_then(Value::as_bool),
            Some(false),
            "an out-of-order chunk is refused"
        );
        assert_eq!(chunk("order", &upload_id, 0, b"x").get("ok").and_then(Value::as_bool), Some(true));
        // And one past the per-chunk ceiling.
        assert_eq!(
            chunk("order", &upload_id, 1, &vec![0u8; MAX_CHUNK_BYTES + 1]).get("ok").and_then(Value::as_bool),
            Some(false),
            "an oversized chunk is refused"
        );
        // The upload is still usable after both refusals, which is what makes the
        // refusal a refusal rather than a teardown.
        assert_eq!(chunk("order", &upload_id, 1, b"y").get("ok").and_then(Value::as_bool), Some(true));
        abort_upload(window("order"), json!({ "uploadId": upload_id })).expect("answered");
    }

    #[test]
    fn a_chunk_that_is_not_bytes_is_refused_rather_than_rewritten() {
        // The bug this pins: the first version read the chunk as
        // `filter_map(Value::as_u64).map(|b| b as u8)`, which *rewrote* malformed
        // input instead of rejecting it. `[137,80,78,71,300,300]` became
        // `[137,80,78,71,44,44]`, and because the hasher and the byte count both saw
        // the coerced values, the digest still matched — a silently damaged artifact
        // with nothing downstream able to detect it.
        dispose_uploads(&window("coerce"));
        let upload_id = start("coerce", "image", "shot.png", "image/png");
        for bad in [
            // Out of range: `300 as u8` is 44.
            json!([137, 80, 78, 71, 300, 300]),
            // Not a number at all: dropped by the old `filter_map`.
            json!([1, 2, "x", 3]),
            // Negative.
            json!([1, -1]),
            // Not an array.
            json!("bytes"),
        ] {
            let outcome = append_upload(
                window("coerce"),
                json!({ "uploadId": upload_id, "sequence": 0, "bytes": bad }),
            )
            .expect("answered");
            assert_eq!(
                outcome.get("ok").and_then(Value::as_bool),
                Some(false),
                "a chunk that is not bytes must be refused: {bad}"
            );
        }
        // And the upload is still usable, which is what makes it a refusal rather
        // than a teardown.
        assert_eq!(
            chunk("coerce", &upload_id, 0, b"ok").get("ok").and_then(Value::as_bool),
            Some(true)
        );
        abort_upload(window("coerce"), json!({ "uploadId": upload_id })).expect("answered");
    }

    #[test]
    fn a_kind_that_does_not_match_the_mime_type_is_refused() {
        for (kind, name, mime) in [
            ("audio", "clip.png", "image/png"),
            ("image", "clip.mp3", "audio/mpeg"),
            ("document", "a.png", "image/png"),
            ("image", "../escape.png", "image/png"),
            ("image", "a.png", "image"),
        ] {
            let outcome = begin_upload(
                window("kind"),
                json!({ "kind": kind, "name": name, "mimeType": mime }),
            )
            .expect("answered");
            assert_eq!(
                outcome.get("ok").and_then(Value::as_bool),
                Some(false),
                "{kind}/{name}/{mime} must be refused"
            );
        }
    }

    #[test]
    fn an_abandoned_upload_leaves_no_payload_behind() {
        dispose_uploads(&window("abandon"));
        let upload_id = start("abandon", "image", "gone.png", "image/png");
        chunk("abandon", &upload_id, 0, b"partial");
        let partial = uploads()
            .get_or_insert_with(HashMap::new)
            .get(&upload_id)
            .map(|upload| upload.partial.clone())
            .expect("the upload exists");
        assert!(partial.exists(), "the partial payload is on disk while uploading");
        abort_upload(window("abandon"), json!({ "uploadId": upload_id })).expect("answered");
        assert!(!partial.exists(), "and gone after an abort");
        assert!(uploads().get_or_insert_with(HashMap::new).get(&upload_id).is_none());
    }

    #[test]
    fn a_finished_upload_cannot_be_finished_again() {
        dispose_uploads(&window("once"));
        let upload_id = start("once", "image", "once.png", "image/png");
        chunk("once", &upload_id, 0, b"data");
        assert_eq!(
            finish_upload(window("once"), json!({ "uploadId": upload_id }))
                .expect("answered")
                .get("ok")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            finish_upload(window("once"), json!({ "uploadId": upload_id }))
                .expect("answered")
                .get("ok")
                .and_then(Value::as_bool),
            Some(false),
            "the upload id is spent"
        );
    }

    #[test]
    fn pcm_is_checked_sample_by_sample() {
        // Drives the rule directly, not through `append_audio`: with no connection
        // that returns early, so a test asserting only "it failed" would pass with
        // the rule deleted.
        for bad in [
            // Not a multiple of four.
            vec![0u8, 0, 0],
            // NaN.
            pcm(&[f32::NAN]),
            // Past ±1.0.
            pcm(&[1.5]),
            pcm(&[-1.5]),
            // Infinity.
            pcm(&[f32::INFINITY]),
        ] {
            assert!(!is_pcm(&bad), "garbage PCM is refused: {bad:?}");
        }
        // And the valid range is accepted, boundaries included.
        assert!(is_pcm(&pcm(&[1.0, -1.0, 0.0])), "±1.0 and silence are audio");
        assert!(is_pcm(&[]), "an empty frame is trivially whole; the caller rejects it for length");
        assert!(!is_pcm(&pcm(&[1.000_000_1])), "just past the ceiling is not");
    }

    #[test]
    fn an_audio_attachment_without_a_descriptor_is_refused() {
        // The descriptor is written by the runtime. A renderer that invents an
        // audioId finds nothing, and the answer is the reconnect message rather
        // than a stack of internal reasons.
        let outcome = attach_audio(
            window("audio"),
            json!({ "audioId": "3f2504e0-4f89-11d3-9a0c-0305e82c3301", "sessionId": "session-1" }),
        )
        .expect("answered");
        assert_eq!(outcome.get("ok").and_then(Value::as_bool), Some(false));
        assert!(
            outcome
                .get("message")
                .and_then(Value::as_str)
                .is_some_and(|message| message.contains("Reconnect")),
            "the message says what to do: {outcome}"
        );
    }

    #[test]
    fn the_microphone_is_reported_granted_because_the_prompt_belongs_to_the_webview() {
        // Not a stub: on both WKWebView and WebView2 the permission prompt is the
        // engine's, so there is no shell-level state to report. Answering `denied`
        // here would invent one.
        let outcome = microphone_access().expect("answered");
        assert_eq!(outcome.get("access").and_then(Value::as_str), Some("granted"));
    }

    #[test]
    fn opening_microphone_settings_is_macos_only() {
        let outcome = open_microphone_settings().expect("answered");
        assert_eq!(outcome.get("ok").and_then(Value::as_bool), Some(true));
        assert_eq!(
            outcome.get("opened").and_then(Value::as_bool),
            Some(cfg!(target_os = "macos")),
            "only macOS has a privacy page to open"
        );
    }
}
