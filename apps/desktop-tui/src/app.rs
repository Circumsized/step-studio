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
        let error = self.error.clone();
        let _ = error;
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
                    dashboards::usage::render(frame, model);
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
/// second ago; but the first failure has to be visible, or the dashboard is
/// silently empty forever.
fn none_or_error(error: serde_json::Error, nothing_showing: bool) -> Option<String> {
    if nothing_showing {
        Some(format!("数据形状不符：{error}"))
    } else {
        None
    }
}
