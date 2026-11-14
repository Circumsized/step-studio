//! The Rust adapter end to end: the Rust Studio Bridge client (verified against
//! the TypeScript adapter) drives this adapter, which drives the real Step CLI.
//!
//! Skips when no CLI is installed, so CI is unaffected.

use std::path::PathBuf;
use std::time::Duration;

use step_studio_bridge::{AdapterLaunch, BridgeSession};
use step_studio_runtime::bridge_server::{AdapterConfig, Runtime};

fn find_step() -> Option<String> {
    let from_env = std::env::var("STEP_STUDIO_REAL_STEP")
        .ok()
        .filter(|v| !v.is_empty());
    if let Some(path) = from_env {
        return std::path::Path::new(&path).exists().then_some(path);
    }
    for dir in (std::env::var("PATH").unwrap_or_default()).split(':') {
        if dir.is_empty() {
            continue;
        }
        let candidate = PathBuf::from(dir).join("step");
        if candidate.exists() {
            return Some(candidate.to_string_lossy().to_string());
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let installed = PathBuf::from(home).join(".stepcode/bin/step");
    installed
        .exists()
        .then(|| installed.to_string_lossy().to_string())
}

/// A wrapper that runs the shared fake CLI, so the adapter can be pointed at a
/// script instead of a single executable.
fn fake_step_wrapper(dir: &std::path::Path) -> PathBuf {
    // Next to the tests that use it. It used to live in
    // `packages/runtime-stepcode/test/`, which was deleted as dead code — and
    // these tests then pointed at a file that no longer existed, wrote a wrapper
    // that exec'd nothing, and failed with "the dialog never reached the Host"
    // rather than "fixture missing". A fixture belongs with its consumers.
    let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fake-step.mjs");
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

/// The fake CLI runs under node, so the approval tests need it present.
fn has_node() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn launch(tag: &str) -> (AdapterConfig, PathBuf) {
    let dir = std::env::temp_dir().join(format!("rust-rt-{tag}-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let step = find_step().expect("no Step CLI installed");
    config_in(&dir, step)
}

fn config_in(dir: &std::path::Path, step_command: String) -> (AdapterConfig, PathBuf) {
    {
        let config = AdapterConfig {
            endpoint: dir.join("bridge.sock").to_string_lossy().to_string(),
            token: "rust-runtime-token".to_string(),
            runtime_epoch: 1,
            cwd: dir.to_string_lossy().to_string(),
            step_command,
            // The whole environment, the same way the production config path passes
            // it: the CLI resolves its config dir and credentials from more than a
            // prefix, and filtering it here silently starves the turn.
            step_env: std::env::vars().collect(),
            continue_session: false,
            exit_on_disconnect: false,
        };
        (config, dir.to_path_buf())
    }
}

#[test]
fn the_rust_adapter_drives_the_real_step_cli() {
    let Some(_) = find_step() else {
        eprintln!("skipping: no Step CLI installed");
        return;
    };
    let (config, dir) = launch("e2e");

    // The CLI's model, pinned here rather than read from the machine's
    // `~/.omp/agent/config.yml`. This test passed only after the developer's
    // default model was swapped for one with credit, which made it depend on
    // state outside the repository and silently edited a personal config to go
    // green. A `--config` overlay overrides `modelRoles.default` without
    // touching that file; the shim injects it because the adapter builds its
    // own argv and has no flag of its own to add.
    let model = std::env::var("STEP_STUDIO_TEST_MODEL")
        .unwrap_or_else(|_| "magpie/stepfun-cn/step-3.7-flash".to_string());
    let overlay = dir.join("model.yml");
    std::fs::write(&overlay, format!("modelRoles:\n  default: {model}\n"))
        .expect("write the model overlay");
    let shim = dir.join("step-under-test");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nexec {} --config {} \"$@\"\n",
            config.step_command,
            overlay.display()
        ),
    )
    .expect("write the shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755));
    }
    let step_command = shim.to_string_lossy().to_string();

    // The adapter owns its socket, so run it on a thread and connect to it.
    let runtime = Runtime::new(AdapterConfig {
        endpoint: config.endpoint.clone(),
        token: config.token.clone(),
        runtime_epoch: config.runtime_epoch,
        cwd: config.cwd.clone(),
        step_command: step_command.clone(),
        step_env: config.step_env.clone(),
        continue_session: false,
        exit_on_disconnect: false,
    });
    std::thread::spawn(move || {
        let _ = runtime.serve();
    });

    // Wait for the socket to appear.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !std::path::Path::new(&config.endpoint).exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the adapter never bound its socket"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let session = BridgeSession::connect(&AdapterLaunch {
        node: String::new(),
        adapter_script: String::new(),
        socket_path: config.endpoint.clone(),
        token_file: String::new(),
        cwd: config.cwd.clone(),
        runtime_epoch: config.runtime_epoch,
        env: Vec::new(),
        // The adapter already holds this token, so the client must not write a
        // competing file.
        token: Some(config.token.clone()),
        command: Vec::new(),
    })
    .expect("connect to the Rust adapter");

    let snapshot = session.snapshot().expect("snapshot");
    let session_id = snapshot
        .pointer("/snapshot/sessionId")
        .and_then(|v| v.as_str())
        .expect("sessionId");
    assert!(
        !session_id.is_empty(),
        "the CLI reported an empty session id"
    );

    let (receipt, events) = session
        .invoke(serde_json::json!({ "kind": "core.prompt", "text": "reply with exactly: PONG" }))
        .expect("prompt");
    assert_eq!(
        receipt.get("status").and_then(|s| s.as_str()),
        Some("completed"),
        "turn failed: {}",
        receipt
            .pointer("/error/message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown")
    );

    let answer: String = events
        .iter()
        .filter(|event| event.get("kind") == Some(&serde_json::json!("conversation.message.delta")))
        .filter(|event| event.get("blockType") == Some(&serde_json::json!("text")))
        .filter_map(|event| event.get("delta").and_then(|d| d.as_str()))
        .collect();
    assert!(answer.contains("PONG"), "no answer streamed; got: {answer}");

    let page = session
        .invoke(serde_json::json!({ "kind": "session.transcript.read" }))
        .expect("transcript")
        .0;
    let items = page
        .pointer("/result/items")
        .and_then(|i| i.as_array())
        .expect("items");
    assert!(
        items
            .iter()
            .any(|item| item.get("role") == Some(&serde_json::json!("assistant"))),
        "the transcript lost the assistant turn"
    );
}

/// The approval loop end to end against the fake CLI, on the Studio
/// interaction contract: the dialog arrives as `interaction.required` with a
/// nested request, the answer travels as decision/value, and the dialog is
/// closed out by `interaction.resolved`. The payloads here are the contract's,
/// not the adapter's: a test that invents its own shape agrees with itself.
#[test]
fn an_approval_round_trips_through_the_rust_adapter() {
    if !has_node() {
        eprintln!("skipping: no node for the fake CLI");
        return;
    }
    let dir = std::env::temp_dir().join(format!("rust-approve-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let (config, _dir) = config_in(&dir, fake_step_wrapper(&dir).to_string_lossy().to_string());

    let runtime = Runtime::new(AdapterConfig {
        endpoint: config.endpoint.clone(),
        token: config.token.clone(),
        runtime_epoch: config.runtime_epoch,
        cwd: config.cwd.clone(),
        step_command: config.step_command.clone(),
        step_env: config.step_env.clone(),
        continue_session: config.continue_session,
        exit_on_disconnect: false,
    });
    std::thread::spawn(move || {
        let _ = runtime.serve();
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !std::path::Path::new(&config.endpoint).exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the adapter never bound its socket"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let session = BridgeSession::connect(&AdapterLaunch {
        node: String::new(),
        adapter_script: String::new(),
        socket_path: config.endpoint.clone(),
        token_file: String::new(),
        cwd: config.cwd.clone(),
        runtime_epoch: config.runtime_epoch,
        env: Vec::new(),
        token: Some(config.token.clone()),
        command: Vec::new(),
    })
    .expect("connect to the Rust adapter");

    let mut saw_required = false;
    let mut saw_resolved = false;
    let receipt = session
        .invoke_with(
            serde_json::json!({ "kind": "core.prompt", "text": "approve" }),
            |event| match event.get("kind").and_then(|k| k.as_str()) {
                Some("interaction.required") => {
                    saw_required = true;
                    let request = event.pointer("/request").expect("nested request");
                    let interaction_id = request
                        .get("interactionId")
                        .and_then(|v| v.as_str())
                        .expect("interactionId")
                        .to_string();
                    // The contract's confirm shape, not a passthrough of the
                    // CLI's frame: title and message, ids the client can echo.
                    assert_eq!(request.get("kind").and_then(|k| k.as_str()), Some("confirm"));
                    assert!(request.get("commandId").and_then(|v| v.as_str()).is_some(), "commandId");
                    assert_eq!(
                        request.get("message").and_then(|m| m.as_str()),
                        Some("delete the build directory"),
                        "the dialog message must survive the mapping"
                    );
                    assert_eq!(event.get("owner").and_then(|o| o.as_str()), Some("gui"));

                    // Answering from inside the callback is the streaming
                    // path: the turn is still open. send_command, not invoke:
                    // an invoke here would consume the event stream the turn is
                    // flowing through, and the resolution would be swallowed.
                    session
                        .send_command(serde_json::json!({
                            "kind": "interaction.respond",
                            "interactionId": interaction_id,
                            "commandId": request.get("commandId").and_then(|v| v.as_str()).unwrap_or(""),
                            "decision": "submit",
                            "value": true,
                        }))
                        .expect("the adapter accepted the answer frame");
                }
                Some("interaction.resolved") => {
                    saw_resolved = true;
                    assert_eq!(
                        event.get("outcome").and_then(|o| o.as_str()),
                        Some("submitted"),
                        "a submitted answer must resolve the dialog"
                    );
                }
                // The turn's own conversation events are expected. Anything
                // else would be an invented kind: the fake also fires a
                // setStatus, and fire-and-forget has no Studio kind to map to.
                Some(other) if other.starts_with("conversation.") => {}
                Some(other) => panic!("unexpected host event kind: {other}"),
                None => {}
            },
        )
        .expect("the approval turn");

    assert!(saw_required, "the dialog never reached the Host");
    assert!(saw_resolved, "the dialog was never resolved for the client");
    assert_eq!(
        receipt.get("status").and_then(|s| s.as_str()),
        Some("completed"),
        "the turn did not settle after the answer: {}",
        receipt
            .pointer("/error/message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown")
    );

    // The transcript proves the CLI saw the answer: it only writes the verdict
    // once the response arrives.
    let page = session
        .invoke(serde_json::json!({ "kind": "session.transcript.read" }))
        .expect("transcript")
        .0;
    let items = page
        .pointer("/result/items")
        .and_then(|i| i.as_array())
        .expect("items");
    assert!(
        items.iter().any(|item| {
            item.get("role") == Some(&serde_json::json!("assistant"))
                && item.to_string().contains("approved")
        }),
        "the CLI never saw the answer: {items:?}"
    );
}

/// A cancel decision reaches the CLI as `cancelled` and resolves the dialog
/// as cancelled, which is how the CLI tells "denied" from "unanswered".
#[test]
fn a_cancelled_answer_reaches_the_cli_as_cancelled() {
    if !has_node() {
        eprintln!("skipping: no node for the fake CLI");
        return;
    }
    let dir = std::env::temp_dir().join(format!("rust-cancel-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let (config, _dir) = config_in(&dir, fake_step_wrapper(&dir).to_string_lossy().to_string());

    let runtime = Runtime::new(AdapterConfig {
        endpoint: config.endpoint.clone(),
        token: config.token.clone(),
        runtime_epoch: config.runtime_epoch,
        cwd: config.cwd.clone(),
        step_command: config.step_command.clone(),
        step_env: config.step_env.clone(),
        continue_session: config.continue_session,
        exit_on_disconnect: false,
    });
    std::thread::spawn(move || {
        let _ = runtime.serve();
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !std::path::Path::new(&config.endpoint).exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the adapter never bound its socket"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let session = BridgeSession::connect(&AdapterLaunch {
        node: String::new(),
        adapter_script: String::new(),
        socket_path: config.endpoint.clone(),
        token_file: String::new(),
        cwd: config.cwd.clone(),
        runtime_epoch: config.runtime_epoch,
        env: Vec::new(),
        token: Some(config.token.clone()),
        command: Vec::new(),
    })
    .expect("connect to the Rust adapter");

    let mut saw_resolved = false;
    let receipt = session
        .invoke_with(
            serde_json::json!({ "kind": "core.prompt", "text": "approve" }),
            |event| {
                if event.get("kind").and_then(|k| k.as_str()) == Some("interaction.required") {
                    let interaction_id = event
                        .pointer("/request/interactionId")
                        .and_then(|v| v.as_str())
                        .expect("interactionId")
                        .to_string();
                    let command_id = event
                        .pointer("/request/commandId")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    session
                        .send_command(serde_json::json!({
                            "kind": "interaction.respond",
                            "interactionId": interaction_id,
                            "commandId": command_id,
                            "decision": "cancel",
                        }))
                        .expect("the adapter accepted the cancel frame");
                }
                if event.get("kind").and_then(|k| k.as_str()) == Some("interaction.resolved") {
                    saw_resolved = true;
                    assert_eq!(
                        event.get("outcome").and_then(|o| o.as_str()),
                        Some("cancelled")
                    );
                }
            },
        )
        .expect("the cancelled approval turn");

    assert!(saw_resolved, "a cancelled dialog must still resolve");
    assert_eq!(
        receipt.get("status").and_then(|s| s.as_str()),
        Some("completed")
    );
    let page = session
        .invoke(serde_json::json!({ "kind": "session.transcript.read" }))
        .expect("transcript")
        .0;
    let items = page
        .pointer("/result/items")
        .and_then(|i| i.as_array())
        .expect("items");
    assert!(
        items.iter().any(|item| item.to_string().contains("denied")),
        "the CLI never saw the denial: {items:?}"
    );
}

/// An answer for an interaction this connection never issued must fail closed:
/// the id could belong to another client's dialog, and a stray frame into the
/// CLI is worse than a rejected command. The client still gets a resolution so
/// its card does not hang.
#[test]
fn a_stale_interaction_answer_fails_closed() {
    if !has_node() {
        eprintln!("skipping: no node for the fake CLI");
        return;
    }
    let dir = std::env::temp_dir().join(format!("rust-stale-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let (config, _dir) = config_in(&dir, fake_step_wrapper(&dir).to_string_lossy().to_string());

    let runtime = Runtime::new(AdapterConfig {
        endpoint: config.endpoint.clone(),
        token: config.token.clone(),
        runtime_epoch: config.runtime_epoch,
        cwd: config.cwd.clone(),
        step_command: config.step_command.clone(),
        step_env: config.step_env.clone(),
        continue_session: config.continue_session,
        exit_on_disconnect: false,
    });
    std::thread::spawn(move || {
        let _ = runtime.serve();
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !std::path::Path::new(&config.endpoint).exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the adapter never bound its socket"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let session = BridgeSession::connect(&AdapterLaunch {
        node: String::new(),
        adapter_script: String::new(),
        socket_path: config.endpoint.clone(),
        token_file: String::new(),
        cwd: config.cwd.clone(),
        runtime_epoch: config.runtime_epoch,
        env: Vec::new(),
        token: Some(config.token.clone()),
        command: Vec::new(),
    })
    .expect("connect to the Rust adapter");

    let mut saw_resolved = false;
    let receipt = session
        .invoke_with(
            serde_json::json!({
                "kind": "interaction.respond",
                "interactionId": "never-issued",
                "commandId": "cmd-never-issued",
                "decision": "submit",
                "value": true,
            }),
            |event| {
                if event.get("kind").and_then(|k| k.as_str()) == Some("interaction.resolved") {
                    saw_resolved = true;
                    assert_eq!(
                        event.get("interactionId").and_then(|v| v.as_str()),
                        Some("never-issued")
                    );
                }
            },
        )
        .expect("the adapter answered the stale request");

    assert_eq!(
        receipt.get("status").and_then(|s| s.as_str()),
        Some("failed"),
        "a stale answer must not be accepted"
    );
    assert_eq!(
        receipt.pointer("/error/code").and_then(|c| c.as_str()),
        Some("INTERACTION_STALE"),
        "the stale answer must carry the stale code"
    );
    assert!(
        saw_resolved,
        "a stale answer must still resolve the client's card"
    );
}

#[test]
fn aborting_settles_the_turn_the_rust_adapter_is_still_running() {
    // The fake CLI has a turn that never settles on its own, so the receipt can
    // only come from the abort. The TypeScript adapter used to drop the turn's
    // receipt here and leave the Host waiting for the prompt RPC to time out.
    let dir = std::env::temp_dir().join(format!("rust-abort-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let (config, _) = config_in(&dir, fake_step_wrapper(&dir).to_string_lossy().to_string());

    let runtime = Runtime::new(AdapterConfig {
        endpoint: config.endpoint.clone(),
        token: config.token.clone(),
        runtime_epoch: config.runtime_epoch,
        cwd: config.cwd.clone(),
        step_command: config.step_command.clone(),
        step_env: config.step_env.clone(),
        continue_session: false,
        exit_on_disconnect: false,
    });
    std::thread::spawn(move || {
        let _ = runtime.serve();
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !std::path::Path::new(&config.endpoint).exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the adapter never bound its socket"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let session = std::sync::Arc::new(
        BridgeSession::connect(&AdapterLaunch {
            node: String::new(),
            adapter_script: String::new(),
            socket_path: config.endpoint.clone(),
            token_file: String::new(),
            cwd: config.cwd.clone(),
            runtime_epoch: config.runtime_epoch,
            env: Vec::new(),
            token: Some(config.token.clone()),
            command: Vec::new(),
        })
        .expect("connect to the Rust adapter"),
    );

    let (first, started) = std::sync::mpsc::channel::<()>();
    let turn = std::sync::Arc::clone(&session);
    let handle = std::thread::spawn(move || {
        turn.invoke_with(
            serde_json::json!({ "kind": "core.prompt", "text": "hang" }),
            |event| {
                if event.get("kind") == Some(&serde_json::json!("conversation.message.delta")) {
                    let _ = first.send(());
                }
            },
        )
    });

    started
        .recv_timeout(Duration::from_secs(30))
        .expect("the turn never started");
    session
        .invoke(serde_json::json!({ "kind": "core.abort" }))
        .expect("abort was refused");

    let receipt = handle
        .join()
        .expect("turn thread")
        .expect("the turn never settled");
    assert_eq!(
        receipt.get("status").and_then(|s| s.as_str()),
        Some("completed"),
        "the aborted turn did not settle: {}",
        receipt
            .pointer("/error/message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown")
    );
}

#[test]
fn a_new_session_mints_a_fresh_session_id() {
    // Probed against the real CLI first: `new_session` returns
    // `{"cancelled":false}` and the next `get_state` reports a different id with
    // the transcript reset. Anything else would make the shell's New button
    // clear a conversation that is still there.
    let Some(step) = find_step() else {
        eprintln!("skipping: no Step CLI installed");
        return;
    };
    let dir = std::env::temp_dir().join(format!("rust-new-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let (config, _) = config_in(&dir, step);

    let runtime = Runtime::new(AdapterConfig {
        endpoint: config.endpoint.clone(),
        token: config.token.clone(),
        runtime_epoch: config.runtime_epoch,
        cwd: config.cwd.clone(),
        step_command: config.step_command.clone(),
        step_env: config.step_env.clone(),
        continue_session: false,
        exit_on_disconnect: false,
    });
    std::thread::spawn(move || {
        let _ = runtime.serve();
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !std::path::Path::new(&config.endpoint).exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the adapter never bound its socket"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let session = BridgeSession::connect(&AdapterLaunch {
        node: String::new(),
        adapter_script: String::new(),
        socket_path: config.endpoint.clone(),
        token_file: String::new(),
        cwd: config.cwd.clone(),
        runtime_epoch: config.runtime_epoch,
        env: Vec::new(),
        token: Some(config.token.clone()),
        command: Vec::new(),
    })
    .expect("connect to the Rust adapter");

    let before = session
        .snapshot()
        .expect("snapshot")
        .pointer("/snapshot/sessionId")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    assert!(!before.is_empty(), "no session id before the reset");

    let receipt = session
        .invoke(serde_json::json!({ "kind": "session.new" }))
        .expect("session.new")
        .0;
    assert_eq!(
        receipt.get("status").and_then(|s| s.as_str()),
        Some("completed"),
        "session.new failed: {}",
        receipt
            .pointer("/error/message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown")
    );

    let after = session
        .snapshot()
        .expect("snapshot after the reset")
        .pointer("/snapshot/sessionId")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    assert_ne!(before, after, "session.new kept the old session id");
}

#[test]
fn a_turn_settles_when_the_cli_dies_mid_turn() {
    // The fake CLI exits on the word "die" without settling. Nothing will ever
    // produce `session_settled` then, so without an explicit check the Host waits
    // forever and the shell sits on "streaming" with a Stop button that talks
    // to a dead process. The TypeScript adapter settles on exit; this pins the
    // same guarantee for the Rust one.
    let dir = std::env::temp_dir().join(format!("rust-die-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let (config, _) = config_in(&dir, fake_step_wrapper(&dir).to_string_lossy().to_string());

    let runtime = Runtime::new(AdapterConfig {
        endpoint: config.endpoint.clone(),
        token: config.token.clone(),
        runtime_epoch: config.runtime_epoch,
        cwd: config.cwd.clone(),
        step_command: config.step_command.clone(),
        step_env: config.step_env.clone(),
        continue_session: false,
        exit_on_disconnect: false,
    });
    std::thread::spawn(move || {
        let _ = runtime.serve();
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !std::path::Path::new(&config.endpoint).exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the adapter never bound its socket"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let session = BridgeSession::connect(&AdapterLaunch {
        node: String::new(),
        adapter_script: String::new(),
        socket_path: config.endpoint.clone(),
        token_file: String::new(),
        cwd: config.cwd.clone(),
        runtime_epoch: config.runtime_epoch,
        env: Vec::new(),
        token: Some(config.token.clone()),
        command: Vec::new(),
    })
    .expect("connect to the Rust adapter");

    // The client's own read timeout is the backstop: without the fix this call
    // hangs rather than returning, so the test fails by timing out.
    let (receipt, _) = session
        .invoke(serde_json::json!({ "kind": "core.prompt", "text": "die" }))
        .expect("the turn never settled after the CLI died");

    assert_eq!(
        receipt.get("status").and_then(|s| s.as_str()),
        Some("failed"),
        "a dead CLI is a failed turn, not a completed one: {receipt}"
    );
    let message = receipt
        .pointer("/error/message")
        .and_then(|m| m.as_str())
        .unwrap_or("");
    assert!(
        message.contains("exited"),
        "the failure must say the CLI died, not something generic: {message}"
    );
}

/// The manifest is a promise: anything it grades `stable` or `limited` must
/// actually be dispatched, or the Host sends a command the runtime rejects.
///
/// Four capabilities were declared and rejected — `core.stream`,
/// `session.state`, `session.history` and a `conversation.compaction` that is
/// really an event — which reads as "the runtime supports this" and fails at
/// runtime instead. This walks the same match the dispatcher does.
#[test]
fn every_declared_capability_is_actually_dispatched() {
    let manifest = step_studio_runtime::capabilities::capability_manifest();
    let declared: Vec<(String, String)> = manifest
        .value
        .pointer("/capabilities")
        .and_then(|list| list.as_array())
        .expect("capabilities")
        .iter()
        .map(|entry| {
            (
                entry
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                entry
                    .get("grade")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            )
        })
        .collect();
    assert!(!declared.is_empty(), "the manifest declared nothing");

    // The dispatch arms, read from the source rather than restated here: a test
    // that duplicates the list would agree with itself.
    let source =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/bridge_server.rs"))
            .expect("read the dispatcher");

    let undelivered: Vec<String> = declared
        .iter()
        .filter(|(_, grade)| grade != "unavailable")
        .filter(|(id, _)| {
            let quoted = format!("\"{id}\"");
            !source.contains(&format!("{quoted} =>"))
                && !source.contains(&format!("| {quoted}"))
                && !source.contains(&format!("{quoted} |"))
        })
        .map(|(id, _)| id.clone())
        .collect();

    assert!(
        undelivered.is_empty(),
        "declared but never dispatched: {undelivered:?}"
    );
}
