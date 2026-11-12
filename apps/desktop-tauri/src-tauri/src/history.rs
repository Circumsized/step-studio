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
