//! The resident host daemon.
//!
//! One daemon per user. It owns the Studio Bridge endpoint, owns the runtime
//! adapter behind it, and relays frames to whichever surface is attached — so
//! killing the shell does not kill the conversation.
//!
//! What is deliberately *not* here yet: the daemon does not terminate the
//! protocol, so it cannot multiplex sessions (M2.3) or answer the
//! models/usage/settings/diagnostics surfaces (M2.4). Until then it forwards
//! bytes and supervises.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use step_studio_bridge::client::AdapterLaunch;
use step_studio_bridge::frame::write_frame;
use step_studio_bridge::transport::{self, Accepted};
use step_studio_host::adapter::{self, AdapterHandle};
use step_studio_host::lifecycle::{self, Acquire, GraceClock, Liveness};
use step_studio_host::relay::{self, pump_frames};
use step_studio_host::state;
use step_studio_host::supervisor::{Next, Policy, Supervisor};

/// How long the daemon keeps running with no surface attached.
///
/// Measured from the moment the **last** surface disconnects, so a shell that
/// is mid-turn but still connected never has its turn cut short, and a shell
/// that died mid-turn lets the turn finish without it.
const DRAIN_GRACE: Duration = Duration::from_secs(600);

/// How long the adapter gets to finish an in-flight turn when the daemon is
/// going away.
const DRAIN_DEADLINE: Duration = Duration::from_secs(5);

/// Longest the supervisor sleeps between polls. Short enough that a draining
/// daemon notices promptly; long enough that an idle one is not spinning.
const POLL_SLICE: Duration = Duration::from_millis(250);

/// How long a connecting surface waits for an adapter to exist before being
/// turned away.
const ADAPTER_WAIT: Duration = Duration::from_secs(20);

struct Config {
    endpoint: String,
    adapter_endpoint: String,
    cwd: String,
    step_command: String,
    /// Where to find the runtime binary or its script, when it is not beside
    /// the daemon.
    repo_root: String,
    grace: Duration,
}

/// Everything the daemon's threads share.
struct Daemon {
    config: Config,
    started: Instant,
    epoch: u64,
    /// The adapter while one is alive. `None` between attempts.
    adapter: Mutex<Option<AdapterHandle>>,
    /// Where the adapter's pump thread puts frames for the active surface.
    ///
    /// One slot, not one per surface: the adapter broadcasts, and two surfaces
    /// reading one stream would split it between them. A second surface waits
    /// rather than competing.
    to_surface: Mutex<Option<Sender<Vec<u8>>>>,
    /// Set once the daemon has decided to go away, so the supervisor does not
    /// restart the adapter it just drained.
    stopping: AtomicBool,
    /// Set by a surface attaching. A halted supervisor is revived by a caller
    /// that actually wants the runtime, which is the only honest probe.
    retry_requested: AtomicBool,
    /// Whether a surface currently owns the relay.
    active: Mutex<bool>,
    /// Signalled when the active surface lets go.
    freed: Condvar,
    grace: Mutex<GraceClock>,
}

fn main() {
    // `reset` clears the daemon's restart budget. It exists because the breaker
    // the daemon checks at startup has no other way out: once it is open the
    // daemon refuses to start, and the only honest probe is a caller that asks.
    if std::env::args().nth(1).as_deref() == Some("reset") {
        match state::reset_breaker() {
            Ok(()) => {
                eprintln!("step-studio-host: the daemon breaker is clear");
                std::process::exit(0);
            }
            Err(error) => {
                eprintln!("step-studio-host: could not clear the breaker: {error}");
                std::process::exit(1);
            }
        }
    }

    let config = match parse_args(std::env::args().skip(1)) {
        Ok(config) => config,
        Err(message) => {
            eprintln!("step-studio-host: {message}");
            std::process::exit(2);
        }
    };

    match lifecycle::acquire(&config.endpoint) {
        Ok(Acquire::AlreadyRunning) => {
            // Not an error: the app was started twice. Say so and leave the
            // running daemon alone.
            eprintln!("step-studio-host: another daemon owns {}", config.endpoint);
            std::process::exit(0);
        }
        Ok(Acquire::Owned) => {}
        Err(error) => {
            eprintln!(
                "step-studio-host: could not take {}: {error}",
                config.endpoint
            );
            std::process::exit(1);
        }
    }

    if state::breaker_is_open() {
        // The daemon has failed to start enough times that relaunching it is
        // no longer a answer. This is a terminal state the surface shows with
        // a manual retry, not something to spin on.
        eprintln!(
            "step-studio-host: not starting; the daemon breaker is open ({}). \
             Clear it with `step-studio-host reset`.",
            state::load().breaker.last_error
        );
        std::process::exit(3);
    }

    // Minted here rather than by the shell, and persisted, so a reconnecting
    // client can tell this daemon from the one it was talking to. The shell
    // used to hardcode `1`, which made the client's epoch rule dead code.
    let epoch = state::next_epoch();
    // Read before `config` moves into the daemon: the grace period is a
    // deployment choice, not a constant, and the drain clock has to see the
    // one that was asked for.
    let grace = config.grace;
    let daemon = Arc::new(Daemon {
        config,
        started: Instant::now(),
        epoch,
        adapter: Mutex::new(None),
        to_surface: Mutex::new(None),
        stopping: AtomicBool::new(false),
        retry_requested: AtomicBool::new(false),
        active: Mutex::new(false),
        freed: Condvar::new(),
        grace: Mutex::new(GraceClock::new(grace)),
    });
    eprintln!(
        "step-studio-host: epoch {epoch}, endpoint {}",
        daemon.config.endpoint
    );

    let supervisor = std::thread::spawn({
        let daemon = Arc::clone(&daemon);
        move || supervise(daemon)
    });
    let watcher = std::thread::spawn({
        let daemon = Arc::clone(&daemon);
        move || watch(daemon)
    });

    serve(Arc::clone(&daemon));
    // The serve loop only returns when the daemon is stopping, which is the
    // watcher's decision. Wait for both so the process does not outlive the
    // drain it just started.
    let _ = watcher.join();
    let _ = supervisor.join();
    shutdown(&daemon);
}

/// Keep the adapter alive for as long as the daemon is.
fn supervise(daemon: Arc<Daemon>) {
    let mut supervisor = Supervisor::new(Policy::default());
    let mut halted_reported = false;
    loop {
        if daemon.stopping.load(Ordering::Relaxed) {
            return;
        }
        match supervisor.poll(daemon.started.elapsed()) {
            Next::Halted => {
                // The failure budget is spent. Report it once, then wait: the
                // only thing that revives this is a surface attaching and
                // asking for a retry, which is a caller that actually wants
                // the runtime.
                if !halted_reported {
                    halted_reported = true;
                    eprintln!(
                        "step-studio-host: adapter halted after {} failed starts; \
                         waiting for a surface to ask for a retry",
                        supervisor.failed_starts()
                    );
                }
                if daemon.retry_requested.swap(false, Ordering::Relaxed) {
                    supervisor.retry(daemon.started.elapsed());
                    halted_reported = false;
                    eprintln!("step-studio-host: retrying the adapter at a surface's request");
                }
                std::thread::sleep(POLL_SLICE);
            }
            Next::Wait(delay) => std::thread::sleep(delay.min(POLL_SLICE)),
            Next::Spawn => {}
        }

        let handle = match bring_up(&daemon) {
            Ok(parts) => parts,
            Err(error) => {
                eprintln!("step-studio-host: {error}");
                supervisor.on_exit(Duration::ZERO, daemon.started.elapsed());
                continue;
            }
        };
        let (handle, reader) = handle;
        let uptime_at_death = {
            let mut slot = daemon.adapter.lock().unwrap_or_else(|p| p.into_inner());
            *slot = Some(handle);
            drop(slot);
            // One pump thread per adapter, not per surface: it owns the read
            // half for the adapter's whole life, and hands frames to whichever
            // surface is currently active.
            spawn_pump(Arc::clone(&daemon), reader);
            wait_for_adapter(&daemon)
        };
        let uptime = uptime_at_death.unwrap_or(Duration::ZERO);
        if daemon.stopping.load(Ordering::Relaxed) {
            return;
        }
        supervisor.on_exit(uptime, daemon.started.elapsed());
    }
}

/// Spawn the adapter and complete the handshake.
fn bring_up(daemon: &Daemon) -> Result<(AdapterHandle, transport::Stream), adapter::AdapterError> {
    let cwd = std::path::PathBuf::from(&daemon.config.cwd);
    let launch = AdapterLaunch {
        // Empty: the daemon spawns the adapter itself, so `node` is only used
        // when the runtime is a Node script rather than the native binary.
        node: String::new(),
        adapter_script: String::new(),
        socket_path: daemon.config.adapter_endpoint.clone(),
        token_file: cwd
            .join(".step-studio")
            .join("token")
            .to_string_lossy()
            .to_string(),
        cwd: daemon.config.cwd.clone(),
        runtime_epoch: daemon.epoch,
        env: vec![
            (
                "STEP_STUDIO_STEP_CMD".to_string(),
                daemon.config.step_command.clone(),
            ),
            (
                "STEP_STUDIO_RUNTIME_CMD".to_string(),
                daemon.config.step_command.clone(),
            ),
            // Resume the previous session rather than starting a fresh one.
            ("STEP_STUDIO_SESSION".to_string(), "continue".to_string()),
            // The daemon is this adapter's only client, so the adapter's
            // lifetime is the daemon's. Without this the adapter outlives a
            // daemon that was killed, still holding `step` and with it the
            // user's session.
            (
                "STEP_STUDIO_EXIT_ON_DISCONNECT".to_string(),
                "1".to_string(),
            ),
        ],
        // The daemon writes the token file, because it launches the adapter.
        token: None,
        command: native_runtime_command(&daemon.config.repo_root),
    };
    adapter::launch(&launch, &adapter::challenge_for(daemon.epoch))
}

/// Where the runtime binary lives: beside the daemon in a packaged build, under
/// `target/` in a checkout.
fn native_runtime_command(repo_root: &str) -> Vec<String> {
    let beside_exe = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("step-studio-runtime")));
    let root = std::path::Path::new(repo_root);
    let candidates = beside_exe.into_iter().chain(
        ["release", "debug"]
            .map(|profile| root.join(format!("target/{profile}/step-studio-runtime"))),
    );
    for candidate in candidates {
        if candidate.is_file() {
            return vec![candidate.to_string_lossy().to_string()];
        }
    }
    Vec::new()
}

/// Own the adapter's read half for as long as it lives.
fn spawn_pump(daemon: Arc<Daemon>, mut reader: transport::Stream) {
    std::thread::spawn(move || {
        let outcome = relay::pump_frames_to(&mut reader, |frame| {
            // Nobody attached is a normal state, not an error: the frames are
            // dropped, because there is nobody to deliver them to.
            let slot = daemon.to_surface.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(sender) = slot.as_ref() {
                let _ = sender.send(frame);
            }
            Ok(())
        });
        if let Err(error) = outcome {
            eprintln!("step-studio-host: adapter read ended: {error}");
        }
    });
}

/// Block until the adapter exits, and return how long it had been up.
///
/// The slot is locked only for the `try_wait` call. Holding it across the poll
/// would deadlock every surface, whose relay needs the same slot to find the
/// adapter at all.
fn wait_for_adapter(daemon: &Daemon) -> Option<Duration> {
    loop {
        let exited = {
            let mut slot = daemon.adapter.lock().unwrap_or_else(|p| p.into_inner());
            let handle = slot.as_mut()?;
            match handle.child.try_wait() {
                // `started` is when this adapter came up, so the uptime the
                // supervisor sees is this adapter's, not the daemon's.
                Ok(Some(_)) => Some(handle.started.elapsed()),
                _ => None,
            }
        };
        match exited {
            Some(uptime) => return Some(uptime),
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

// --- serving surfaces -----------------------------------------------------

/// Accept surfaces for as long as the daemon runs.
///
/// The two arms differ because the platforms do: a Unix listener accepts
/// repeatedly, while a Windows single-instance pipe serves exactly one client
/// and has to be recreated for the next. Same shape as the runtime's own serve
/// loop (`crates/step-studio-runtime/src/bridge_server.rs`).
fn serve(daemon: Arc<Daemon>) {
    let endpoint = transport::endpoint_for_bind(&daemon.config.endpoint);
    #[cfg(unix)]
    {
        let listener = match lifecycle::bind(&endpoint) {
            Ok(Some(listener)) => listener,
            // Somebody else won the race to bind. Not an error.
            Ok(None) => {
                eprintln!(
                    "step-studio-host: another daemon owns {}",
                    daemon.config.endpoint
                );
                return;
            }
            Err(error) => {
                eprintln!(
                    "step-studio-host: could not bind {}: {error}",
                    daemon.config.endpoint
                );
                return;
            }
        };
        loop {
            if daemon.stopping.load(Ordering::Relaxed) {
                return;
            }
            match transport::accept(listener.try_clone().expect("the listener duplicates")) {
                Ok(connection) => {
                    let daemon = Arc::clone(&daemon);
                    std::thread::spawn(move || serve_surface(connection, daemon));
                }
                Err(error) => eprintln!("step-studio-host: accept failed: {error}"),
            }
        }
    }
    #[cfg(not(unix))]
    {
        let mut listener = match lifecycle::bind(&endpoint) {
            Ok(Ok(listener)) => listener,
            Ok(Err(Acquire::AlreadyRunning)) => {
                eprintln!(
                    "step-studio-host: another daemon owns {}",
                    daemon.config.endpoint
                );
                return;
            }
            Err(error) => {
                eprintln!(
                    "step-studio-host: could not bind {}: {error}",
                    daemon.config.endpoint
                );
                return;
            }
        };
        loop {
            if daemon.stopping.load(Ordering::Relaxed) {
                return;
            }
            let connection = match transport::accept(listener) {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("step-studio-host: accept failed: {error}");
                    return;
                }
            };
            // Re-bind before serving, so the next client is not kept waiting
            // behind this one.
            listener = match transport::bind(&endpoint) {
                Ok(next) => next,
                Err(error) => {
                    eprintln!("step-studio-host: rebind failed: {error}");
                    return;
                }
            };
            let daemon = Arc::clone(&daemon);
            std::thread::spawn(move || serve_surface(connection, daemon));
        }
    }
}

/// Relay one surface's frames to and from the adapter.
///
/// One surface at a time. The adapter broadcasts its events on one connection
/// with no addressing, so two surfaces reading that stream would split it
/// between them and neither would settle its commands. A second surface waits
/// rather than competing — that is the whole reason for the `active` flag, and
/// the next person to read "the daemon should serve many surfaces" needs to
/// know that multiplexing is M2.3's work, not a missing feature here.
///
/// The handshake is the one thing the daemon answers itself. The adapter
/// handshakes exactly once per connection, so a surface's hello relayed to it
/// would be ignored and the surface would wait forever for an answer that
/// never comes. Everything after the hello goes through as the original bytes.
fn serve_surface(connection: Accepted, daemon: Arc<Daemon>) {
    let stream = connection.into_connection();

    // Wait for an adapter. Refusing outright would turn a restart's backoff
    // into a hard failure for whoever was connecting.
    let waited = Instant::now();
    loop {
        if daemon.stopping.load(Ordering::Relaxed) {
            return;
        }
        if daemon
            .adapter
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
        {
            break;
        }
        if waited.elapsed() >= ADAPTER_WAIT {
            eprintln!(
                "step-studio-host: no adapter after {}s; turning the surface away",
                ADAPTER_WAIT.as_secs()
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // Take the relay. Somebody else may already own it.
    {
        let mut active = daemon.active.lock().unwrap_or_else(|p| p.into_inner());
        while *active && !daemon.stopping.load(Ordering::Relaxed) {
            active = daemon.freed.wait(active).unwrap_or_else(|p| p.into_inner());
        }
        if daemon.stopping.load(Ordering::Relaxed) {
            return;
        }
        *active = true;
    }
    daemon
        .grace
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .attach();
    // A surface attaching is a caller that wants the runtime, which is the only
    // thing that revives a halted supervisor.
    daemon.retry_requested.store(true, Ordering::Relaxed);

    // Publish the sender the adapter's pump thread delivers to. Dropping it is
    // what ends the writer thread below, which is what closes the socket.
    let (sender, receiver) = mpsc::channel::<Vec<u8>>();
    {
        let mut slot = daemon.to_surface.lock().unwrap_or_else(|p| p.into_inner());
        *slot = Some(sender.clone());
    }

    // The writer owns the surface's write half and drains until the sender is
    // dropped, which happens when this function returns.
    let writer = std::thread::spawn({
        let mut surface_writer = match stream.try_clone() {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!("step-studio-host: {error}");
                daemon.let_go();
                return;
            }
        };
        move || {
            use std::io::Write;
            while let Ok(frame) = receiver.recv() {
                if surface_writer.write_all(&frame).is_err() {
                    return;
                }
            }
        }
    });

    let mut surface_reader = match stream.try_clone() {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("step-studio-host: {error}");
            daemon.let_go();
            return;
        }
    };

    // Borrowed, not held: the slot stays free for the supervisor's wait loop
    // and for the drain path, both of which need it while a surface is
    // attached.
    let handle = {
        let slot = daemon.adapter.lock().unwrap_or_else(|p| p.into_inner());
        slot.as_ref().map(AdapterHandle::view)
    };
    let Some(handle) = handle else {
        let _ = writer.join();
        daemon.let_go();
        return;
    };

    // The first frame is the surface's hello. Answer it from what the adapter
    // reported, and keep anything else to forward verbatim.
    let mut head = None;
    match relay::next_frame(&mut surface_reader) {
        Ok(Some(frame)) => match relay::body_of(&frame) {
            Some(body) if body.get("type").and_then(|t| t.as_str()) == Some("studio.hello") => {
                let challenge = body.get("challenge").and_then(|c| c.as_str()).unwrap_or("");
                let request_id = body.get("requestId").cloned().unwrap_or(Value::Null);
                let answer = adapter::hello_result(&handle, challenge, &request_id);
                let mut bytes = Vec::new();
                if write_frame(&mut bytes, handle.epoch, &answer).is_ok() {
                    let _ = sender.send(bytes);
                }
            }
            _ => head = Some(frame),
        },
        Ok(None) => {}
        Err(error) => {
            eprintln!("step-studio-host: surface said nothing readable: {error}");
        }
    }

    let outcome = {
        let mut adapter_writer = AdapterWriter(handle.writer);
        if let Some(frame) = head {
            use std::io::Write;
            let _ = adapter_writer.write_all(&frame);
        }
        pump_frames(&mut surface_reader, &mut adapter_writer)
    };
    if let Err(error) = outcome {
        // A surface that goes away mid-frame is normal; anything else is worth
        // a line, because it is the only place the relay's health is visible.
        if error.kind() != std::io::ErrorKind::UnexpectedEof {
            eprintln!("step-studio-host: surface relay ended: {error}");
        }
    }

    // Order matters, and so does dropping the local sender: the writer thread
    // ends when the channel reports closed, and that needs both senders gone —
    // the one the slot holds and this one. Dropping only the slot's would leave
    // the join below waiting for a close this function is the only one that can
    // cause.
    drop(sender);
    daemon.let_go();
    let _ = writer.join();
    daemon
        .grace
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .detach(daemon.started.elapsed());
}

impl Daemon {
    /// Give up the relay and wake whoever is waiting for it.
    fn let_go(&self) {
        {
            let mut slot = self.to_surface.lock().unwrap_or_else(|p| p.into_inner());
            *slot = None;
        }
        {
            let mut active = self.active.lock().unwrap_or_else(|p| p.into_inner());
            *active = false;
        }
        self.freed.notify_all();
    }
}

/// A `Write` over the adapter's shared write half.
struct AdapterWriter(Arc<Mutex<step_studio_bridge::transport::Stream>>);

impl std::io::Write for AdapterWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut stream = self.0.lock().unwrap_or_else(|p| p.into_inner());
        stream.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let mut stream = self.0.lock().unwrap_or_else(|p| p.into_inner());
        stream.flush()
    }
}

// --- going away -----------------------------------------------------------

/// Drain the adapter and stop.
///
/// Called when the grace period expires with no surface attached, which is the
/// only thing that ends a daemon on its own. Everything else — the shell being
/// killed, the OS shutting down — closes the daemon's file descriptors, and the
/// adapter exits on its own when the socket it is serving reaches EOF. That
/// cascade is why there is no signal handler here yet; see `design/plan.md`
/// §19 for what it would buy and what it would cost.
fn shutdown(daemon: &Arc<Daemon>) {
    daemon.stopping.store(true, Ordering::Relaxed);
    if let Some(mut handle) = daemon
        .adapter
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take()
    {
        eprintln!("step-studio-host: draining the adapter");
        adapter::drain(&mut handle, DRAIN_DEADLINE);
    }
    // A Unix socket is a file; a Windows pipe name is not, so there is nothing
    // to remove there.
    #[cfg(unix)]
    let _ = std::fs::remove_file(&daemon.config.endpoint);
    eprintln!("step-studio-host: exiting");
}

/// Watch the grace clock and the stop flag while surfaces come and go.
fn watch(daemon: Arc<Daemon>) {
    loop {
        std::thread::sleep(POLL_SLICE);
        if daemon.stopping.load(Ordering::Relaxed) {
            return;
        }
        let liveness = daemon
            .grace
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .poll(daemon.started.elapsed());
        match liveness {
            Liveness::Alive => {}
            Liveness::Draining { remaining } => {
                eprintln!(
                    "step-studio-host: no surface attached; draining in {}s",
                    remaining.as_secs()
                );
            }
            Liveness::Expired => {
                shutdown(&daemon);
                return;
            }
        }
    }
}

// --- arguments ------------------------------------------------------------

fn parse_args(args: impl Iterator<Item = String>) -> Result<Config, String> {
    let mut config = Config {
        endpoint: lifecycle::default_endpoint(),
        adapter_endpoint: std::env::temp_dir()
            .join("step-studio-adapter.sock")
            .to_string_lossy()
            .to_string(),
        cwd: std::env::var("STEP_STUDIO_CWD").unwrap_or_else(|_| ".".to_string()),
        step_command: std::env::var("STEP_STUDIO_STEP_CMD").unwrap_or_else(|_| "step".to_string()),
        repo_root: String::new(),
        grace: DRAIN_GRACE,
    };
    let mut args = args.peekable();
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--endpoint" => config.endpoint = value()?,
            "--adapter-endpoint" => config.adapter_endpoint = value()?,
            "--cwd" => config.cwd = value()?,
            "--step-cmd" => config.step_command = value()?,
            "--repo-root" => config.repo_root = value()?,
            "--grace" => {
                let secs: u64 = value()?
                    .parse()
                    .map_err(|_| "--grace takes whole seconds".to_string())?;
                config.grace = Duration::from_secs(secs);
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(config)
}
