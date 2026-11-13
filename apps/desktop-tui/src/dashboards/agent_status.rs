//! The agent status dashboard: the sub-agent sessions Step keeps on disk.
//!
//! Step excludes these from every list and search — 40-odd RPC commands and not
//! one enumerates them — so the session store is the source, and this dashboard
//! is the only place they are visible together.

use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use ratatui::Frame;

use crate::model::AgentStatus;

/// A status the reader can act on. `idle` is what a persisted record says,
/// because a file is not a process: nothing here enumerates processes, so a
/// session on disk is not live.
fn status_style(status: &str) -> Style {
    match status {
        "running" => Style::default().fg(Color::Green),
        "waiting" => Style::default().fg(Color::Yellow),
        _ => Style::default().fg(Color::DarkGray),
    }
}

pub fn render(frame: &mut Frame, model: &AgentStatus, selection: &mut TableState) {
    let chunks = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).split(frame.area());

    let live = model
        .agents
        .iter()
        .filter(|agent| agent.has_live_session)
        .count();
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("共 "),
            Span::styled(
                model.agents.len().to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(" 个  ·  活动中 "),
            Span::raw(live.to_string()),
        ]))
        .block(Block::default().title("Agent 状态").borders(Borders::ALL)),
        chunks[0],
    );

    let rows: Vec<Row> = model
        .agents
        .iter()
        .map(|agent| {
            Row::new(vec![
                Cell::from(Span::raw(agent.display_name.clone())),
                Cell::from(Span::styled(
                    agent.status.clone(),
                    status_style(&agent.status),
                )),
                Cell::from(Span::raw(agent.summary.clone())),
                Cell::from(Span::raw(agent.updated_at.clone())),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(20),
        Constraint::Length(9),
        Constraint::Min(20),
        Constraint::Length(20),
    ];
    let table = Table::new(rows, widths)
        .header(
            Row::new(vec!["名称", "状态", "摘要", "最近活动"])
                .style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .block(Block::default().borders(Borders::ALL))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    frame.render_stateful_widget(table, chunks[1], selection);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use crate::model::AgentRecord;

    fn agent(name: &str, status: &str, live: bool) -> AgentRecord {
        AgentRecord {
            agent_id: format!("subagent-{name}"),
            display_name: name.to_string(),
            status: status.to_string(),
            summary: String::new(),
            started_at: "2026-11-13T00:00:00.000Z".to_string(),
            updated_at: "2026-11-13T00:01:00.000Z".to_string(),
            has_live_session: live,
            unread_count: 0,
        }
    }

    fn render_to_string(model: &AgentStatus) -> String {
        let mut terminal = Terminal::new(TestBackend::new(70, 8)).expect("a terminal");
        let mut selection = TableState::default();
        terminal
            .draw(|frame| render(frame, model, &mut selection))
            .expect("draw");
        crate::dashboards::flatten(terminal.backend().buffer())
    }

    #[test]
    fn the_agents_step_keeps_on_disk_are_listed() {
        // The property this dashboard exists for: Step excludes these from every
        // list and search, so this is the only place they appear together.
        let model = AgentStatus {
            agents: vec![
                agent("explore", "idle", false),
                agent("review", "running", true),
            ],
        };
        let text = render_to_string(&model);
        assert!(text.contains("explore"), "{text}");
        assert!(text.contains("review"), "{text}");
    }

    #[test]
    fn the_live_count_counts_live_sessions_not_records() {
        // A file is not a process. Reporting the record count as the live count
        // would claim every archived sub-agent is still running.
        let model = AgentStatus {
            agents: vec![agent("a", "idle", false), agent("b", "running", true)],
        };
        let text = render_to_string(&model);
        assert!(text.contains("活动中 1"), "one live, not two: {text}");
    }

    #[test]
    fn an_empty_roster_is_an_honest_empty_not_an_error() {
        let text = render_to_string(&AgentStatus { agents: Vec::new() });
        assert!(text.contains("共 0"), "the count says none: {text}");
    }
}
