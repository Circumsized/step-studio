//! The read models the four dashboards consume.
//!
//! Typed rather than `Value`, and deliberately so: the shell's M2.5 found nine
//! queries silently emptying their panels because a shape change produced
//! `result: null` rather than an error, so nothing failed and nothing rendered.
//! Deserializing into a struct turns that into a compile-time or test-time
//! failure instead of an empty panel.
//!
//! These mirror `packages/client-contract/src/read-models.ts`. The daemon
//! produces the JSON; nothing here re-derives it.

// The read models carry a few fields these four dashboards do not draw yet. They
// stay: dropping one would make the struct claim the model is smaller than it
// is, and the next dashboard that needs it would have to guess the name back.
#![allow(dead_code)]

use serde::Deserialize;

/// The daemon's own process tree. Only the daemon can report this: it is started
/// in its own session, so it is reparented to init when the shell exits — which
/// is what makes a shell restart survivable, and what makes a shell-side walk of
/// `/proc` blind to it.
#[derive(Debug, Clone, Deserialize)]
pub struct ProcessTree {
    #[serde(rename = "generatedAt")]
    pub generated_at: String,
    #[serde(rename = "selfPid")]
    pub self_pid: u32,
    pub processes: Vec<ProcessRecord>,
    #[serde(rename = "totalRssBytes", default)]
    pub total_rss_bytes: u64,
    /// False where the platform has no `/proc`, so a short total is not mistaken
    /// for an exact one.
    #[serde(default)]
    pub complete: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProcessRecord {
    pub pid: u32,
    #[serde(rename = "parentPid")]
    pub parent_pid: Option<u32>,
    /// Derived from the depth in the tree, not from the executable's name: the
    /// daemon's direct child is the adapter, and everything below it is the CLI.
    pub role: Role,
    #[serde(rename = "rssBytes")]
    pub rss_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Daemon,
    Adapter,
    Cli,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Self::Daemon => "daemon",
            Self::Adapter => "adapter",
            Self::Cli => "cli",
        }
    }
}

/// The sub-agent sessions Step keeps on disk, as the Agent Hub reads them.
///
/// Step deliberately excludes these from every list and search, so the session
/// store is the only source. The shell's `session.agents.list` reads it there.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentStatus {
    pub agents: Vec<AgentRecord>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentRecord {
    #[serde(rename = "agentId")]
    pub agent_id: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub status: String,
    #[serde(default)]
    pub summary: String,
    #[serde(rename = "startedAt")]
    pub started_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    /// A file is not a process: nothing enumerates processes, so a session on
    /// disk is not live.
    #[serde(rename = "hasLiveSession", default)]
    pub has_live_session: bool,
    #[serde(rename = "unreadCount", default)]
    pub unread_count: u64,
}

/// Token usage. The structured arrays are absent whenever the CLI could not
/// produce them, which is the normal case — it renders a heatmap of intensity
/// buckets and reports totals only in aggregate, so there are no per-hour,
/// per-model or exact per-day numbers to fill them with.
#[derive(Debug, Clone, Deserialize)]
pub struct Usage {
    #[serde(rename = "generatedAt")]
    pub generated_at: String,
    #[serde(default)]
    pub days: Vec<UsageDay>,
    #[serde(default)]
    pub models: Vec<UsageModel>,
    #[serde(rename = "byModel", default)]
    pub by_model: Vec<UsageModelDay>,
    #[serde(default)]
    pub hours: Vec<UsageHour>,
    #[serde(rename = "unavailableReason")]
    pub unavailable_reason: Option<String>,
    /// The CLI's own `/usage` panel, verbatim. Present precisely when the arrays
    /// above are not: the panel is real data the user asked for, and buckets
    /// dressed up as token counts would be a fabrication.
    pub panel: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UsageDay {
    pub date: String,
    #[serde(default)]
    pub tokens: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UsageModel {
    pub model: String,
    #[serde(default)]
    pub tokens: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UsageModelDay {
    pub model: String,
    pub date: String,
    #[serde(default)]
    pub tokens: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UsageHour {
    pub hour: u32,
    #[serde(default)]
    pub tokens: u64,
}

/// The session tree: Step's own session store, one project per directory and one
/// session per file.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionTree {
    pub entries: Vec<SessionEntry>,
    #[serde(default)]
    pub total: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionEntry {
    #[serde(rename = "historyId")]
    pub history_id: String,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    /// Safe persisted title; absent while native OMP still considers the session
    /// untitled.
    pub title: Option<String>,
    /// Safe summary snippet; never a path.
    pub summary: Option<String>,
    #[serde(rename = "startedAt")]
    pub started_at: String,
    #[serde(rename = "lastActiveAt")]
    pub last_active_at: String,
    #[serde(rename = "messageCount", default)]
    pub message_count: u64,
    pub status: String,
    #[serde(default)]
    pub pinned: bool,
}
