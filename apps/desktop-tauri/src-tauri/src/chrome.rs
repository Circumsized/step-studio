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

use std::path::PathBuf;
use std::process::Command;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::Manager;

/// Rejection payload, shaped like the rest of the shell's IPC errors.
#[derive(Serialize, Debug)]
pub struct ChromeError {
    pub code: String,
    pub message: String,
}

impl ChromeError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: "INVALID_ARGUMENT".to_string(),
            message: message.into(),
        }
    }

    fn failed(message: impl Into<String>) -> Self {
        Self {
            code: "CHROME_FAILED".to_string(),
            message: message.into(),
        }
    }
}

type ChromeResult<T> = Result<T, ChromeError>;

/// `<app_data>/step-studio`, the shell's own state directory.
///
/// Mirrors what the Electron host keeps under `%APPDATA%\omp-studio`; the name
/// is the only thing that changed.
fn state_dir(app: &tauri::AppHandle) -> ChromeResult<PathBuf> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|error| ChromeError::failed(format!("cannot locate the app data directory: {error}")))?
        .join("step-studio");
    std::fs::create_dir_all(&dir)
        .map_err(|error| ChromeError::failed(format!("cannot create {}: {error}", dir.display())))?;
    Ok(dir)
}

fn profile_dir(app: &tauri::AppHandle) -> ChromeResult<PathBuf> {
    let dir = state_dir(app)?.join("profile");
    std::fs::create_dir_all(&dir)
        .map_err(|error| ChromeError::failed(format!("cannot create {}: {error}", dir.display())))?;
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
pub fn save_avatar(
    app: tauri::AppHandle,
    mime: String,
    bytes: Vec<u8>,
) -> ChromeResult<Value> {
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
        ("editor", Command::new("open").args(["-t"]).arg(path).spawn())
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
    let role = sample.get("role").and_then(Value::as_str).unwrap_or("unknown");
    // Whichever figure the renderer actually sent. It reports heap numbers from
    // its own runtime and RSS only where the platform exposes it, so logging a
    // fixed "rss" field would print `unknown` for every renderer sample.
    let figures: Vec<String> = ["rssBytes", "heapUsedBytes", "heapTotalBytes", "arrayBuffersBytes"]
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
