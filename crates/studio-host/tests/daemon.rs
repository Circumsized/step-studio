//! The daemon end to end, against the real adapter.
//!
//! The property M2 exists for is one sentence: killing the shell does not kill
//! the conversation. Everything here is in service of watching that hold, and
//! of watching it fail the other way — that killing the daemon does not leave
//! the runtime orphaned, holding the user's session open.

#![cfg(unix)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use step_studio_bridge::{AdapterLaunch, BridgeSession};

/// A running daemon, killed when the test ends.
///
/// `log` holds the daemon's stderr. It is piped rather than inherited so a test
/// can read what the daemon said — the upgrade marker drill needs the epoch the
/// daemon logged at startup to name it — and `Drop` echoes it back only when the
/// test is already failing, so a green run stays quiet and a red one keeps its
/// evidence.
struct Daemon {
    child: Child,
    endpoint: String,
    adapter_endpoint: String,
    dir: PathBuf,
    log: Arc<Mutex<Vec<String>>>,
}

impl Daemon {
    fn start(tag: &str) -> Self {
        Self::build(tag, 600)
    }

    /// `grace` is how long the daemon keeps running with no surface attached.
    /// Tests that reconnect need it longer than the gap between the two
    /// connections, or the daemon correctly drains before the second shell
    /// arrives.
    fn build(tag: &str, grace: u64) -> Self {
        let root = repo_root();
        let dir = std::env::temp_dir().join(format!("host-daemon-{tag}-{}", std::process::id()));
        Self::spawn(tag, grace, &root.join("target/debug/step-studio-host"), &dir, &root)
    }

    /// Start a daemon from a chosen binary, in a chosen directory.
    ///
    /// The upgrade drill needs both: it swaps a *copy* of the daemon inside a
    /// scratch install directory, and must not disturb the binary the other
    /// tests in this file run. `dir` is the daemon's cwd, which is also where it
    /// looks for the upgrade marker, so the drill decides where that lands.
    fn spawn(tag: &str, grace: u64, daemon_bin: &Path, dir: &Path, root: &Path) -> Self {
        let _ = tag;
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).expect("make a temp dir");
        assert!(
            daemon_bin.is_file(),
            "build the daemon first: cargo build -p step-studio-host"
        );

        let endpoint = dir.join("daemon.sock").to_string_lossy().to_string();
        let adapter_endpoint = dir.join("adapter.sock").to_string_lossy().to_string();
        let _ = root;
        // The Rust runtime takes the CLI as one executable and does not split a
        // command line, so the fake arrives through a wrapper rather than as
        // `node <script>`. (The TypeScript adapter did split it; that divergence
        // predates M2 and is not this test's business.)
        let step = dir.join("step");
        std::fs::write(
            &step,
            format!(
                "#!/bin/sh\nexec node {}\n",
                // The same fixture the runtime crate's e2e tests use, reached by
                // path. It used to live in `packages/runtime-stepcode/test/`,
                // which was deleted as dead code — and these tests then wrote a
                // wrapper that exec'd nothing, so every turn came back `failed`
                // with a spawn error that looked like a runtime regression.
                root.join("crates/step-studio-runtime/tests/fixtures/fake-step.mjs")
                    .to_string_lossy()
            ),
        )
        .expect("write the step wrapper");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&step, std::fs::Permissions::from_mode(0o755))
                .expect("make the wrapper executable");
        }
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut child = Command::new(daemon_bin)
            .stderr(Stdio::piped())
            .arg("--endpoint")
            .arg(&endpoint)
            .arg("--adapter-endpoint")
            .arg(&adapter_endpoint)
            .arg("--cwd")
            .arg(dir)
            .arg("--repo-root")
            .arg(root)
            .arg("--step-cmd")
            .arg(&step)
            .arg("--grace")
            .arg(grace.to_string())
            .spawn()
            .expect("spawn the daemon");

        // Drain stderr into the shared buffer for as long as the daemon lives.
        // A daemon that is killed mid-line leaves a partial record, which is
        // fine: nothing here parses across lines.
        let mut stderr = child.stderr.take().expect("piped stderr");
        let log_reader = Arc::clone(&log);
        std::thread::spawn(move || {
            let mut buffer = String::new();
            let mut chunk = [0u8; 4096];
            loop {
                match stderr.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        buffer.push_str(&String::from_utf8_lossy(&chunk[..read]));
                        while let Some(index) = buffer.find('\n') {
                            let line = buffer[..index].trim().to_string();
                            buffer.drain(..=index);
                            if !line.is_empty() {
                                if let Ok(mut held) = log_reader.lock() {
                                    held.push(line);
                                }
                            }
                        }
                    }
                }
            }
        });

        let daemon = Self {
            child,
            endpoint,
            adapter_endpoint,
            dir: dir.to_path_buf(),
            log,
        };
        daemon.wait_until_reachable();
        daemon
    }

    /// Whether the daemon has exited on its own.
    ///
    /// The state field, not the directory's existence: an unreaped child is a
    /// zombie and `/proc/<pid>` is still there, which made the first version of
    /// this report a daemon that had just logged "exiting" as still running.
    /// `Child::try_wait` would be the honest call but needs `&mut self`, and
    /// several assertions read this at once. This file is `#![cfg(unix)]`
    /// already, so the probe is available wherever the tests are.
    fn exited(&self) -> bool {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", self.child.id())) else {
            return true;
        };
        // `comm` is parenthesised and may itself contain spaces or parentheses,
        // so the state is the first field after the *last* ')'.
        match stat.rfind(')') {
            Some(close) => stat[close + 1..]
                .split_whitespace()
                .next()
                .is_none_or(|state| state == "Z"),
            None => true,
        }
    }

    /// Everything the daemon has written to stderr so far.
    fn log(&self) -> Vec<String> {
        self.log.lock().map(|held| held.clone()).unwrap_or_default()
    }

    /// This daemon's identity, the way an updater has to name it.
    ///
    /// The daemon logs its epoch at startup (`studio-host.rs`: "step-studio-host:
    /// epoch {epoch}, endpoint …") and builds its instance id from that epoch and
    /// its own pid. Neither is on any query, so the marker a test writes has to be
    /// assembled from the log plus the pid it already knows — which is exactly
    /// what the real updater has to do too, since the shell learns the instance
    /// id the same way.
    fn instance_id(&self) -> String {
        let epoch = self
            .log()
            .iter()
            .find_map(|line| {
                // The line is "step-studio-host: epoch 361, endpoint …", so the
                // number carries a trailing comma and is not at a fixed index.
                let mut words = line.split_whitespace();
                while let Some(word) = words.next() {
                    if word == "epoch" {
                        return words
                            .next()
                            .and_then(|value| value.trim_end_matches(',').parse::<u64>().ok());
                    }
                }
                None
            })
            .unwrap_or_else(|| panic!("no epoch in the daemon log: {:?}", self.log()));
        format!("daemon-{epoch}-{}", self.child.id())
    }

    /// Wait until the daemon answers on its endpoint.
    fn wait_until_reachable(&self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if std::os::unix::net::UnixStream::connect(&self.endpoint).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("the daemon never answered on {}", self.endpoint);
    }

    /// The adapter process the daemon is supervising, if it is up.
    fn adapter_pids(&self) -> Vec<u32> {
        adapter_pids(&self.adapter_endpoint)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // Killing an already-reaped child is an error, not a problem: the
        // point is that nothing is left behind either way.
        let _ = self.child.kill();
        let _ = self.child.wait();
        // A daemon that exited on its own is the interesting case for these
        // tests, and the only place its reasoning exists is stderr. Echo it
        // only when the test is already failing: a green run must stay quiet.
        if std::thread::panicking() {
            for line in self.log() {
                eprintln!("[daemon] {line}");
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root")
        .to_path_buf()
}

/// Connect as a surface: client-only, so this session owns no child and must
/// not shut the runtime down when it is dropped.
fn connect(daemon: &Daemon) -> BridgeSession {
    let launch = AdapterLaunch {
        node: String::new(),
        adapter_script: String::new(),
        socket_path: daemon.endpoint.clone(),
        token_file: daemon.dir.join("token").to_string_lossy().to_string(),
        cwd: daemon.dir.to_string_lossy().to_string(),
        runtime_epoch: 1,
        env: Vec::new(),
        // The daemon owns the adapter and therefore the token.
        token: Some("step-studio".to_string()),
        command: Vec::new(),
    };
    BridgeSession::connect(&launch).expect("connect to the daemon")
}

/// Every process whose command line mentions `needle`.
///
/// The daemon's own command line mentions the adapter endpoint too — as
/// `--adapter-endpoint` — so a caller looking for the adapter has to be specific
/// about which flag it means.
fn pids_matching(needle: &str) -> Vec<u32> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return found;
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(cmdline) = std::fs::read_to_string(entry.path().join("cmdline")) else {
            continue;
        };
        if cmdline.replace('\0', " ").contains(needle) {
            found.push(pid);
        }
    }
    found
}

/// The adapter processes, as opposed to the daemon that launched them.
fn adapter_pids(endpoint: &str) -> Vec<u32> {
    pids_matching(&format!("--bridge-endpoint {endpoint}"))
}

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn a_shell_restart_keeps_the_conversation() {
    let daemon = Daemon::start("restart");

    // A turn, so there is a conversation to keep.
    let first = connect(&daemon);
    let (receipt, events) = first
        .invoke(serde_json::json!({ "kind": "core.prompt", "text": "hi" }))
        .expect("the first turn");
    assert_eq!(
        receipt.get("status").and_then(|s| s.as_str()),
        Some("completed")
    );
    let deltas: String = events
        .iter()
        .filter(|event| event.get("kind") == Some(&serde_json::json!("conversation.message.delta")))
        .filter_map(|event| event.get("delta").and_then(|d| d.as_str()))
        .collect();
    assert_eq!(deltas, "hello!");

    // The shell goes away. Dropping the session must not end the runtime: the
    // daemon owns it, and this is the whole point of M2.
    drop(first);
    wait_until("the adapter to survive the shell", || {
        !daemon.adapter_pids().is_empty()
    });
    assert!(
        !daemon.adapter_pids().is_empty(),
        "the adapter died with the shell, which is the behaviour M2 removes"
    );

    // A new shell arrives and sees the same session.
    let second = connect(&daemon);
    let snapshot = second.snapshot().expect("snapshot after reconnect");
    let session_id = snapshot
        .pointer("/snapshot/sessionId")
        .and_then(|v| v.as_str())
        .expect("sessionId");
    assert!(!session_id.is_empty(), "the session did not survive");
    let (receipt, _) = second
        .invoke(serde_json::json!({ "kind": "core.prompt", "text": "again" }))
        .expect("a turn after reconnect");
    assert_eq!(
        receipt.get("status").and_then(|s| s.as_str()),
        Some("completed")
    );
}

#[test]
fn killing_the_daemon_does_not_orphan_the_runtime() {
    let mut daemon = Daemon::start("orphan");
    let session = connect(&daemon);
    let _ = session.snapshot().expect("snapshot");
    wait_until("the adapter to come up", || {
        !daemon.adapter_pids().is_empty()
    });

    // The daemon holds the adapter's socket. Killing it closes that socket, and
    // the adapter's serve loop ends on the EOF — which is what keeps the
    // runtime from being reparented and holding the session open.
    daemon.child.kill().expect("kill the daemon");
    let _ = daemon.child.wait();
    wait_until("the adapter to notice", || daemon.adapter_pids().is_empty());
}

#[test]
fn the_daemon_drains_and_exits_when_no_surface_stays() {
    let daemon = Daemon::build("drain", 3);
    let session = connect(&daemon);
    let _ = session.snapshot().expect("snapshot");
    // Leaving starts the grace clock, which this daemon was given as 3 seconds.
    drop(session);

    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if daemon.adapter_pids().is_empty() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("the daemon never drained its adapter");
}

#[test]
fn a_second_daemon_defers_to_the_one_that_is_running() {
    let daemon = Daemon::start("single");
    let root = repo_root();
    let second = Command::new(root.join("target/debug/step-studio-host"))
        .arg("--endpoint")
        .arg(&daemon.endpoint)
        .arg("--adapter-endpoint")
        .arg(daemon.dir.join("other.sock"))
        .arg("--cwd")
        .arg(&daemon.dir)
        .arg("--repo-root")
        .arg(&root)
        .arg("--grace")
        .arg("3")
        .output()
        .expect("run a second daemon");
    // Starting the app twice is not an error: the second process says so and
    // leaves the running daemon alone.
    assert!(
        second.status.success(),
        "the second daemon should exit cleanly"
    );
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("another daemon owns"),
        "the second daemon should say why it is leaving: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    // And the first is still the one serving.
    let session = connect(&daemon);
    let _ = session.snapshot().expect("the first daemon still serves");
}

#[test]
fn a_shutdown_drains_the_turn_in_flight() {
    let daemon = Daemon::start("drain-turn");
    let session = connect(&daemon);
    let _ = session.snapshot().expect("snapshot");

    // Both fired without waiting, so the shutdown lands while the turn is
    // still running — the fake CLI takes 1.5 s to settle a "slow" turn.
    session
        .send_command(serde_json::json!({ "kind": "core.prompt", "text": "slow" }))
        .expect("prompt");
    session
        .send_command(serde_json::json!({ "kind": "runtime.shutdown", "drain": true }))
        .expect("shutdown");

    // The turn has to finish. `drain: true` used to abort it and call that a
    // drain, which is a truncated turn the client cannot tell from a real one.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut deltas = String::new();
    while Instant::now() < deadline {
        for event in session.drain_events() {
            if event.get("kind") == Some(&serde_json::json!("conversation.message.delta")) {
                if let Some(delta) = event.get("delta").and_then(|d| d.as_str()) {
                    deltas.push_str(delta);
                }
            }
        }
        if !deltas.is_empty() {
            assert_eq!(deltas, "hello!", "the drained turn produced its answer");
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the turn was truncated by the shutdown instead of being drained");
}

#[test]
fn a_surface_is_released_when_its_adapter_dies() {
    // The daemon's pump ends when the adapter does, and the surface has to be
    // told: a socket left open on both sides leaves the shell waiting on a peer
    // that will never speak again, which is what stopped the shell from ever
    // reconnecting.
    let daemon = Daemon::start("adapter-death");
    let session = connect(&daemon);
    let _ = session.snapshot().expect("snapshot");
    wait_until("the adapter to come up", || {
        !daemon.adapter_pids().is_empty()
    });

    // Kill the adapter out from under the daemon. The supervisor will bring up
    // another one, but this surface belongs to the dead one.
    for pid in daemon.adapter_pids() {
        let _ = std::process::Command::new("kill")
            .arg("-9")
            .arg(pid.to_string())
            .status();
    }

    // The surface must be released rather than left holding a dead relay. What
    // it observes is the connection ending: the next command fails instead of
    // hanging, which is the difference between "reconnect" and "wait forever".
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut released = false;
    while Instant::now() < deadline {
        if session.close_reason().is_some() {
            released = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        released,
        "the surface was never told its adapter died, so the shell would never reconnect"
    );

    // And the daemon is still serving: a new surface gets a new session.
    let replacement = connect(&daemon);
    let snapshot = replacement
        .snapshot()
        .expect("a snapshot from the new adapter");
    assert!(
        snapshot.pointer("/snapshot/sessionId").is_some(),
        "the daemon kept serving after the adapter was replaced"
    );
}

#[test]
fn a_usage_prompt_comes_back_on_the_notify_channel() {
    // `/usage` is a slash command, not a message: the CLI dispatches it and the
    // panel arrives on the fire-and-forget notify channel rather than as the
    // prompt's response. stdin has to stay open while it waits, or the CLI
    // cancels its activity scan and all that comes back is `extension_error`.
    let daemon = Daemon::start("usage");
    let session = connect(&daemon);

    let (receipt, _events) = session
        .invoke(serde_json::json!({ "kind": "usage.panel" }))
        .expect("usage.panel");
    assert_eq!(
        receipt.get("status").and_then(|s| s.as_str()),
        Some("completed")
    );

    let panel = receipt
        .pointer("/result/panel")
        .and_then(|v| v.as_str())
        .expect("the panel text");
    assert!(
        panel.contains("Usage"),
        "the panel is the CLI's own: {panel}"
    );
    assert!(
        panel.contains("Total tokens"),
        "the panel carries the totals the structured arrays cannot: {panel}"
    );
}

#[test]
fn a_model_catalog_reaches_the_shell_through_the_bridge() {
    // The facade asks for `models.get`; the adapter answers from the CLI's
    // catalog. The reshape that turns one into the other is the shell's job, so
    // what this pins is that the catalog itself survives the trip.
    let daemon = Daemon::start("models");
    let session = connect(&daemon);

    let (receipt, _events) = session
        .invoke(serde_json::json!({ "kind": "session.models.list" }))
        .expect("session.models.list");
    assert_eq!(
        receipt.get("status").and_then(|s| s.as_str()),
        Some("completed")
    );
    let models = receipt
        .pointer("/result/models")
        .and_then(Value::as_array)
        .expect("the catalog");
    assert!(!models.is_empty(), "the CLI reported a catalog");
    // A selector is what the composer keys its picker on, so every entry needs
    // one — an entry without it is a model the UI cannot select.
    for model in models {
        assert!(
            model
                .get("selector")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty()),
            "every catalog entry carries a selector: {model}"
        );
    }
}

#[test]
fn a_daemon_whose_adapter_cannot_start_exits_instead_of_degrading() {
    // Sitting there would be worse than leaving: the daemon would hold the
    // endpoint and answer nothing, a surface would wait out its connect timeout,
    // and the launcher would never count a failure — so the "the daemon cannot
    // start" terminal state could never be reached.
    let dir = std::env::temp_dir().join(format!("host-daemon-halt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("make a temp dir");
    let root = repo_root();
    let endpoint = dir.join("daemon.sock").to_string_lossy().to_string();
    let child = Command::new(root.join("target/debug/step-studio-host"))
        .arg("--endpoint")
        .arg(&endpoint)
        .arg("--adapter-endpoint")
        .arg(dir.join("adapter.sock"))
        .arg("--cwd")
        .arg(&dir)
        .arg("--repo-root")
        .arg(&root)
        // A CLI that does not exist, so every adapter start fails.
        .arg("--step-cmd")
        .arg(dir.join("no-such-step"))
        .arg("--grace")
        .arg("5")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn the daemon");

    // Three failed starts, then the halt grace. Bounded so a hang fails rather
    // than stalls the suite: `wait_with_output` takes the child, so it is tried
    // once and the deadline is enforced by killing instead.
    let mut child = child;
    let deadline = Instant::now() + Duration::from_secs(90);
    let output = loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                break child
                    .wait_with_output()
                    .expect("collect the daemon's output")
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(200));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the daemon never exited, so it would have degraded in place");
            }
            Err(error) => panic!("waiting for the daemon: {error}"),
        }
    };
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("exiting after"),
        "the daemon should say why it is leaving: {stderr}"
    );
    // It left the endpoint behind for nobody.
    assert!(
        !Path::new(&endpoint).exists(),
        "the socket file was cleaned up"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_usage_prompt_does_not_disturb_the_live_conversation() {
    // `/usage` is sent as a prompt, and §9.6 requires that such a prompt not
    // occupy a conversation turn. Nothing verified that, so this measures it:
    // a turn is started, the usage panel is fetched mid-turn, and the turn has
    // to finish exactly as it would have without the interruption.
    let daemon = Daemon::start("usage-mid-turn");
    let session = connect(&daemon);
    let _ = session.snapshot().expect("snapshot");
    let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));

    // Start a turn that outlives the panel fetch: the fake settles "slow" after
    // 1.5 s, and the fetch takes about that long too.
    session
        .send_command(serde_json::json!({ "kind": "core.prompt", "text": "slow" }))
        .expect("start the turn");

    // The turn runs, and the panel is fetched on the same surface while it is
    // in flight — which is the arrangement the shell actually has, one surface.
    //
    // The subtler half of the requirement cannot be measured here and is not
    // pretended to be: `/usage` emits its own `agent_settled`, which the adapter
    // reads as "the turn is over", so the turn's terminal receipt *could* be
    // settled early. Proving it isn't needs the panel fetched from a second
    // concurrent surface, and the daemon serves one at a time until M2.3. What
    // this does prove is that the turn's answer survives the fetch intact.
    let collector = std::sync::Arc::clone(&seen);
    let (receipt, _) = session
        .invoke_collecting(serde_json::json!({ "kind": "usage.panel" }), move |event| {
            if event.get("kind") == Some(&serde_json::json!("conversation.message.delta")) {
                if let Some(delta) = event.get("delta").and_then(|d| d.as_str()) {
                    collector.lock().expect("lock").push_str(delta);
                }
            }
        })
        .expect("usage.panel mid-turn");
    assert_eq!(
        receipt.get("status").and_then(|s| s.as_str()),
        Some("completed"),
        "the panel must come back even with a turn in flight"
    );

    // Anything that landed after the fetch is drained here.
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline && seen.lock().expect("lock").is_empty() {
        for event in session.drain_events() {
            if event.get("kind") == Some(&serde_json::json!("conversation.message.delta")) {
                if let Some(delta) = event.get("delta").and_then(|d| d.as_str()) {
                    seen.lock().expect("lock").push_str(delta);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        &*seen.lock().expect("lock"),
        "hello!",
        "the turn in flight completed its answer despite the panel fetch"
    );
}

#[test]
fn a_surface_from_another_uid_is_refused() {
    // The endpoint lives in a shared temp directory, so without a peer check any
    // local user who can reach it drives another user's session and working
    // directory. This process cannot *be* another uid, so the test pins the
    // mechanism rather than the attack: the check is applied, and a same-uid
    // surface — the normal case — still gets through.
    let daemon = Daemon::start("peer-uid");
    let session = connect(&daemon);
    let snapshot = session.snapshot().expect("the same-uid surface is served");
    assert!(
        snapshot.pointer("/snapshot/sessionId").is_some(),
        "a surface from the daemon's own user must not be refused"
    );
}

#[test]
fn the_daemon_answers_the_process_tree_itself() {
    // The process tree is a fact about the daemon, and the adapter has no such
    // capability — so the daemon answers it rather than forwarding it. This is
    // the first operation that terminates in the daemon instead of passing
    // through, which is the seam M2.3's multiplexing goes in.
    let daemon = Daemon::start("process-tree");
    let session = connect(&daemon);

    let (receipt, _events) = session
        .invoke(serde_json::json!({ "kind": "process.tree.get" }))
        .expect("process.tree.get");
    assert_eq!(
        receipt.get("status").and_then(|s| s.as_str()),
        Some("completed"),
        "the daemon answered rather than refusing: {receipt}"
    );
    let processes = receipt
        .pointer("/result/processes")
        .and_then(Value::as_array)
        .expect("the tree");
    assert!(
        !processes.is_empty(),
        "the daemon itself is in its own tree"
    );
    // The daemon is the root, and its direct child is the adapter — the role
    // comes from the depth, not from the executable's name.
    assert_eq!(
        processes[0].get("role").and_then(Value::as_str),
        Some("daemon"),
        "the tree is rooted at the daemon"
    );
    assert!(
        processes
            .iter()
            .any(|p| p.get("role").and_then(Value::as_str) == Some("adapter")),
        "the adapter the daemon spawned is in the tree: {processes:?}"
    );
    // And the total is the sum of what was reported, never invented.
    let summed: u64 = processes
        .iter()
        .filter_map(|p| p.get("rssBytes").and_then(Value::as_u64))
        .sum();
    assert_eq!(
        receipt
            .pointer("/result/totalRssBytes")
            .and_then(Value::as_u64),
        Some(summed)
    );
}

#[test]
fn two_surfaces_both_receive_the_same_broadcast() {
    // The adapter broadcasts with no addressing, so the daemon has to fan out
    // rather than let two readers split one stream — which is what the old
    // one-surface gate did, by making the second surface wait instead. Both
    // surfaces must see the same events, and neither may see a gap: a frame one
    // of them missed must not advance the other's view of the stream.
    let daemon = Daemon::start("fan-out");
    let first = connect(&daemon);
    let second = connect(&daemon);

    let (receipt, first_events) = first
        .invoke(serde_json::json!({ "kind": "core.prompt", "text": "hi" }))
        .expect("the first surface's turn");
    assert_eq!(
        receipt.get("status").and_then(|s| s.as_str()),
        Some("completed")
    );

    // The second surface was attached the whole time, so it saw the same turn
    // arrive — its events were queued while it issued nothing, which is exactly
    // the case `drain_events` exists for.
    let mut second_events = second.drain_events();
    // A turn takes a moment to cross; give it the same chance the first got.
    if second_events.is_empty() {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && second_events.is_empty() {
            second_events = second.drain_events();
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    let deltas = |events: &[Value]| -> String {
        events
            .iter()
            .filter(|event| {
                event.get("kind") == Some(&serde_json::json!("conversation.message.delta"))
            })
            .filter_map(|event| event.get("delta").and_then(|d| d.as_str()))
            .collect()
    };
    assert_eq!(
        deltas(&first_events),
        "hello!",
        "the surface that ran the turn saw its answer"
    );
    assert_eq!(
        deltas(&second_events),
        "hello!",
        "the surface that only watched saw the same answer — the stream fanned out \\
         rather than being split"
    );

    // And the watcher can still act: its own command reaches the same adapter.
    let (receipt, _) = second
        .invoke(serde_json::json!({ "kind": "session.models.list" }))
        .expect("the watcher's own query");
    assert_eq!(
        receipt.get("status").and_then(|s| s.as_str()),
        Some("completed"),
        "a second surface can drive the same session"
    );
}

#[test]
fn two_surfaces_asking_at_the_same_time_each_get_their_own_reply() {
    // What this pins: two surfaces attached at once can each complete a
    // request, with both in flight simultaneously. Two relay threads then write
    // to the one adapter connection through the shared writer, and a frame
    // interleaved between them would corrupt the adapter's stream — the adapter
    // would fail to decode, nobody would answer, and this would time out.
    //
    // What it does *not* pin is the duplicate-id collision. That one is
    // `routes::tests::a_repeated_id_is_reported_rather_than_absorbed`, because
    // it is not observable from here: with two surfaces minting the same id,
    // the second reply finds nothing to settle and falls through to the
    // broadcast, which rescues the surface that lost its slot. An end-to-end
    // test therefore passes either way, which is exactly why the routing table
    // was moved into the library where it can be tested directly.
    //
    // The existing `two_surfaces_both_receive_the_same_broadcast` asks
    // sequentially, so only this one overlaps the two requests.
    let daemon = Daemon::start("concurrent");
    let first = connect(&daemon);
    let second = connect(&daemon);
    // One request each, in flight at the same moment. Attached before either
    // asks, so both are live surfaces of one daemon.
    let (first_tx, first_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = first_tx.send(first.snapshot().map(|snapshot| {
            snapshot
                .pointer("/snapshot/sessionId")
                .and_then(|id| id.as_str())
                .unwrap_or("")
                .to_string()
        }));
    });
    let (second_tx, second_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = second_tx.send(second.snapshot().map(|snapshot| {
            snapshot
                .pointer("/snapshot/sessionId")
                .and_then(|id| id.as_str())
                .unwrap_or("")
                .to_string()
        }));
    });

    for (who, rx) in [
        ("the first surface", first_rx),
        ("the second surface", second_rx),
    ] {
        // A reply that never arrives is the failure. Without a timeout here the
        // test would hang rather than fail, and a hung test proves nothing to
        // whoever runs the suite next.
        let session_id = rx
            .recv_timeout(Duration::from_secs(30))
            .unwrap_or_else(|_| panic!("{who} never got a reply to its own request"))
            .unwrap_or_else(|error| panic!("{who} failed its own request: {error}"));
        assert!(
            !session_id.is_empty(),
            "{who} got a snapshot with no session"
        );
    }
}

// ---------------------------------------------------------------------------
// §12.4's hot-replace sequence, end to end.
//
// The three thirds of the sequence each have unit tests: `upgrade::decide` for
// the drain decision, `swap`/`rollback` for the package move, `signing` for the
// manifest. What none of them prove is that the thirds compose — that a real
// daemon actually leaves when an update names it, that the package that lands is
// the one that was staged, and that the user's data is still there afterwards.
// `design/progress/m5.md` recorded these as "卡 B1 (no Windows machine)", which
// is wrong: the sequence is platform-independent Rust, and the only part that
// needs Windows is the installer that calls it.
// ---------------------------------------------------------------------------

/// Where an updater leaves the marker the daemon polls for.
///
/// The daemon reads `<cwd>/.step-studio/upgrade-requested.json`, so the test's
/// own `--cwd` decides where the marker goes — no guessing.
fn marker_in(dir: &Path) -> PathBuf {
    dir.join(".step-studio")
}

#[test]
fn an_upgrade_marker_drains_the_daemon_that_is_running() {
    // Grace is deliberately far longer than the test: the only thing that may
    // end this daemon is the marker. A daemon that left because its grace
    // expired would make this test pass for the wrong reason.
    let daemon = Daemon::build("upgrade-drain", 600);
    let instance_id = daemon.instance_id();

    step_studio_host::upgrade::request(&marker_in(&daemon.dir), &instance_id)
        .expect("write the upgrade marker");

    // The watch loop polls every 250 ms, so "immediately" is a quarter second.
    // Two seconds is already eight polls of slack for a loaded machine.
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && !daemon.exited() {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        daemon.exited(),
        "the daemon did not drain for an update that named it; log: {:?}",
        daemon.log()
    );
    assert!(
        !marker_in(&daemon.dir).join("upgrade-requested.json").exists(),
        "the daemon drained but left the marker behind, so the next daemon would read it as stale"
    );
}

#[test]
fn a_marker_left_by_a_half_finished_update_does_not_stop_the_next_daemon() {
    // The failure this guards: an update writes a marker, the daemon it named
    // dies before consuming it, and every daemon after that honours it anyway —
    // a machine whose daemon never boots, caused by an update that half-finished.
    //
    // The marker goes into the daemon's own cwd, which is where it looks, so it
    // is written after the daemon is up rather than into a directory of the
    // test's own invention — the first version did that and asserted against a
    // file the daemon had never been told about.
    let daemon = Daemon::build("upgrade-stale", 600);
    step_studio_host::upgrade::request(&marker_in(&daemon.dir), "daemon-999999-somebody-else")
        .expect("write the stale marker");

    // Long enough to have crossed several poll intervals, so a daemon that meant
    // to honour the marker has had every chance to.
    std::thread::sleep(Duration::from_millis(1200));
    assert!(
        !daemon.exited(),
        "the daemon exited for a marker naming another daemon; log: {:?}",
        daemon.log()
    );
    assert!(
        marker_in(&daemon.dir).join("upgrade-requested.json").exists(),
        "the stale marker was consumed; the operator lost the evidence an update did not land"
    );
    assert!(
        daemon.log().iter().any(|line| line.contains("which is not this one")),
        "the daemon did not report the stale marker; log: {:?}",
        daemon.log()
    );
}

#[test]
fn the_hot_replace_sequence_lands_the_staged_build_and_a_rollback_restores_the_old_one() {
    // §12.4's four acceptance cases, minus the two `signing` already covers
    // (downgrade refused) and the two `migrate` already covers (credentials
    // passed through). What is left is the sequence itself: drain → swap →
    // restart, then rollback, with the user's data untouched throughout.
    let root = repo_root();
    let daemon_bin = root.join("target/debug/step-studio-host");
    assert!(daemon_bin.is_file(), "build the daemon first");

    let dir = std::env::temp_dir().join(format!("host-daemon-swap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let install = dir.join("install");
    let staged = dir.join("staged");
    let data = dir.join("data");
    let work = dir.join("work");
    for each in [&install, &staged, &data, &work] {
        std::fs::create_dir_all(each).expect("make a scratch dir");
    }

    // Two distinguishable builds. The swap is a rename, so "which one is live"
    // is a question about content, not about a version string nothing records.
    let original = std::fs::read(&daemon_bin).expect("read the daemon binary");
    let mut staged_bytes = original.clone();
    staged_bytes.extend_from_slice(b"staged-build");
    let target = install.join("step-studio-host");
    let new = staged.join("step-studio-host");
    std::fs::write(&target, &original).expect("install the current build");
    std::fs::write(&new, &staged_bytes).expect("stage the new build");
    // `fs::write` creates a 0644 file, and a daemon nobody can exec is a daemon
    // that never starts — which would look like a swap failure rather than the
    // permissions problem it is.
    #[cfg(unix)]
    for binary in [&target, &new] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(binary, std::fs::Permissions::from_mode(0o755))
            .expect("make the build executable");
    }

    // The user's data: a file the sequence has no business touching.
    let kept = data.join("index.sqlite");
    let kept_bytes = b"the user's index, which no update may rewrite";
    std::fs::write(&kept, kept_bytes).expect("write the user's data");

    // --- first daemon, drained by an update that names it ---
    let first = Daemon::spawn("swap-first", 600, &target, &work, &root);
    step_studio_host::upgrade::request(&marker_in(&work), &first.instance_id())
        .expect("write the upgrade marker");
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && !first.exited() {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        first.exited(),
        "the running daemon did not drain for the update; log: {:?}",
        first.log()
    );

    // --- the swap ---
    let outcome = step_studio_host::swap::swap(&new, &target).expect("the swap");
    assert_eq!(
        std::fs::read(&target).expect("read the installed build"),
        staged_bytes,
        "the staged build did not land"
    );
    assert_eq!(
        std::fs::read(outcome.aside.expect("the old build was moved aside"))
            .expect("read the aside build"),
        original,
        "the previous version was not kept aside for a rollback"
    );
    assert_eq!(
        std::fs::read(&kept).expect("read the user's data"),
        kept_bytes,
        "the swap touched the user's data"
    );

    // --- second daemon, from the swapped-in build ---
    let second = Daemon::spawn("swap-second", 600, &target, &work, &root);
    assert!(
        std::os::unix::net::UnixStream::connect(&second.endpoint).is_ok(),
        "the new build did not come up; log: {:?}",
        second.log()
    );

    // --- and the rollback, which must put the first build back exactly ---
    step_studio_host::swap::rollback(&target).expect("the rollback");
    assert_eq!(
        std::fs::read(&target).expect("read the rolled-back build"),
        original,
        "the rollback did not restore the previous build byte for byte"
    );
    assert_eq!(
        std::fs::read(&kept).expect("read the user's data"),
        kept_bytes,
        "the rollback touched the user's data"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
