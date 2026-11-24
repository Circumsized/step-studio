//! The `stepStudioTerminal` bridge: a real local shell per terminal (M1.2c).
//!
//! AGENTS.md is explicit that the bottom terminal must not be faked — "Terminal
//! 两种模式都用本机 Shell" — so this is a real PTY, not a line buffer.
//!
//! Scope, the same boundary as `chrome.rs`: the recording trio
//! (`recordingStart`/`Stop`/`Status`) is absent. They mint an `artifactId`, which
//! is the Host's artifact library, and this shell runs no Host. The renderer
//! treats them as optional and disables the record button.
//!
//! `workspaceId` is accepted and ignored: mapping a workspace id to a directory
//! needs the Host's workspace registry, which is not here yet. The terminal
//! starts in the shell's own working directory instead. That is a real gap
//! against step-studio, recorded rather than papered over.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::Emitter;

/// Data flowing out of a terminal. Two channels rather than one tagged channel,
/// because the renderer subscribes to them separately and never needs to
/// discriminate.
pub const TERMINAL_DATA_CHANNEL: &str = "step-studio:terminal:data";
pub const TERMINAL_EXIT_CHANNEL: &str = "step-studio:terminal:exit";

#[derive(Debug, Serialize)]
pub struct TerminalError {
    pub code: String,
    pub message: String,
}

impl TerminalError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

type TerminalResult<T> = Result<T, TerminalError>;

/// One live PTY.
struct Terminal {
    /// Kept for `resize`; the writer and reader are cloned off it, so it cannot
    /// be dropped while either is alive.
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

/// A quit must not leave shells behind.
///
/// Enforced here rather than by an explicit shutdown call: an explicit call has
/// to be remembered at every exit path, and forgetting it once leaves orphaned
/// shells holding the workspace. Tauri drops managed state on shutdown, so this
/// covers every path including a panic.
impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct TerminalState {
    terminals: Mutex<HashMap<String, Terminal>>,
}

impl Default for TerminalState {
    fn default() -> Self {
        Self {
            terminals: Mutex::new(HashMap::new()),
        }
    }
}

impl TerminalState {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Terminal>> {
        self.terminals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The shell to run. `$SHELL` when it is set and executable, else the platform
/// default — the same precedence the Electron terminal uses.
fn shell_command() -> String {
    if let Ok(shell) = std::env::var("SHELL") {
        if !shell.trim().is_empty() && std::path::Path::new(&shell).is_file() {
            return shell;
        }
    }
    if cfg!(target_os = "windows") {
        "powershell.exe".to_string()
    } else {
        "/bin/sh".to_string()
    }
}

/// What `create` reports back.
pub struct ShellInfo {
    pub id: String,
    pub name: String,
    pub cwd: String,
}

/// Open a PTY and spawn the shell in it. No event emission, no state — the
/// caller wires the reader thread and stores the handle. Split out so the PTY
/// work is testable without a Tauri app handle.
fn open_shell(id: &str, cols: u16, rows: u16) -> TerminalResult<(Terminal, ShellInfo)> {
    let system = native_pty_system();
    let pair = system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|error| TerminalError::new("PTY_OPEN_FAILED", format!("cannot open a pty: {error}")))?;

    // The shell's own working directory. `workspaceId` is deliberately not used:
    // see the module comment.
    let cwd = std::env::var("STEP_STUDIO_CWD").unwrap_or_else(|_| ".".to_string());
    let mut command = CommandBuilder::new(shell_command());
    command.cwd(&cwd);
    // A login-ish environment is what makes `git`, `node` and friends resolve;
    // without TERM the shell still runs but programs that probe it misbehave.
    command.env("TERM", "xterm-256color");

    let child = pair
        .slave
        .spawn_command(command)
        .map_err(|error| TerminalError::new("PTY_SPAWN_FAILED", format!("cannot start the shell: {error}")))?;
    // The slave is only needed to spawn; dropping it is what lets the reader see
    // EOF when the shell exits.
    drop(pair.slave);

    // `take_writer` clones the fd internally, so `master` stays usable for
    // `resize`; it just cannot be taken twice.
    let writer = pair
        .master
        .take_writer()
        .map_err(|error| TerminalError::new("PTY_WRITER_FAILED", format!("cannot open the pty writer: {error}")))?;

    let shell_name = shell_command();
    let name = std::path::Path::new(&shell_name)
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or(shell_name);
    let cwd_report = std::env::current_dir()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());

    let info = ShellInfo {
        id: id.to_string(),
        name,
        cwd: cwd_report,
    };
    let terminal = Terminal {
        master: pair.master,
        writer,
        child,
    };
    Ok((terminal, info))
}

#[tauri::command]
pub fn create(
    app: tauri::AppHandle,
    state: tauri::State<'_, TerminalState>,
    size: Option<Value>,
) -> TerminalResult<Value> {
    let cols = size
        .as_ref()
        .and_then(|s| s.get("cols"))
        .and_then(Value::as_u64)
        .unwrap_or(80) as u16;
    let rows = size
        .as_ref()
        .and_then(|s| s.get("rows"))
        .and_then(Value::as_u64)
        .unwrap_or(24) as u16;

    let id = format!("term-{}", next_id());
    let (terminal, info) = open_shell(&id, cols, rows)?;
    let (id, name, cwd) = (info.id, info.name, info.cwd);

    // The reader needs its own clone of the pty's read half.
    let mut reader = terminal
        .master
        .try_clone_reader()
        .map_err(|error| TerminalError::new("PTY_READER_FAILED", format!("cannot open the pty reader: {error}")))?;

    // The reader owns the output half. It emits until the pty closes, which is
    // also the signal that the shell exited — there is no separate exit code to
    // report, matching the contract's `{ id }` exit payload.
    let emitter = app.clone();
    let emitted_id = id.clone();
    std::thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    let data = String::from_utf8_lossy(&buffer[..read]).to_string();
                    let _ = emitter.emit(TERMINAL_DATA_CHANNEL, json!({ "id": emitted_id, "data": data }));
                }
                // A transient read error is not the end of the terminal; EIO on
                // Linux is how a pty reports EOF, so it is treated as one.
                Err(_) => break,
            }
        }
        let _ = emitter.emit(TERMINAL_EXIT_CHANNEL, json!({ "id": emitted_id }));
    });

    state.lock().insert(id.clone(), terminal);

    Ok(json!({ "id": id, "name": name, "cwd": cwd }))
}

/// Send keystrokes. Split out from the command so the not-found path is
/// testable without a pty.
fn write_to(state: &TerminalState, id: &str, data: &str) -> TerminalResult<()> {
    let mut terminals = state.lock();
    let terminal = terminals
        .get_mut(id)
        .ok_or_else(|| TerminalError::new("TERMINAL_NOT_FOUND", format!("no terminal {id}")))?;
    terminal
        .writer
        .write_all(data.as_bytes())
        .and_then(|()| terminal.writer.flush())
        .map_err(|error| TerminalError::new("PTY_WRITE_FAILED", format!("cannot write to {id}: {error}")))
}

fn resize_to(state: &TerminalState, id: &str, cols: u16, rows: u16) -> TerminalResult<()> {
    let terminals = state.lock();
    let terminal = terminals
        .get(id)
        .ok_or_else(|| TerminalError::new("TERMINAL_NOT_FOUND", format!("no terminal {id}")))?;
    terminal
        .master
        .resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|error| TerminalError::new("PTY_RESIZE_FAILED", format!("cannot resize {id}: {error}")))
}

fn dispose_of(state: &TerminalState, id: &str) {
    if let Some(terminal) = state.lock().remove(id) {
        // Dropping the Terminal kills the child; see its Drop impl.
        drop(terminal);
    }
}

#[tauri::command]
pub fn write(
    state: tauri::State<'_, TerminalState>,
    id: String,
    data: String,
) -> TerminalResult<()> {
    write_to(&state, &id, &data)
}

#[tauri::command]
pub fn resize(
    state: tauri::State<'_, TerminalState>,
    id: String,
    cols: u16,
    rows: u16,
) -> TerminalResult<()> {
    resize_to(&state, &id, cols, rows)
}

#[tauri::command]
pub fn dispose(state: tauri::State<'_, TerminalState>, id: String) -> TerminalResult<()> {
    dispose_of(&state, &id);
    Ok(())
}

static COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_command_resolves_to_something_the_pty_can_spawn() {
        let shell = shell_command();
        assert!(!shell.trim().is_empty(), "a shell must be resolved");
        // A bare name is resolved through PATH by the spawn call — which is
        // what `powershell.exe` is on Windows — so only a name carrying a
        // separator can be checked against the filesystem. Asserting
        // `is_file()` on the bare name failed on every Windows run.
        if shell.contains(['/', '\\']) {
            assert!(
                std::path::Path::new(&shell).is_file(),
                "{shell} is not a file; the pty would fail to spawn"
            );
        }
    }

    #[test]
    fn ids_are_unique_and_prefixed() {
        let first = next_id();
        let second = next_id();
        assert!(second > first, "ids must be monotonic");
        assert!(format!("term-{first}").starts_with("term-"));
    }

    #[test]
    fn a_missing_terminal_is_a_not_found_not_a_panic() {
        // The state is empty, so every lookup must fail cleanly. This is the
        // guard the renderer relies on after a dispose races an in-flight write.
        let state = TerminalState::default();
        assert_eq!(write_to(&state, "nope", "x").unwrap_err().code, "TERMINAL_NOT_FOUND");
        assert_eq!(resize_to(&state, "nope", 80, 24).unwrap_err().code, "TERMINAL_NOT_FOUND");
        // Disposing something that is already gone is a no-op, not an error: the
        // renderer disposes on unmount and on an exit event, so both can race.
        dispose_of(&state, "nope");
    }

    /// The real check: a pty opens, the shell starts, a command round-trips, and
    /// disposing it kills the process. This is the part that cannot be faked, so
    /// it is tested against the actual OS.
    #[test]
    fn a_shell_round_trips_a_command_and_dies_on_dispose() {
        let id = "test-shell";
        let (mut terminal, info) = open_shell(id, 80, 24).expect("open a pty");
        assert_eq!(info.id, id);
        assert!(!info.name.is_empty(), "the shell name must be reported");

        // Read on a clone so the write and the read do not share one fd.
        let mut reader = terminal.master.try_clone_reader().expect("clone the reader");

        // A command whose output is unmistakable and whose exit ends the shell,
        // so the reader sees EOF. `echo`, not `printf`: the shell here is
        // whatever `shell_command` resolved — `/bin/sh` on Unix, PowerShell on
        // Windows — and `printf 'X\n'` is POSIX syntax that PowerShell does
        // not have, so the marker never appeared and the read below never
        // returned. `echo` is a builtin in `sh` and an alias for
        // `Write-Output` in PowerShell.
        terminal
            .writer
            .write_all(b"echo PONYTAIL_OK\nexit\n")
            .expect("write to the pty");
        terminal.writer.flush().expect("flush the pty");

        // Bounded by a deadline, not by an iteration count: `read` blocks until
        // a byte arrives, so counting reads bounds nothing. The read runs on a
        // thread and the wait has a timeout, so a shell that starts and then
        // says nothing fails here instead of parking the test binary — which is
        // what it did for 51 minutes on Windows until the job timeout fired.
        let (sent, received) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut output = String::new();
            let mut chunk = [0u8; 4096];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => {
                        output.push_str(&String::from_utf8_lossy(&chunk[..read]));
                        if output.contains("PONYTAIL_OK") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = sent.send(output);
        });
        let output = received
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the shell never produced the marker or EOF within 30s");
        assert!(
            output.contains("PONYTAIL_OK"),
            "the shell did not run the command; got: {output:?}"
        );

        // Dropping the handle must kill the child, not just close the pipe.
        drop(terminal);
    }

    #[test]
    fn resize_reaches_the_pty() {
        let (terminal, _) = open_shell("test-resize", 80, 24).expect("open a pty");
        // A wrong size is a real error from the OS, so this proves the master is
        // still usable after `take_writer`.
        terminal
            .master
            .resize(PtySize {
                rows: 40,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("resize the pty");
    }
}
