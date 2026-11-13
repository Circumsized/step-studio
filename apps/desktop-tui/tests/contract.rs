//! The TUI's read models against what the daemon actually emits.
//!
//! The point is contract drift. The shell's M2.5 found nine queries silently
//! emptying their panels because a changed shape produced `result: null` rather
//! than an error, so nothing failed and nothing rendered. Deserializing the real
//! output — not a hand-written sample — is what turns that into a test failure.

use serde_json::Value;
use step_studio_tui::model::{AgentStatus, ProcessTree, SessionTree, Usage};

/// The daemon's own process tree, straight from the function that serves it.
#[test]
fn the_daemons_own_process_tree_deserializes() {
    let produced: Value = step_studio_host::process::read_model();
    let tree: ProcessTree =
        serde_json::from_value(produced).expect("the daemon's tree matches the TUI's model");
    assert!(
        !tree.processes.is_empty(),
        "the daemon sees itself at least"
    );
    assert!(
        tree.processes
            .iter()
            .any(|process| process.role.label() == "daemon"),
        "and it knows which one it is"
    );
}

/// A usage model with no structured arrays, which is the normal case: the CLI's
/// `/usage` panel is a heatmap of intensity buckets with totals only in
/// aggregate, so there is nothing to chart.
#[test]
fn a_usage_model_with_only_the_cli_panel_deserializes() {
    let produced = serde_json::json!({
        "generatedAt": "2026-11-13T00:00:00.000Z",
        "days": [],
        "models": [],
        "byModel": [],
        "hours": [],
        "panel": "Total tokens: 1.2M",
    });
    let usage: Usage = serde_json::from_value(produced).expect("the panel-only shape parses");
    assert_eq!(usage.panel.as_deref(), Some("Total tokens: 1.2M"));
    assert!(usage.days.is_empty());
}

/// The optional-on-the-wire fields, absent. A model that required them would
/// refuse the shape an older Host sends.
#[test]
fn a_usage_model_with_no_optional_fields_deserializes() {
    let produced = serde_json::json!({
        "generatedAt": "2026-11-13T00:00:00.000Z",
        "days": [],
        "models": [],
        "byModel": [],
        "hours": [],
    });
    let usage: Usage = serde_json::from_value(produced).expect("the bare shape parses");
    assert!(usage.panel.is_none());
    assert!(usage.unavailable_reason.is_none());
}

/// The real store reader, against a store this test builds itself.
///
/// The reader is called with the store path rather than by mutating `HOME`:
/// `std::env::set_var` is `unsafe` and this workspace forbids `unsafe` outright.
/// Building a fixture is also what makes these tests exercise the *code that
/// reads the store* instead of a literal in this file — the first version
/// hand-wrote both payloads, so `read_agent` could have dropped or renamed a
/// field and this test would still have passed while the Agent 状态 panel went
/// empty at runtime. That is the exact drift the file exists to catch.
fn fixture_store(tag: &str) -> std::path::PathBuf {
    // Tagged, not just pid: two tests building a fixture store in parallel would
    // otherwise share one directory and delete each other's files. This is the
    // third time this session a pid-only fixture name has made a test
    // order-dependent, so the tag is part of the signature rather than a
    // convention.
    let dir = std::env::temp_dir().join(format!("tui-contract-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let project = dir.join("-home-ubuntu-ws");
    std::fs::create_dir_all(&project).expect("make a store");
    let ordinary = serde_json::json!({
        "type": "session", "id": "s-1", "cwd": "/home/ubuntu/ws",
        "timestamp": "2026-11-13T00:00:00.000Z",
    });
    let subagent = serde_json::json!({
        "type": "session", "id": "subagent-explore", "origin": "subagent",
        "cwd": "/home/ubuntu/ws", "timestamp": "2026-11-13T00:01:00.000Z",
    });
    let message = serde_json::json!({
        "type": "message", "timestamp": "2026-11-13T00:02:00.000Z",
        "message": { "role": "user", "content": [{ "type": "text", "text": "hello" }] },
    });
    for (name, header) in [
        ("2026-11-12T10-00-00.jsonl", &ordinary),
        ("2026-11-12T11-00-00.jsonl", &subagent),
    ] {
        let mut text = serde_json::to_string(header).expect("serialize the header");
        text.push('\n');
        text.push_str(&serde_json::to_string(&message).expect("serialize the message"));
        std::fs::write(project.join(name), text).expect("write a session");
    }
    dir
}

#[test]
fn the_real_store_reader_feeds_the_agent_roster() {
    let store = fixture_store("agents");
    let produced = step_studio_sessions::agents_in(&store);
    let roster: AgentStatus =
        serde_json::from_value(produced).expect("the real reader's roster parses");
    assert_eq!(
        roster.agents.len(),
        1,
        "the sub-agent, not the ordinary session"
    );
    assert_eq!(roster.agents[0].display_name, "subagent-explore");
    assert!(
        !roster.agents[0].has_live_session,
        "a file is not a process, and the reader says so"
    );
    let _ = std::fs::remove_dir_all(&store);
}

#[test]
fn the_real_store_reader_feeds_the_session_tree() {
    let store = fixture_store("tree");
    let produced = step_studio_sessions::list_in(&store, Some(200), None, None);
    let tree: SessionTree =
        serde_json::from_value(produced).expect("the real reader's tree parses");
    // Both sessions, not one: `history.list` is the store's contents, and
    // filtering the sub-agents out is `session.agents.list`'s job, not this
    // one's. (The first version of this assertion expected 1 and was wrong.)
    assert_eq!(tree.total, 2);
    assert_eq!(
        tree.entries[0].message_count, 1,
        "newest first, and it has one message"
    );
    assert!(tree.unavailable_reason.is_none(), "the store is there");
    let _ = std::fs::remove_dir_all(&store);
}

#[test]
fn a_machine_with_no_store_says_so_rather_than_looking_empty() {
    // The reader produces `unavailableReason` precisely so "no store" is not
    // conflated with "no sessions". The TUI used to drop the field, which made a
    // fresh machine render identically to a genuinely empty one.
    // A path that does not exist, not an empty directory: an empty store *is* a
    // store and is answered with no reason. The reason is for a store that is
    // not there — a fresh machine, or a `HOME` that points nowhere.
    let missing = std::env::temp_dir().join(format!("tui-nostore-{}-absent", std::process::id()));
    let _ = std::fs::remove_dir_all(&missing);
    let produced = step_studio_sessions::list_in(&missing, Some(200), None, None);
    let _ = std::fs::remove_dir_all(&missing);

    let tree: SessionTree = serde_json::from_value(produced).expect("parses");
    assert_eq!(tree.total, 0);
    assert!(
        tree.unavailable_reason.is_some(),
        "a missing store is reported, not rendered as an empty one"
    );
}

/// A shape the daemon does not emit must fail loudly rather than parse into a
/// panel of defaults.
#[test]
fn a_missing_required_field_is_an_error_not_a_default() {
    // `generatedAt` and `processes` are required. A model that filled them in
    // would render a confident, empty process tree.
    let produced = serde_json::json!({ "selfPid": 1 });
    let parsed = serde_json::from_value::<ProcessTree>(produced);
    assert!(parsed.is_err(), "a short model must not parse");
}
