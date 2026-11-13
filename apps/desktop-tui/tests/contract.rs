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

/// What the shell's `session.agents.list` emits, field for field.
#[test]
fn the_shells_agent_roster_deserializes() {
    let produced = serde_json::json!({
        "agents": [{
            "agentId": "subagent-explore",
            "generation": 1,
            "kind": "subagent",
            "displayName": "subagent-explore",
            "status": "idle",
            "summary": "",
            "startedAt": "2026-11-13T00:00:00.000Z",
            "updatedAt": "2026-11-13T00:01:00.000Z",
            "hasLiveSession": false,
            "hasTranscript": true,
            "unreadCount": 0,
            "activeJobIds": [],
            "workspaceId": "/home/ubuntu/workspace/step-studio",
        }],
    });
    let roster: AgentStatus = serde_json::from_value(produced).expect("the roster parses");
    assert_eq!(roster.agents.len(), 1);
    assert!(
        !roster.agents[0].has_live_session,
        "a file is not a process"
    );
}

/// What the shell's `history.list` emits.
#[test]
fn the_shells_session_tree_deserializes() {
    let produced = serde_json::json!({
        "entries": [{
            "historyId": "h-1",
            "threadId": "t-1",
            "environmentId": "e-1",
            "sessionId": "s-1",
            "title": "a session",
            "summary": "a summary",
            "startedAt": "2026-11-13T00:00:00.000Z",
            "lastActiveAt": "2026-11-13T00:01:00.000Z",
            "messageCount": 7,
            "status": "active",
            "pinned": false,
        }],
        "total": 1,
    });
    let tree: SessionTree = serde_json::from_value(produced).expect("the tree parses");
    assert_eq!(tree.total, 1);
    assert_eq!(tree.entries[0].message_count, 7);
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
