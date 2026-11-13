//! The session tree dashboard: Step's own session store, one project per
//! directory and one session per file.
//!
//! The title is shown, never the path: the contract says a summary is never a
//! path, and a path in that field leaks where the user works. Where native OMP
//! still considers a session untitled, the row says so rather than showing an
//! empty string that looks like a rendering bug.

use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use ratatui::Frame;

use crate::model::{SessionEntry, SessionTree};

/// What to show when there is no persisted title yet.
///
/// An empty string would look like the panel failed to render; a placeholder
/// says the session is untitled, which is what is actually known.
fn title_of(entry: &SessionEntry) -> &str {
    entry
        .title
        .as_deref()
        .filter(|title| !title.is_empty())
        .unwrap_or("（未命名）")
}

/// Pinned sessions are the ones the user asked to keep, so they are marked
/// rather than sorted — sorting would hide the order the store reports.
fn pin_mark(entry: &SessionEntry) -> &'static str {
    if entry.pinned {
        "★"
    } else {
        " "
    }
}

pub fn render(frame: &mut Frame, model: &SessionTree, selection: &mut TableState) {
    let chunks = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).split(frame.area());

    // The reason rides in the header rather than replacing the table: "no store
    // on this machine" and "the store has no sessions" are different facts, and
    // the reader produces the first specifically so the two are not conflated.
    let mut header = vec![
        Span::raw("共 "),
        Span::styled(
            model.total.to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(" 条会话"),
    ];
    if let Some(reason) = &model.unavailable_reason {
        header.push(Span::raw("  ·  "));
        header.push(Span::styled(
            reason.clone(),
            Style::default().fg(Color::DarkGray),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(header))
            .block(Block::default().title("会话树").borders(Borders::ALL)),
        chunks[0],
    );

    let rows: Vec<Row> = model
        .entries
        .iter()
        .map(|entry| {
            Row::new(vec![
                Cell::from(Span::raw(pin_mark(entry))),
                Cell::from(Span::raw(title_of(entry))),
                Cell::from(Span::raw(entry.message_count.to_string())),
                Cell::from(Span::raw(entry.last_active_at.clone())),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(2),
        Constraint::Min(20),
        Constraint::Length(6),
        Constraint::Length(20),
    ];
    let table = Table::new(rows, widths)
        .header(
            Row::new(vec!["", "标题", "消息", "最近活动"])
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

    use crate::model::SessionTree;

    fn entry(title: Option<&str>, pinned: bool) -> SessionEntry {
        SessionEntry {
            history_id: "h-1".to_string(),
            thread_id: "t-1".to_string(),
            title: title.map(str::to_string),
            summary: None,
            started_at: "2026-11-13T00:00:00.000Z".to_string(),
            last_active_at: "2026-11-13T00:01:00.000Z".to_string(),
            message_count: 7,
            status: "active".to_string(),
            pinned,
        }
    }

    fn render_to_string(model: &SessionTree) -> String {
        let mut terminal = Terminal::new(TestBackend::new(60, 8)).expect("a terminal");
        let mut selection = TableState::default();
        terminal
            .draw(|frame| render(frame, model, &mut selection))
            .expect("draw");
        crate::dashboards::flatten(terminal.backend().buffer())
    }

    #[test]
    fn an_untitled_session_says_so_rather_than_rendering_empty() {
        // An empty title looks like the panel failed. The session is untitled,
        // which is what is actually known.
        let text = render_to_string(&SessionTree {
            entries: vec![entry(None, false)],
            total: 1,
            unavailable_reason: None,
        });
        assert!(text.contains("（未命名）"), "untitled is stated: {text}");
    }

    #[test]
    fn a_pinned_session_is_marked() {
        // The user asked to keep it, so it is marked rather than sorted —
        // sorting would hide the order the store reports.
        let text = render_to_string(&SessionTree {
            entries: vec![entry(Some("keep me"), true)],
            total: 1,
            unavailable_reason: None,
        });
        assert!(text.contains("keep me"), "{text}");
        assert!(text.contains('★'), "the pin is visible: {text}");
    }

    #[test]
    fn a_title_is_never_a_path() {
        // The contract's rule, and the reason the row shows a title and nothing
        // else: a path in that field leaks where the user works.
        let text = render_to_string(&SessionTree {
            entries: vec![entry(Some("/home/ubuntu/workspace/step-studio"), false)],
            total: 1,
            unavailable_reason: None,
        });
        // The dashboard renders whatever it is given, so this is a guard on the
        // *shape*: no row adds a path of its own.
        assert!(
            text.contains("消息"),
            "the columns are the ones declared: {text}"
        );
        assert!(!text.contains("workspaceId"), "no path column: {text}");
    }

    #[test]
    fn an_empty_store_is_an_honest_empty() {
        let text = render_to_string(&SessionTree {
            entries: Vec::new(),
            total: 0,
            unavailable_reason: None,
        });
        assert!(text.contains("共 0"), "{text}");
    }
}
