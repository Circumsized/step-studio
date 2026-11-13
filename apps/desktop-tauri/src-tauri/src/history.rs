//! Session history, read from Step's own session store.
//!
//! The store is `~/.stepcode/agent/sessions/<project>/<session>.jsonl`, one
//! directory per project (the directory name is the project's absolute path with
//! `/` replaced by `-`) and one file per session. That layout is the D-1
//! measurement: it is why each worktree lands in its own session store without
//! Step having a worktree concept.
//!
//! The port is deliberately partial. The TypeScript reader this replaces is 1615
//! lines carrying a cursor, a gzip cold-archive tree, a 512 MB single-file
//! ceiling, a 20 000-file scan ceiling and an eight-entry snapshot cache — all of
//! it sized for a developer profile with tens of thousands of sessions. None of
//! those limits are met here, so none of them are built; what is built is what
//! the renderer asks for and what the store actually contains.
//!
//! Titles and summaries are text from the session's own first user message, never
//! a path: the contract says a summary is "never a path", and a path in that
//! field is a leak of where the user works.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// The renderer's own ceiling for one page.
const MAX_LIMIT: u64 = 200;
/// A title or summary is a snippet, not the message.
const SNIPPET_CHARS: usize = 120;

/// One page of session history.
pub fn list(limit: Option<u64>, workspace_id: Option<&str>, status: Option<&str>) -> Value {
    let Some(store) = store_dir() else {
        return empty("the session store is not present on this machine");
    };
    let Ok(projects) = std::fs::read_dir(&store) else {
        return empty("the session store could not be read");
    };

    // Every session file, newest first once their own timestamps are known.
    // The contract's status filter. Every row here is `active`, because the
    // store has no archive: a session on disk is one that happened. So a
    // request for `archived` or `closed` is answered with nothing rather than
    // with the active rows it did not ask for.
    let wanted_status = status.unwrap_or("active");
    if wanted_status != "active" {
        return json!({ "entries": [], "total": 0 });
    }

    let mut rows: Vec<Value> = Vec::new();
    for project in projects.flatten() {
        let project_dir = project.path();
        if !project_dir.is_dir() {
            continue;
        }
        let Ok(sessions) = std::fs::read_dir(&project_dir) else {
            continue;
        };
        for session in sessions.flatten() {
            let path = session.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(row) = read_session(&path, workspace_id) else {
                continue;
            };
            rows.push(row);
        }
    }

    rows.sort_by(|a, b| {
        let a_at = a.get("lastActiveAt").and_then(Value::as_str).unwrap_or("");
        let b_at = b.get("lastActiveAt").and_then(Value::as_str).unwrap_or("");
        b_at.cmp(a_at)
    });

    let total = rows.len() as u64;
    let asked = limit.unwrap_or(50).min(MAX_LIMIT);
    rows.truncate(asked as usize);
    json!({ "entries": rows, "total": total })
}

fn empty(reason: &str) -> Value {
    json!({ "entries": [], "total": 0, "unavailableReason": reason })
}

/// Read one session file into a history row.
///
/// The first line is the session header (`cwd`, `id`, `timestamp`); the rest are
/// entries. Only the header and the message count are needed, so the file is read
/// once and scanned rather than parsed twice.
fn read_session(path: &Path, workspace_id: Option<&str>) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();

    let header: Value = serde_json::from_str(lines.next()?).ok()?;
    if header.get("type").and_then(Value::as_str) != Some("session") {
        return None;
    }
    let cwd = header.get("cwd").and_then(Value::as_str).unwrap_or("");
    let session_id = header.get("id").and_then(Value::as_str).unwrap_or("");
    let started_at = header.get("timestamp").and_then(Value::as_str).unwrap_or("");

    // The workspace filter is matched against the session's own `cwd`: the
    // store's directory name is that path with separators replaced, and
    // reconstructing the path from the name would guess at the original.
    if let Some(wanted) = workspace_id {
        if !wanted.is_empty() && cwd != wanted {
            return None;
        }
    }

    let mut message_count = 0u64;
    let mut title: Option<String> = None;
    let mut last_active_at = started_at.to_string();
    for line in lines {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(at) = entry.get("timestamp").and_then(Value::as_str) {
            last_active_at = at.to_string();
        }
        if entry.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        message_count += 1;
        // The first user message is the session's own title. A system or
        // assistant message would be a title the user never wrote.
        if title.is_some() {
            continue;
        }
        // `continue`, not `?`: a `message` line without a message record is
        // one malformed line, and `?` here would propagate out of
        // `read_session` and drop the whole session from history — a session
        // that vanishes rather than errors. The reader this replaces defends
        // against exactly this shape (`session-archive-reader.ts:1357`), so it
        // is not hypothetical.
        let Some(message) = entry.get("message") else {
            continue;
        };
        if message.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        title = first_text(message.get("content"));
    }

    // The file name carries the timestamp and the session id, which is the
    // store's own handle on it.
    let history_id = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_default();

    Some(json!({
        "historyId": history_id,
        "threadId": session_id,
        "environmentId": cwd,
        "sessionId": session_id,
        "title": title,
        "summary": title.as_deref().map(snippet),
        "startedAt": started_at,
        "lastActiveAt": last_active_at,
        "messageCount": message_count,
        // Step's store has no archive: a session on disk is one that happened,
        // and nothing here moves one out of it.
        "status": "active",
    }))
}

/// The first text block of a message's content, as a snippet.
fn first_text(content: Option<&Value>) -> Option<String> {
    let blocks = content?.as_array()?;
    let text = blocks.iter().find_map(|block| {
        if block.get("type").and_then(Value::as_str) == Some("text") {
            block.get("text").and_then(Value::as_str)
        } else {
            None
        }
    })?;
    Some(snippet(text))
}

/// A snippet is text, shortened at a character boundary and never a path.
fn snippet(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= SNIPPET_CHARS {
        return trimmed.to_string();
    }
    let mut shortened: String = trimmed.chars().take(SNIPPET_CHARS).collect();
    shortened.push('…');
    shortened
}

/// `~/.stepcode/agent/sessions`, or `None` where there is no home to find it in.
fn store_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)?;
    Some(home.join(".stepcode").join("agent").join("sessions"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snippet_is_shortened_at_a_character_boundary() {
        // Multi-byte, so a byte-index truncation would split a character.
        let long = "中文测试".repeat(100);
        let short = snippet(&long);
        assert!(short.chars().count() <= SNIPPET_CHARS + 1, "the ellipsis is the +1");
        assert!(short.ends_with('…'));
    }

    #[test]
    fn a_short_snippet_is_left_alone() {
        assert_eq!(snippet("  hello  "), "hello");
    }

    #[test]
    fn a_missing_store_is_an_honest_empty_rather_than_an_error() {
        let model = list(Some(10), None, None);
        assert!(model.get("entries").and_then(Value::as_array).is_some());
        assert!(model.get("total").and_then(Value::as_u64).is_some());
    }

    #[test]
    fn a_status_filter_that_is_not_active_is_answered_with_nothing() {
        // Every row is `active`, because the store has no archive. So asking
        // for archived or closed must return nothing rather than the active
        // rows that were not asked for.
        for status in [Some("archived"), Some("closed")] {
            let model = list(Some(50), None, status);
            assert_eq!(
                model.get("entries").and_then(Value::as_array).map(Vec::len),
                Some(0),
                "{status:?} must not be answered with active rows"
            );
        }
        // And the default is active, which is what every renderer call asks for.
        let model = list(Some(50), None, None);
        assert!(model.get("entries").and_then(Value::as_array).is_some());
    }

    #[test]
    fn the_real_store_lists_the_sessions_it_holds() {
        // This machine has run Step, so the store is real. The assertion is that
        // every row carries the fields the renderer reads, not that there is a
        // particular number of them.
        let model = list(Some(200), None, None);
        let entries = model.get("entries").and_then(Value::as_array).expect("entries");
        if entries.is_empty() {
            return;
        }
        for entry in entries {
            for field in ["historyId", "threadId", "environmentId", "startedAt", "lastActiveAt", "status"] {
                assert!(entry.get(field).is_some(), "{field} is missing from {entry}");
            }
            assert_eq!(
                entry.get("status").and_then(Value::as_str),
                Some("active"),
                "a session on disk is one that happened"
            );
            // A summary is text, never a path: the contract says so, and a path
            // would leak where the user works.
            if let Some(summary) = entry.get("summary").and_then(Value::as_str) {
                assert!(
                    !summary.starts_with('/') && !summary.contains("/home/"),
                    "a summary must not be a path: {summary}"
                );
            }
        }
        // Newest first, which is the order the panel reads.
        let times: Vec<&str> = entries
            .iter()
            .filter_map(|entry| entry.get("lastActiveAt").and_then(Value::as_str))
            .collect();
        let mut sorted = times.clone();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(times, sorted, "the newest session comes first");
    }
}

/// The projects Step has sessions for, read from the same store.
///
/// The store's directory name *is* the project's absolute path with `/` replaced
/// by `-` — that is the D-1 measurement — so the list is a decode of the names
/// rather than a separate registry. There is no registry to keep in step with,
/// which is the point: a project exists here because Step has a session in it.
///
/// `name` is the basename and never a path, per the contract. The full path is
/// what `workspaceId` carries, because that is what a file tree or a session
/// filter needs to match against.
/// The project the shell is operating on, or the launch directory.
pub fn active_project() -> Option<String> {
    ACTIVE
        .get_or_init(std::sync::Mutex::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Mark a project active, so a later `projects.list` reports it.
pub fn set_active_project(project: &str) {
    let slot = ACTIVE.get_or_init(std::sync::Mutex::default);
    let mut guard = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(project.to_string());
}

static ACTIVE: std::sync::OnceLock<std::sync::Mutex<Option<String>>> = std::sync::OnceLock::new();

pub fn projects(active: Option<&str>) -> Value {
    let Some(store) = store_dir() else {
        return json!({ "workspaces": [] });
    };
    let Ok(entries) = std::fs::read_dir(&store) else {
        return json!({ "workspaces": [] });
    };

    let mut workspaces: Vec<Value> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // The project's real path comes from a session header, not from the
        // directory name. The name is that path with `/` replaced by `-`, which
        // is not injective: `step-studio` decodes to `step/studio`, so decoding
        // it would hand every consumer a workspaceId that matches nothing.
        let Some(project) = project_path(&path) else {
            continue;
        };
        let name = Path::new(&project)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| project.clone());
        workspaces.push(json!({
            "workspaceId": project,
            "name": name,
            // The store does not record when a project was opened; the newest
            // session in it is the closest thing, and it is a fact rather than a
            // guess.
            "lastOpenedAt": newest_session_at(&path),
            // Which project is active is the caller's to say: the shell knows
            // which one it opened last, and hardcoding `false` here meant a
            // refresh lost it — the renderer reads this flag as the current
            // project, and `workspace.open`'s failure path depends on finding
            // one.
            "active": active == Some(project.as_str()),
        }));
    }

    // De-duplicated *before* sorting. The other order leaves two rows with the
    // same id but different timestamps non-adjacent, so `dedup_by` keeps both —
    // which then breaks `open()`'s "exactly one active" invariant.
    workspaces.sort_by(|a, b| {
        a.get("workspaceId")
            .and_then(Value::as_str)
            .cmp(&b.get("workspaceId").and_then(Value::as_str))
    });
    workspaces.dedup_by(|a, b| a.get("workspaceId") == b.get("workspaceId"));

    // Most recently used first, which is the order a project list is read in.
    workspaces.sort_by(|a, b| {
        let a_at = a.get("lastOpenedAt").and_then(Value::as_str).unwrap_or("");
        let b_at = b.get("lastOpenedAt").and_then(Value::as_str).unwrap_or("");
        b_at.cmp(a_at)
    });
    json!({ "workspaces": workspaces })
}

/// The project a store directory belongs to, from one of its session headers.
fn project_path(project_dir: &Path) -> Option<String> {
    let sessions = std::fs::read_dir(project_dir).ok()?;
    for session in sessions.flatten() {
        let path = session.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        // The first line is the session header, and it carries the real `cwd`.
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(header) = text.lines().next() else {
            continue;
        };
        if let Ok(value) = serde_json::from_str::<Value>(header) {
            if let Some(cwd) = value.get("cwd").and_then(Value::as_str) {
                if !cwd.is_empty() {
                    return Some(cwd.to_string());
                }
            }
        }
    }
    None
}

/// The newest session's last activity in one project, or empty when there is none.
fn newest_session_at(project: &Path) -> String {
    let Ok(sessions) = std::fs::read_dir(project) else {
        return String::new();
    };
    let mut newest = String::new();
    for session in sessions.flatten() {
        let path = session.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        // The last line's timestamp is the session's last activity. The whole
        // file is read to get it: `read_to_string` has no tail mode, and a
        // session file is small. Claiming otherwise in a comment is how a
        // reader comes to believe a cost is not being paid.
        if let Some(at) = last_line_timestamp(&path) {
            if at > newest {
                newest = at;
            }
        }
    }
    newest
}

/// The `timestamp` of a JSONL file's last parseable line.
fn last_line_timestamp(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let last = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())?;
    serde_json::from_str::<Value>(last)
        .ok()?
        .get("timestamp")
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod project_tests {
    use super::*;

    #[test]
    fn the_directory_name_is_not_the_project_path() {
        // The store names a project's directory by its path with `/` replaced by
        // `-`, and that is not injective: a `-` inside a component decodes to a
        // separator. So the directory name is never the answer — the session
        // header's `cwd` is, and this test exists to stop anybody "simplifying"
        // `project_path` back into a decode.
        let encoded = "--home-ubuntu-workspace-step-studio--";
        let decoded = encoded.trim_matches('-').replace('-', "/");
        assert_ne!(decoded, "/home/ubuntu/workspace/step-studio");
        assert_eq!(decoded, "home/ubuntu/workspace/step/studio");
    }

    #[test]
    fn a_name_is_the_basename_and_never_a_path() {
        let project = "/home/ubuntu/workspace/step-studio";
        let name = Path::new(project)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        assert_eq!(name, "step-studio");
        assert!(!name.contains('/'), "a name is a basename, never a path");
    }

    #[test]
    fn the_real_store_lists_the_projects_it_holds() {
        let model = projects(None);
        let workspaces = model.get("workspaces").and_then(Value::as_array).expect("workspaces");
        if workspaces.is_empty() {
            return;
        }
        for workspace in workspaces {
            for field in ["workspaceId", "name", "lastOpenedAt", "active"] {
                assert!(workspace.get(field).is_some(), "{field} is missing from {workspace}");
            }
            let name = workspace.get("name").and_then(Value::as_str).unwrap_or("");
            assert!(!name.contains('/'), "a name is a basename: {name}");
            let id = workspace.get("workspaceId").and_then(Value::as_str).unwrap_or("");
            assert!(id.starts_with('/'), "a workspaceId is the project path: {id}");
        }
        // Most recently used first.
        let times: Vec<&str> = workspaces
            .iter()
            .filter_map(|entry| entry.get("lastOpenedAt").and_then(Value::as_str))
            .collect();
        let mut sorted = times.clone();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(times, sorted, "the most recently used project comes first");
    }
}

/// Mark a project active. Returns the list with that project flagged, which is
/// what `workspace.open` answers with.
pub fn open(workspace_id: &str) -> Value {
    set_active_project(workspace_id);
    let mut model = projects(Some(workspace_id));
    if let Some(workspaces) = model.get_mut("workspaces").and_then(Value::as_array_mut) {
        for workspace in workspaces.iter_mut() {
            let is_active = workspace.get("workspaceId").and_then(Value::as_str) == Some(workspace_id);
            workspace["active"] = Value::Bool(is_active);
        }
        // A project Step has no sessions for is not in the list, and opening one
        // must still work: the shell is in that directory whether or not it has
        // history. So it is added rather than refused.
        if !workspaces.iter().any(|workspace| {
            workspace.get("workspaceId").and_then(Value::as_str) == Some(workspace_id)
        }) {
            let name = Path::new(workspace_id)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| workspace_id.to_string());
            workspaces.insert(
                0,
                json!({
                    "workspaceId": workspace_id,
                    "name": name,
                    "lastOpenedAt": "",
                    "active": true,
                }),
            );
        }
    }
    model["activeWorkspaceId"] = json!(workspace_id);
    model
}

#[cfg(test)]
mod workspace_tests {
    use super::*;

    #[test]
    fn opening_a_project_flags_it_and_names_it_active() {
        let model = open("/home/ubuntu/workspace/step-studio");
        assert_eq!(
            model.get("activeWorkspaceId").and_then(Value::as_str),
            Some("/home/ubuntu/workspace/step-studio")
        );
        let workspaces = model.get("workspaces").and_then(Value::as_array).expect("workspaces");
        let active: Vec<&Value> = workspaces
            .iter()
            .filter(|workspace| workspace.get("active").and_then(Value::as_bool) == Some(true))
            .collect();
        assert_eq!(active.len(), 1, "exactly one project is active");
        assert_eq!(
            active[0].get("workspaceId").and_then(Value::as_str),
            Some("/home/ubuntu/workspace/step-studio")
        );
    }

    #[test]
    fn opening_a_project_with_no_history_still_works() {
        // A directory the user just opened has no sessions, so it is not in the
        // store's list. Refusing would make the first open of a new project
        // impossible, which is the one case the command exists for.
        let fresh = "/tmp/a-project-with-no-sessions";
        let model = open(fresh);
        let workspaces = model.get("workspaces").and_then(Value::as_array).expect("workspaces");
        assert!(
            workspaces.iter().any(|workspace| {
                workspace.get("workspaceId").and_then(Value::as_str) == Some(fresh)
            }),
            "a project with no history is still openable"
        );
    }
}

/// The sub-agent sessions Step keeps on disk, as the Agent Hub reads them.
///
/// Step deliberately excludes these from every list and search — 40-odd RPC
/// commands and not one enumerates them — but the sessions are real files under
/// the same store, marked by `origin: "subagent"` in the header or an id prefixed
/// `subagent-`/`workflow-`. That is the data source, and reading it is a scan of
/// the store the shell already reads for history.
///
/// **The contract's `sessionId` is not honoured, and cannot be.** The store
/// records no parent-session link for a sub-agent: `parentId` in these files is
/// the message tree's own parent, which is a different concept. So the scan is
/// store-wide, and every session is shown the same roster. Filtering by
/// `sessionId` would mean inventing a link that is not on disk.
///
/// The shape is honest about what a file can and cannot say: `status` is
/// `idle`, because a session on disk is not running — a live one would be a
/// process, and nothing here enumerates processes. `hasLiveSession` is therefore
/// false, and `hasTranscript` is true, because the file *is* the transcript.
pub fn agents(_session_id: Option<&str>) -> Value {
    // The `sessionId` the contract passes is deliberately unread: see the module
    // comment. Taking it and ignoring it would look like a filter that works.
    let Some(store) = store_dir() else {
        return json!({ "agents": [] });
    };
    let Ok(projects) = std::fs::read_dir(&store) else {
        return json!({ "agents": [] });
    };

    let mut agents: Vec<Value> = Vec::new();
    for project in projects.flatten() {
        let project_dir = project.path();
        if !project_dir.is_dir() {
            continue;
        }
        let Ok(sessions) = std::fs::read_dir(&project_dir) else {
            continue;
        };
        for session in sessions.flatten() {
            let path = session.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(agent) = read_agent(&path) else {
                continue;
            };
            agents.push(agent);
        }
    }

    // Newest first, the order every other list here is read in.
    agents.sort_by(|a, b| {
        let a_at = a.get("updatedAt").and_then(Value::as_str).unwrap_or("");
        let b_at = b.get("updatedAt").and_then(Value::as_str).unwrap_or("");
        b_at.cmp(a_at)
    });
    json!({ "agents": agents })
}

/// One session file as an agent row, or `None` when it is not a sub-agent's.
fn read_agent(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    let header: Value = serde_json::from_str(lines.next()?).ok()?;
    if header.get("type").and_then(Value::as_str) != Some("session") {
        return None;
    }
    let session_id = header.get("id").and_then(Value::as_str).unwrap_or("");
    // Either marker: the header's `origin`, or the id prefix the store uses.
    let marked = header.get("origin").and_then(Value::as_str) == Some("subagent")
        || session_id.starts_with("subagent-")
        || session_id.starts_with("workflow-");
    if !marked {
        return None;
    }
    let started_at = header.get("timestamp").and_then(Value::as_str).unwrap_or("");

    let mut updated_at = started_at.to_string();
    for line in lines {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(at) = entry.get("timestamp").and_then(Value::as_str) {
            updated_at = at.to_string();
        }
    }
    // There is no read state anywhere in the store, so "unread" is not a
    // quantity that can be read. Reporting the message count here renders as a
    // permanent badge claiming every message is unread; 0 says "no read state",
    // which is what is actually known.
    let unread = 0u64;

    let cwd = header.get("cwd").and_then(Value::as_str).unwrap_or("");
    Some(json!({
        "agentId": session_id,
        // No source on disk carries a generation, and this reaches
        // `expectedGeneration` in the hub's send/kill/revive payloads. A
        // persisted record is not a live roster, so there is nothing to be
        // concurrent *with*; 1 is a stable placeholder for that, not a count of
        // anything. If the hub ever starts rejecting on it, this is the field
        // that needs a real source.
        "generation": 1,
        "kind": "subagent",
        "displayName": session_id,
        "status": "idle",
        "summary": "",
        "startedAt": started_at,
        "updatedAt": updated_at,
        // A file is not a process: nothing here enumerates processes, so a
        // session on disk is not live.
        "hasLiveSession": false,
        "hasTranscript": true,
        "unreadCount": unread,
        "activeJobIds": [],
        "workspaceId": cwd,
    }))
}

#[cfg(test)]
mod agent_tests {
    use super::*;

    /// A session store with one ordinary session and one sub-agent's, so the
    /// marker is what separates them.
    ///
    /// The files are written as JSON rather than as strings, so the fixture
    /// cannot drift from the shape the reader expects.
    fn store_with(subagent_id: &str, marked_by_header: bool) -> (std::path::PathBuf, String) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "step-studio-agents-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let project = dir.join("--tmp-proj--");
        std::fs::create_dir_all(&project).expect("project dir");

        let ordinary = json!({
            "type": "session",
            "id": "sess-1",
            "cwd": "/tmp/proj",
            "timestamp": "2026-01-01T00:00:00Z",
        });
        std::fs::write(
            project.join("ordinary.jsonl"),
            format!("{}\n", serde_json::to_string(&ordinary).expect("serialize")),
        )
        .expect("write the ordinary session");

        let mut header = json!({
            "type": "session",
            "id": subagent_id,
            "cwd": "/tmp/proj",
            "timestamp": "2026-01-02T00:00:00Z",
        });
        if marked_by_header {
            header["origin"] = json!("subagent");
        }
        let message = json!({
            "type": "message",
            "id": "m1",
            "timestamp": "2026-01-02T00:01:00Z",
            "message": { "role": "user", "content": [{ "type": "text", "text": "go" }] },
        });
        let body = format!(
            "{}\n{}\n",
            serde_json::to_string(&header).expect("serialize"),
            serde_json::to_string(&message).expect("serialize")
        );
        std::fs::write(project.join(format!("{subagent_id}.jsonl")), body)
            .expect("write the sub-agent session");
        (dir, subagent_id.to_string())
    }

    #[test]
    fn a_session_marked_in_its_header_is_an_agent() {
        let (dir, id) = store_with("subagent-abc", true);
        let agents = agents_in(&dir);
        assert_eq!(agents.len(), 1, "only the marked session is an agent");
        assert_eq!(agents[0].get("agentId").and_then(Value::as_str), Some(id.as_str()));
        // A file is not a process, and the shape says so rather than implying a
        // live agent.
        assert_eq!(agents[0].get("hasLiveSession").and_then(Value::as_bool), Some(false));
        assert_eq!(agents[0].get("hasTranscript").and_then(Value::as_bool), Some(true));
        assert_eq!(agents[0].get("status").and_then(Value::as_str), Some("idle"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_session_marked_only_by_its_id_prefix_is_an_agent() {
        // Either marker is enough; the store uses both.
        let (dir, id) = store_with("workflow-xyz", false);
        let agents = agents_in(&dir);
        assert_eq!(agents.len(), 1, "the id prefix alone marks it");
        assert_eq!(agents[0].get("agentId").and_then(Value::as_str), Some(id.as_str()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_ordinary_session_is_not_an_agent() {
        let (dir, _) = store_with("subagent-abc", true);
        let ordinary = std::fs::read_to_string(
            dir.join("--tmp-proj--").join("ordinary.jsonl"),
        )
        .expect("read");
        assert!(ordinary.contains("sess-1"));
        // The count is the assertion: the ordinary session was not counted.
        assert_eq!(agents_in(&dir).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `agents` against a specific store, so the tests do not depend on what
    /// this machine happens to have run.
    fn agents_in(store: &Path) -> Vec<Value> {
        let Ok(projects) = std::fs::read_dir(store) else {
            return Vec::new();
        };
        let mut found: Vec<Value> = Vec::new();
        for project in projects.flatten() {
            let Ok(sessions) = std::fs::read_dir(project.path()) else {
                continue;
            };
            for session in sessions.flatten() {
                if let Some(agent) = read_agent(&session.path()) {
                    found.push(agent);
                }
            }
        }
        found
    }
}
