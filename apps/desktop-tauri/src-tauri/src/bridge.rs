//! The Desktop IPC bridge, wired to a real Studio Bridge session.
//!
//! M1.2 wires the Desktop IPC bridge to a real Studio Bridge session. The
//! session wiring was ported from the egui shell's `bridge_client.rs`, which
//! already drove the adapter; the difference is where the event stream goes —
//! that shell sent over an mpsc channel to its own view, this one emits on the
//! Tauri event channel the renderer already listens to. (That crate is gone:
//! v5.0 deleted the egui surface and M7 removed the dead code.)
//!
//! M1.2b added the working half: `query` and `command` are routed through
//! `pump`, the one thread that owns the session and translates between the
//! renderer's facade vocabulary and the adapter's operation kinds.
//!
//! What this shell does NOT do is reimplement the Host. The Electron main
//! process composes ~160 operations across github, archive, residents, models
//! and workspace services that live in `apps/desktop/src/host-composition.ts`;
//! none of that is here, so anything outside `pump::resolve_operation` returns
//! an honest unavailable and the renderer draws its empty state. Reusing the
//! whole Host instead of porting it is a separate decision — see
//! `design/plan.md` §19.

use std::path::{Path, PathBuf};
use std::time::Duration;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{json, Value};
use step_studio_bridge::{AdapterLaunch, BridgeSession};
use step_studio_host as host;

use crate::pump::{self, Job};

/// Rejection payload. Tauri hands the deserialized error to JS, where the
/// sender re-wraps it as an `Error` so the UI sees a transport failure rather
/// than a typed client error.
#[derive(Serialize)]
pub struct IpcError {
    pub code: String,
    pub message: String,
}

impl IpcError {
    /// The renderer re-wraps this as a plain `Error`, so the message is what
    /// reaches the user. Name the operation and say why it is missing.
    fn unavailable(operation: &str) -> Self {
        Self {
            code: "CAPABILITY_UNAVAILABLE".to_string(),
            message: format!(
                "{operation} 在当前外壳不可用：Tauri 壳只桥接 Step 会话本身，Host 服务（Git / 模型配置 / 归档）尚未接入。"
            ),
        }
    }

    fn gone() -> Self {
        Self {
            code: "DESKTOP_BRIDGE_CLOSED".to_string(),
            message: "desktop bridge: the session has been closed".to_string(),
        }
    }

    pub(crate) fn raw(message: String) -> Self {
        Self {
            code: "DESKTOP_BRIDGE_ERROR".to_string(),
            message,
        }
    }
}

// --- the unavailable bootstrap, kept as the fallback -----------------------
//
// Mirrors `unavailableBootstrap()` in `apps/renderer/src/App.tsx:385-394`.
// When the session cannot be established — no Step CLI, no socket — the shell
// reports this instead of failing, and the renderer draws its honest empty
// state.

#[derive(Serialize)]
pub struct UnavailableBootstrap {
    #[serde(rename = "contractVersion")]
    pub contract_version: u32,
    pub authority: UnavailableAuthority,
    pub runtime: UnavailableRuntime,
    pub surface: UnavailableSurface,
    #[serde(rename = "capabilityManifest")]
    pub capability_manifest: UnavailableCapabilityManifest,
    #[serde(rename = "commandManifestHash")]
    pub command_manifest_hash: String,
    pub selected: serde_json::Value,
}

#[derive(Serialize)]
pub struct UnavailableAuthority {
    #[serde(rename = "authorityId")]
    pub authority_id: String,
    #[serde(rename = "authorityEpoch")]
    pub authority_epoch: u32,
}

#[derive(Serialize)]
pub struct UnavailableRuntime {
    pub status: String,
    pub classification: String,
}

#[derive(Serialize)]
pub struct UnavailableSurface {
    #[serde(rename = "terminalAttach")]
    pub terminal_attach: bool,
    #[serde(rename = "fileReveal")]
    pub file_reveal: bool,
    #[serde(rename = "previewInput")]
    pub preview_input: bool,
    #[serde(rename = "openExternal")]
    pub open_external: bool,
}

#[derive(Serialize)]
pub struct UnavailableCapabilityManifest {
    pub profile: String,
    #[serde(rename = "generatedAt")]
    pub generated_at: String,
    pub hash: String,
    pub capabilities: Vec<serde_json::Value>,
}

/// `CLIENT_CONTRACT_VERSION` from `packages/client-contract/src/index.ts:55`.
const CLIENT_CONTRACT_VERSION: u32 = 2;

fn unavailable_bootstrap() -> UnavailableBootstrap {
    UnavailableBootstrap {
        contract_version: CLIENT_CONTRACT_VERSION,
        authority: UnavailableAuthority {
            authority_id: "desktop-unavailable".to_string(),
            authority_epoch: 1,
        },
        runtime: UnavailableRuntime {
            status: "unavailable".to_string(),
            classification: "unavailable".to_string(),
        },
        surface: UnavailableSurface {
            terminal_attach: false,
            file_reveal: false,
            preview_input: false,
            open_external: false,
        },
        capability_manifest: UnavailableCapabilityManifest {
            profile: "limited".to_string(),
            generated_at: "1970-01-01T00:00:00.000Z".to_string(),
            hash: "unavailable".to_string(),
            capabilities: Vec::new(),
        },
        command_manifest_hash: "unavailable".to_string(),
        selected: serde_json::json!({}),
    }
}

// --- the session ----------------------------------------------------------

/// State shared with the Tauri commands.
///
/// A handle over [`SessionInner`] rather than the fields themselves, because a
/// reconnect has to replace the session underneath commands that hold a
/// `State<'_, SessionState>` for the window's lifetime.
pub struct SessionState {
    pub inner: Arc<SessionInner>,
}

/// What a reconnect replaces.
pub struct SessionInner {
    /// The real snapshot once a session is established. Cached because the
    /// webview's `bootstrap` must not block on a socket round-trip.
    pub bootstrap: Mutex<Option<serde_json::Value>>,
    /// Where events go. Shared with the pump rather than owned by a command,
    /// because the pump emits from its own thread for the whole turn.
    pub window: Arc<Mutex<Option<tauri::WebviewWindow>>>,
    /// The pump's inbox. `close` takes the sender, which ends the pump.
    pub jobs: Mutex<Option<mpsc::Sender<Job>>>,
    /// Bumped on every successful connect, reconnect included.
    ///
    /// The renderer cannot tell a reconnected shell from the one it was talking
    /// to without this: the epoch alone does not move when the daemon is the
    /// same one and only the adapter restarted.
    pub generation: AtomicU64,
}

impl SessionInner {
    /// Take over the relay: the old pump's sender goes, the new one arrives.
    fn install(&self, bootstrap: serde_json::Value, jobs: mpsc::Sender<Job>) {
        {
            let mut slot = self.jobs.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            *slot = Some(jobs);
        }
        {
            let mut slot = self
                .bootstrap
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *slot = Some(bootstrap);
        }
        self.generation.fetch_add(1, Ordering::Relaxed);
    }
}

/// A live session: the bootstrap the renderer asked for, plus the pump's
/// identity for the event envelope.
pub struct Connected {
    pub bootstrap: serde_json::Value,
    pub session: BridgeSession,
    /// Which adapter this session reached. It changes when the adapter
    /// restarts, which is what tells a reconnecting shell that the authority
    /// moved even though the daemon's epoch did not.
    pub instance_id: String,
    pub runtime_epoch: u64,
    pub authority_epoch: u64,
    pub state_version: u64,
}

/// Query envelopes carry no `requestId`, but the pump correlates by one.
fn next_query_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!("tauri-query-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Walk up looking for the workspace root, so a shell run from a subdirectory
/// still finds the adapter.
fn find_repo_root(start: &Path) -> PathBuf {
    let mut current = Some(start);
    while let Some(dir) = current {
        if dir.join("crates/studio-bridge").is_dir() {
            return dir.to_path_buf();
        }
        current = dir.parent();
    }
    start.to_path_buf()
}

fn env_or(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.to_string())
}

/// Everything a reconnect needs, captured once.
///
/// The supervisor rebuilds a session from this, so it has to survive the first
/// one ending.
#[derive(Clone)]
pub struct ConnectionConfig {
    pub endpoint: String,
    pub cwd: PathBuf,
    pub repo_root: PathBuf,
    pub step_command: String,
    pub session_dir: PathBuf,
}

/// Read the configuration out of the environment.
pub fn connection_config() -> Option<ConnectionConfig> {
    let cwd = PathBuf::from(env_or("STEP_STUDIO_CWD", "."));
    let repo_root = find_repo_root(
        &std::env::var_os("STEP_STUDIO_REPO_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| cwd.clone())),
    );
    let step_command = env_or("STEP_STUDIO_STEP_CMD", "step");

    // A path that does not exist is a configuration mistake; the daemon would
    // only report it as a closed socket after the handshake.
    if step_command.contains('/') && !Path::new(&step_command).is_file() {
        eprintln!("[bridge] Step CLI not found at {step_command}");
        return None;
    }

    let session_dir = cwd.join(".step-studio");
    // Hardened, not just created. `create_dir_all` derives its mode from the
    // umask, and under the common `002` that is `0775` — group-writable, so
    // another member of the group can replace the token file this directory
    // holds. Measured, not assumed. The daemon's own endpoint directory gets
    // the same treatment in `lifecycle::bind`; this one did not, which is the
    // same threat in a different place.
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let _ = std::fs::create_dir_all(&session_dir);
        if let Ok(metadata) = std::fs::metadata(&session_dir) {
            if metadata.uid() == host::lifecycle::own_uid() {
                let _ = std::fs::set_permissions(
                    &session_dir,
                    std::fs::Permissions::from_mode(0o700),
                );
            }
        }
    }
    #[cfg(not(unix))]
    let _ = std::fs::create_dir_all(&session_dir);
    Some(ConnectionConfig {
        endpoint: host::lifecycle::default_endpoint(),
        cwd,
        repo_root,
        step_command,
        session_dir,
    })
}

/// Connect to the host daemon, starting it if nothing is answering.
///
/// The shell is a client. It launches the daemon and nothing else, and the
/// daemon owns the adapter — which is what makes a shell restart survivable:
/// dropping this session closes a socket and half-closes it, and the adapter
/// keeps running under the daemon.
fn connect_with(config: &ConnectionConfig) -> Option<Connected> {
    if !ensure_daemon(
        &config.endpoint,
        &config.cwd,
        &config.repo_root,
        &config.step_command,
        &config.session_dir,
    ) {
        return None;
    }

    // Client-only: an empty `node` means this session spawns nothing, so its
    // drop must not end the runtime. The daemon owns the adapter and therefore
    // the token file, which is why the token is passed rather than written.
    let launch = AdapterLaunch {
        node: String::new(),
        adapter_script: String::new(),
        socket_path: config.endpoint.clone(),
        token_file: config
            .session_dir
            .join("token")
            .to_string_lossy()
            .to_string(),
        cwd: config.cwd.to_string_lossy().to_string(),
        runtime_epoch: 1,
        env: Vec::new(),
        // No token means no daemon published one, and connecting anyway would
        // fail at the handshake with a proof mismatch that says nothing about
        // the real cause.
        token: bridge_token(),
        command: Vec::new(),
    };

    let session = match BridgeSession::connect(&launch) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("[bridge] connect failed: {error}");
            return None;
        }
    };

    let snapshot = match session.snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            eprintln!("[bridge] snapshot failed: {error}");
            return None;
        }
    };
    let bootstrap = bootstrap_from_snapshot(&snapshot);
    let inner = snapshot.get("snapshot").unwrap_or(&snapshot);
    Some(Connected {
        bootstrap,
        // The daemon mints the epoch and persists it, so a daemon that restarted
        // under a reconnecting shell is visible as a new authority rather than
        // looking like the same one. The shell used to hardcode 1 here, which
        // made the client's epoch rule dead code.
        runtime_epoch: session.epoch(),
        authority_epoch: inner
            .get("runtimeEpoch")
            .and_then(Value::as_u64)
            .unwrap_or(1),
        state_version: inner
            .get("stateVersion")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        instance_id: session.instance_id().to_string(),
        session,
    })
}

/// Keep a session alive for as long as the shell runs.
///
/// The daemon owns the adapter, so a session ending is usually the adapter
/// restarting underneath it — which is recoverable. Without this loop a
/// crash-looping runtime looks like a dead app: every command fails, the
/// renderer draws its error state, and nothing ever tries again.
///
/// Backoff is bounded and the loop never gives up: the shell is open, so the
/// user is still waiting. The cap is what stops a daemon that is down from
/// turning into a connect storm.
pub fn supervise(
    config: ConnectionConfig,
    inner: Arc<SessionInner>,
    sink: impl Fn() -> pump::EventSink + Send + Sync + 'static,
) {
    std::thread::spawn(move || {
        let mut backoff = Backoff::new();
        loop {
            let Some(connected) = connect_with(&config) else {
                std::thread::sleep(backoff.after_failure());
                continue;
            };
            backoff.reset();
            // The snapshot travels with the pump so `home.get` can assemble a
            // read model around it without reaching into session state.
            let snapshot = connected
                .bootstrap
                .get("snapshot")
                .cloned()
                .unwrap_or(Value::Null);
            let (jobs, pump_thread) = pump::spawn_pump(
                connected.session,
                sink(),
                connected.runtime_epoch,
                connected.authority_epoch,
                connected.state_version,
                connected.instance_id,
                snapshot,
            );
            inner.install(connected.bootstrap, jobs);
            eprintln!("[bridge] session {} established", inner.generation.load(Ordering::Relaxed));

            // The pump ends when the session does. Joining it is what makes the
            // next iteration's connect a reconnect rather than a second
            // concurrent session.
            let reason = pump_thread.join().ok().flatten();
            match reason {
                Some(reason) => eprintln!("[bridge] session ended: {reason}; reconnecting"),
                // The job channel was closed, which is `close`: the shell is
                // going away, so there is nothing to reconnect to.
                None => return,
            }
        }
    });
}

/// How long to wait before the next connect attempt.
///
/// Doubling from 250 ms to a 5 s ceiling. No jitter: there is exactly one
/// shell, so there is nobody to spread out against, and `std` has no RNG.
pub struct Backoff {
    delay: Duration,
}

const BACKOFF_CEILING: Duration = Duration::from_secs(5);

impl Backoff {
    pub fn new() -> Self {
        Self {
            delay: Duration::from_millis(250),
        }
    }

    /// Wait this long after a failed attempt.
    pub fn after_failure(&mut self) -> Duration {
        let waited = self.delay;
        self.delay = (self.delay * 2).min(BACKOFF_CEILING);
        waited
    }

    /// A connection worked, so the next failure starts from the beginning.
    pub fn reset(&mut self) {
        self.delay = Duration::from_millis(250);
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backoff_doubles_and_caps() {
        let mut backoff = Backoff::new();
        assert_eq!(backoff.after_failure(), Duration::from_millis(250));
        assert_eq!(backoff.after_failure(), Duration::from_millis(500));
        assert_eq!(backoff.after_failure(), Duration::from_secs(1));
        assert_eq!(backoff.after_failure(), Duration::from_secs(2));
        assert_eq!(backoff.after_failure(), Duration::from_secs(4));
        // The ceiling holds from here on, however many failures follow.
        for _ in 0..10 {
            assert_eq!(backoff.after_failure(), Duration::from_secs(5));
        }
    }

    #[test]
    fn a_connection_that_worked_resets_the_backoff() {
        let mut backoff = Backoff::new();
        for _ in 0..5 {
            backoff.after_failure();
        }
        assert_eq!(backoff.after_failure(), Duration::from_secs(5));
        backoff.reset();
        assert_eq!(backoff.after_failure(), Duration::from_millis(250));
    }

    #[test]
    fn installing_a_session_bumps_the_generation() {
        let inner = Arc::new(SessionInner {
            bootstrap: Mutex::new(None),
            window: Arc::new(Mutex::new(None)),
            jobs: Mutex::new(None),
            generation: AtomicU64::new(0),
        });
        let (sender, _receiver) = mpsc::channel::<Job>();
        inner.install(serde_json::json!({ "generation": 1 }), sender);
        assert_eq!(inner.generation.load(Ordering::Relaxed), 1);
        assert!(inner.bootstrap.lock().unwrap().is_some());

        // A reconnect replaces both, and the renderer can see that it happened.
        let (sender, _receiver) = mpsc::channel::<Job>();
        inner.install(serde_json::json!({ "generation": 2 }), sender);
        assert_eq!(inner.generation.load(Ordering::Relaxed), 2);
        assert_eq!(
            inner.bootstrap.lock().unwrap().as_ref().and_then(|b| b.get("generation")),
            Some(&serde_json::json!(2))
        );
    }
}

/// The bridge token. The daemon writes the same value into the token file it
/// owns, so a surface that already knows it does not have to read the file.
/// The bridge token. The daemon writes the same value into the token file it
/// owns, so a surface that already knows it does not have to read the file.
/// The bridge token the daemon published, or `None` when it cannot be read.
///
/// Read from the daemon's own state directory rather than compiled in: the
/// value is minted per daemon start, so a surface authenticates against the
/// daemon it actually reached. `None` means no daemon has published one, and
/// the caller reports that instead of guessing.
fn bridge_token() -> Option<String> {
    step_studio_bridge::token::read_token_file(&host::state::token_path())
}

/// Make sure a daemon is answering on `endpoint`, starting one when it is not.
///
/// Returns false when no daemon could be reached, in which case the caller keeps
/// reporting the unavailable shape rather than pretending to be connected.
fn ensure_daemon(
    endpoint: &str,
    cwd: &Path,
    repo_root: &Path,
    step_command: &str,
    session_dir: &Path,
) -> bool {
    let target = step_studio_bridge::transport::endpoint_for_bind(endpoint);
    if step_studio_bridge::transport::connect(&target).is_ok() {
        return true;
    }
    if !spawn_daemon(endpoint, cwd, repo_root, step_command, session_dir) {
        return false;
    }

    // Poll rather than block: the daemon has to spawn the adapter and complete a
    // handshake with it before it can answer anybody, and that is not instant.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if step_studio_bridge::transport::connect(&target).is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
    }

    // The launcher counts, not the daemon: a daemon that died cannot record its
    // own failure, and the budget has to survive the restart it is meant to
    // stop.
    eprintln!("[bridge] the host daemon did not come up");
    let mut state = host::state::load();
    if state.breaker.record_failure(
        host::state::now_secs(),
        "the daemon did not answer after 30s",
    ) {
        eprintln!(
            "[bridge] the daemon breaker is open after {} failed starts; \
             run `step-studio-host reset` to try again",
            state.breaker.attempts
        );
    }
    let _ = host::state::save(&state);
    false
}

/// Start the daemon and leave it running.
///
/// Detached, because the daemon has to outlive the shell: a child in the shell's
/// process group would be killed by the same signal that ends the shell, which
/// is the whole thing M2 removes. Stdio goes to a file, since a daemon with no
/// terminal has nowhere to write and a GUI shell has none to give it.
fn spawn_daemon(
    endpoint: &str,
    cwd: &Path,
    repo_root: &Path,
    step_command: &str,
    session_dir: &Path,
) -> bool {
    let Some(binary) = daemon_binary(repo_root) else {
        eprintln!("[bridge] no step-studio-host binary found");
        return false;
    };
    let log = std::fs::File::create(session_dir.join("daemon.log")).unwrap_or_else(|_| {
        std::fs::File::create(std::env::temp_dir().join("step-studio-daemon.log"))
            .expect("a log file")
    });
    let mut command = std::process::Command::new(&binary);
    command
        .arg("--endpoint")
        .arg(endpoint)
        .arg("--adapter-endpoint")
        .arg(
            session_dir
                .join("bridge.sock")
                .to_string_lossy()
                .to_string(),
        )
        .arg("--cwd")
        .arg(cwd)
        .arg("--repo-root")
        .arg(repo_root)
        .arg("--step-cmd")
        .arg(step_command);
    #[cfg(unix)]
    {
        // A new session, so a SIGHUP to the shell's terminal does not take the
        // daemon with it.
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command.stdout(log.try_clone().expect("the log duplicates"));
    command.stderr(log);
    match command.spawn() {
        Ok(_) => true,
        Err(error) => {
            eprintln!("[bridge] could not start the daemon: {error}");
            false
        }
    }
}

/// Where the daemon lives: beside the shell in a packaged build, under
/// `target/` in a checkout. The first hit wins, because the candidates overlap.
fn daemon_binary(repo_root: &Path) -> Option<PathBuf> {
    let beside_exe = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("step-studio-host")));
    let candidates = beside_exe.into_iter().chain(
        ["release", "debug"]
            .map(|profile| repo_root.join(format!("target/{profile}/step-studio-host"))),
    );
    candidates.into_iter().find(|candidate| candidate.is_file())
}

/// Translate a Studio snapshot into the client contract's `ClientBootstrap`.
///
/// The two are different shapes and the renderer validates the contract
/// version, so returning the snapshot verbatim fails with "Client contract
/// mismatch". `session.snapshot()` wraps the runtime's snapshot under a
/// `snapshot` key; the runtime side is built in
/// `crates/step-studio-runtime/src/snapshot.rs`.
///
/// `surface` and `capabilityManifest` are deliberately minimal in M1.2a: the
/// terminal and chrome bridges that would justify them land in M1.2c, so
/// claiming them now would make the renderer offer actions that fail.
fn bootstrap_from_snapshot(response: &serde_json::Value) -> serde_json::Value {
    let snapshot = response.get("snapshot").unwrap_or(response);
    // The Studio snapshot IS the OperatorStateSnapshot the renderer keeps in
    // `entities.snapshot`. Leaving it out is not a smaller payload: with
    // `snapshot` absent the client holds null forever, `snapshotReady` stays
    // false (`App.tsx:4205`) and the composer never enables.
    let state_version = snapshot
        .get("stateVersion")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let runtime_id = snapshot
        .get("runtimeId")
        .and_then(|v| v.as_str())
        .unwrap_or("step-runtime");
    let runtime_epoch = snapshot
        .get("runtimeEpoch")
        .and_then(|v| v.as_u64())
        .unwrap_or(1);
    let session_id = snapshot
        .get("sessionId")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    let mut selected = serde_json::json!({});
    if let Some(model) = snapshot.get("model") {
        if let Some(selector) = model.get("selector").and_then(|v| v.as_str()) {
            selected["model"] = serde_json::json!(selector);
        }
        if let Some(thinking) = model.get("thinking").and_then(|v| v.as_str()) {
            selected["thinking"] = serde_json::json!(thinking);
        }
    }

    serde_json::json!({
        "contractVersion": CLIENT_CONTRACT_VERSION,
        "authority": {
            "authorityId": runtime_id,
            "authorityEpoch": runtime_epoch,
        },
        "runtime": {
            // The shell spawned the adapter, which is what "managed" means.
            "status": "connected",
            "classification": "managed",
            "runtimeId": runtime_id,
            "runtimeEpoch": runtime_epoch,
        },
        "surface": {
            "terminalAttach": false,
            "fileReveal": false,
            "previewInput": false,
            "openExternal": false,
        },
        // Derived from the dispatch table, so the manifest and what the shell
        // can actually do cannot disagree. An empty list would leave the
        // renderer's `can()` false for everything and the composer dead.
        "capabilityManifest": pump::capability_manifest(),
        "commandManifestHash": pump::capability_manifest()["hash"]
            .as_str()
            .unwrap_or("step-studio")
            .to_string(),
        "selected": selected,
        "snapshot": snapshot,
        "stateVersion": state_version,
        // The subscription resume point. The pump's first event carries cursor
        // 1, so starting here leaves no gap for the client to resync on.
        "cursor": 0,
        "sessionId": session_id,
    })
}

// --- the Tauri commands ---------------------------------------------------

#[tauri::command]
pub fn bootstrap(state: tauri::State<'_, SessionState>) -> serde_json::Value {
    // A real snapshot when the session is up, the honest unavailable shape
    // when it is not.
    let cached = state
        .inner
        .bootstrap
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match cached.as_ref() {
        Some(snapshot) => snapshot.clone(),
        None => {
            serde_json::to_value(unavailable_bootstrap()).unwrap_or_else(|_| serde_json::json!({}))
        }
    }
}

/// Send one operation to the pump and wait for its answer.
///
/// The wait is bounded by the operation itself: a turn is `Dispatch::Turn`, so
/// it is answered the moment the adapter accepts it and never blocks here.
fn round_trip(
    state: &SessionState,
    name: &str,
    input: Value,
    request_id: &str,
    want: pump::Want,
) -> Result<Value, IpcError> {
    let (operation, dispatch) =
        pump::resolve_operation(name, &input).ok_or_else(|| IpcError::unavailable(name))?;
    let sender = state
        .inner
        .jobs
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .ok_or_else(IpcError::gone)?;
    let (reply, inbox) = mpsc::channel();
    sender
        .send(Job {
            request_id: request_id.to_string(),
            command_name: name.to_string(),
            operation,
            dispatch,
            want,
            reply,
        })
        .map_err(|_| IpcError::gone())?;
    let answer = inbox.recv().map_err(|_| IpcError::gone())?;
    answer.map_err(IpcError::raw)
}

#[tauri::command]
pub fn query(
    state: tauri::State<'_, SessionState>,
    request: serde_json::Value,
) -> Result<serde_json::Value, IpcError> {
    let name = request
        .get("queryName")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let input = request.get("input").cloned().unwrap_or(Value::Null);
    let payload = round_trip(&state, name, input, &next_query_id(), pump::Want::Result)?;
    Ok(json!({ "ok": true, "queryName": name, "result": shape_query_result(name, payload) }))
}

/// Give a query the result shape the facade would have returned.
///
/// Three queries need it. `session.state` answers `{snapshot}` while the facade
/// hands back the snapshot itself. `conversation.open` is assembled rather than
/// passed through: the adapter reads a transcript page, and the renderer also
/// needs the resolved target and the live-replay watermark. `usage.get` is
/// reshaped because the CLI's panel is ANSI text — see [`usage_read_model`].
fn shape_query_result(name: &str, payload: Value) -> Value {
    if name == "session.state" {
        return payload.get("snapshot").cloned().unwrap_or(payload);
    }
    if name == "usage.get" {
        return usage_read_model(&payload);
    }
    if name == "models.get" {
        return model_config_read_model(&payload);
    }
    if name == "mcp.get" {
        return mcp_read_model(&payload);
    }
    if name == "skills.get" {
        return skills_read_model(&payload);
    }
    if name == "git.repository.get"
        || name == "git.log.list"
        || name == "git.branches.list"
        || name == "git.diff.get"
        || name == "workspace.fileTree"
    {
        // Already in the facade's shape: the shell builds `GitRepositoryReadModel`
        // directly, so there is nothing to translate. The arm exists so a reader
        // looking for the git reshape finds it, rather than concluding the
        // payload was passed through by accident.
        return payload;
    }
    if name != "conversation.open" {
        return payload;
    }
    // The adapter reports one session per process, so the page's own id is the
    // conversation id. The renderer drops live events whose sessionId differs
    // from `target.conversationSessionId`, so these must agree.
    let session_id = payload
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    json!({
        "target": {
            "kind": "session",
            "sessionId": session_id,
            "conversationSessionId": session_id,
        },
        "page": payload,
        // No replay: the page was just read from the live session, so starting
        // the watermark at 0 lets every subsequent event apply.
        "live": { "status": "complete", "watermark": 0, "events": [] },
    })
}

/// Reshape the adapter's `/usage` panel into the facade's read model.
///
/// What the CLI sends is ANSI text. It carries the totals (total tokens, total
/// chats, active days) and a heatmap whose cells are intensity buckets — and no
/// per-hour, per-model, or exact per-day numbers. So the structured arrays stay
/// empty with a reason that says what is missing, and the panel travels
/// verbatim: filling `days` with buckets would be inventing token counts, and
/// an empty heatmap with an honest reason is better than a full-looking one that
/// is not true.
fn usage_read_model(payload: &Value) -> Value {
    let panel = payload.get("panel").and_then(Value::as_str).unwrap_or("");
    let generated_at = pump::iso_utc(pump::now_millis());
    if panel.is_empty() {
        return json!({
            "generatedAt": generated_at,
            "days": [], "models": [], "byModel": [], "hours": [],
            "unavailableReason": "the CLI did not return a usage panel",
        });
    }
    json!({
        "generatedAt": generated_at,
        "days": [], "models": [], "byModel": [], "hours": [],
        "unavailableReason":
            "Step 的 /usage 面板以 ANSI 文本回传：含总量与热度分档，不含按小时、按模型与精确按日的 token 数。",
        "panel": panel,
    })
}

/// Reshape the adapter's model catalog into the facade's config read model.
///
/// What the adapter has is a catalog: provider, id, selector, name, and the
/// execution metadata. What the facade's model also carries is OMP's
/// multi-provider machinery — roles, presets, cycle order, fallback chains,
/// login providers, the web-search routing table — and Step's RPC exposes none
/// of it. So those stay empty with a reason that names them, rather than being
/// invented: a role the CLI has no concept of cannot be listed, and a fallback
/// chain nobody configured cannot be shown.
fn model_config_read_model(payload: &Value) -> Value {
    let entries = payload
        .get("models")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();

    let available_models: Vec<Value> = entries
        .iter()
        .map(|entry| {
            json!({
                "provider": entry.get("provider").and_then(Value::as_str).unwrap_or(""),
                "id": entry.get("id").and_then(Value::as_str).unwrap_or(""),
                "selector": entry.get("selector").and_then(Value::as_str).unwrap_or(""),
                "name": entry.get("name").and_then(Value::as_str).unwrap_or(""),
                "reasoning": entry.get("reasoning").and_then(Value::as_bool).unwrap_or(false),
                "contextWindow": entry.get("contextWindow").and_then(Value::as_i64),
                "maxTokens": entry.get("maxTokens").and_then(Value::as_i64),
            })
        })
        .collect();

    // One record per distinct provider, carrying the models that belong to it.
    //
    // `models` is required by `ModelProviderRecord`, and the composer's picker
    // maps over it — a record without it throws once `availableModels` is
    // non-empty, which is exactly the case that matters.
    let mut providers: Vec<Value> = Vec::new();
    for entry in entries {
        let id = entry.get("provider").and_then(Value::as_str).unwrap_or("");
        if id.is_empty() || providers.iter().any(|p| p.get("id") == Some(&json!(id))) {
            continue;
        }
        let mine: Vec<Value> = entries
            .iter()
            .filter(|other| other.get("provider").and_then(Value::as_str) == Some(id))
            .map(|other| {
                json!({
                    "id": other.get("id").and_then(Value::as_str).unwrap_or(""),
                    "selector": other.get("selector").and_then(Value::as_str).unwrap_or(""),
                    "name": other.get("name").and_then(Value::as_str).unwrap_or(""),
                })
            })
            .collect();
        providers.push(json!({
            "id": id,
            // The catalog gives no display name, so the id is the name. Better
            // than inventing one.
            "name": id,
            "source": "runtime",
            "status": "available",
            "statusDetail": "Step CLI catalog",
            "api": entry.get("api").and_then(Value::as_str).unwrap_or(""),
            "local": false,
            "enabled": true,
            "models": mine,
            // The shell never sees the CLI's credentials, so it asserts nothing
            // about them: `hasSecret: false` would be a claim, not a fact.
            "auth": { "type": "unknown", "hasSecret": false },
        }));
    }

    json!({
        "providers": providers,
        // No OMP role/preset/cycle machinery in Step's RPC.
        "presets": [],
        "roles": [],
        "cycleOrder": [],
        "availableModels": available_models,
        "loginProviders": [],
        "generatedModelsYml": "",
        "generatedConfigYml": "",
        "runtimeEffectHint": "本壳不写 Step 配置；模型选择直接经 CLI catalog。",
        "loginAvailable": false,
        "ompAvailable": false,
        "modelRoleStorage": "global",
        "projectScopeAvailable": false,
        "modelProviderOrder": providers.iter().filter_map(|p| p.get("id").and_then(Value::as_str)).map(str::to_string).collect::<Vec<_>>(),
        "fallbackChains": {},
        "fallbackRevertPolicy": "never",
        // Every member `WebSearchConfigReadModel` requires. The shell has no
        // web-search configuration to report, so each is the empty value rather
        // than a claim about the CLI's settings.
        "webSearch": {
            "enabled": false,
            "order": [],
            "exclude": [],
            "timeoutSeconds": 0,
            "geminiModel": "",
            "providers": [],
            "advanced": {},
        },
        "unavailableReason":
            "Step RPC 只暴露模型 catalog；角色、预设、循环顺序、回退链、登录供应商、webSearch 配置与各供应商的              本地/启用/凭据状态均无对应数据。providers[].local、enabled、auth 是占位而非实测，             auth.type 为 unknown 即表示本壳看不到凭据。",
    })
}

/// Reshape the adapter's extension inventory into the facade's MCP read model.
///
/// The adapter reports one entry per *extension*, each carrying the MCP servers
/// it declares; the facade's model is one row per *server*. So the reshape
/// flattens — a plugin declaring three servers becomes three rows, which is what
/// the page lists.
fn mcp_read_model(payload: &Value) -> Value {
    let mut servers: Vec<Value> = Vec::new();
    for extension in payload.get("extensions").and_then(Value::as_array).into_iter().flatten() {
        // `installed` separates a marketplace catalog entry from a `plugins/`
        // install — not project from user. So it cannot produce a scope, and a
        // catalog entry that was never installed is not a configured server at
        // all. Those are dropped rather than reported as enabled.
        if extension.get("installed").and_then(Value::as_bool) == Some(false) {
            continue;
        }
        let scope = "user";
        let label = "用户";
        for server in extension.get("mcpServers").and_then(Value::as_array).into_iter().flatten() {
            let name = server.get("name").and_then(Value::as_str).unwrap_or("");
            if name.is_empty() {
                continue;
            }
            servers.push(json!({
                "name": name,
                // Every server the adapter reports came from a manifest's
                // `mcpServers`, which is a stdio launch. `http`/`sse` would need
                // a transport field the manifest does not carry, and guessing
                // one from the URL shape would be a guess.
                "transport": "stdio",
                // The adapter reports what is configured, not what is running:
                // there is no connection state here, so `enabled` says the
                // entry exists rather than that it answered.
                "enabled": true,
                "status": "enabled",
                "sourceLabel": label,
                "scope": scope,
            }));
        }
    }
    json!({
        "servers": servers,
        "warnings": [],
        "generatedAt": pump::iso_utc(pump::now_millis()),
    })
}

/// Reshape the CLI's command catalog into the facade's skills read model.
///
/// The adapter's own comment says it: the command catalog *is* the skills
/// surface, because Step has no separate skills registry to read. So the
/// reshape maps one to the other rather than inventing a second source.
fn skills_read_model(payload: &Value) -> Value {
    let mut skills: Vec<Value> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for command in payload.get("commands").and_then(Value::as_array).into_iter().flatten() {
        let Some(name) = command.get("name").and_then(Value::as_str) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        if !seen.insert(name.to_string()) {
            // Two sources declaring the same slash command is a real collision,
            // and `warnings` is where the contract says it goes. Listing both
            // rows silently makes the page show a duplicate with no explanation.
            warnings.push(format!("duplicate command name: {name}"));
            continue;
        }
        skills.push(json!({
                "name": name,
                "desc": command.get("description").and_then(Value::as_str).unwrap_or(""),
                // The catalog is "slash commands from every source", so `builtin`
                // would be wrong for all of them. There is no scope in the data;
                // `global` is the least wrong, because these come from the
                // user's own CLI configuration rather than from a workspace
                // directory.
                "scope": "global",
                "sourceKind": "native",
                "sourceLabel": "Step",
                "enabled": true,
                "hide": false,
            }));
    }
    json!({
        "skills": skills,
        "plugins": [],
        "warnings": warnings,
        "generatedAt": pump::iso_utc(pump::now_millis()),
    })
}

#[cfg(test)]
mod usage_tests {
    use super::*;

    #[test]
    fn a_panel_travels_verbatim_and_the_arrays_stay_honestly_empty() {
        let model = usage_read_model(&json!({ "panel": "Usage\nTotal tokens 1,234" }));
        assert_eq!(
            model.get("panel").and_then(Value::as_str),
            Some("Usage\nTotal tokens 1,234")
        );
        // Not fabricated from the panel's intensity buckets.
        assert_eq!(model.get("days").and_then(Value::as_array).map(Vec::len), Some(0));
        assert_eq!(model.get("hours").and_then(Value::as_array).map(Vec::len), Some(0));
        assert_eq!(model.get("byModel").and_then(Value::as_array).map(Vec::len), Some(0));
        let reason = model.get("unavailableReason").and_then(Value::as_str).unwrap_or("");
        assert!(reason.contains("ANSI"), "the reason says what is missing: {reason}");
    }

    #[test]
    fn a_missing_panel_is_reported_rather_than_shaped_into_nothing() {
        let model = usage_read_model(&json!({}));
        assert!(model.get("panel").is_none());
        assert_eq!(
            model.get("unavailableReason").and_then(Value::as_str),
            Some("the CLI did not return a usage panel")
        );
    }

    #[test]
    fn a_model_catalog_becomes_available_models_and_providers() {
        let model = model_config_read_model(&json!({
            "models": [
                { "provider": "local-gateway", "id": "stepfun-cn/step-3.7-flash",
                  "selector": "local-gateway/stepfun-cn/step-3.7-flash",
                  "name": "Step 3.7 Flash", "reasoning": true,
                  "contextWindow": 128000, "maxTokens": 8192, "api": "openai-completions" },
                { "provider": "local-gateway", "id": "zed/claude-haiku-5-5",
                  "selector": "local-gateway/zed/claude-haiku-5-5",
                  "name": "Claude Haiku 5.5", "reasoning": false },
            ],
        }));
        let available = model.get("availableModels").and_then(Value::as_array).expect("availableModels");
        assert_eq!(available.len(), 2, "every catalog entry is an available model");
        assert_eq!(
            available[0].get("selector").and_then(Value::as_str),
            Some("local-gateway/stepfun-cn/step-3.7-flash")
        );
        assert_eq!(available[0].get("contextWindow").and_then(Value::as_i64), Some(128000));
        assert_eq!(available[0].get("reasoning").and_then(Value::as_bool), Some(true));

        // One provider record per distinct provider, not one per model.
        let providers = model.get("providers").and_then(Value::as_array).expect("providers");
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].get("id").and_then(Value::as_str), Some("local-gateway"));
        assert_eq!(
            model.get("modelProviderOrder").and_then(Value::as_array).map(Vec::len),
            Some(1)
        );
    }

    #[test]
    fn what_step_has_no_data_for_stays_empty_with_a_reason() {
        let model = model_config_read_model(&json!({ "models": [] }));
        for field in ["presets", "roles", "cycleOrder", "loginProviders"] {
            assert_eq!(
                model.get(field).and_then(Value::as_array).map(Vec::len),
                Some(0),
                "{field} has no Step data source, so it must not be invented"
            );
        }
        assert_eq!(model.get("loginAvailable").and_then(Value::as_bool), Some(false));
        assert_eq!(model.get("ompAvailable").and_then(Value::as_bool), Some(false));
        let reason = model.get("unavailableReason").and_then(Value::as_str).unwrap_or("");
        assert!(reason.contains("角色"), "the reason names what is missing: {reason}");
    }

    #[test]
    fn the_reshape_is_reachable_from_the_dispatch_table() {
        // The manifest is derived from the dispatch tables, so a query that
        // reshapes must also resolve — otherwise the renderer would offer it and
        // every call would be refused.
        let (operation, _) = pump::resolve_operation("usage.get", &json!({})).expect("resolves");
        assert_eq!(operation.get("kind").and_then(Value::as_str), Some("usage.panel"));
        let (operation, _) = pump::resolve_operation("models.get", &json!({})).expect("resolves");
        assert_eq!(
            operation.get("kind").and_then(Value::as_str),
            Some("session.models.list")
        );
        // The page's own query must stay refused rather than resolve to
        // something that cannot answer it.
        assert!(pump::resolve_operation("models.provider.list", &json!({})).is_none());
    }
}

#[tauri::command]
pub fn command(
    state: tauri::State<'_, SessionState>,
    request: serde_json::Value,
) -> Result<Value, IpcError> {
    let name = request
        .get("commandName")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let input = request.get("input").cloned().unwrap_or(Value::Null);
    let request_id = request
        .get("requestId")
        .and_then(Value::as_str)
        .map(str::to_string);
    // `Want::Ack`, not `Want::Result`: a command is answered with its ack, and
    // the result travels on the receipt the renderer waits for. Asking for the
    // result here resolves `client.command` with the payload — which carries no
    // `requestId` — so `waitReceipt(client, undefined, 60_000)` hangs for a
    // minute and then reports a timeout.
    round_trip(&state, &name, input, &request_id.unwrap_or_default(), pump::Want::Ack)
}

#[tauri::command]
pub fn subscribe(
    state: tauri::State<'_, SessionState>,
    window: tauri::WebviewWindow,
) -> Result<(), IpcError> {
    let mut slot = state
        .inner
        .window
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *slot = Some(window);
    Ok(())
}

#[tauri::command]
pub fn close(state: tauri::State<'_, SessionState>) -> Result<(), IpcError> {
    // Dropping the sender is what ends the pump: its `recv_timeout` then sees a
    // disconnected channel and returns, which drops the session. The session
    // owns no child — the daemon owns the adapter — so this closes a socket and
    // nothing else. The daemon keeps the conversation for its grace period.
    let sender = state
        .inner
        .jobs
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    drop(sender);
    Ok(())
}

#[cfg(test)]
mod capability_tests {
    use super::*;

    #[test]
    fn one_extension_with_three_servers_becomes_three_rows() {
        // The adapter reports per extension; the facade's model is per server.
        // A plugin declaring three servers must surface as three rows, because
        // that is what the page lists.
        let model = mcp_read_model(&json!({
            "extensions": [{
                "id": "playwright",
                "name": "Playwright",
                "installed": true,
                "mcpServers": [
                    { "name": "playwright", "command": "npx", "args": [] },
                    { "name": "playwright-two", "command": "npx", "args": [] },
                    { "name": "playwright-three", "command": "npx", "args": [] },
                ],
            }],
        }));
        let servers = model.get("servers").and_then(Value::as_array).expect("servers");
        assert_eq!(servers.len(), 3, "one row per server, not per extension");
        for server in servers {
            assert_eq!(server.get("transport").and_then(Value::as_str), Some("stdio"));
            assert_eq!(server.get("scope").and_then(Value::as_str), Some("user"));
            assert_eq!(server.get("status").and_then(Value::as_str), Some("enabled"));
        }
    }

    #[test]
    fn a_server_without_a_name_is_skipped_rather_than_listed_empty() {
        let model = mcp_read_model(&json!({
            "extensions": [{ "id": "x", "installed": true, "mcpServers": [{ "command": "npx" }] }],
        }));
        assert_eq!(
            model.get("servers").and_then(Value::as_array).map(Vec::len),
            Some(0),
            "a nameless server is not a row"
        );
    }

    #[test]
    fn the_command_catalog_becomes_the_skills_list() {
        // Step has no separate skills registry, so the command catalog is the
        // skills surface — the adapter's own comment says as much.
        let model = skills_read_model(&json!({
            "commands": [
                { "name": "usage", "description": "Show usage" },
                { "name": "models", "description": "Pick a model" },
                { "name": "", "description": "ignored" },
                // A second `usage` from another source: a real collision, which
                // belongs in `warnings` rather than as a silent duplicate row.
                { "name": "usage", "description": "Show usage again" },
            ],
        }));
        let skills = model.get("skills").and_then(Value::as_array).expect("skills");
        assert_eq!(skills.len(), 2, "the collision is not a second row");
        assert_eq!(skills[0].get("name").and_then(Value::as_str), Some("usage"));
        assert_eq!(skills[0].get("scope").and_then(Value::as_str), Some("global"));
        assert_eq!(skills[0].get("sourceLabel").and_then(Value::as_str), Some("Step"));
        // The facade has a plugins list too, and Step's catalog has no plugins
        // in it — so it is empty rather than filled with the same rows twice.
        assert_eq!(
            model.get("plugins").and_then(Value::as_array).map(Vec::len),
            Some(0)
        );
        let warnings = model.get("warnings").and_then(Value::as_array).expect("warnings");
        assert_eq!(warnings.len(), 1, "the collision is reported, not hidden");
        assert!(
            warnings[0].as_str().is_some_and(|w| w.contains("usage")),
            "the warning names the command: {warnings:?}"
        );
    }
}
