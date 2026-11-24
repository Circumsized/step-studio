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

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use serde_json::Value;
use step_studio_bridge::client::AdapterLaunch;
use step_studio_bridge::frame::write_frame;
use step_studio_bridge::transport::{self, Accepted};
use step_studio_host::adapter::{self, AdapterHandle};
use step_studio_host::lifecycle::{self, Acquire, GraceClock, Liveness};
use step_studio_host::relay;
use step_studio_host::routes::Routes;
use step_studio_host::state;
use step_studio_host::supervisor::{Next, Policy, Supervisor};
use step_studio_peer_uid::peer_uid;

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

/// How long a halted daemon keeps holding the endpoint before giving up.
///
/// Long enough for a shell that is already connecting to arrive and ask for the
/// retry that revives it; short enough that a daemon nobody wants does not sit
/// there answering nothing.
const HALTED_GRACE: Duration = Duration::from_secs(30);

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
    /// The bridge token this daemon mints and publishes at startup. One value
    /// per daemon, so a surface authenticates against the daemon it actually
    /// reached rather than against a constant every installation shares.
    token: String,
    /// The adapter while one is alive. `None` between attempts.
    adapter: Mutex<Option<AdapterHandle>>,
    /// Where the adapter's pump thread puts each frame for the surface that
    /// asked for it.
    ///
    /// One entry per attached surface.
    ///
    /// A set rather than one slot, because the adapter broadcasts with no
    /// addressing: every attached surface reads the same stream, and each tracks
    /// its own delivery. A frame one surface missed must not advance another's,
    /// which is why they do not share a channel.
    ///
    /// The serial tag is what lets a pump tell whether an entry is still its own:
    /// `mpsc::Sender` has no identity to compare, and clearing a replacement
    /// adapter's entries would drop a healthy adapter's frames into sockets
    /// nobody closes.
    to_surface: Mutex<Vec<Surface>>,
    /// Which surface is waiting on which request, so a reply can be handed to
    /// the one that asked instead of to everybody.
    pending: Mutex<Routes>,
    /// Set once the daemon has decided to go away, so the supervisor does not
    /// restart the adapter it just drained.
    stopping: AtomicBool,
    /// Set by a surface attaching. A halted supervisor is revived by a caller
    /// that actually wants the runtime, which is the only honest probe.
    retry_requested: AtomicBool,
    /// Which adapter the current `to_surface` entries belong to.
    adapter_serial: AtomicU64,
    /// Identity of each attached surface, because `mpsc::Sender` has none to
    /// compare: two surfaces attached at once have separate senders, and removing
    /// by anything else would silence the wrong one.
    surface_serial: AtomicU64,
    /// This daemon's identity, for the upgrade marker.
    instance_id: String,
    /// Whether this process bound the endpoint, so it is the only one that may
    /// unlink it.
    bound_endpoint: AtomicBool,
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
    // Minted and published *before* the listener exists, so a surface that
    // connects the moment the endpoint answers always finds the file. Minting
    // it inside `bring_up` instead would race: the supervisor thread spawns the
    // adapter while `serve` binds, so the token would appear at an
    // unpredictable moment — and again on every adapter retry.
    //
    // Refusing to start is the honest outcome. A daemon whose token never
    // reached the disk serves surfaces that cannot authenticate it, and every
    // one of them would report a confusing handshake failure instead of this.
    let token = step_studio_bridge::token::generate_token();
    if let Err(error) = state::write_token(&token) {
        eprintln!("step-studio-host: could not publish the bridge token: {error}");
        std::process::exit(1);
    }
    // Read before `config` moves into the daemon: the grace period is a
    // deployment choice, not a constant, and the drain clock has to see the
    // one that was asked for.
    let grace = config.grace;
    let daemon = Arc::new(Daemon {
        config,
        started: Instant::now(),
        epoch,
        token,
        adapter: Mutex::new(None),
        to_surface: Mutex::new(Vec::new()),
        pending: Mutex::new(Routes::new()),
        adapter_serial: AtomicU64::new(0),
        surface_serial: AtomicU64::new(0),
        bound_endpoint: AtomicBool::new(false),
        stopping: AtomicBool::new(false),
        retry_requested: AtomicBool::new(false),
        grace: Mutex::new(GraceClock::new(grace)),
        // This daemon's identity, for the upgrade marker: an update names the
        // daemon it means to stop, so a marker left by a half-finished update is
        // read as stale by the next one rather than as an instruction to exit.
        // See `upgrade::decide`.
        instance_id: format!("daemon-{epoch}-{}", std::process::id()),
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
    // How long the supervisor has been halted with nobody asking for a retry.
    let mut halted_for = Duration::ZERO;
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
                    continue;
                }
                // Nothing is asking for the runtime, and the budget is spent.
                // Sitting here would be worse than leaving: the daemon would
                // hold the endpoint and answer nothing, so a surface would wait
                // out its connect timeout and the launcher would never count a
                // failure — which is exactly the "the daemon cannot start"
                // terminal state the plan asks to be visible.
                if halted_for > HALTED_GRACE {
                    eprintln!(
                        "step-studio-host: exiting after {} failed starts and no retry request",
                        supervisor.failed_starts()
                    );
                    daemon.stopping.store(true, Ordering::Relaxed);
                    return;
                }
                halted_for += POLL_SLICE;
                std::thread::sleep(POLL_SLICE);
                // Without this the arm falls through to `bring_up` below and
                // spawns anyway — which is how a halted supervisor ends up
                // logging "halted after 3", "halted after 4", … forever.
                continue;
            }
            Next::Wait(delay) => {
                std::thread::sleep(delay.min(POLL_SLICE));
                continue;
            }
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
            let alive = Arc::clone(&handle.alive);
            let serial = daemon.adapter_serial.fetch_add(1, Ordering::Relaxed) + 1;
            let mut slot = daemon.adapter.lock().unwrap_or_else(|p| p.into_inner());
            *slot = Some(handle);
            drop(slot);
            // One pump thread per adapter, not per surface: it owns the read
            // half for the adapter's whole life, and hands frames to whichever
            // surface is currently active.
            spawn_pump(Arc::clone(&daemon), reader, alive, serial);
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
/// The Runtime's private roots, as environment variables.
///
/// The names are the Runtime's spelling, not this crate's — they are a
/// cross-process contract, asserted by
/// `scripts/cross-process-contracts.test.mjs`. Until this existed nothing set
/// them, so on a default install the media service answered `available: false`,
/// live audio and prediction lost their socket directory, and the auth broker
/// closed on a missing file. All five services then silently degraded.
///
/// A directory that cannot be prepared is skipped rather than fatal: the daemon
/// has already authenticated this adapter by this point, and refusing to start
/// because a scratch directory is unwritable would take the whole session down
/// for a capability that is optional. The failure is reported on stderr, which
/// lands in the daemon log the diagnostics page exports.
fn runtime_private_dirs() -> Vec<(String, String)> {
    runtime_private_dirs_at(&step_studio_host::state::state_base())
}

/// [`runtime_private_dirs`] under a chosen base, so a test can assert the whole
/// contract without touching the real state directory.
///
/// This is also the half the grep-based gate cannot see: that gate catches a
/// rename, but not a helper that is defined and never called. Asserting the
/// returned vector has all three names is what closes that.
fn runtime_private_dirs_at(base: &std::path::Path) -> Vec<(String, String)> {
    let mut vars = Vec::new();
    for (name, env) in [
        ("media", "OMP_STUDIO_MEDIA_ROOT"),
        ("sockets", "OMP_STUDIO_SOCKET_DIR"),
    ] {
        match step_studio_host::state::private_subdir_at(base, name) {
            Ok(dir) => vars.push((env.to_string(), dir.to_string_lossy().to_string())),
            Err(error) => eprintln!(
                "step-studio-host: could not prepare the private {name} directory: {error}"
            ),
        }
    }
    // A file, not a directory: the broker opens it and closes itself when it is
    // absent. Its parent is the state directory, already `0700`.
    let broker = step_studio_host::state::state_dir_under(base).join("auth-broker.json");
    vars.push((
        "OMP_STUDIO_AUTH_BROKER_FILE".to_string(),
        broker.to_string_lossy().to_string(),
    ));
    vars
}

fn bring_up(daemon: &Daemon) -> Result<(AdapterHandle, transport::Stream), adapter::AdapterError> {
    let launch = AdapterLaunch {
        // Empty: the daemon spawns the adapter itself, so `node` is only used
        // when the runtime is a Node script rather than the native binary.
        node: String::new(),
        adapter_script: String::new(),
        socket_path: daemon.config.adapter_endpoint.clone(),
        // The same file the daemon published and the surfaces read: one token,
        // one writer, two readers. It used to be `cwd/.step-studio/token`,
        // which is per-workspace — so a second workspace on the machine got a
        // second token file for a daemon that only ever minted one.
        token_file: step_studio_host::state::token_path()
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
        ]
        .into_iter()
        .chain(runtime_private_dirs())
        .collect(),
        // The daemon's own token, the one it published: the adapter proves it
        // knows this value, and `hello_result` re-signs with it for surfaces.
        token: Some(daemon.token.clone()),
        command: native_runtime_command(daemon_exe_dir().as_deref(), &daemon.config.repo_root),
    };
    adapter::launch(&launch, &adapter::challenge_for(daemon.epoch))
}

/// The directory this daemon was started from, which is where a packaged
/// install puts the adapter.
fn daemon_exe_dir() -> Option<std::path::PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
}

/// Where the runtime binary lives: beside the daemon in a packaged build, under
/// `target/` in a checkout.
///
/// The bare name is tried, then the `.exe` one: Windows appends the extension
/// to the installed file, so `step-studio-runtime` alone never matches and a
/// packaged daemon launches no adapter at all.
fn native_runtime_command(exe_dir: Option<&std::path::Path>, repo_root: &str) -> Vec<String> {
    let beside_exe = exe_dir
        .map(std::path::Path::to_path_buf)
        .into_iter()
        .flat_map(|dir| {
            ["step-studio-runtime", "step-studio-runtime.exe"].map(move |name| dir.join(name))
        });
    let root = std::path::Path::new(repo_root);
    let candidates = beside_exe.chain(
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
///
/// Returns a liveness flag this adapter's thread clears on the way out. Without
/// it a surface has no way to learn the adapter ended: the pump thread would
/// simply return, the slot's sender would stay alive, the surface's writer would
/// keep waiting on a channel nobody closes, and the socket would stay open on
/// both sides — so the shell's session would never die and its pump would never
/// reconnect.
fn spawn_pump(
    daemon: Arc<Daemon>,
    mut reader: transport::Stream,
    alive: Arc<AtomicBool>,
    serial: u64,
) {
    std::thread::spawn(move || {
        let outcome = relay::pump_frames_to(&mut reader, |frame| {
            // Nobody attached is a normal state, not an error: the frames are
            // dropped, because there is nobody to deliver them to.
            let mut surfaces = daemon.to_surface.lock().unwrap_or_else(|p| p.into_inner());
            // An answer goes to the surface that asked for it and nowhere else.
            // Anything else is an event, and events are for everybody — the
            // adapter broadcasts them with no addressing, so there is nothing
            // to narrow them by.
            //
            // This is M2.3's multiplexing, and it is the whole reason two
            // surfaces can attach at once: before it, every reply went to every
            // surface, so each one saw the other's receipts and turns.
            // Terminality matters: the runtime sends `accepted` before the
            // terminal receipt, both with the same requestId, and consuming the
            // routing entry on the first would broadcast the terminal one to
            // every surface.
            let asked =
                relay::receipt_of(&frame).and_then(|(id, terminal)| daemon.settle(&id, terminal));
            match asked {
                Some(serial) => {
                    // The others keep their slots: dropping them would close
                    // sockets that are mid-conversation.
                    match surfaces.iter().position(|(_, _, held)| *held == serial) {
                        Some(index) => {
                            // A send that fails means that surface is gone.
                            // Retaining it would leak a sender per dead surface
                            // for the daemon's lifetime.
                            if surfaces[index].0.send(frame.clone()).is_err() {
                                surfaces.remove(index);
                            }
                        }
                        None => {
                            // A surface that asked for this and is no longer in
                            // the list. Its relay thread is still forwarding
                            // requests, so every reply it is owed lands here and
                            // is dropped — and the surface's `await_reply` has no
                            // timeout, so it waits forever with nothing in any
                            // log to say why. Reachable when the write half fails
                            // while the read half stays open: the writer thread
                            // exits, the pump removes the entry on the next
                            // failed send, and the relay thread carries on.
                            //
                            // Logged rather than silently dropped. The fix beyond
                            // the log is to close the socket when the writer
                            // fails, which is what makes the relay thread notice
                            // — that belongs with the writer's own error path.
                            eprintln!(
                                "step-studio-host: a reply for surface {serial} has \
                                 nowhere to go; that surface will wait forever"
                            );
                        }
                    }
                }
                None => {
                    // Same failure handling, across everybody.
                    surfaces.retain(|(sender, _, _)| sender.send(frame.clone()).is_ok());
                }
            }
            Ok(())
        });
        if let Err(error) = outcome {
            eprintln!("step-studio-host: adapter read ended: {error}");
        }
        // The adapter is gone, so whatever it was feeding is over: release the
        // surface, and say so.
        //
        // Only if the slot still holds *this* pump's sender. The slot is global
        // and a replacement adapter may already have published its own, and
        // clearing that one would drop frames from a healthy adapter into a
        // socket nobody closes — the shell would sit on a silent connection and
        // every command would block in `await_reply` forever.
        {
            let mut surfaces = daemon.to_surface.lock().unwrap_or_else(|p| p.into_inner());
            surfaces.retain(|(_, held, _)| *held != serial);
        }
        adapter::mark_dead(&alive);
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
            Ok(Some(listener)) => {
                // Recorded so `shutdown` knows this process may unlink it. Two
                // daemons can race for the endpoint, and the loser deleting the
                // winner's socket file leaves every later connect with ENOENT
                // against a listener that still holds the inode.
                daemon.bound_endpoint.store(true, Ordering::Relaxed);
                listener
            }
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
        // Non-blocking, so this loop notices `stopping`. A blocking accept
        // would sit here forever after the supervisor gave up, and the daemon
        // would hold the endpoint while answering nothing — which is the
        // degraded state that made the breaker impossible to reach.
        listener
            .set_nonblocking(true)
            .expect("the listener can be made non-blocking");
        loop {
            if daemon.stopping.load(Ordering::Relaxed) {
                return;
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    let daemon = Arc::clone(&daemon);
                    std::thread::spawn(move || {
                        serve_surface(transport::Accepted::Stream(stream), daemon)
                    });
                }
                // Nobody connecting is the normal state for an idle daemon.
                Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(POLL_SLICE);
                }
                Err(error) => eprintln!("step-studio-host: accept failed: {error}"),
            }
        }
    }
    #[cfg(not(unix))]
    {
        let mut listener = match lifecycle::bind(&endpoint) {
            Ok(Some(listener)) => {
                daemon.bound_endpoint.store(true, Ordering::Relaxed);
                listener
            }
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
        // This arm used to call `transport::accept`, which is `ConnectNamedPipe`
        // with no timeout — so `stopping` was checked once, before a block that
        // never ended, and a parked daemon never exited: not on the grace clock,
        // not on an upgrade marker, because `serve` runs on the main thread and
        // `shutdown` only runs after it returns. `accept_until` bounds the wait
        // so the flag is re-read; see its comment for why that is sound.
        loop {
            if daemon.stopping.load(Ordering::Relaxed) {
                return;
            }
            let connection = match transport::accept_until(&listener, &daemon.stopping) {
                Ok(Some(connection)) => connection,
                // Stopping: this loop is over.
                Ok(None) => return,
                // An accept that produced no connection is not a reason to stop
                // serving. The Unix arm below never exits on an accept error
                // either — it logs, sleeps, and loops — and this arm used to
                // `return` on any error, which made one transient accept fatal
                // for a daemon whose whole job is to sit there. That asymmetry
                // is the defect; which specific error is transient is not
                // something this machine can demonstrate, so the arm matches
                // Unix rather than guessing at an error code.
                Err(error) => {
                    eprintln!("step-studio-host: accept failed: {error}");
                    std::thread::sleep(POLL_SLICE);
                    continue;
                }
            };
            // Re-bind before serving, so the next client is not kept waiting
            // behind this one. On Windows this needs `FILE_FLAG_FIRST_PIPE_INSTANCE`
            // cleared in `transport::bind` — with the crate default set, the
            // second `CreateNamedPipeW` on a live name fails with
            // ERROR_ACCESS_DENIED and this arm ends the serve loop after one
            // surface.
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
/// How long a surface has to say its first frame.
///
/// The surface slot is exclusive — the adapter broadcasts on one connection with
/// no addressing, so two surfaces reading it would split it — which means a client
/// that connects and sends nothing holds the only slot forever and the real shell
/// is never served. On Unix `set_read_timeout` bounds this; on Windows it is
/// accepted and ignored, so the bound has to come from the wait.
const SURFACE_FIRST_FRAME_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

/// Read one frame, giving up after `budget`.
///
/// Platform-generic on purpose: `set_read_timeout` is a no-op on Windows, and
/// miow's `Read for &NamedPipe` is overlapped underneath (a thread-local
/// `OVERLAPPED`, handle opened `FILE_FLAG_OVERLAPPED`), so abandoning the read and
/// closing the connection completes it rather than wedging the process. The reader
/// is consumed because `wait_or_stop`'s closure is `'static`; the caller
/// re-duplicates for the relay loop, which is sound because a completed read
/// consumed exactly one frame.
fn first_frame_within(
    mut reader: transport::Connection,
    budget: std::time::Duration,
) -> Option<std::io::Result<Option<Vec<u8>>>> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    let stop = Arc::new(AtomicBool::new(false));
    let setter = Arc::clone(&stop);
    std::thread::spawn(move || {
        std::thread::sleep(budget);
        setter.store(true, Ordering::Relaxed);
    });
    transport::wait_or_stop(move || relay::next_frame(&mut reader), &stop)
}

/// Whether a surface whose peer reports `peer` may be served.
///
/// Split out of `serve_surface` so the decision is testable at all: inline, the
/// only branch a single-user machine can exercise is the one where the peer *is*
/// identified, which is how the Windows fail-open survived a review that read the
/// comment above it and believed it.
///
/// The `None` arm is the divergence, written as it behaves rather than as the
/// comment claims: refused on Unix, **accepted on Windows**. That is TH13. It is
/// not fixed here because fixing it means either unsafe FFI for the SID or an
/// explicit decision to ship Windows with no peer gate at all, and neither is
/// mine to decide. Writing `None => false` would close the hole and take the
/// product down on Windows in the same line, which is why the arm says what it
/// does instead of what it should.
fn peer_may_serve(peer: Option<u32>) -> bool {
    match peer {
        Some(uid) => uid == lifecycle::own_uid(),
        // `!cfg!`, not `cfg!`: the refusing arm was `else if cfg!(unix)`, so a
        // platform that cannot identify the peer is refused on Unix and accepted
        // on Windows. Writing this arm the other way round inverts the gate —
        // refusing this user's own surfaces and serving everyone else's — and the
        // test below is what caught that on the first attempt.
        None => !cfg!(unix),
    }
}

fn serve_surface(connection: Accepted, daemon: Arc<Daemon>) {
    let stream = connection.into_connection();

    // Only this user's surfaces. The endpoint sits in a shared temp directory,
    // so without this any local user who can reach it drives another user's
    // session and working directory. Where the platform cannot say who is on
    // the other end, the connection is refused rather than trusted: an unknown
    // peer is not a permitted one.
    let peer = peer_uid(&stream);
    if !peer_may_serve(peer) {
        match peer {
            Some(uid) => eprintln!("step-studio-host: refusing a surface from uid {uid}"),
            None => eprintln!("step-studio-host: cannot tell who this surface is; refusing it"),
        }
        return;
    }

    // Wait for a *live* adapter. The slot keeps the dead one until its
    // replacement is up, and a surface handed the dead handle would be released
    // again immediately — which is a broken pipe for whoever just connected.
    let waited = Instant::now();
    loop {
        if daemon.stopping.load(Ordering::Relaxed) {
            return;
        }
        let live = daemon
            .adapter
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .is_some_and(|handle| adapter::is_alive(&handle.alive));
        if live {
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

    if daemon.stopping.load(Ordering::Relaxed) {
        return;
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
    let surface = daemon.surface_serial.fetch_add(1, Ordering::Relaxed) + 1;
    {
        let serial = daemon.adapter_serial.load(Ordering::Relaxed);
        let mut surfaces = daemon.to_surface.lock().unwrap_or_else(|p| p.into_inner());
        surfaces.push((sender.clone(), serial, surface));
    }

    // The writer owns the surface's write half and drains until the sender is
    // dropped, which happens when this function returns.
    // The writer reports its own death, because a write that fails while the read
    // half still delivers is otherwise invisible: the thread exits, the pump
    // removes the surface on its next failed send, and the relay thread keeps
    // forwarding requests whose replies now have nowhere to go. The surface then
    // waits forever, because `await_reply` has no timeout of its own.
    let (writer_dead, writer_dead_rx) = mpsc::channel::<()>();
    let writer = std::thread::spawn({
        let mut surface_writer = match transport::connection_duplicate(&stream) {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!("step-studio-host: {error}");
                // No writer thread exists yet, so there is nothing to join — but
                // the grace clock is already attached, and leaving it so would
                // keep the daemon alive forever with no surface.
                daemon.release_surface(surface);
                return;
            }
        };
        let writer_dead = writer_dead;
        move || {
            use std::io::Write;
            while let Ok(frame) = receiver.recv() {
                if surface_writer.write_all(&frame).is_err() {
                    eprintln!("step-studio-host: a surface's write half failed; closing it");
                    let _ = writer_dead.send(());
                    return;
                }
            }
        }
    });

    let surface_reader = match transport::connection_duplicate(&stream) {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("step-studio-host: {error}");
            // The writer thread is running, and dropping both senders is what
            // ends it: this one goes out of scope, and `release_surface` drops
            // the slot's.
            daemon.release_surface(surface);
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
        // Order, and it is the difference between returning and hanging. The
        // writer ends when its channel reports closed, and that needs *both*
        // senders gone — the one in the slot and this function's local one. The
        // previous order joined first and dropped second, so `recv()` never
        // returned `Err` and the join waited forever on a thread that could not
        // finish.
        drop(sender);
        daemon.release_surface(surface);
        let _ = writer.join();
        return;
    };

    // The first frame is the surface's hello. Answer it from what the adapter
    // reported, and keep anything else to forward verbatim. Bounded, because the
    // slot is exclusive and a silent peer would otherwise hold it forever.
    // The daemon's own challenge, carried out of the hello answer so the proof
    // that follows can be checked against it.
    let mut peer_challenge: Option<String> = None;
    let Some(first) = first_frame_within(surface_reader, SURFACE_FIRST_FRAME_BUDGET) else {
        // Budget spent with no first frame. Release the slot and close, so the
        // silent peer stops holding it and the abandoned read is completed.
        eprintln!("step-studio-host: a surface never said hello; releasing the slot");
        daemon.release_surface(surface);
        let _ = transport::close_connection(&stream);
        return;
    };
    // A reader for the relay loop. The bounded read consumed its own, and it
    // completed exactly one frame on the way here, so this starts at the next.
    let surface_reader = match transport::connection_duplicate(&stream) {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("step-studio-host: {error}");
            daemon.release_surface(surface);
            return;
        }
    };
    match first {
        Ok(Some(frame)) => match relay::body_of(&frame) {
            Some(body) if body.get("type").and_then(|t| t.as_str()) == Some("studio.hello") => {
                let challenge = body.get("challenge").and_then(|c| c.as_str()).unwrap_or("");
                let request_id = body.get("requestId").cloned().unwrap_or(Value::Null);
                let daemon_challenge =
                    step_studio_bridge::handshake::fresh_challenge("daemon", handle.epoch);
                let answer =
                    adapter::hello_result(&handle, challenge, &request_id, &daemon_challenge);
                let mut bytes = Vec::new();
                if write_frame(&mut bytes, handle.epoch, &answer).is_ok() {
                    let _ = sender.send(bytes);
                }
                peer_challenge = Some(daemon_challenge);
            }
            // Anything else as a *first* frame used to be forwarded verbatim,
            // which is how a peer that never shook hands at all still drove the
            // runtime. A surface opens with a hello or it does not open.
            _ => {
                eprintln!("step-studio-host: a surface's first frame was not a hello; refusing it");
                daemon.release_surface(surface);
                drop(sender);
                let _ = transport::close_connection(&stream);
                return;
            }
        },
        Ok(None) => {}
        Err(error) => {
            eprintln!("step-studio-host: surface said nothing readable: {error}");
        }
    }

    // The surface must now prove it holds the token, answering the challenge we
    // just chose. Until this existed the daemon signed whatever challenge it was
    // handed, so any peer that could open the endpoint picked a value and was
    // given a valid proof for it — and a first frame that was not a hello was
    // forwarded verbatim, so a peer that never shook hands at all still drove the
    // runtime. NIST SP 800-63B-4: "the verifier sends the claimant a challenge".
    let Some(daemon_challenge) = peer_challenge else {
        // No hello was answered, which the arm above already refused. Reaching
        // here would mean the two disagree.
        daemon.release_surface(surface);
        drop(sender);
        let _ = transport::close_connection(&stream);
        return;
    };
    let Some(proof) = first_frame_within(surface_reader, SURFACE_FIRST_FRAME_BUDGET) else {
        eprintln!("step-studio-host: a surface never proved itself; releasing the slot");
        daemon.release_surface(surface);
        drop(sender);
        let _ = transport::close_connection(&stream);
        return;
    };
    // A reader for the relay loop, for the same reason as above: the bounded read
    // consumed its own, and a completed read consumed exactly one frame.
    let surface_reader = match transport::connection_duplicate(&stream) {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("step-studio-host: {error}");
            daemon.release_surface(surface);
            return;
        }
    };
    let proved = match proof {
        Ok(Some(frame)) => match relay::body_of(&frame) {
            Some(body)
                if body.get("type").and_then(|t| t.as_str()) == Some("studio.hello.proof") =>
            {
                step_studio_bridge::handshake::verify_proof(
                    &body,
                    &handle.token,
                    &daemon_challenge,
                    &handle.instance_id,
                )
            }
            _ => Err("the frame after a hello was not a proof".to_string()),
        },
        Ok(None) => Err("the surface closed before proving itself".to_string()),
        Err(error) => Err(format!("the proof was unreadable: {error}")),
    };
    if let Err(error) = proved {
        eprintln!("step-studio-host: refusing a surface that failed to prove itself: {error}");
        daemon.release_surface(surface);
        drop(sender);
        let _ = transport::close_connection(&stream);
        return;
    }

    {
        let mut adapter_writer = AdapterWriter(Arc::clone(&handle.writer));
        // The relay runs on its own thread so this one can watch the adapter:
        // a surface that keeps sending while its adapter has died is a socket
        // nobody is reading, and the shell behind it would wait forever.
        let (relay_done, relay_done_rx) = mpsc::channel::<()>();
        // Frames the daemon answered itself go back on the surface's *own*
        // sender, which is the same one the adapter's pump writes to and the same
        // one this surface's writer thread drains. One writer per socket.
        //
        // There used to be a second thread here, draining an `answered` channel
        // and writing to `stream.try_clone()` — which contradicted the comment
        // directly above it ("two owners of one fd is how a frame ends up
        // interleaved"). On Linux the socket lock serialises the two writers, so
        // nothing visibly broke; on a Windows byte-mode named pipe a partial write
        // from one lands the other's bytes inside its frame, and the client's
        // `read_frame` sees a corrupt length and closes with a protocol error.
        let sender_for_relay = sender.clone();
        let mut surface_reader = surface_reader;
        let daemon_for_relay = Arc::clone(&daemon);
        std::thread::spawn(move || {
            let daemon = daemon_for_relay;
            loop {
                match relay::next_frame(&mut surface_reader) {
                    Ok(Some(frame)) => {
                        // Operations the daemon answers itself never reach the
                        // adapter. This is the first of them — the process tree
                        // is a fact about the daemon and the adapter has no such
                        // capability — and the seam is where M2.3's
                        // multiplexing goes.
                        if let Some(body) = relay::body_of(&frame) {
                            if body.get("type").and_then(|t| t.as_str()) == Some("studio.request")
                                && body.pointer("/operation/kind").and_then(|k| k.as_str())
                                    == Some("process.tree.get")
                            {
                                let request_id =
                                    body.get("requestId").cloned().unwrap_or(Value::Null);
                                let answer = json!({
                                    "type": "studio.receipt",
                                    "requestId": request_id,
                                    "status": "completed",
                                    "result": step_studio_host::process::read_model(),
                                });
                                let mut bytes = Vec::new();
                                if write_frame(&mut bytes, daemon.epoch, &answer).is_ok() {
                                    let _ = sender_for_relay.send(bytes);
                                }
                                continue;
                            }
                        }
                        // Recorded before it goes, so the answer can find its
                        // way back here. A frame the daemon answers itself
                        // never reaches this point, so it is never recorded and
                        // never routed: it already went straight to the surface.
                        if let Some(id) = relay::request_id_of(&frame) {
                            daemon.expect(&id, surface);
                        }
                        use std::io::Write;
                        if adapter_writer.write_all(&frame).is_err() {
                            let _ = relay_done.send(());
                            return;
                        }
                    }
                    // A surface that stopped sending is gone; one that broke
                    // mid-frame is broken. Either way the relay is over.
                    Ok(None) => {
                        let _ = relay_done.send(());
                        return;
                    }
                    Err(error) => {
                        eprintln!("step-studio-host: surface read failed: {error}");
                        let _ = relay_done.send(());
                        return;
                    }
                }
            }
        });
        // A second writer for the daemon's own answers, on the surface's write
        // half, so the relay thread never touches it.
        loop {
            if daemon.stopping.load(Ordering::Relaxed) {
                break;
            }
            // The adapter this surface was talking to ended. The next one is
            // already coming up, and this surface must not be handed to it: the
            // shell reconnects and gets a fresh session instead.
            if !adapter::is_alive(&handle.alive) {
                eprintln!("step-studio-host: the adapter ended; releasing the surface");
                break;
            }
            if relay_done_rx.try_recv().is_ok() {
                break;
            }
            // The writer died while the reader still delivers. Closing the socket
            // is what makes the relay thread notice: its read then ends, the
            // session reports closed, and the shell's supervisor reconnects.
            if writer_dead_rx.try_recv().is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // Closing the socket is what makes the surface notice. Its reader
        // thread then sees EOF, the session reports closed, and the shell's
        // supervisor reconnects — which is the whole point of B1.
        close_socket(&stream);
    }

    // Order matters, and so does dropping the local sender: the writer thread
    // ends when the channel reports closed, and that needs both senders gone —
    // the one the slot holds and this one. Dropping only the slot's would leave
    // the join below waiting for a close this function is the only one that can
    // cause.
    drop(sender);
    daemon.release_surface(surface);
    let _ = writer.join();
}

/// Close a surface's socket in both directions.
///
/// Half-closing from the daemon's side is what turns "the adapter died" into
/// something the shell can act on: its reader thread sees EOF, the session
/// reports closed, and the pump reconnects. Leaving the socket open leaves the
/// shell waiting on a peer that will never speak again.
#[cfg(unix)]
fn close_socket(stream: &transport::Connection) {
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

/// Half-close a Windows pipe, the counterpart of `shutdown(Both)`.
///
/// The old body was an empty function justified by "a named pipe handle has no
/// equivalent" — which is not true: `DisconnectNamedPipe` is exactly that, and
/// `miow` exposes it as `NamedPipe::disconnect`. Without it a Windows surface's
/// reader blocks in `read` forever once the daemon stops writing, so the shell
/// waits on a peer that will never speak instead of reconnecting.
///
/// What the client sees is an **error**, not EOF: Microsoft — "the client
/// receives an error the next time it attempts to access the pipe". Every reader
/// on this path already treats a read error as closed (the shell's
/// `spawn_reader`, this file's relay loop, the runtime's pump), so the outcome
/// is the same — but that is now load-bearing rather than incidental; see the
/// note in `transport.rs` about broken-pipe normalisation.
#[cfg(not(unix))]
fn close_socket(stream: &transport::Connection) {
    let _ = stream.disconnect();
}

impl Daemon {
    /// Undo everything a surface's attach set up.
    ///
    /// One call rather than two, because the two halves were previously separate
    /// and three of the four exit paths did only one of them: the surface's slot
    /// went, the grace clock's attachment stayed. A daemon whose grace clock
    /// still counts a departed surface never drains — it holds the endpoint and
    /// the adapter indefinitely with nobody attached.
    fn release_surface(&self, surface: u64) {
        self.forget_routes(surface);
        {
            let mut surfaces = self.to_surface.lock().unwrap_or_else(|p| p.into_inner());
            surfaces.retain(|(_, _, held)| *held != surface);
        }
        self.grace
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .detach(self.started.elapsed());
    }

    /// Give up the relay and wake whoever is waiting for it.
    fn forget_routes(&self, surface: u64) {
        // The requests it will never get an answer to. Keeping them would grow
        // the map once per surface that ever attached, for as long as the daemon
        // lives.
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .forget(surface);
    }

    /// Remember that `surface` is waiting on `request_id`, so the reply can be
    /// routed back to it rather than broadcast.
    ///
    /// A displaced surface is reported rather than swallowed: it means two
    /// surfaces minted the same id, and the one that lost its slot is about to
    /// wait for a reply that goes to the other. Without the log line that is a
    /// hang with nothing to say why.
    fn expect(&self, request_id: &str, surface: u64) {
        // The lock is dropped before the log line, and that ordering is the
        // whole point. `if let` keeps its scrutinee's temporaries alive to the
        // end of the block, so writing the `eprintln!` inside it held `pending`
        // across a write to stderr — and the adapter's pump takes `to_surface`
        // and then `pending`, so a stderr whose reader had gone away would fill
        // its pipe buffer and stall the pump, and with it every surface.
        let displaced = {
            let mut routes = self.pending.lock().unwrap_or_else(|p| p.into_inner());
            routes.expect(request_id, surface)
        };
        if let Some(displaced) = displaced {
            eprintln!(
                "step-studio-host: request {request_id} was already waiting on surface \
                 {displaced}; surface {surface} took it over"
            );
        }
    }

    /// Take the surface waiting on `request_id`. `None` means the frame is an
    /// event rather than an answer, and events are for everybody.
    ///
    /// `terminal` says whether this receipt ends the request; see
    /// [`step_studio_host::relay::receipt_of`] for why the non-terminal case has
    /// to keep the entry.
    fn settle(&self, request_id: &str, terminal: bool) -> Option<u64> {
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .settle(request_id, terminal)
    }
}

/// One attached surface: where its frames go, and which adapter and surface they
/// belong to.
///
/// The two serials are what let a pump drop exactly its own entries — an adapter's
/// when it ends, a surface's when that surface detaches — without being able to
/// compare a `Sender`, which has no identity.
type Surface = (Sender<Vec<u8>>, u64, u64);

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
    // to remove there. Only if this process bound it: two daemons can race, and
    // the loser used to delete the winner's socket file, leaving every later
    // connect with ENOENT against a listener that still held the inode.
    #[cfg(unix)]
    if daemon.bound_endpoint.load(Ordering::Relaxed) {
        let _ = std::fs::remove_file(&daemon.config.endpoint);
    }
    eprintln!("step-studio-host: exiting");
}

/// Watch the grace clock and the stop flag while surfaces come and go.
fn watch(daemon: Arc<Daemon>) {
    // The poll slice is short so a draining daemon reacts promptly, but the log
    // is not: at 250 ms a ten-minute grace period is 2400 identical lines. One
    // line per whole second left, and only when that second changes.
    let mut announced: Option<u64> = None;
    loop {
        std::thread::sleep(POLL_SLICE);
        if daemon.stopping.load(Ordering::Relaxed) {
            return;
        }
        // An upgrade that means *this* daemon: drain now, with the grace period
        // forced to zero. The marker names the daemon, so one left by a
        // half-finished update is stale to the next daemon and does not stop it.
        // The daemon's own state directory, not the shell's per-workspace
        // `<cwd>/.step-studio`. `upgrade.rs` says the marker lives in "the
        // daemon's own state directory" and `state.rs` says that directory is
        // machine-level because one daemon serves every workspace — but this
        // read the workspace one, so an update triggered from any workspace
        // other than the daemon's own was invisible to it.
        let state_dir = step_studio_host::state::state_dir();
        match step_studio_host::upgrade::decide(&state_dir, &daemon.instance_id) {
            decision if decision.should_drain() => {
                eprintln!("step-studio-host: {}", decision.reason());
                step_studio_host::upgrade::consume(&state_dir);
                shutdown(&daemon);
                return;
            }
            decision => {
                // Logged, not acted on. A stale marker is evidence an update did
                // not land, and swallowing it would hide that.
                if !matches!(decision, step_studio_host::upgrade::Decision::Absent) {
                    eprintln!("step-studio-host: {}", decision.reason());
                }
            }
        }
        let liveness = daemon
            .grace
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .poll(daemon.started.elapsed());
        match liveness {
            Liveness::Alive => announced = None,
            Liveness::Draining { remaining } => {
                let seconds = remaining.as_secs();
                if announced != Some(seconds) {
                    announced = Some(seconds);
                    eprintln!("step-studio-host: no surface attached; draining in {seconds}s");
                }
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
        // Derived from this daemon's own endpoint, never a fixed path in the
        // temp dir. It was `temp_dir()/step-studio-adapter.sock`, which two
        // daemons — or one daemon and a stale adapter from a previous run —
        // collide on: the second one binds nothing, connects to the first
        // one's adapter, and serves a different daemon's runtime under its own
        // epoch. That is how a rebuilt binary kept failing exactly like the
        // unrebuilt one, with nothing in the log to say why.
        adapter_endpoint: String::new(),
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
    // Paired with this daemon's endpoint unless the caller said otherwise, so
    // two daemons on one machine never share an adapter socket. A daemon with
    // its own endpoint but a shared adapter is the collision described at
    // `adapter_endpoint`'s declaration.
    if config.adapter_endpoint.is_empty() {
        let endpoint = std::path::Path::new(&config.endpoint);
        let stem = endpoint.file_stem().and_then(|s| s.to_str()).unwrap_or("daemon");
        config.adapter_endpoint = endpoint
            .with_file_name(format!("{stem}-adapter.sock"))
            .to_string_lossy()
            .to_string();
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows accepts a peer it cannot identify. That is TH13, and this test
    /// exists so the gap is visible here rather than only in a comment.
    ///
    /// It is not a PoC of the vulnerability — nothing on this machine can be,
    /// because the fail-open branch is unreachable wherever `peer_uid` works.
    /// What it does is fail when somebody closes the gap, which is the moment
    /// TH13 has to be rewritten anyway. A test that only ever passes on the
    /// platform with the bug is still worth having for that reason.
    #[test]
    #[cfg(windows)]
    fn an_unidentifiable_peer_is_accepted_on_windows_which_is_th13() {
        assert!(
            peer_may_serve(None),
            "Windows now refuses an unidentifiable peer: TH13 is fixed and must be updated"
        );
    }

    /// The gate refuses an unidentifiable peer — on this platform.
    ///
    /// What this pins: the Unix half. What it cannot pin: the Windows divergence,
    /// because on Unix `!cfg!(unix)` and `false` evaluate the same, so no
    /// assertion here can tell them apart. Verified by sabotage: changing the
    /// `None` arm to a flat `false` leaves this test green. The divergence is
    /// therefore documented in `peer_may_serve`'s comment and in TH13, not pinned
    /// by a test — and saying otherwise would be the exact overclaim this round
    /// has been about.
    #[test]
    #[cfg(unix)]
    fn the_gate_refuses_what_it_should_on_this_platform() {
        assert!(
            peer_may_serve(Some(crate::lifecycle::own_uid())),
            "this user's own surface was refused"
        );
        assert!(
            !peer_may_serve(Some(0)),
            "a surface from another uid was served"
        );
        assert!(
            !peer_may_serve(None),
            "a peer this platform cannot identify was served"
        );
    }

    /// A surface that connects and says nothing must not hold the slot.
    ///
    /// The slot is exclusive — the adapter broadcasts on one connection with no
    /// addressing — so before the bound, this peer held it forever and the real
    /// shell was never served. `set_read_timeout` bounds it on Unix and is
    /// ignored on Windows, which is why the bound comes from the wait instead.
    #[test]
    fn a_silent_surface_releases_the_slot_instead_of_holding_it_forever() {
        let dir = std::env::temp_dir().join(format!("ss-first-frame-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let endpoint = dir.join("silent-surface.sock");

        let for_client = step_studio_bridge::transport::endpoint_for_bind(&endpoint.to_string_lossy());
        // Bind the pipe endpoint, not the socket path: on Windows `bind` hands
        // its argument straight to `CreateNamedPipeW`, so a filesystem path is
        // not a pipe name.
        let listener = step_studio_bridge::transport::bind(&for_client)
            .expect("bind");
        // Connects, then says nothing at all — and stays connected. Dropping the
        // client handle would be EOF, which `next_frame` already reports as
        // `Ok(None)`; the defect is a peer that is *open and silent*.
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let silent = std::thread::spawn(move || {
            let client = step_studio_bridge::transport::connect(&for_client)?;
            let _ = held_tx.send(client);
            std::io::Result::<()>::Ok(())
        });
        let _held = held_rx.recv().expect("the surface connected");

        let accepted = step_studio_bridge::transport::accept(listener).expect("accept");
        let reader = accepted.into_connection();
        let started = std::time::Instant::now();
        let outcome = first_frame_within(reader, std::time::Duration::from_millis(300));

        assert!(outcome.is_none(), "a silent surface was not given up on");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "the bound took {:?}",
            started.elapsed()
        );
        let _ = silent.join();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The bound must not eat a frame that did arrive.
    #[test]
    fn a_surface_that_speaks_still_gets_its_first_frame() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("ss-first-frame-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let endpoint = dir.join("speaking-surface.sock");

        let for_client = step_studio_bridge::transport::endpoint_for_bind(&endpoint.to_string_lossy());
        // Bind the pipe endpoint, not the socket path: on Windows `bind` hands
        // its argument straight to `CreateNamedPipeW`, so a filesystem path is
        // not a pipe name.
        let listener = step_studio_bridge::transport::bind(&for_client)
            .expect("bind");
        let speaker = std::thread::spawn(move || -> std::io::Result<()> {
            let mut client = step_studio_bridge::transport::connect(&for_client)?;
            let hello = serde_json::json!({
                "type": "studio.hello",
                "requestId": "surface-hello",
                "supportedProtocolVersions": [step_studio_bridge::PROTOCOL_VERSION],
                "requiredProfile": "limited",
                "challenge": "a-challenge",
            });
            let mut bytes = Vec::new();
            step_studio_bridge::frame::write_frame(&mut bytes, 1, &hello)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            client.write_all(&bytes)?;
            client.flush()
        });

        let accepted = step_studio_bridge::transport::accept(listener).expect("accept");
        let reader = accepted.into_connection();
        let first = first_frame_within(reader, std::time::Duration::from_secs(5))
            .expect("not given up on")
            .expect("the read succeeded")
            .expect("a frame arrived");
        let body = relay::body_of(&first).expect("the frame has a body");
        assert_eq!(
            body.get("type").and_then(|t| t.as_str()),
            Some("studio.hello"),
            "the first frame was not the hello"
        );
        let _ = speaker.join();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A packaged Windows install puts `step-studio-runtime.exe` beside the
    /// daemon, and the resolver used to look for the extension-less name only —
    /// so an installed daemon launched no adapter at all. Both spellings are
    /// accepted now; this pins the `.exe` one, the only spelling the installed
    /// layout can produce.
    #[test]
    fn the_adapter_is_found_beside_the_daemon_with_or_without_the_exe_suffix() {
        let dir = std::env::temp_dir().join(format!("ss-adapter-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("make an install dir");
        let installed = dir.join("step-studio-runtime.exe");
        std::fs::write(&installed, b"not a real binary").expect("write the sibling");

        assert_eq!(
            native_runtime_command(Some(&dir), "/nowhere"),
            vec![installed.to_string_lossy().to_string()],
            "the installed adapter was not found"
        );

        let checkout = dir.join("step-studio-runtime");
        std::fs::write(&checkout, b"not a real binary").expect("write the sibling");
        assert_eq!(
            native_runtime_command(Some(&dir), "/nowhere"),
            vec![checkout.to_string_lossy().to_string()],
            "the checkout adapter was not found"
        );

        let _ = std::fs::remove_file(&checkout);
        let _ = std::fs::remove_file(&installed);
        assert!(
            native_runtime_command(Some(&dir), "/nowhere").is_empty(),
            "a directory with no adapter resolved to one"
        );
        assert!(
            native_runtime_command(None, "/nowhere").is_empty(),
            "no exe dir resolved to an adapter"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// The Runtime's three private roots are all exported, with real directories
    /// behind them.
    ///
    /// The grep-based gate in `scripts/cross-process-contracts.test.mjs` catches a
    /// rename, but it cannot catch a helper that is defined and never called — the
    /// name is still sitting right there in the source. This asserts the vector
    /// `bring_up` actually chains, which is the half the grep cannot see.
    #[test]
    fn every_private_root_is_exported_with_a_directory_behind_it() {
        let base = std::env::temp_dir().join(format!("ss-privars-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("make a base");

        let vars = runtime_private_dirs_at(&base);
        let names: Vec<&str> = vars.iter().map(|(name, _)| name.as_str()).collect();

        for expected in [
            "OMP_STUDIO_MEDIA_ROOT",
            "OMP_STUDIO_SOCKET_DIR",
            "OMP_STUDIO_AUTH_BROKER_FILE",
        ] {
            assert!(names.contains(&expected), "{expected} is not exported: {names:?}");
        }
        assert_eq!(names.len(), 3, "unexpected extras: {names:?}");

        // The two directory roots must exist, not merely be named — a path that
        // was never created is the same user-visible failure as no variable at all.
        for (name, value) in &vars {
            if name == "OMP_STUDIO_AUTH_BROKER_FILE" {
                continue;
            }
            assert!(
                std::path::Path::new(value).is_dir(),
                "{name} points at {value}, which is not a directory"
            );
        }
        // And the broker file lives inside the state directory, which is 0700.
        let broker = vars
            .iter()
            .find(|(name, _)| name == "OMP_STUDIO_AUTH_BROKER_FILE")
            .map(|(_, value)| std::path::PathBuf::from(value))
            .expect("the broker variable is exported");
        assert_eq!(
            broker,
            step_studio_host::state::state_dir_under(&base).join("auth-broker.json")
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Removing the chain must fail here, not in front of a user.
    ///
    /// The regression this suite exists for: `bring_up` stopped chaining
    /// `runtime_private_dirs()`, so five services silently degraded on a default
    /// install while every test stayed green — the helper was still there, still
    /// correct, still uncalled.
    #[test]
    fn bring_up_chains_the_private_roots() {
        // `include_str!` embeds this whole file, including the assertion below, so
        // checking for the bare call would be satisfied by the test itself. The
        // call site is the only place the chain is followed by `.collect()`.
        let source = include_str!("studio-host.rs");
        assert!(
            source.contains(".chain(runtime_private_dirs())\n        .collect()"),
            "bring_up no longer chains the private roots, so media, live audio, \
             prediction, browser observation and the auth broker all degrade silently"
        );
    }

}
