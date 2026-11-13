//! The four dashboards §9.4 names: process tree, agent status, usage, session
//! tree.
//!
//! Each one is a pure function from its read model to a rendered frame. Pure
//! because that is what makes the golden fixtures possible: the same function
//! renders to a `TestBackend` buffer in a test and to the terminal at runtime,
//! and a one-cell drift in any of them fails a test rather than being eyeballed
//! over SSH.

pub mod agent_status;
pub mod process_tree;
pub mod session_tree;
pub mod usage;

/// Which dashboard is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dashboard {
    ProcessTree,
    AgentStatus,
    Usage,
    SessionTree,
}

impl Dashboard {
    /// Tab order, matching §9.4's keybinding对标 tuicr.
    pub const ALL: [Self; 4] = [
        Self::ProcessTree,
        Self::AgentStatus,
        Self::Usage,
        Self::SessionTree,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::ProcessTree => "进程树",
            Self::AgentStatus => "Agent 状态",
            Self::Usage => "用量",
            Self::SessionTree => "会话树",
        }
    }

    /// The query that feeds it. Every one is answered by the daemon itself
    /// except the last two, which the shell also answers — but the TUI is a
    /// surface of the daemon, so they travel the same wire either way.
    pub fn operation(self) -> &'static str {
        match self {
            Self::ProcessTree => "process.tree.get",
            Self::AgentStatus => "session.agents.list",
            Self::Usage => "usage.get",
            Self::SessionTree => "history.list",
        }
    }

    pub fn next(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }

    pub fn previous(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(0);
        Self::ALL[(index + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

/// Flatten a rendered buffer the way a terminal would show it.
///
/// ratatui writes a wide grapheme into one cell and *resets* the cells it covers
/// — they become spaces with the default style, hidden behind the grapheme on a
/// real terminal. Joining one symbol per cell therefore turns CJK text into
/// `进 程 树`, and every comparison then fails on spacing rather than on content,
/// which is how this helper came to exist.
pub fn flatten(buffer: &ratatui::buffer::Buffer) -> String {
    use unicode_width::UnicodeWidthChar;
    let mut text = String::new();
    for y in 0..buffer.area.height {
        let mut x = 0u16;
        while x < buffer.area.width {
            let symbol = buffer[(x, y)].symbol();
            text.push_str(symbol);
            // A grapheme two columns wide covers the cell to its right, which
            // ratatui has already reset to a space. Emitting that space too is
            // what doubles every CJK character.
            let covered: usize = symbol
                .chars()
                .next()
                .and_then(UnicodeWidthChar::width)
                .unwrap_or(1)
                .saturating_sub(1);
            x += 1 + covered as u16;
        }
        text.push('\n');
    }
    text
}
