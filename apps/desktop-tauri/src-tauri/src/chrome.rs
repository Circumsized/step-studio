//! The `ompStudioChrome` bridge: shell-level operations the renderer expects
//! from the desktop shell (M1.2c).
//!
//! Scope is deliberately narrow, and the boundary is a real one rather than a
//! to-do list. `apps/renderer/src/vite-env.d.ts:51-290` declares 139 entries;
//! the second half is the auto-updater (packaging, not a product surface), and
//! most of the first half is Host-backed — artifacts, media upload,
//! prediction, browser observation, service definitions. Those need the Host,
//! which this shell does not run, so they stay absent and the renderer draws
//! its honest unavailable state.
//!
//! What is here is what the shell itself owns: the OS it runs on, opening a
//! link, a system notification, the operator avatar, and the perf reporters.
//! Every entry below has a live call site in `apps/renderer/src`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::process::Command;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::Manager;
use tauri_plugin_dialog::DialogExt;

/// Rejection payload, shaped like the rest of the shell's IPC errors.
#[derive(Serialize, Debug)]
pub struct ChromeError {
    pub code: String,
    pub message: String,
}

impl ChromeError {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: "INVALID_ARGUMENT".to_string(),
            message: message.into(),
        }
    }

    pub(crate) fn failed(message: impl Into<String>) -> Self {
        Self {
            code: "CHROME_FAILED".to_string(),
            message: message.into(),
        }
    }
}

pub(crate) type ChromeResult<T> = Result<T, ChromeError>;

/// `<app_data>/step-studio`, the shell's own state directory.
///
/// Mirrors what the Electron host keeps under `%APPDATA%\omp-studio`; the name
/// is the only thing that changed.
pub(crate) fn state_dir(app: &tauri::AppHandle) -> ChromeResult<PathBuf> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|error| {
            ChromeError::failed(format!("cannot locate the app data directory: {error}"))
        })?
        .join("step-studio");
    std::fs::create_dir_all(&dir).map_err(|error| {
        ChromeError::failed(format!("cannot create {}: {error}", dir.display()))
    })?;
    Ok(dir)
}

fn profile_dir(app: &tauri::AppHandle) -> ChromeResult<PathBuf> {
    let dir = state_dir(app)?.join("profile");
    std::fs::create_dir_all(&dir).map_err(|error| {
        ChromeError::failed(format!("cannot create {}: {error}", dir.display()))
    })?;
    Ok(dir)
}

const AVATAR_MAX_BYTES: usize = 200_000;
const AVATAR_MIN_BYTES: usize = 24;
const AVATAR_EXTENSIONS: &[(&str, &str)] = &[
    ("image/jpeg", "jpg"),
    ("image/webp", "webp"),
    ("image/png", "png"),
];

/// The avatar file for one mime type. Clearing removes all three extensions,
/// because a saved `.png` must not survive a later `.jpg` save.
fn avatar_path(dir: &std::path::Path, mime: &str) -> ChromeResult<PathBuf> {
    let extension = AVATAR_EXTENSIONS
        .iter()
        .find(|(known, _)| *known == mime)
        .map(|(_, extension)| *extension)
        .ok_or_else(|| ChromeError::invalid(format!("unsupported avatar mime type: {mime}")))?;
    Ok(dir.join(format!("avatar.{extension}")))
}

#[tauri::command]
pub fn save_avatar(app: tauri::AppHandle, mime: String, bytes: Vec<u8>) -> ChromeResult<Value> {
    if bytes.len() > AVATAR_MAX_BYTES {
        return Err(ChromeError::invalid(format!(
            "avatar is {} bytes; the limit is {AVATAR_MAX_BYTES}",
            bytes.len()
        )));
    }
    if bytes.len() < AVATAR_MIN_BYTES {
        return Err(ChromeError::invalid("avatar is too small to be an image"));
    }
    let dir = profile_dir(&app)?;
    let path = avatar_path(&dir, &mime)?;
    std::fs::write(&path, &bytes)
        .map_err(|error| ChromeError::failed(format!("cannot write the avatar: {error}")))?;
    // Every other extension goes. `load_avatar` scans in a fixed order and
    // returns the first hit, so leaving a previous format on disk means saving
    // PNG over an existing JPG still loads the JPG — the user sees the avatar
    // they just replaced.
    for (known, _) in AVATAR_EXTENSIONS {
        if *known == mime {
            continue;
        }
        let stale = avatar_path(&dir, known)?;
        // A file that is not there is the common case, and a removal that fails
        // is not worth failing the save over: the write above already succeeded.
        let _ = std::fs::remove_file(stale);
    }
    Ok(json!({ "ok": true }))
}

#[tauri::command]
pub fn load_avatar(app: tauri::AppHandle) -> ChromeResult<Value> {
    let dir = profile_dir(&app)?;
    for (mime, _) in AVATAR_EXTENSIONS {
        let path = avatar_path(&dir, mime)?;
        if let Ok(bytes) = std::fs::read(&path) {
            return Ok(json!({ "mime": mime, "bytes": bytes }));
        }
    }
    Ok(Value::Null)
}

#[tauri::command]
pub fn clear_avatar(app: tauri::AppHandle) -> ChromeResult<()> {
    let dir = profile_dir(&app)?;
    for (mime, _) in AVATAR_EXTENSIONS {
        let path = avatar_path(&dir, mime)?;
        // A missing file is the goal, not an error.
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

/// Open an https link in the system browser.
///
/// The scheme is checked here rather than in the renderer because this is where
/// the command actually runs: a `file:` or `javascript:` URL would otherwise
/// reach the shell and be handed to the OS.
#[tauri::command]
pub fn open_url(url: String) -> ChromeResult<()> {
    if !url.starts_with("https://") {
        return Err(ChromeError::invalid("only https links can be opened"));
    }
    let outcome = if cfg!(target_os = "macos") {
        Command::new("open").arg(&url).spawn()
    } else if cfg!(target_os = "windows") {
        Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(&url)
            .spawn()
    } else {
        Command::new("xdg-open").arg(&url).spawn()
    };
    outcome.map_err(|error| ChromeError::failed(format!("cannot open the link: {error}")))?;
    Ok(())
}

/// Reveal a project's directory in the OS file manager.
///
/// The `workspaceId` is the project's absolute path — that is what the session
/// store's `cwd` carries and what a file tree matches against. It is validated
/// as an existing directory rather than trusted: this string arrives from the
/// renderer, and handing an arbitrary one to `xdg-open` would run whatever the
/// renderer asked for.
#[tauri::command]
pub fn open_project_directory(workspace_id: String) -> ChromeResult<()> {
    let path = project_dir(&workspace_id)?;
    // The path is passed as one argv element, never through a shell: a project
    // directory can contain anything, and `sh -c` would interpret it.
    let outcome = if cfg!(target_os = "macos") {
        Command::new("open").arg(path).spawn()
    } else if cfg!(target_os = "windows") {
        Command::new("explorer").arg(path).spawn()
    } else {
        Command::new("xdg-open").arg(path).spawn()
    };
    outcome
        .map_err(|error| ChromeError::failed(format!("cannot reveal the directory: {error}")))?;
    Ok(())
}

/// Open a project's directory in the system's default editor.
///
/// Same path, same validation. Reported as unavailable rather than attempted
/// where the platform has no such notion — `xdg-open` opens a *viewer*, which on
/// most desktops is the file manager again, and calling that "open in editor"
/// would be a lie the user discovers by clicking.
#[tauri::command]
pub fn open_project_in_editor(workspace_id: String) -> ChromeResult<Value> {
    let path = project_dir(&workspace_id)?;
    // macOS names the editor; Windows and Linux have no portable one, so they
    // reveal instead and say which happened. The renderer shows the returned
    // `mode`, so a caller can tell "opened in an editor" from "revealed".
    let (mode, outcome) = if cfg!(target_os = "macos") {
        (
            "editor",
            Command::new("open").args(["-t"]).arg(path).spawn(),
        )
    } else if cfg!(target_os = "windows") {
        ("reveal", Command::new("explorer").arg(path).spawn())
    } else {
        ("reveal", Command::new("xdg-open").arg(path).spawn())
    };
    outcome.map_err(|error| ChromeError::failed(format!("cannot open the project: {error}")))?;
    Ok(json!({ "mode": mode }))
}

/// Validate a `workspaceId` as a directory this machine actually has.
///
/// Separate from the commands that use it so the check is testable without
/// spawning `xdg-open`: a test that really opened a file manager would be a test
/// that leaves a window behind. It is also the one place the trust boundary is
/// enforced — both commands go through here, so neither can be reached without
/// it.
fn project_dir(workspace_id: &str) -> ChromeResult<PathBuf> {
    // The `workspaceId` is the project's absolute path, which is what the session
    // store's `cwd` carries and what a file tree matches against. It arrives from
    // the renderer, so it is checked rather than trusted: an arbitrary string
    // handed to `xdg-open` would run whatever the renderer asked for.
    let path = PathBuf::from(workspace_id);
    if !path.is_dir() {
        return Err(ChromeError::invalid("not a directory on this machine"));
    }
    Ok(path)
}

/// `<cwd>/.step-studio`: where this shell keeps its own per-project state.
///
/// The daemon log lives here, and so does the Skillshare secrets directory.
/// Derived the same way the bridge derives it (`connection_config`), because two
/// derivations of "where the state lives" is how a reveal button ends up opening
/// a directory with nothing in it — and how a secret gets written to one place
/// and read from another.
pub(crate) fn shell_state_dir() -> PathBuf {
    // A test may have installed a directory. Checked first so a test never has
    // to touch the process environment: `std::env::set_var` is `unsafe`, and this
    // workspace forbids `unsafe` outright.
    if let Some(dir) = STATE_DIR_OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    {
        return dir;
    }
    let cwd = std::env::var_os("STEP_STUDIO_CWD")
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());
    state_dir_from(Path::new(&cwd))
}

/// Where a test says the shell's state lives. `None` in production.
///
/// Held only for the length of one read or one write — never across a call into
/// this module. See [`skillshare_tests::StateDirGuard`] for why that matters.
static STATE_DIR_OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Serialises the tests that install [`STATE_DIR_OVERRIDE`].
///
/// Separate from the override so the override's own lock is never held across a
/// call that needs to read it. `cfg(test)`: nothing outside the tests touches it,
/// and an unused static in a release build is an error under `-D warnings`.
#[cfg(test)]
static STATE_DIR_SERIALISE: Mutex<()> = Mutex::new(());

/// The same, from a cwd the caller names.
///
/// Split out so the path is testable without touching the process environment:
/// `std::env::set_var` is `unsafe` and this workspace forbids `unsafe`. It is
/// also the shape the bridge itself would use if it ever needed to ask.
fn state_dir_from(cwd: &Path) -> PathBuf {
    cwd.join(".step-studio")
}

/// Reveal the shell's log directory in the OS file manager.
#[tauri::command]
pub fn open_log_dir() -> ChromeResult<()> {
    let dir = shell_state_dir();
    // Created rather than required: a first run has no log yet, and revealing a
    // path that does not exist is a file manager error rather than an answer.
    let _ = std::fs::create_dir_all(&dir);
    let outcome = if cfg!(target_os = "macos") {
        Command::new("open").arg(&dir).spawn()
    } else if cfg!(target_os = "windows") {
        Command::new("explorer").arg(&dir).spawn()
    } else {
        Command::new("xdg-open").arg(&dir).spawn()
    };
    outcome.map_err(|error| {
        ChromeError::failed(format!("cannot reveal the log directory: {error}"))
    })?;
    Ok(())
}

/// Save a copy of the daemon's log where the user chooses.
///
/// A copy, not a move: the log is still being written, and the diagnostics page
/// offers this more than once. The dialog picks the destination, and a cancelled
/// dialog is a success with nothing written rather than a failure — the caller
/// shows nothing either way.
#[tauri::command]
pub fn export_logs(app: tauri::AppHandle) -> ChromeResult<Value> {
    let source = shell_state_dir().join("daemon.log");
    if !source.is_file() {
        return Err(ChromeError::failed("no daemon log has been written yet"));
    }
    let Some(destination) = app
        .dialog()
        .file()
        .set_file_name("step-studio-daemon.log")
        .blocking_save_file()
    else {
        return Ok(json!({ "ok": true, "cancelled": true }));
    };
    // `FilePath` covers both a real path and, on Android, a content URI. A URI
    // has no filesystem path to copy to, and treating it as a cancellation would
    // be a silent wrong answer.
    let Some(destination) = destination.as_path() else {
        return Err(ChromeError::failed("cannot write to the chosen location"));
    };
    std::fs::copy(&source, destination)
        .map_err(|error| ChromeError::failed(format!("cannot copy the log: {error}")))?;
    Ok(json!({ "ok": true, "cancelled": false }))
}

/// Pick where the artifact library lives.
///
/// The library is moved, not pointed at a second one: `set_directory` keeps the
/// old root in the catalog, so artifacts already stored stay resolvable. That is
/// why choosing a directory is a shell command at all — the dialog is the only
/// part needing the OS, and the state it returns is the renderer's read model.
#[tauri::command]
pub fn choose_artifact_directory() -> ChromeResult<Value> {
    let Some(handle) = crate::app_handle() else {
        return Err(ChromeError::failed(
            "the artifact location picker is not available: no app handle",
        ));
    };
    let picked = tauri_plugin_dialog::DialogExt::dialog(handle)
        .file()
        .set_title("选择产物存储位置")
        .blocking_pick_folder();
    // A content URI (Android) has no filesystem path, so there is nothing to
    // adopt. Treating it as a cancellation would be a silent wrong answer.
    let Some(directory) = picked.as_ref().and_then(|folder| folder.as_path()) else {
        return Ok(json!({ "ok": true, "cancelled": true }));
    };
    let storage = crate::artifacts::library()
        .set_directory(&directory.to_string_lossy())
        // A refusal still has to reach the user: the renderer shows `message`
        // verbatim, so this says what to do about it rather than what failed.
        .map_err(|error| ChromeError::failed(error.message))?;
    Ok(json!({ "ok": true, "cancelled": false, "storage": storage }))
}

/// Import a file into the artifact library.
///
/// The file picker is filtered by the artifact kind, because a `.png` chosen for
/// an audio artifact is a mis-click and storing it would put a row in the library
/// that no player can open.
#[tauri::command]
pub fn import_artifact(input: Value) -> ChromeResult<Value> {
    let kind = input.get("kind").and_then(Value::as_str).unwrap_or("");
    if !crate::artifacts::ARTIFACT_KINDS.contains(&kind) {
        return Err(ChromeError::invalid("that is not an artifact kind"));
    }
    let Some(handle) = crate::app_handle() else {
        return Err(ChromeError::failed(
            "the file picker is not available: no app handle",
        ));
    };
    let picked = tauri_plugin_dialog::DialogExt::dialog(handle)
        .file()
        .set_title("选择要导入的文件")
        .add_filter(kind, &[kind])
        .blocking_pick_file();
    let Some(file) = picked.as_ref().and_then(|file| file.as_path()) else {
        return Ok(json!({ "ok": true, "cancelled": true }));
    };
    let name = file
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let artifact = crate::artifacts::library()
        .register(
            &json!({
                "kind": kind,
                "name": name,
                "mimeType": crate::artifacts::mime_for(&name),
                "workspaceId": input.get("workspaceId").cloned().unwrap_or(Value::Null),
                "sessionId": input.get("sessionId").cloned().unwrap_or(Value::Null),
            }),
            file,
        )
        .map_err(|error| ChromeError::failed(error.message))?;
    Ok(json!({ "ok": true, "cancelled": false, "artifact": artifact }))
}

/// Copy one artifact out to a place the user chooses.
///
/// Reads the payload through `resolve`, which checks the file still matches the
/// catalog's size before handing over a path — a payload that changed underneath
/// is refused rather than exported.
#[tauri::command]
pub fn export_artifact(input: Value) -> ChromeResult<Value> {
    let artifact_id = input.get("artifactId").and_then(Value::as_str).unwrap_or("");
    if !crate::artifacts::is_artifact_id(artifact_id) {
        return Err(ChromeError::invalid("that is not an artifact id"));
    }
    let (record, source) = crate::artifacts::library()
        .resolve(artifact_id)
        .map_err(|error| ChromeError::failed(error.message))?;
    let name = record.get("name").and_then(Value::as_str).unwrap_or("artifact");
    let Some(handle) = crate::app_handle() else {
        return Err(ChromeError::failed(
            "the save dialog is not available: no app handle",
        ));
    };
    let picked = tauri_plugin_dialog::DialogExt::dialog(handle)
        .file()
        .set_file_name(name)
        .blocking_save_file();
    let Some(destination) = picked.as_ref().and_then(|file| file.as_path()) else {
        return Ok(json!({ "ok": true, "cancelled": true }));
    };
    std::fs::copy(&source, destination)
        .map_err(|error| ChromeError::failed(format!("cannot write the file: {error}")))?;
    Ok(json!({ "ok": true, "cancelled": false }))
}

/// Reveal a Skillshare token, once.
///
/// A one-time handoff, and every check here exists because the file being read
/// was written by a different process and is being asked for by a renderer that
/// may not be the one it was written for. The shape mirrors the Electron shell's
/// `takeSkillshareToken` exactly — same validations, same expiry window, same
/// claim-then-unlink dance — because the two shells read the same directory and a
/// divergence would be a security difference rather than a refactor.
///
/// The token is returned to the renderer and never routed through Host receipts
/// or snapshots: those are logged, and a token in a log is a leaked token.
#[tauri::command]
pub fn reveal_skillshare_token(secret_id: String, session_id: String) -> ChromeResult<Value> {
    // Shape first. `secretId` is a UUID and `sessionId` a bounded non-empty
    // string; anything else is refused before a path is built, because the
    // secretId becomes a filename.
    if !is_uuid(&secret_id) {
        return Err(ChromeError::invalid("invalid secret identity"));
    }
    if session_id.is_empty() || session_id.len() > 512 {
        return Err(ChromeError::invalid("invalid secret identity"));
    }

    let file = shell_state_dir()
        .join("skillshare")
        .join("secrets")
        .join(format!("{secret_id}.json"));
    // `symlink_metadata`, not `metadata`: a symlink must be refused rather than
    // followed, or a link placed in the secrets directory reads whatever it
    // points at. This is the check the Electron side makes with `lstat`.
    let metadata = match std::fs::symlink_metadata(&file) {
        Ok(metadata) => metadata,
        // Absent and expired are the same answer to the caller: the token can no
        // longer be revealed. Saying which would tell a prober whether an id
        // exists.
        Err(_) => return Ok(unavailable_token()),
    };
    if !metadata.is_file() || metadata.len() > 8192 {
        return Ok(unavailable_token());
    }

    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(_) => return Ok(unavailable_token()),
    };
    let stored: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(_) => return Ok(unavailable_token()),
    };

    // The secret belongs to the session it was written for. A renderer asking
    // for another session's token is refused rather than served.
    if stored.get("sessionId").and_then(Value::as_str) != Some(session_id.as_str()) {
        return Ok(unavailable_token());
    }

    let now_ms = now_millis();
    let expires_at = stored.get("expiresAt").and_then(Value::as_i64);
    match expires_at {
        // Already expired: the file is removed so it cannot be asked for again.
        Some(expires_at) if expires_at <= now_ms => {
            let _ = std::fs::remove_file(&file);
            return Ok(unavailable_token());
        }
        // The window is short by design. A descriptor claiming a token valid for
        // a long time is not one this handoff wrote, so it is refused rather than
        // served — and served longer than intended is the failure that matters.
        Some(expires_at) if expires_at > now_ms + 121_000 => {
            return Ok(unavailable_token());
        }
        Some(_) => {}
        None => return Ok(unavailable_token()),
    }

    let token = stored.get("token").and_then(Value::as_str).unwrap_or("");
    let name = stored.get("name").and_then(Value::as_str).unwrap_or("");
    if token.len() > 4096 || !token.starts_with("sks_") || !token.is_ascii() {
        return Ok(unavailable_token());
    }
    if name.len() > 128 {
        return Ok(unavailable_token());
    }

    // Claim then read, so two readers cannot both be served: the rename is
    // atomic, and only one of them gets to keep the file. The claim is removed
    // either way, so a crash between the two leaves nothing behind.
    let claim = format!("{}.{}.claim", file.display(), std::process::id());
    if std::fs::rename(&file, &claim).is_err() {
        return Ok(unavailable_token());
    }
    let revealed = json!({ "ok": true, "token": token, "name": name });
    let _ = std::fs::remove_file(&claim);
    Ok(revealed)
}

/// The one answer for every way this can fail.
///
/// Deliberately identical for "no such secret", "expired", "belongs to another
/// session" and "malformed": distinguishing them would let a prober learn
/// whether an id exists. The message tells the user what to do, not what
/// happened.
fn unavailable_token() -> Value {
    json!({
        "ok": false,
        "message": "This token can no longer be revealed. It may have expired or already been read. \
                    Revoke it before creating a replacement if necessary.",
    })
}

/// Whether a string is a canonical UUID.
///
/// Checked here rather than trusted because the value becomes a filename: a
/// `secretId` of `../../etc/passwd` would otherwise build a path outside the
/// secrets directory.
fn is_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        let expected_separator = matches!(index, 8 | 13 | 18 | 23);
        if expected_separator {
            if *byte != b'-' {
                return false;
            }
        } else if !byte.is_ascii_digit() && !(b'a'..=b'f').contains(byte) {
            return false;
        }
    }
    true
}

/// Milliseconds since the epoch, for the secret's expiry window.
///
/// Local rather than borrowed from `pump`: chrome does not otherwise depend on
/// the pump, and a secret's expiry is the one place this module needs a clock.
/// `SystemTime::now` cannot fail backwards, so the `unwrap_or` is the
/// unreachable arm rather than a swallowed error.
pub(crate) fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

/// `now_millis` as RFC 3339 UTC with millisecond precision.
///
/// Inlined rather than pulled from `chrono`: the artifact catalog's `createdAt`
/// is the only consumer, and one timestamp format is not worth a dependency tree.
/// The algorithm is Howard Hinnant's `civil_from_days`, run backwards.
pub(crate) fn now_iso_millis() -> String {
    let millis = now_millis();
    let (year, month, day, hour, minute, second) = civil_from_unix_millis(millis);
    format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{:03}Z",
        millis.rem_euclid(1000)
    )
}

/// Days since the epoch, to a calendar date and a time of day.
///
/// Handles dates before 1970 (`div_euclid`/`rem_euclid` rather than `/` and `%`),
/// which a clock skew can produce and a truncating division would render as a
/// year in the far future.
pub(crate) fn civil_from_unix_millis(millis: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = millis.div_euclid(86_400_000);
    let seconds_of_day = millis.rem_euclid(86_400_000);
    let hour = (seconds_of_day / 3_600_000) as u32;
    let minute = ((seconds_of_day / 60_000) % 60) as u32;
    let second = ((seconds_of_day / 1000) % 60) as u32;

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u32;
    let month = (mp + if mp < 10 { 3 } else { -9 }) as u32;
    (year + i64::from(month <= 2), month, day, hour, minute, second)
}

/// A system notification with fixed text.
#[tauri::command]
pub fn notify(title: String, body: Option<String>) -> ChromeResult<()> {
    if title.trim().is_empty() {
        return Err(ChromeError::invalid("a notification needs a title"));
    }
    let outcome = if cfg!(target_os = "macos") {
        let script = match &body {
            Some(body) => format!(
                "display notification {} with title {}",
                apple_script_string(body),
                apple_script_string(&title)
            ),
            None => format!("display notification {}", apple_script_string(&title)),
        };
        Command::new("osascript").args(["-e", &script]).spawn()
    } else if cfg!(target_os = "windows") {
        // No notification primitive is reachable without a plugin, and the one
        // call site is a settings notice the page already shows inline.
        return Ok(());
    } else {
        let mut command = Command::new("notify-send");
        command.arg(&title);
        if let Some(body) = &body {
            command.arg(body);
        }
        command.spawn()
    };
    // A machine with no notifier is not an error worth surfacing: the notice is
    // also visible in the page that asked for it.
    let _ = outcome;
    Ok(())
}

fn apple_script_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The titlebar tint.
///
/// A no-op on purpose. The Electron version tints the native titlebar overlay,
/// which exists only on Windows; under a pure-Tauri shell the whole titlebar is
/// web content the renderer already themes itself. Returning early rather than
/// reaching for a plugin keeps that honest.
#[tauri::command]
pub fn set_theme(theme: String) -> ChromeResult<()> {
    if theme != "light" && theme != "dark" {
        return Err(ChromeError::invalid("theme must be light or dark"));
    }
    Ok(())
}

/// Which conversation sessions the renderer currently shows.
///
/// A no-op on purpose: the Electron host uses it to throttle work for sessions
/// whose webview is hidden. This shell runs no Host, so there is nothing to
/// throttle — and there is exactly one webview, so nothing is hidden either.
#[tauri::command]
pub fn set_conversation_view_state(state: Value) -> ChromeResult<bool> {
    // Validate the shape even though nothing consumes it, so a malformed
    // payload fails here rather than silently doing nothing.
    state
        .get("visibleSessionIds")
        .and_then(Value::as_array)
        .ok_or_else(|| ChromeError::invalid("visibleSessionIds must be a list"))?;
    Ok(true)
}

/// The renderer's payload-health heartbeat.
#[tauri::command]
pub fn report_payload_health(status: String) -> ChromeResult<bool> {
    if status != "ready" && status != "failed" {
        return Err(ChromeError::invalid("status must be ready or failed"));
    }
    if status == "failed" {
        eprintln!("[chrome] renderer reported a failed payload");
    }
    Ok(true)
}

/// The renderer's memory sample.
///
/// Logged, not stored: the perf harness that would consume these lands in M1.5,
/// and inventing a store before there is a reader is speculation.
#[tauri::command]
pub fn report_performance_sample(sample: Value) -> ChromeResult<bool> {
    let role = sample
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    // Whichever figure the renderer actually sent. It reports heap numbers from
    // its own runtime and RSS only where the platform exposes it, so logging a
    // fixed "rss" field would print `unknown` for every renderer sample.
    let figures: Vec<String> = [
        "rssBytes",
        "heapUsedBytes",
        "heapTotalBytes",
        "arrayBuffersBytes",
    ]
    .iter()
    .filter_map(|key| sample.get(*key).and_then(Value::as_u64))
    .map(|bytes| format!("{} KiB", bytes / 1024))
    .collect();
    match figures.is_empty() {
        true => eprintln!("[chrome] perf sample role={role} (no figures)"),
        false => eprintln!("[chrome] perf sample role={role} {}", figures.join(" ")),
    }
    Ok(true)
}

#[cfg(test)]
mod project_tests {
    use super::*;

    #[test]
    fn a_real_directory_is_accepted() {
        // The repository itself, which every run of this test has.
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("the repo root")
            .to_path_buf();
        assert!(project_dir(&root.to_string_lossy()).is_ok());
    }

    #[test]
    fn a_path_that_is_not_a_directory_is_refused() {
        // The trust boundary: this string arrives from the renderer and is handed
        // to `xdg-open`. A file, a missing path and a bare word are all refused
        // rather than passed through.
        for candidate in [
            "/etc/hostname",
            "/a/path/that/does/not/exist",
            "",
            "xdg-open --help",
            "-t",
        ] {
            let outcome = project_dir(candidate);
            assert!(
                outcome.is_err(),
                "{candidate:?} must be refused, not handed to the OS opener"
            );
            let error = outcome.expect_err("refused");
            assert!(
                !error.code.is_empty(),
                "{candidate:?} refused with a code the renderer can read"
            );
        }
    }
}

#[cfg(test)]
mod log_tests {
    use super::*;

    #[test]
    fn the_log_directory_is_the_one_the_bridge_writes_to() {
        // Two derivations of "where the log lives" is how a reveal button ends up
        // opening a directory with nothing in it. The bridge writes
        // `<cwd>/.step-studio/daemon.log`, so this has to agree with it.
        assert_eq!(
            state_dir_from(Path::new("/tmp/a-project")),
            PathBuf::from("/tmp/a-project/.step-studio")
        );
    }

    #[test]
    fn a_cwd_with_a_trailing_separator_does_not_double_it() {
        // `Path::join` handles this, and a test is what stops somebody from
        // building the path with `format!` instead.
        assert_eq!(
            state_dir_from(Path::new("/tmp/a-project/")),
            PathBuf::from("/tmp/a-project/.step-studio")
        );
    }

    #[test]
    fn a_relative_cwd_stays_relative() {
        assert_eq!(
            state_dir_from(Path::new(".")),
            PathBuf::from("./.step-studio")
        );
    }
}

#[cfg(test)]
mod skillshare_tests {
    use super::*;

    const SESSION: &str = "session-1";
    const SECRET: &str = "3f2504e0-4f89-11d3-9a0c-0305e82c3301";

    /// Write a secret descriptor the way the creating process does.
    fn write_secret(dir: &Path, secret_id: &str, body: &Value) {
        let secrets = dir.join("skillshare").join("secrets");
        std::fs::create_dir_all(&secrets).expect("make the secrets directory");
        std::fs::write(
            secrets.join(format!("{secret_id}.json")),
            serde_json::to_string(body).expect("serialize the descriptor"),
        )
        .expect("write the descriptor");
    }

    fn valid(expires_in_ms: i64) -> Value {
        json!({
            "sessionId": SESSION,
            "token": "sks_abc-DEF_123",
            "name": "a package",
            "expiresAt": now_millis() + expires_in_ms,
        })
    }

    #[test]
    fn a_valid_secret_is_revealed_once() {
        let dir = std::env::temp_dir().join(format!("ss-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("make a state dir");
        write_secret(&dir, SECRET, &valid(60_000));

        // Point the module at this directory by making it the state dir.
        let guard = StateDirGuard::set(&dir);
        let revealed =
            reveal_skillshare_token(SECRET.to_string(), SESSION.to_string()).expect("revealed");
        assert_eq!(revealed.get("ok").and_then(Value::as_bool), Some(true));
        assert_eq!(
            revealed.get("token").and_then(Value::as_str),
            Some("sks_abc-DEF_123")
        );

        // And only once: the file is gone, so a second ask is refused.
        let again = reveal_skillshare_token(SECRET.to_string(), SESSION.to_string())
            .expect("refused, not an error");
        assert_eq!(again.get("ok").and_then(Value::as_bool), Some(false));
        drop(guard);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_secret_for_another_session_is_refused() {
        // The descriptor names the session it was written for. A renderer asking
        // for a different one is refused rather than served.
        let dir = std::env::temp_dir().join(format!("ss-other-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("make a state dir");
        write_secret(&dir, SECRET, &valid(60_000));

        let guard = StateDirGuard::set(&dir);
        let revealed = reveal_skillshare_token(SECRET.to_string(), "someone-else".to_string())
            .expect("refused");
        assert_eq!(revealed.get("ok").and_then(Value::as_bool), Some(false));
        drop(guard);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_expired_secret_is_removed_and_refused() {
        // Removed, so it cannot be asked for again — and so a prober learns
        // nothing from asking twice.
        let dir = std::env::temp_dir().join(format!("ss-expired-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("make a state dir");
        write_secret(&dir, SECRET, &valid(-1_000));

        let guard = StateDirGuard::set(&dir);
        let revealed =
            reveal_skillshare_token(SECRET.to_string(), SESSION.to_string()).expect("refused");
        assert_eq!(revealed.get("ok").and_then(Value::as_bool), Some(false));
        assert!(
            !dir.join("skillshare").join("secrets").join(format!("{SECRET}.json")).exists(),
            "an expired secret is deleted, not left to be asked for again"
        );
        drop(guard);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_secret_id_that_would_escape_the_directory_is_refused() {
        // The secretId becomes a filename. `../../etc/passwd` must be refused
        // before a path is built, which is why the UUID shape is checked first.
        for candidate in [
            "../../etc/passwd",
            "3f2504e0-4f89-11d3-9a0c-0305e82c330",
            "3f2504e04f8911d39a0c0305e82c3301",
            "",
            "3f2504e0-4f89-11d3-9a0c-0305e82c3301/",
        ] {
            assert!(reveal_skillshare_token(candidate.to_string(), SESSION.to_string()).is_err());
        }
        // And a real one is the right shape.
        assert!(is_uuid(SECRET));
    }

    /// Point the module at a directory for the length of a test.
    ///
    /// Two locks, and the split is the whole point. The override is held only for
    /// the length of one write or one read, so `shell_state_dir()` can lock it
    /// while a test runs. Serialisation is a *separate* mutex this guard keeps
    /// for the test's lifetime — the first version used one lock for both, and
    /// because `std::sync::Mutex` is not reentrant the test deadlocked against
    /// itself the moment it called the function it had just pointed at a
    /// directory. Nothing in the output said so: it hung, which is how a
    /// self-deadlock reports.
    ///
    /// No `unsafe`, and no dependence on test execution order: the state dir is
    /// otherwise read through an environment variable, and `set_var` is `unsafe`,
    /// which this workspace forbids.
    struct StateDirGuard {
        _serialise: std::sync::MutexGuard<'static, ()>,
    }

    impl StateDirGuard {
        fn set(dir: &Path) -> Self {
            let serialise = STATE_DIR_SERIALISE
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *STATE_DIR_OVERRIDE
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(dir.to_path_buf());
            Self {
                _serialise: serialise,
            }
        }
    }

    impl Drop for StateDirGuard {
        fn drop(&mut self) {
            *STATE_DIR_OVERRIDE
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        }
    }
}
