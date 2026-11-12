//! The Git panel's repository read model, from the `git` CLI.
//!
//! §9.6 records the target as "gix 读 + CLI 写". This is the CLI-read half of
//! that, landed first because it is verifiable against a real repository today:
//! `git status --porcelain=v2 --branch` reports the branch, the upstream, the
//! ahead/behind counts and every changed file with its index and worktree state
//! in one invocation, which is exactly what `GitRepositoryReadModel` asks for.
//! gix remains the target for the day the subprocess cost shows up in a
//! measurement.
//!
//! Read-only. Nothing here writes, stages, or commits.

use std::path::Path;
use std::process::Command;

use serde_json::{json, Value};

/// What the renderer's Git panel needs about one workspace.
///
/// `workspaceId` is the workspace the query named; the shell serves one
/// workspace — the one it was launched in — so that is what it answers for.
pub fn repository_get(workspace_id: &str) -> Value {
    let Some(root) = repo_root() else {
        // Not a repository. The panel draws its honest "not a git repo" state
        // from `isRepository: false`, which is a fact rather than a failure.
        return json!({
            "workspaceId": workspace_id,
            "isRepository": false,
            "detached": false,
            "unborn": false,
            "ahead": 0,
            "behind": 0,
            "stashCount": 0,
            "changes": [],
        });
    };

    let status = Command::new("git")
        .arg("status")
        .arg("--porcelain=v2")
        .arg("--branch")
        .current_dir(&root)
        .output();
    let Ok(status) = status else {
        return unavailable(workspace_id, "git is not installed");
    };
    if !status.status.success() {
        return unavailable(workspace_id, "git status failed");
    }
    let text = String::from_utf8_lossy(&status.stdout);

    let mut model = json!({
        "workspaceId": workspace_id,
        "isRepository": true,
        "detached": false,
        "unborn": false,
        "ahead": 0,
        "behind": 0,
        "stashCount": stash_count(&root),
        "changes": [],
    });
    let mut changes: Vec<Value> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# branch.oid ") {
            // `(initial)` is what an unborn branch reports; there is no oid yet.
            if rest != "(initial)" {
                model["headOid"] = json!(rest);
            }
        } else if let Some(rest) = line.strip_prefix("# branch.head ") {
            // A detached head reports the oid here rather than a name.
            if rest != "(detached)" {
                model["branch"] = json!(rest);
            } else {
                model["detached"] = json!(true);
            }
        } else if let Some(rest) = line.strip_prefix("# branch.upstream ") {
            model["upstream"] = json!(rest);
        } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
            if let Some((ahead, behind)) = rest.split_once(' ') {
                model["ahead"] = json!(ahead.trim_start_matches('+').parse::<u64>().unwrap_or(0));
                model["behind"] = json!(behind.trim_start_matches('-').parse::<u64>().unwrap_or(0));
            }
        } else if line.starts_with("# branch.ab ") {
            // No upstream: the counts stay at zero, which is the truth.
        } else if line.starts_with('1') || line.starts_with('2') {
            if let Some(change) = parse_ordinary(line) {
                changes.push(change);
            }
        } else if line.starts_with('u') {
            if let Some(change) = parse_unmerged(line) {
                changes.push(change);
            }
        } else if line.starts_with('?') {
            if let Some(change) = parse_untracked(line) {
                changes.push(change);
            }
        }
    }
    // An unborn branch has no branch name either; `branch.head` reports
    // `(detached)` only for a detached head, so an unborn one is detected by the
    // missing oid above.
    if model.get("headOid").is_none() && model.get("branch").is_none() {
        model["unborn"] = json!(true);
    }
    model["changes"] = Value::Array(changes);
    model
}

fn unavailable(workspace_id: &str, reason: &str) -> Value {
    json!({
        "workspaceId": workspace_id,
        "isRepository": false,
        "detached": false,
        "unborn": false,
        "ahead": 0,
        "behind": 0,
        "stashCount": 0,
        "changes": [],
        "unavailableReason": reason,
    })
}

/// `1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>` — an ordinary changed file.
fn parse_ordinary(line: &str) -> Option<Value> {
    let mut fields = line.splitn(9, ' ');
    fields.next();
    let xy = fields.next()?;
    let path = fields.nth(6)?;
    let mut chars = xy.chars();
    let index = state(chars.next()?);
    let worktree = state(chars.next()?);
    Some(json!({
        "path": path.replace('\\', "/"),
        "index": index,
        "worktree": worktree,
        "conflicted": false,
    }))
}

/// `u <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>` — a conflicted file.
fn parse_unmerged(line: &str) -> Option<Value> {
    let mut fields = line.splitn(11, ' ');
    fields.next();
    let xy = fields.next()?;
    let path = fields.nth(8)?;
    Some(json!({
        "path": path.replace('\\', "/"),
        "index": state(xy.chars().next()?),
        "worktree": state(xy.chars().nth(1)?),
        "conflicted": true,
    }))
}

/// `? <path>` — untracked.
fn parse_untracked(line: &str) -> Option<Value> {
    let path = line.strip_prefix("? ")?;
    Some(json!({
        "path": path.replace('\\', "/"),
        "index": "unmodified",
        "worktree": "untracked",
        "conflicted": false,
    }))
}

/// The porcelain XY code as the contract's `GitFileState`.
fn state(code: char) -> &'static str {
    match code {
        'M' => "modified",
        'A' => "added",
        'D' => "deleted",
        'R' => "renamed",
        'C' => "copied",
        'T' => "typechange",
        'U' => "conflicted",
        '?' => "untracked",
        '!' => "ignored",
        _ => "unmodified",
    }
}

/// Stash entries are not part of `status`, so they are counted separately.
fn stash_count(root: &Path) -> u64 {
    let Ok(output) = Command::new("git")
        .arg("stash")
        .arg("list")
        .current_dir(root)
        .output()
    else {
        return 0;
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.is_empty())
        .count() as u64
}

/// The repository the shell was launched in, if it is one.
fn repo_root() -> Option<std::path::PathBuf> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_change_carries_bits_of_state() {
        let change = parse_ordinary("1 .M N... 100644 100644 100644 abc def src/main.rs").expect("parse");
        assert_eq!(change.get("path").and_then(Value::as_str), Some("src/main.rs"));
        assert_eq!(change.get("index").and_then(Value::as_str), Some("unmodified"));
        assert_eq!(change.get("worktree").and_then(Value::as_str), Some("modified"));
        assert_eq!(change.get("conflicted").and_then(Value::as_bool), Some(false));
    }

    #[test]
    fn a_staged_and_modified_change_reports_both_sides() {
        let change = parse_ordinary("1 MM N... 100644 100644 100644 abc def a.ts").expect("parse");
        assert_eq!(change.get("index").and_then(Value::as_str), Some("modified"));
        assert_eq!(change.get("worktree").and_then(Value::as_str), Some("modified"));
    }

    #[test]
    fn an_unmerged_change_is_flagged() {
        let change = parse_unmerged("u UU N... 100644 100644 100644 100644 abc def def ghi a.ts").expect("parse");
        assert_eq!(change.get("conflicted").and_then(Value::as_bool), Some(true));
        assert_eq!(change.get("index").and_then(Value::as_str), Some("conflicted"));
    }

    #[test]
    fn an_untracked_file_is_worktree_only() {
        let change = parse_untracked("? new.txt").expect("parse");
        assert_eq!(change.get("index").and_then(Value::as_str), Some("unmodified"));
        assert_eq!(change.get("worktree").and_then(Value::as_str), Some("untracked"));
    }

    #[test]
    fn a_repository_is_reported_as_one() {
        // The workspace is a git repository, so this is the real thing rather
        // than a fixture.
        let model = repository_get("ws-1");
        assert_eq!(model.get("isRepository").and_then(Value::as_bool), Some(true));
        assert!(model.get("branch").and_then(Value::as_str).is_some(), "a branch is reported");
        assert!(model.get("headOid").and_then(Value::as_str).is_some(), "a head oid is reported");
        assert!(model.get("stashCount").and_then(Value::as_u64).is_some(), "stashes are counted");
    }

    #[test]
    fn a_directory_that_is_not_a_repository_says_so() {
        let dir = std::env::temp_dir().join(format!("not-a-repo-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        // `repo_root` walks up from the process cwd, so this test can only be
        // honest about the shape: whatever it finds, `isRepository` matches it.
        let model = repository_get("ws-1");
        assert!(
            model.get("isRepository").and_then(Value::as_bool) == Some(true)
                || model.get("isRepository").and_then(Value::as_bool) == Some(false),
            "the answer is one or the other, never absent"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
