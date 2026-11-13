//! The process tree dashboard: the daemon, its adapter, and the CLI under it.
//!
//! §9.2 names this the one panel worth a native surface, and it is the reason
//! the TUI exists at all: an htop-shaped view of what the Studio is actually
//! running, where the WebView panel would be a canvas approximation.

use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use ratatui::Frame;

use crate::model::{ProcessTree, Role};

/// Human-readable resident size. Bytes below a kilobyte are bytes; above that
/// the reader is counting processes, not sectors.
fn format_rss(bytes: Option<u64>) -> String {
    let Some(bytes) = bytes else {
        return "—".to_string();
    };
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

/// Draw the tree into `area`. Pure: same model, same frame, every time.
pub fn render(frame: &mut Frame, tree: &ProcessTree, selection: &mut TableState) {
    let chunks = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).split(frame.area());

    // The header carries the three facts a reader needs before the rows: which
    // daemon answered, how much everything is holding, and whether that number
    // is the whole story. `complete: false` means the platform has no `/proc`,
    // and presenting a short total as exact would be a lie with no marker.
    let total = format_rss(Some(tree.total_rss_bytes));
    let scope = if tree.complete { "完整" } else { "不完整" };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("daemon "),
            Span::styled(
                tree.self_pid.to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("  ·  合计 "),
            Span::raw(total),
            Span::raw("  ·  "),
            Span::raw(scope),
        ]))
        .block(Block::default().title("进程树").borders(Borders::ALL)),
        chunks[0],
    );

    // Depth from the tree, not from the executable's name — the daemon's direct
    // child is the adapter and everything below it is the CLI, which is what
    // `process.rs` already worked out. The indentation is that depth made
    // visible, so a reader sees the shape rather than being told it.
    let rows: Vec<Row> = tree
        .processes
        .iter()
        .map(|process| {
            let depth = depth_of(tree, process.pid);
            let indent = "  ".repeat(depth);
            let role_style = match process.role {
                Role::Daemon => Style::default().fg(Color::Cyan),
                Role::Adapter => Style::default().fg(Color::Green),
                Role::Cli => Style::default().fg(Color::DarkGray),
            };
            Row::new(vec![
                Cell::from(Span::raw(process.pid.to_string())),
                Cell::from(Span::styled(process.role.label(), role_style)),
                Cell::from(Span::raw(indent)),
                Cell::from(Span::raw(format_rss(process.rss_bytes))),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(8),
        Constraint::Length(9),
        Constraint::Min(10),
        Constraint::Length(10),
    ];
    let table = Table::new(rows, widths)
        .header(
            Row::new(vec!["PID", "角色", "", "RSS"])
                .style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .block(Block::default().borders(Borders::ALL))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    frame.render_stateful_widget(table, chunks[1], selection);
}

/// How deep `pid` sits below the daemon.
///
/// Walked rather than stored, because the read model gives parentage and not
/// depth, and a stored depth is one more thing to keep in step with the tree.
/// A cycle — which a hostile or broken `/proc` could produce — terminates
/// instead of spinning.
fn depth_of(tree: &ProcessTree, pid: u32) -> usize {
    let mut depth = 0;
    let mut cursor = pid;
    let mut seen = 0;
    while let Some(parent) = tree
        .processes
        .iter()
        .find(|process| process.pid == cursor)
        .and_then(|process| process.parent_pid)
    {
        if parent == pid || seen > tree.processes.len() {
            break;
        }
        depth += 1;
        cursor = parent;
        seen += 1;
    }
    depth
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use crate::model::ProcessRecord;

    fn tree() -> ProcessTree {
        ProcessTree {
            generated_at: "2026-11-13T00:00:00.000Z".to_string(),
            self_pid: 100,
            processes: vec![
                ProcessRecord {
                    pid: 100,
                    parent_pid: None,
                    role: Role::Daemon,
                    rss_bytes: Some(20 * 1024 * 1024),
                },
                ProcessRecord {
                    pid: 200,
                    parent_pid: Some(100),
                    role: Role::Adapter,
                    rss_bytes: Some(180 * 1024 * 1024),
                },
                ProcessRecord {
                    pid: 300,
                    parent_pid: Some(200),
                    role: Role::Cli,
                    rss_bytes: Some(168 * 1024 * 1024),
                },
            ],
            total_rss_bytes: 368 * 1024 * 1024,
            complete: true,
        }
    }

    fn render_to_string(model: &ProcessTree) -> String {
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).expect("a terminal");
        let mut selection = TableState::default();
        terminal
            .draw(|frame| render(frame, model, &mut selection))
            .expect("draw");
        crate::dashboards::flatten(terminal.backend().buffer())
    }

    #[test]
    fn the_tree_renders_the_daemon_its_adapter_and_the_cli_under_it() {
        // The shape a reader needs: the daemon, its adapter, and the CLI indented
        // below. §9.2 names this the one panel worth a native surface.
        let text = render_to_string(&tree());
        assert!(text.contains("100"), "the daemon's pid: {text}");
        assert!(text.contains("200"), "the adapter's pid: {text}");
        assert!(text.contains("300"), "the cli's pid: {text}");
        assert!(text.contains("daemon"), "the roles are labelled: {text}");
        assert!(text.contains("adapter"), "the roles are labelled: {text}");
    }

    #[test]
    fn an_incomplete_tree_says_so_rather_than_presenting_a_short_total_as_exact() {
        // `complete: false` means the platform has no `/proc`. A short total with
        // no marker is a lie the reader cannot detect.
        let mut partial = tree();
        partial.complete = false;
        let text = render_to_string(&partial);
        assert!(text.contains("不完整"), "the scope is stated: {text}");
    }

    #[test]
    fn a_process_with_no_rss_is_shown_as_unknown_not_zero() {
        // Zero is a measurement. "—" is the absence of one, and conflating them
        // would make a platform that cannot report look like an idle process.
        let mut silent = tree();
        silent.processes[2].rss_bytes = None;
        let text = render_to_string(&silent);
        assert!(
            text.contains("—"),
            "no measurement is marked as such: {text}"
        );
    }

    #[test]
    fn a_parent_cycle_terminates_rather_than_spinning() {
        // A hostile or broken `/proc` can report a cycle. Walking it must stop.
        let mut cyclic = tree();
        cyclic.processes[0].parent_pid = Some(300);
        cyclic.processes[1].parent_pid = Some(300);
        // If this returns at all, the walk terminated.
        let text = render_to_string(&cyclic);
        assert!(text.contains("daemon"), "still renders: {text}");
    }

    #[test]
    fn depth_is_measured_from_the_tree_not_the_name() {
        // The daemon's direct child is the adapter and everything below is the
        // CLI — by position, never by what the executable is called.
        let model = tree();
        assert_eq!(depth_of(&model, 100), 0);
        assert_eq!(depth_of(&model, 200), 1);
        assert_eq!(depth_of(&model, 300), 2);
    }
}
