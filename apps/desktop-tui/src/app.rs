//! The application: attach to the daemon, refresh the active dashboard, draw.
//!
//! The TUI is a client of the daemon and holds no child of its own — exactly
//! like the Tauri shell. That is what makes it a *second* surface: the daemon
//! multiplexes (M2.3), so the TUI and the GUI can be attached at the same time
//! and each gets its own receipts back.

use std::time::{Duration, Instant};

use ratatui::widgets::TableState;
use step_studio_bridge::{AdapterLaunch, BridgeSession};

use crate::dashboards::{self, Dashboard};
use crate::model::{AgentStatus, ProcessTree, SessionTree, Usage};

/// How often the active dashboard's data is refetched.
///
/// Not the frame rate: ratatui redraws only what changed, so a keypress costs
/// one diff. This is how often the *data* moves, and one second is fast enough
/// to watch a process tree while cheap enough to leave the daemon alone.
const REFRESH: Duration = Duration::from_secs(1);

pub struct App {
    session: BridgeSession,
    dashboard: Dashboard,
    process_tree: Option<ProcessTree>,
    agent_status: Option<AgentStatus>,
    usage: Option<Usage>,
    session_tree: Option<SessionTree>,
    /// Why a dashboard is empty, when it is. An empty panel with no reason is
    /// indistinguishable from one that failed to render.
    error: Option<String>,
    selection: TableState,
    /// How far the CLI's own `/usage` panel is scrolled.
    ///
    /// State rather than a rendering input, which is why the dashboard takes it
    /// as an argument: a pure render function given a scroll position is no
    /// longer pure, and the golden fixtures depend on it being pure.
    panel_offset: u16,
    /// A one-shot confirmation for the status line, as `y`'s copy produces.
    ///
    /// Separate from `error` because the two say different things and the reader
    /// acts differently: an error means the data above is stale, a notice means
    /// an action was taken. Folding them into one field would make a successful
    /// copy look like a failure.
    notice: Option<String>,
    refreshed: Instant,
}

impl App {
    /// Attach to the daemon at `endpoint`, or fail saying where it tried.
    pub fn attach(endpoint: &str) -> Result<Self, String> {
        let launch = AdapterLaunch {
            node: String::new(),
            adapter_script: String::new(),
            socket_path: endpoint.to_string(),
            token_file: String::new(),
            cwd: std::env::current_dir()
                .map(|dir| dir.to_string_lossy().to_string())
                .unwrap_or_default(),
            runtime_epoch: 1,
            env: Vec::new(),
            // The daemon owns the adapter and therefore the token.
            token: Some("step-studio".to_string()),
            command: Vec::new(),
        };
        let session = BridgeSession::connect(&launch)
            .map_err(|error| format!("连不上 daemon（{endpoint}）：{error}。壳是否在运行？"))?;
        let mut app = Self {
            session,
            dashboard: Dashboard::ProcessTree,
            process_tree: None,
            agent_status: None,
            usage: None,
            session_tree: None,
            error: None,
            selection: TableState::default(),
            panel_offset: 0,
            notice: None,
            refreshed: Instant::now() - REFRESH,
        };
        app.refresh();
        Ok(app)
    }

    /// Refetch the active dashboard, if its interval has elapsed.
    pub fn tick(&mut self) {
        if self.refreshed.elapsed() < REFRESH {
            return;
        }
        self.refresh();
    }

    fn refresh(&mut self) {
        self.refreshed = Instant::now();
        // The two the daemon answers travel the wire; the two it does not are
        // read from the store and never sent at all. Asking anyway yields
        // `result: null`, which is not an error and so would empty the panel
        // silently — the exact failure M2.5 found nine times over.
        let outcome = match self.dashboard {
            Dashboard::AgentStatus => Ok((
                serde_json::json!({ "result": step_studio_sessions::agents(None) }),
                Vec::new(),
            )),
            Dashboard::SessionTree => Ok((
                serde_json::json!({ "result": step_studio_sessions::list(Some(200), None, None) }),
                Vec::new(),
            )),
            _ => self
                .session
                .invoke(serde_json::json!({ "kind": self.dashboard.operation() })),
        };
        match outcome {
            Ok((receipt, _)) => {
                // The status is checked before the result. A `failed` or
                // `rejected` receipt carries no `result`, and reading only the
                // result turns it into `Null` — which then fails to parse and
                // gets suppressed by `none_or_error` whenever a model is already
                // showing. The panel would keep rendering last minute's token
                // counts as current, with nothing to say otherwise.
                let status = receipt
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("completed");
                if status != "completed" {
                    let detail = receipt
                        .pointer("/error/message")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("（无原因）");
                    self.error = Some(format!(
                        "{} 返回 {status}：{detail}",
                        self.dashboard.title()
                    ));
                    return;
                }
                let result = receipt
                    .get("result")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                // Deserialized, not read as `Value`: a shape change must fail
                // loudly. The shell's M2.5 found nine panels silently emptying
                // because a changed shape produced `result: null` rather than an
                // error, so nothing failed and nothing rendered.
                self.error = match self.dashboard {
                    Dashboard::ProcessTree => Self::adopt(
                        serde_json::from_value::<ProcessTree>(result),
                        &mut self.process_tree,
                    ),
                    Dashboard::AgentStatus => Self::adopt(
                        serde_json::from_value::<AgentStatus>(result),
                        &mut self.agent_status,
                    ),
                    Dashboard::Usage => {
                        Self::adopt(serde_json::from_value::<Usage>(result), &mut self.usage)
                    }
                    Dashboard::SessionTree => Self::adopt(
                        serde_json::from_value::<SessionTree>(result),
                        &mut self.session_tree,
                    ),
                };
            }
            Err(error) => {
                self.error = Some(format!("{} 失败：{error}", self.dashboard.operation()));
            }
        }
    }

    /// Store a freshly deserialized model, or report why it would not parse.
    fn adopt<T>(parsed: Result<T, serde_json::Error>, slot: &mut Option<T>) -> Option<String> {
        match parsed {
            Ok(model) => {
                *slot = Some(model);
                None
            }
            Err(error) => none_or_error(error, slot.is_none()),
        }
    }

    pub fn next_dashboard(&mut self) {
        self.dashboard = self.dashboard.next();
        self.selection = TableState::default();
        self.refresh();
    }

    pub fn previous_dashboard(&mut self) {
        self.dashboard = self.dashboard.previous();
        self.selection = TableState::default();
        self.refresh();
    }

    /// Down: select the next row where there are rows, scroll the panel where
    /// there are none.
    ///
    /// The usage dashboard's CLI panel has no rows, so `j` there was a dead key
    /// and a panel taller than the pane could not be reached at all.
    pub fn move_down(&mut self) {
        if self.dashboard == Dashboard::Usage
            && self
                .usage
                .as_ref()
                .is_some_and(|model| model.panel.is_some())
        {
            self.panel_offset = self.panel_offset.saturating_add(1);
            return;
        }
        self.select_next();
    }

    pub fn move_up(&mut self) {
        if self.dashboard == Dashboard::Usage
            && self
                .usage
                .as_ref()
                .is_some_and(|model| model.panel.is_some())
        {
            self.panel_offset = self.panel_offset.saturating_sub(1);
            return;
        }
        self.select_previous();
    }

    /// `y`: copy the visible rows to the clipboard, as tuicr does.
    ///
    /// The one tuicr binding this TUI was missing. crossterm carries
    /// `CopyToClipboard` (OSC 52), so there is no new dependency and no
    /// clipboard library — the terminal does the storing, which is also why it
    /// works over SSH where an X11 or Wayland selection would not.
    ///
    /// Returns what was copied, so the caller can say so in the status line.
    /// Nothing is copied for the usage dashboard: its content is the CLI's own
    /// panel, and copying a truncated view of it would be worse than copying
    /// nothing.
    pub fn copy_selection(&mut self) -> Option<String> {
        let text = self.selection_text()?;
        // OSC 52 asks the terminal to hold the text. A terminal that ignores it
        // is a terminal where the copy silently did nothing, which is why the
        // status line reports what was written rather than claiming success.
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::clipboard::CopyToClipboard::to_clipboard_from(text.clone())
        );
        Some(text)
    }

    /// What `y` would copy for the active dashboard.
    ///
    /// Dispatches to a free function per dashboard rather than reading the
    /// models through `&self`, because `App` holds a `BridgeSession` and there is
    /// no way to build one without a socket — which would make the selection
    /// logic untestable. The free functions take the model they need and nothing
    /// else.
    pub fn selection_text(&self) -> Option<String> {
        match self.dashboard {
            Dashboard::ProcessTree => self.process_tree.as_ref().and_then(process_rows),
            Dashboard::AgentStatus => self.agent_status.as_ref().and_then(agent_rows),
            Dashboard::Usage => None,
            Dashboard::SessionTree => self.session_tree.as_ref().and_then(session_rows),
        }
    }

    /// Put a one-shot confirmation on the status line.
    pub fn set_status(&mut self, message: String) {
        self.notice = Some(message);
    }

    pub fn select_next(&mut self) {
        let rows = self.row_count();
        if rows == 0 {
            return;
        }
        let current = self.selection.selected().unwrap_or(0);
        self.selection.select(Some((current + 1) % rows));
    }

    pub fn select_previous(&mut self) {
        let rows = self.row_count();
        if rows == 0 {
            return;
        }
        let current = self.selection.selected().unwrap_or(0);
        self.selection.select(Some((current + rows - 1) % rows));
    }

    fn row_count(&self) -> usize {
        match self.dashboard {
            Dashboard::ProcessTree => self
                .process_tree
                .as_ref()
                .map_or(0, |tree| tree.processes.len()),
            Dashboard::AgentStatus => self
                .agent_status
                .as_ref()
                .map_or(0, |model| model.agents.len()),
            Dashboard::Usage => 0,
            Dashboard::SessionTree => self
                .session_tree
                .as_ref()
                .map_or(0, |tree| tree.entries.len()),
        }
    }

    pub fn draw(&mut self, frame: &mut ratatui::Frame) {
        let title = self.dashboard.title();
        match self.dashboard {
            Dashboard::ProcessTree => {
                if let Some(tree) = &self.process_tree {
                    dashboards::process_tree::render(frame, tree, &mut self.selection);
                } else {
                    self.draw_empty(frame, title);
                }
            }
            Dashboard::AgentStatus => {
                if let Some(model) = &self.agent_status {
                    dashboards::agent_status::render(frame, model, &mut self.selection);
                } else {
                    self.draw_empty(frame, title);
                }
            }
            Dashboard::Usage => {
                if let Some(model) = &self.usage {
                    dashboards::usage::render(frame, model, self.panel_offset);
                } else {
                    self.draw_empty(frame, title);
                }
            }
            Dashboard::SessionTree => {
                if let Some(tree) = &self.session_tree {
                    dashboards::session_tree::render(frame, tree, &mut self.selection);
                } else {
                    self.draw_empty(frame, title);
                }
            }
        }
        self.draw_status(frame);
    }

    /// The status line, drawn over the last row when something is wrong.
    ///
    /// Only when there is something to say: the normal path leaves the
    /// dashboard's own border alone. When a refresh fails or a model stops
    /// parsing, the model above is still the last good one — and without this
    /// line it would pass for current data, which is the failure M2.5 kept
    /// finding. Labelling it stale is cheaper than blanking it, and more honest
    /// than showing it silently.
    fn draw_status(&self, frame: &mut ratatui::Frame) {
        use ratatui::layout::Rect;
        use ratatui::style::{Color, Modifier, Style};
        use ratatui::widgets::Paragraph;
        // An error outranks a notice: "the data above is stale" is the more
        // important of the two things this line can say.
        let (text, style) = match &self.error {
            Some(reason) => (
                format!("⚠ {reason}（上方为上一次成功的数据）"),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            None => match &self.notice {
                Some(notice) => (
                    notice.clone(),
                    Style::default().fg(Color::Black).bg(Color::Green),
                ),
                None => return,
            },
        };
        let area = frame.area();
        if area.height == 0 || area.width == 0 {
            return;
        }
        let line = Rect::new(area.x, area.y + area.height - 1, area.width, 1);
        frame.render_widget(Paragraph::new(text).style(style), line);
    }

    /// An honest empty, with the reason when there is one.
    ///
    /// Never a fabricated panel: a dashboard whose data did not arrive says so
    /// rather than showing a plausible-looking empty one. An error is red and a
    /// genuine "nothing here yet" is grey, because "the query failed" and "the
    /// store is empty" are different facts and the reader acts differently on
    /// each.
    fn draw_empty(&self, frame: &mut ratatui::Frame, title: &'static str) {
        use ratatui::style::Style;
        use ratatui::widgets::{Block, Borders, Paragraph};
        let (message, style) = match &self.error {
            Some(reason) => (
                reason.clone(),
                Style::default().fg(ratatui::style::Color::Red),
            ),
            None => (
                "暂无数据".to_string(),
                Style::default().fg(ratatui::style::Color::DarkGray),
            ),
        };
        frame.render_widget(
            Paragraph::new(message)
                .style(style)
                .block(Block::default().title(title).borders(Borders::ALL)),
            frame.area(),
        );
    }
}

/// Report a parse failure only when there is nothing already showing.
///
/// A model that stops parsing must not blank a panel that was rendering fine a
/// second ago — but the failure is still recorded, and `draw` shows it in the
/// status line, so the stale model is labelled rather than passed off as
/// current. Suppressing it entirely is what made a failed refresh invisible.
fn none_or_error(error: serde_json::Error, nothing_showing: bool) -> Option<String> {
    if nothing_showing {
        Some(format!("数据形状不符：{error}"))
    } else {
        None
    }
}

/// Tab-separated process rows. No paths, and no rss where there is no rss.
fn process_rows(tree: &ProcessTree) -> Option<String> {
    let rows: Vec<String> = tree
        .processes
        .iter()
        .map(|process| {
            // A dash, not a zero: zero is a measurement, and the CLI's rss is
            // absent rather than nil.
            let rss = process
                .rss_bytes
                .map_or_else(|| "-".to_string(), |bytes| bytes.to_string());
            format!("{}\t{}\t{}", process.pid, process.role.label(), rss)
        })
        .collect();
    non_empty(rows)
}

fn agent_rows(model: &AgentStatus) -> Option<String> {
    let rows: Vec<String> = model
        .agents
        .iter()
        .map(|agent| {
            format!(
                "{}\t{}\t{}",
                agent.display_name, agent.status, agent.updated_at
            )
        })
        .collect();
    non_empty(rows)
}

fn session_rows(model: &SessionTree) -> Option<String> {
    let rows: Vec<String> = model
        .entries
        .iter()
        .map(|entry| {
            // "(untitled)" rather than an empty field, for the same reason the
            // dashboard shows a placeholder: an empty cell looks like a fault.
            let title = entry
                .title
                .as_deref()
                .filter(|title| !title.is_empty())
                .unwrap_or("(untitled)");
            format!(
                "{}\t{}\t{}",
                title, entry.message_count, entry.last_active_at
            )
        })
        .collect();
    non_empty(rows)
}

/// `None` for an empty list, so a copy of nothing is not a copy of an empty
/// string — which is indistinguishable from a failed copy.
fn non_empty(rows: Vec<String>) -> Option<String> {
    if rows.is_empty() {
        return None;
    }
    Some(rows.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ProcessRecord, ProcessTree, Role};

    fn tree() -> ProcessTree {
        ProcessTree {
            generated_at: "2026-11-13T00:00:00.000Z".to_string(),
            self_pid: 10,
            processes: vec![
                ProcessRecord {
                    pid: 10,
                    parent_pid: None,
                    role: Role::Daemon,
                    rss_bytes: Some(1024),
                },
                ProcessRecord {
                    pid: 11,
                    parent_pid: Some(10),
                    role: Role::Adapter,
                    rss_bytes: None,
                },
            ],
            total_rss_bytes: 1024,
            complete: true,
        }
    }

    #[test]
    fn y_copies_the_rows_the_dashboard_is_showing() {
        // tuicr's copy binding, which was the one binding this TUI was missing.
        assert_eq!(
            process_rows(&tree()).expect("something to copy"),
            "10\tdaemon\t1024\n11\tadapter\t-"
        );
    }

    #[test]
    fn a_row_with_no_measurement_copies_as_a_dash_not_a_zero() {
        // Zero is a measurement. The CLI's rss is absent, and copying 0 would
        // claim something the store never said.
        assert!(process_rows(&tree())
            .expect("copies")
            .contains("11\tadapter\t-"));
    }

    #[test]
    fn an_empty_dashboard_copies_nothing_rather_than_an_empty_string() {
        // An empty string in the clipboard is indistinguishable from a failed
        // copy, and the status line would report a row count of zero.
        let bare = ProcessTree {
            processes: Vec::new(),
            ..tree()
        };
        assert!(process_rows(&bare).is_none());
    }
}
