//! Golden fixtures: a whole rendered frame, compared cell for cell.
//!
//! §9.4 asks for 13 groups as the diff gate. The point is not the 13 — it is
//! that a fixture compares *everything*, so a change nobody thought to assert
//! still fails here. The per-dashboard unit tests assert what their author
//! thought of; a golden catches the rest.
//!
//! The expected frames are built with `Buffer::with_lines`, which goes through
//! the same wide-grapheme handling the renderer does — so a CJK title matches
//! without either side having to reason about column widths.

use std::path::{Path, PathBuf};

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::widgets::TableState;
use ratatui::Terminal;
use step_studio_tui::dashboards::{self, Dashboard};
use step_studio_tui::model::{
    AgentRecord, AgentStatus, ProcessRecord, ProcessTree, Role, SessionEntry, SessionTree, Usage,
    UsageDay, UsageHour, UsageModel,
};

/// Where the fixtures live, so a diff is a file edit and not a code edit.
fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
}

/// Render one dashboard into a buffer at a fixed size.
///
/// The size is part of the fixture: a golden that passed at 80×24 and is read at
/// 120×40 proves nothing, because the layout is a function of the area.
const WIDTH: u16 = 80;
const HEIGHT: u16 = 24;

fn render<F>(draw: F) -> Buffer
where
    F: FnOnce(&mut ratatui::Frame, &mut TableState),
{
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("a terminal");
    let mut selection = TableState::default();
    terminal
        .draw(|frame| draw(frame, &mut selection))
        .expect("draw");
    terminal.backend().buffer().clone()
}

fn render_tree(model: &ProcessTree) -> Buffer {
    render(|frame, selection| dashboards::process_tree::render(frame, model, selection))
}

fn render_agents(model: &AgentStatus) -> Buffer {
    render(|frame, selection| dashboards::agent_status::render(frame, model, selection))
}

fn render_usage(model: &Usage) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("a terminal");
    terminal
        .draw(|frame| dashboards::usage::render(frame, model))
        .expect("draw");
    terminal.backend().buffer().clone()
}

fn render_sessions(model: &SessionTree) -> Buffer {
    render(|frame, selection| dashboards::session_tree::render(frame, model, selection))
}

/// One fixture: the rendered buffer, and the file it must match.
///
/// A missing file is a failure that says how to create it, rather than a panic
/// about a path — the first person to add a fixture should not have to guess.
///
/// Compared as text rather than as `Buffer`s. `Buffer`'s `PartialEq` includes
/// style, and a fixture that had to carry colours would be unreadable and
/// unmaintainable — while the drift worth catching is layout and content: a
/// column that moved, a row that vanished, a label that changed. Styles are
/// pinned where they are set, by the dashboards' own tests.
fn assert_golden(name: &str, rendered: &Buffer) {
    let path = golden_dir().join(format!("{name}.txt"));
    if !path.is_file() {
        panic!(
            "no fixture at {}. Create it with `UPDATE_GOLDEN=1 cargo test -p step-studio-tui --test golden`.",
            path.display()
        );
    }
    let expected = std::fs::read_to_string(&path).expect("read the fixture");
    assert_eq!(
        step_studio_tui::dashboards::flatten(rendered),
        expected,
        "{} drifted from its fixture at {}",
        name,
        path.display()
    );
}

/// Write the fixtures instead of comparing them.
///
/// Opt-in through the environment, never automatic: a test that rewrites its own
/// expectation on every run is a test that cannot fail.
///
/// Written with `flatten`, not one symbol per cell: a wide grapheme occupies one
/// cell and ratatui *resets* the cells it covers, so a per-cell write puts an
/// extra space after every CJK character — and `Buffer::with_lines` then reads it
/// back as six one-column characters where the renderer wrote three two-column
/// ones. The fixture has to be what `with_lines` can reproduce.
fn update_golden(name: &str, rendered: &Buffer) {
    let path = golden_dir().join(format!("{name}.txt"));
    std::fs::write(&path, step_studio_tui::dashboards::flatten(rendered))
        .expect("write the fixture");
    eprintln!("wrote {}", path.display());
}

fn golden(name: &str, rendered: &Buffer) {
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        update_golden(name, rendered);
    } else {
        assert_golden(name, rendered);
    }
}

// --- the models ------------------------------------------------------------

fn daemon_tree() -> ProcessTree {
    ProcessTree {
        generated_at: "2026-11-13T00:00:00.000Z".to_string(),
        self_pid: 4242,
        processes: vec![
            ProcessRecord {
                pid: 4242,
                parent_pid: None,
                role: Role::Daemon,
                rss_bytes: Some(21 * 1024 * 1024),
            },
            ProcessRecord {
                pid: 4243,
                parent_pid: Some(4242),
                role: Role::Adapter,
                rss_bytes: Some(184 * 1024 * 1024),
            },
            ProcessRecord {
                pid: 4244,
                parent_pid: Some(4243),
                role: Role::Cli,
                rss_bytes: Some(171 * 1024 * 1024),
            },
        ],
        total_rss_bytes: 376 * 1024 * 1024,
        complete: true,
    }
}

fn agent_roster() -> AgentStatus {
    AgentStatus {
        agents: vec![
            AgentRecord {
                agent_id: "subagent-explore".to_string(),
                display_name: "explore".to_string(),
                status: "running".to_string(),
                summary: "reading the bridge codec".to_string(),
                started_at: "2026-11-13T00:00:00.000Z".to_string(),
                updated_at: "2026-11-13T00:04:00.000Z".to_string(),
                has_live_session: true,
                unread_count: 0,
            },
            AgentRecord {
                agent_id: "subagent-review".to_string(),
                display_name: "review".to_string(),
                status: "idle".to_string(),
                summary: String::new(),
                started_at: "2026-11-13T00:01:00.000Z".to_string(),
                updated_at: "2026-11-13T00:05:00.000Z".to_string(),
                has_live_session: false,
                unread_count: 0,
            },
        ],
    }
}

fn usage_with_arrays() -> Usage {
    Usage {
        generated_at: "2026-11-13T00:00:00.000Z".to_string(),
        days: vec![
            UsageDay {
                date: "2026-11-11".to_string(),
                tokens: 120_000,
            },
            UsageDay {
                date: "2026-11-12".to_string(),
                tokens: 480_000,
            },
            UsageDay {
                date: "2026-11-13".to_string(),
                tokens: 60_000,
            },
        ],
        models: vec![
            UsageModel {
                model: "step-3".to_string(),
                tokens: 500_000,
            },
            UsageModel {
                model: "step-3-mini".to_string(),
                tokens: 160_000,
            },
        ],
        by_model: Vec::new(),
        hours: vec![
            UsageHour {
                hour: 9,
                tokens: 20_000,
            },
            UsageHour {
                hour: 14,
                tokens: 400_000,
            },
            UsageHour {
                hour: 22,
                tokens: 240_000,
            },
        ],
        unavailable_reason: None,
        panel: None,
    }
}

fn usage_panel_only() -> Usage {
    Usage {
        generated_at: "2026-11-13T00:00:00.000Z".to_string(),
        days: Vec::new(),
        models: Vec::new(),
        by_model: Vec::new(),
        hours: Vec::new(),
        unavailable_reason: None,
        panel: Some("Total tokens 660000\nToday 60000".to_string()),
    }
}

fn usage_unavailable() -> Usage {
    Usage {
        generated_at: "2026-11-13T00:00:00.000Z".to_string(),
        days: Vec::new(),
        models: Vec::new(),
        by_model: Vec::new(),
        hours: Vec::new(),
        unavailable_reason: Some("CLI 未安装".to_string()),
        panel: None,
    }
}

fn session_list() -> SessionTree {
    SessionTree {
        entries: vec![
            SessionEntry {
                history_id: "2026-11-12T10-00-00".to_string(),
                thread_id: "thread-a".to_string(),
                title: Some("把 daemon 拆出来".to_string()),
                summary: None,
                started_at: "2026-11-12T10:00:00.000Z".to_string(),
                last_active_at: "2026-11-12T10:31:00.000Z".to_string(),
                message_count: 24,
                status: "active".to_string(),
                pinned: true,
            },
            SessionEntry {
                history_id: "2026-11-12T14-00-00".to_string(),
                thread_id: "thread-b".to_string(),
                title: None,
                summary: None,
                started_at: "2026-11-12T14:00:00.000Z".to_string(),
                last_active_at: "2026-11-12T14:02:00.000Z".to_string(),
                message_count: 1,
                status: "active".to_string(),
                pinned: false,
            },
        ],
        total: 2,
    }
}

// --- the fixtures ----------------------------------------------------------

#[test]
fn process_tree_full() {
    golden("process-tree-full", &render_tree(&daemon_tree()));
}

#[test]
fn process_tree_incomplete_scope() {
    // `complete: false` means the platform has no `/proc`, so a short total must
    // not be mistaken for an exact one.
    let mut partial = daemon_tree();
    partial.complete = false;
    partial.total_rss_bytes = 21 * 1024 * 1024;
    golden("process-tree-incomplete", &render_tree(&partial));
}

#[test]
fn process_tree_no_rss() {
    // Zero is a measurement; "—" is the absence of one.
    let mut silent = daemon_tree();
    for process in &mut silent.processes {
        process.rss_bytes = None;
    }
    silent.total_rss_bytes = 0;
    golden("process-tree-no-rss", &render_tree(&silent));
}

#[test]
fn process_tree_deep() {
    // A CLI that spawned its own children, so the indentation has to show depth
    // rather than a flat list.
    let mut deep = daemon_tree();
    deep.processes.push(ProcessRecord {
        pid: 4245,
        parent_pid: Some(4244),
        role: Role::Cli,
        rss_bytes: Some(12 * 1024 * 1024),
    });
    deep.processes.push(ProcessRecord {
        pid: 4246,
        parent_pid: Some(4245),
        role: Role::Cli,
        rss_bytes: Some(8 * 1024 * 1024),
    });
    deep.total_rss_bytes = 396 * 1024 * 1024;
    golden("process-tree-deep", &render_tree(&deep));
}

#[test]
fn process_tree_empty() {
    // The daemon is up but nothing is under it. An honest empty, not an error.
    let bare = ProcessTree {
        generated_at: "2026-11-13T00:00:00.000Z".to_string(),
        self_pid: 4242,
        processes: vec![ProcessRecord {
            pid: 4242,
            parent_pid: None,
            role: Role::Daemon,
            rss_bytes: Some(21 * 1024 * 1024),
        }],
        total_rss_bytes: 21 * 1024 * 1024,
        complete: true,
    };
    golden("process-tree-empty", &render_tree(&bare));
}

#[test]
fn agent_status_roster() {
    golden("agent-status-roster", &render_agents(&agent_roster()));
}

#[test]
fn agent_status_empty() {
    golden(
        "agent-status-empty",
        &render_agents(&AgentStatus { agents: Vec::new() }),
    );
}

#[test]
fn usage_with_arrays_fixture() {
    golden("usage-arrays", &render_usage(&usage_with_arrays()));
}

#[test]
fn usage_panel_only_fixture() {
    // The honest case: the CLI's own panel, verbatim, when the structured arrays
    // are not there.
    golden("usage-panel", &render_usage(&usage_panel_only()));
}

#[test]
fn usage_unavailable_fixture() {
    golden("usage-unavailable", &render_usage(&usage_unavailable()));
}

#[test]
fn usage_all_zero() {
    // Zero is the normal empty case, not an error.
    let zero = Usage {
        generated_at: "2026-11-13T00:00:00.000Z".to_string(),
        days: vec![UsageDay {
            date: "2026-11-12".to_string(),
            tokens: 0,
        }],
        models: Vec::new(),
        by_model: Vec::new(),
        hours: vec![UsageHour { hour: 9, tokens: 0 }],
        unavailable_reason: None,
        panel: None,
    };
    golden("usage-zero", &render_usage(&zero));
}

#[test]
fn session_tree_list() {
    golden("session-tree-list", &render_sessions(&session_list()));
}

#[test]
fn session_tree_empty() {
    golden(
        "session-tree-empty",
        &render_sessions(&SessionTree {
            entries: Vec::new(),
            total: 0,
        }),
    );
}

#[test]
fn session_tree_untitled() {
    // An empty title looks like the panel failed; the session is untitled, which
    // is what is actually known.
    let mut untitled = session_list();
    untitled.entries[0].title = None;
    golden("session-tree-untitled", &render_sessions(&untitled));
}

/// The tab order §9.4 asks for, as a fixture of its own.
///
/// Not a rendered frame — a table, because what is being pinned is the order and
/// the query each dashboard sends, and a picture of a title bar would pin the
/// wrong thing.
#[test]
fn dashboard_order_and_queries() {
    let order: Vec<String> = Dashboard::ALL
        .iter()
        .map(|dashboard| format!("{} -> {}", dashboard.title(), dashboard.operation()))
        .collect();
    let text = format!("{}\n", order.join("\n"));
    let path = golden_dir().join("dashboard-order.txt");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, &text).expect("write the fixture");
        eprintln!("wrote {}", path.display());
        return;
    }
    let expected = std::fs::read_to_string(&path).expect("read the fixture");
    assert_eq!(
        text, expected,
        "the tab order or a dashboard's query changed"
    );
}
