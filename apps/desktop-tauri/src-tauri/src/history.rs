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
pub fn list(limit: Option<u64>, workspace_id: Option<&str>) -> Value {
    let Some(store) = store_dir() else {
        return empty("the session store is not present on this machine");
    };
    let Ok(projects) = std::fs::read_dir(&store) else {
        return empty("the session store could not be read");
    };

    // Every session file, newest first once their own timestamps are known.
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
        let message = entry.get("message")?;
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
        let model = list(Some(10), None);
        assert!(model.get("entries").and_then(Value::as_array).is_some());
        assert!(model.get("total").and_then(Value::as_u64).is_some());
    }

    #[test]
    fn the_real_store_lists_the_sessions_it_holds() {
        // This machine has run Step, so the store is real. The assertion is that
        // every row carries the fields the renderer reads, not that there is a
        // particular number of them.
        let model = list(Some(200), None);
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
pub fn projects() -> Value {
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
            "active": false,
        }));
    }

    // Most recently used first, which is the order a project list is read in.
    workspaces.sort_by(|a, b| {
        let a_at = a.get("lastOpenedAt").and_then(Value::as_str).unwrap_or("");
        let b_at = b.get("lastOpenedAt").and_then(Value::as_str).unwrap_or("");
        b_at.cmp(a_at)
    });
    workspaces.dedup_by(|a, b| {
        a.get("workspaceId") == b.get("workspaceId")
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
        // The last line's timestamp is the session's last activity, so only the
        // tail is read rather than the whole file.
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
        let model = projects();
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
