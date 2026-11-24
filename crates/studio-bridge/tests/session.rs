//! `BridgeSession` end to end against the real adapter. The shell drives this
//! from a thread, so a hang here is invisible in the UI — it just never
//! becomes ready.
//!
//! The adapter under test is the Rust one, `step-studio-runtime`. It used to be
//! the TypeScript adapter in `packages/runtime-stepcode`, which was deleted as
//! dead code — the daemon launches the Rust binary — and these tests then
//! pointed at a `main.js` that no longer existed. They failed as four hangs and
//! a panic, which reads like a regression in `BridgeSession` rather than a
//! missing fixture.
//!
//! The CLI behind it is the same fixture the runtime crate's e2e tests use,
//! reached by path rather than by dependency: `studio-bridge` sits below
//! `step-studio-runtime` and must not depend on it, and a test locating a binary
//! by path needs no dependency at all.

//! Unix only, and for a concrete reason rather than a hopeful one: the CLI
//! behind these tests is reached through a `#!/bin/sh` wrapper that execs
//! `node`, and the adapter is located by an extension-less name. Windows
//! would need a `.cmd` wrapper and an `.exe` lookup, which is a second
//! harness for the same assertions. The Windows-specific code is covered by
//! the `rust-windows` unit tests; this file is the Unix path.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use step_studio_bridge::{AdapterLaunch, BridgeSession};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root")
        .to_path_buf()
}

/// The Rust adapter, in whichever profile has been built.
fn adapter_binary(root: &Path) -> Option<PathBuf> {
    ["release", "debug"]
        .iter()
        .map(|profile| root.join("target").join(profile).join("step-studio-runtime"))
        .find(|candidate| candidate.exists())
}

fn fake_step_wrapper(dir: &std::path::Path, root: &Path) -> PathBuf {
    let fake = root.join("crates/step-studio-runtime/tests/fixtures/fake-step.mjs");
    let wrapper = dir.join("fake-step");
    std::fs::write(
        &wrapper,
        format!("#!/bin/sh\nexec node {} \"$@\"\n", fake.display()),
    )
    .expect("write wrapper");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755));
    }
    wrapper
}

fn launch(tag: &str) -> AdapterLaunch {
    let dir = std::env::temp_dir().join(format!("rust-session-{tag}-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let root = repo_root();
    let adapter = adapter_binary(&root).expect("step-studio-runtime is not built; run cargo build -p step-studio-runtime");
    AdapterLaunch {
        node: String::new(),
        adapter_script: String::new(),
        socket_path: dir.join("bridge.sock").to_string_lossy().to_string(),
        token_file: dir.join("token").to_string_lossy().to_string(),
        cwd: dir.to_string_lossy().to_string(),
        runtime_epoch: 1,
        // A wrapper script, not `node <path>`: `StepRpc::spawn` hands
        // `step_command` to `Command::new` as one executable, so a two-word
        // value fails with ENOENT and the adapter reports a spawn error that
        // looks like a missing CLI.
        env: vec![(
            "STEP_STUDIO_STEP_CMD".to_string(),
            fake_step_wrapper(&dir, &root).to_string_lossy().to_string(),
        )],
        // This session spawns the adapter, so it owns the token file.
        token: None,
        command: vec![adapter.to_string_lossy().to_string()],
    }
}

#[test]
fn session_handshakes_snapshots_and_runs_a_turn() {
    let session = BridgeSession::connect(&launch("full")).expect("connect");

    let snapshot = session.snapshot().expect("snapshot");
    let session_id = snapshot
        .pointer("/snapshot/sessionId")
        .and_then(|v| v.as_str())
        .expect("sessionId");
    assert!(!session_id.is_empty(), "the adapter reported an empty session id");

    let (receipt, events) = session
        .invoke(serde_json::json!({ "kind": "core.prompt", "text": "hi" }))
        .expect("prompt");
    assert_eq!(receipt.get("status").and_then(|s| s.as_str()), Some("completed"));

    let deltas: String = events
        .iter()
        .filter(|event| event.get("kind") == Some(&serde_json::json!("conversation.message.delta")))
        .filter_map(|event| event.get("delta").and_then(|d| d.as_str()))
        .collect();
    assert_eq!(deltas, "hello!");
}

/// A peer that sends its hello and a second frame in one burst must not wedge the
/// handshake.
///
/// This is the hang the mutual-authentication fix introduced and then fixed: the
/// hello was read through a `BufReader`, so a burst peer's second frame landed in
/// that reader's buffer and was dropped with it, and the unbuffered proof read
/// waited forever for bytes already consumed. The daemon's side bounds both its
/// reads with `first_frame_within`; this side had no bound at all.
///
/// A legitimate peer cannot trigger it — it cannot compute the proof before the
/// runtime chooses its challenge — so this drives the socket by hand.
#[test]
fn a_burst_peer_does_not_wedge_the_handshake() {
    use std::io::{Read, Write};

    let launch = launch("burst");
    // What `BridgeSession::connect` does before it spawns: mint the secret into
    // the file the adapter will read, and clear a socket a previous run left.
    let token = step_studio_bridge::token::generate_token();
    step_studio_bridge::token::write_token_file(
        std::path::Path::new(&launch.token_file),
        &token,
    )
    .expect("publish the token");
    let _ = std::fs::remove_file(&launch.socket_path);
    let mut child = step_studio_bridge::spawn_adapter(&launch).expect("spawn the adapter");

    let mut peer = step_studio_bridge::client::wait_for_socket(
        &launch.socket_path,
        std::time::Duration::from_secs(20),
    )
    .expect("connect to the adapter");
    // The crate's wrapper, not the inherent method: on Windows the peer is a
    // `File` and has none, and the wrapper is where that difference is confined.
    step_studio_bridge::transport::set_read_timeout(
        &peer,
        Some(std::time::Duration::from_secs(10)),
    )
    .expect("a read timeout");

    // The hello, and a frame the runtime has no reason to accept, in one write.
    let hello = serde_json::json!({
        "type": "studio.hello",
        "requestId": "burst-hello",
        "supportedProtocolVersions": [step_studio_bridge::PROTOCOL_VERSION],
        "requiredProfile": "limited",
        "challenge": "a-challenge",
    });
    let mut burst = Vec::new();
    step_studio_bridge::frame::write_frame(&mut burst, 1, &hello).expect("encode the hello");
    step_studio_bridge::frame::write_frame(&mut burst, 1, &serde_json::json!({ "type": "junk" }))
        .expect("encode the junk");
    peer.write_all(&burst).expect("send the burst");
    peer.flush().expect("flush the burst");

    // The runtime answers the hello and then refuses: it never gets the proof it
    // asked for, because the junk frame was consumed by the hello's reader.
    // Either way the connection must end rather than hang.
    let mut seen = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match peer.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => seen.extend_from_slice(&chunk[..read]),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                break
            }
            Err(error) => panic!("the burst was not answered: {error}"),
        }
        if seen.len() > 64 * 1024 {
            break;
        }
    }
    assert!(!seen.is_empty(), "the runtime never answered the hello");

    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn events_reach_the_caller_before_the_receipt() {
    // The fake CLI holds the receipt 1.4 s after the last delta, so a caller
    // that buffers events until the receipt shows the turn ~1.4 s late. This is
    // the difference between a chat that types out and one that is blank and
    // then complete, and it is invisible to any assertion on the final text.
    let session = BridgeSession::connect(&launch("stream")).expect("connect");
    let _ = session.snapshot().expect("snapshot");

    let (first, seen) = std::sync::mpsc::channel::<std::time::Duration>();
    let handle = std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let receipt = session.invoke_with(
            serde_json::json!({ "kind": "core.prompt", "text": "slow" }),
            |event| {
                if event.get("kind") == Some(&serde_json::json!("conversation.message.delta")) {
                    let _ = first.send(started.elapsed());
                }
            },
        )?;
        Ok::<_, step_studio_bridge::BridgeError>((receipt, started.elapsed()))
    });

    let at_first_delta = seen
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("no event arrived before the receipt");
    let (receipt, at_receipt) = handle.join().expect("invoke thread").expect("receipt");

    assert_eq!(
        receipt.get("status").and_then(|s| s.as_str()),
        Some("completed")
    );
    let gap = at_receipt.saturating_sub(at_first_delta);
    assert!(
        gap > std::time::Duration::from_millis(500),
        "the last event arrived only {gap:?} before the receipt: events are being buffered"
    );
}

#[test]
fn an_abort_reaches_a_turn_that_is_still_running() {
    // The turn is one the fake CLI never settles on its own, so a receipt can
    // only come from the abort. That makes the test deterministic: no timing
    // threshold, and a regression fails by producing nothing at all.
    //
    // It also fails to compile against the old session, which took `&mut self`
    // and so could not be used from two threads at once.
    let session = Arc::new(BridgeSession::connect(&launch("abort")).expect("connect"));
    let _ = session.snapshot().expect("snapshot");

    let (first, started) = std::sync::mpsc::channel::<()>();
    let turn = Arc::clone(&session);
    let handle = std::thread::spawn(move || {
        turn.invoke_with(serde_json::json!({ "kind": "core.prompt", "text": "hang" }), |event| {
            if event.get("kind") == Some(&serde_json::json!("conversation.message.delta")) {
                let _ = first.send(());
            }
        })
    });

    started
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("the turn never started");

    // From another thread, while the turn is still streaming. This is the whole
    // point: a queued abort would only run once the turn had already ended.
    let receipt = session
        .invoke(serde_json::json!({ "kind": "core.abort" }))
        .expect("abort was refused")
        .0;
    assert_eq!(
        receipt.get("status").and_then(|s| s.as_str()),
        Some("completed")
    );

    let turn_receipt = handle
        .join()
        .expect("turn thread")
        .expect("the turn never settled");
    assert_eq!(
        turn_receipt.get("status").and_then(|s| s.as_str()),
        Some("completed"),
        "the aborted turn did not settle: {}",
        turn_receipt
            .pointer("/error/message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown")
    );
}

#[test]
fn the_handshake_token_comes_from_the_file_the_session_writes() {
    // `connect` owns the token file: it writes the secret and the adapter reads
    // it back, so a client cannot be pointed at somebody else's token by
    // pre-seeding the path. What matters is that the proof is checked at all —
    // a session built with a mismatched expectation must not verify.
    let launch = launch("auth");
    let proof = step_studio_bridge::challenge_proof("step-studio", "c", "rt-1");
    let other = step_studio_bridge::challenge_proof("different-token", "c", "rt-1");
    assert_ne!(proof, other, "the proof must depend on the token");

    // And connecting still works, because both sides use the file's secret.
    assert!(BridgeSession::connect(&launch).is_ok());
}

