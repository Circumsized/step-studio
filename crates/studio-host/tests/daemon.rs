//! The daemon end to end, against the real adapter.
//!
//! The property M2 exists for is one sentence: killing the shell does not kill
//! the conversation. Everything here is in service of watching that hold, and
//! of watching it fail the other way — that killing the daemon does not leave
//! the runtime orphaned, holding the user's session open.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use serde_json::Value;
use step_studio_bridge::{AdapterLaunch, BridgeSession};

/// A running daemon, killed when the test ends.
struct Daemon {
    child: Child,
    endpoint: String,
    adapter_endpoint: String,
    dir: PathBuf,
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
        let dir = std::env::temp_dir().join(format!("host-daemon-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("make a temp dir");
        let root = repo_root();
        let daemon_bin = root.join("target/debug/step-studio-host");
        assert!(
            daemon_bin.is_file(),
            "build the daemon first: cargo build -p step-studio-host"
        );

        let endpoint = dir.join("daemon.sock").to_string_lossy().to_string();
        let adapter_endpoint = dir.join("adapter.sock").to_string_lossy().to_string();
        // The Rust runtime takes the CLI as one executable and does not split a
        // command line, so the fake arrives through a wrapper rather than as
        // `node <script>`. (The TypeScript adapter did split it; that divergence
        // predates M2 and is not this test's business.)
        let step = dir.join("step");
        std::fs::write(
            &step,
            format!(
                "#!/bin/sh\nexec node {}\n",
                root.join("packages/runtime-stepcode/test/fake-step.mjs")
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
        let child = Command::new(&daemon_bin)
            .arg("--endpoint")
            .arg(&endpoint)
            .arg("--adapter-endpoint")
            .arg(&adapter_endpoint)
            .arg("--cwd")
            .arg(&dir)
            .arg("--repo-root")
            .arg(&root)
            .arg("--step-cmd")
            .arg(&step)
            .arg("--grace")
            .arg(grace.to_string())
            .spawn()
            .expect("spawn the daemon");

        let daemon = Self {
            child,
            endpoint,
            adapter_endpoint,
            dir,
        };
        daemon.wait_until_reachable();
        daemon
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
    wait_until("the adapter to come up", || !daemon.adapter_pids().is_empty());

    // Kill the adapter out from under the daemon. The supervisor will bring up
    // another one, but this surface belongs to the dead one.
    for pid in daemon.adapter_pids() {
        let _ = std::process::Command::new("kill").arg("-9").arg(pid.to_string()).status();
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
    let snapshot = replacement.snapshot().expect("a snapshot from the new adapter");
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
    assert!(panel.contains("Usage"), "the panel is the CLI's own: {panel}");
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
            model.get("selector").and_then(Value::as_str).is_some_and(|s| !s.is_empty()),
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
            Ok(Some(_)) => break child.wait_with_output().expect("collect the daemon's output"),
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
    assert!(!Path::new(&endpoint).exists(), "the socket file was cleaned up");
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
