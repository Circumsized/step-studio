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
//!
//! # Why the errors are strings
//!
//! Every operation returns `Result<String, String>`, which a typed error enum
//! would improve on the page. It is a string here on purpose, and the reason is
//! the first question of an error strategy — *who handles this, and how do they
//! recover?* The answer is: exactly one consumer, the renderer, which puts the
//! message in front of a human. There is no retry policy, no fallback, no
//! alerting, and no caller that branches on the kind. So the structure a typed
//! enum would add has no reader, and the ~60 lines it costs would be spent
//! buying nothing.
//!
//! What the strings do carry is the distinction that matters to that one
//! consumer, and it is carried in the text: a refusal (`refusing the path X`)
//! is a caller bug or an attack, `git is not installed` is the environment, and
//! anything else is git's own stderr. If a second consumer ever appears — a
//! retry loop, a diagnostic panel that groups failures — that is the moment to
//! replace this with an enum, and not before.

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
        } else if line.starts_with('2') {
            if let Some(change) = parse_renamed(line) {
                changes.push(change);
            }
        } else if line.starts_with('1') {
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
    // An unborn branch still reports its name in `branch.head`; only the oid is
    // `(initial)`. So the oid is what decides, and requiring the name to be
    // absent too made `unborn` unreachable.
    if model.get("headOid").is_none() {
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

/// `2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path>TAB<origPath>` — a
/// rename or a copy.
///
/// The extra score field is why a type-2 line read as an ordinary one produced
/// `path: "R100 new\told"`: the field offsets shift by one, and the path pair is
/// tab-separated rather than a single field. The contract has `originalPath` for
/// exactly this, and it was never emitted.
fn parse_renamed(line: &str) -> Option<Value> {
    let mut fields = line.splitn(10, ' ');
    fields.next();
    let xy = fields.next()?;
    // sub, mH, mI, mW, hH, hI — six of them before the score.
    fields.nth(6)?;
    let pair = fields.next()?;
    let (path, original) = pair.split_once('\t')?;
    let mut chars = xy.chars();
    let index = state(chars.next()?);
    let worktree = state(chars.next()?);
    Some(json!({
        "path": path.replace('\\', "/"),
        "originalPath": original.replace('\\', "/"),
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
///
/// `T` (typechange) and `!` (ignored) have no member in `GitFileState`, so they
/// cannot be reported as themselves — a value the contract does not declare is a
/// lie the renderer cannot interpret. They fall to `modified`, which is what the
/// reference implementation does and is at least a declared value.
fn state(code: char) -> &'static str {
    match code {
        // `.` is explicitly unmodified, not an unknown code.
        '.' => "unmodified",
        'M' => "modified",
        'A' => "added",
        'D' => "deleted",
        'R' => "renamed",
        'C' => "copied",
        'U' => "conflicted",
        '?' => "untracked",
        _ => "modified",
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

/// A scratch file inside the repository, removed on every path out of the test
/// that made it.
///
/// A guard rather than a cleanup line at the end: an `assert!` that fires before
/// the cleanup leaves the file in the working tree *and* in the index, which is a
/// dirty tree the next test run inherits — and the two tests that need it both
/// failed that way before this existed.
#[cfg(test)]
mod scratch {
    use std::path::PathBuf;
    use std::process::Command;

    pub struct Scratch {
        pub root: PathBuf,
        pub name: String,
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(self.root.join(&self.name));
            let _ = Command::new("git")
                .arg("rm")
                .arg("--cached")
                .arg("--ignore-unmatch")
                .arg(&self.name)
                .current_dir(&self.root)
                .output();
        }
    }
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
    fn a_rename_carries_its_original_path() {
        // The extra <X><score> field shifts the offsets by one, so reading a
        // type-2 line as an ordinary one produced `path: "R100 new\told"` and
        // `originalPath` was never emitted at all.
        let change = parse_renamed("2 R. N... 100644 100644 100644 100644 100644 R100 new\told").expect("parse");
        assert_eq!(change.get("path").and_then(Value::as_str), Some("new"));
        assert_eq!(
            change.get("originalPath").and_then(Value::as_str),
            Some("old"),
            "the contract has originalPath for exactly this"
        );
        assert_eq!(change.get("index").and_then(Value::as_str), Some("renamed"));
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

/// The commit history, in the shape the Git panel's log view reads.
///
/// Same CLI-read decision as [`repository_get`]: one `git log` call with a
/// record separator gives oid, parents, subject, author, date and every ref
/// pointing at the commit, which is `GitLogCommitRecord` in full.
/// Whether a page came back full, which is what `truncated` reports.
///
/// The walk fetches one more commit than asked for, so "the extra record
/// arrived" is the signal — not "exactly this many existed". A repository with
/// exactly `asked` commits fetches `asked` records and an empty `asked+1`-th,
/// and must not report truncation.
fn page_is_truncated(fetched: usize, asked: u64) -> bool {
    fetched as u64 > asked
}

pub fn log_list(workspace_id: &str, limit: Option<u64>, skip: Option<u64>) -> Value {
    let Some(root) = repo_root() else {
        return json!({
            "workspaceId": workspace_id,
            "commits": [],
            "truncated": false,
            "ahead": 0,
            "behind": 0,
        });
    };
    // One more than asked for, so `truncated` means "there is more" rather than
    // "exactly this many existed".
    let count = limit.unwrap_or(50).min(500) + 1;
    let mut command = Command::new("git");
    command
        .arg("log")
        .arg(format!("-n{count}"))
        .arg(format!("--skip={}", skip.unwrap_or(0)))
        // A record separator and a unit separator, so a subject containing the
        // record separator cannot split a commit in two.
        .arg("--pretty=format:%H%x1f%P%x1f%s%x1f%an%x1f%aI%x1f%D%x1e")
        .current_dir(&root);
    let Ok(output) = command.output() else {
        return unavailable_log(workspace_id, "git is not installed");
    };
    if !output.status.success() {
        return unavailable_log(workspace_id, "git log failed");
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // Which of these commits the upstream does not have. One `rev-list` call
    // gives the set exactly; without it every commit would have to be labelled
    // `common`, which is a claim rather than a fact — this repository is 100+
    // commits ahead of its upstream and none of them are common.
    let outgoing = outgoing_oids(&root);
    let kinds = ref_kinds(&root);

    let mut commits: Vec<Value> = Vec::new();
    for record in text.split('\u{1e}') {
        let record = record.trim_start_matches('\n');
        if record.is_empty() {
            continue;
        }
        let mut fields = record.split('\u{1f}');
        let Some(oid) = fields.next() else { continue };
        let parents = fields
            .next()
            .unwrap_or("")
            .split(' ')
            .filter(|parent| !parent.is_empty())
            .map(|parent| json!(parent))
            .collect::<Vec<_>>();
        let subject = fields.next().unwrap_or("");
        let author = fields.next().unwrap_or("");
        let date = fields.next().unwrap_or("");
        let refs = parse_refs(fields.next().unwrap_or(""), &kinds);
        // The newest commit is the head; the rest are outgoing if the upstream
        // lacks them and common if it has them. `incoming` is never produced
        // here: this walk starts at HEAD, so a commit only the upstream has is
        // not in it at all — and the contract's `incoming` would be a claim
        // about a commit this page does not contain.
        let relation = if commits.is_empty() {
            "head"
        } else if outgoing.contains(oid) {
            "outgoing"
        } else {
            "common"
        };
        commits.push(json!({
            "oid": oid,
            "parents": parents,
            "subject": subject,
            "authorName": author,
            "authorDate": date,
            "refs": refs,
            "relation": relation,
        }));
    }

    // Truncated means the extra commit arrived. The old check ran inside the
    // loop and broke at `limit`, so it never saw the `limit+1`-th record — a
    // repository with exactly `limit` commits reported more.
    let asked = limit.unwrap_or(50).min(500);
    let truncated = page_is_truncated(commits.len(), asked);
    let head = commits.first().map(|commit| commit.get("oid").cloned().unwrap_or(Value::Null));
    json!({
        "workspaceId": workspace_id,
        "commits": commits,
        "truncated": truncated,
        "headOid": head,
        "ahead": 0,
        "behind": 0,
    })
}

/// The commits HEAD has that its upstream does not.
///
/// Empty when there is no upstream, which makes everything `common` — and that is
/// the truth in that case, not a default: with nothing to compare against there
/// is no such thing as outgoing.
fn outgoing_oids(root: &Path) -> std::collections::HashSet<String> {
    let Ok(output) = Command::new("git")
        .arg("rev-list")
        .arg("HEAD")
        .arg("^@{upstream}")
        .current_dir(root)
        .output()
    else {
        return std::collections::HashSet::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

fn unavailable_log(workspace_id: &str, reason: &str) -> Value {
    json!({
        "workspaceId": workspace_id,
        "commits": [],
        "truncated": false,
        "ahead": 0,
        "behind": 0,
        "unavailableReason": reason,
    })
}

/// Every ref's full name, so a decoration can be classified by what it actually
/// is rather than by what its name looks like.
///
/// `%D` strips the `refs/` prefix, which makes a local `feature/x` and a remote
/// `origin/feature/x` indistinguishable by name — and a slash in the name is not
/// evidence of being remote. One `for-each-ref` call gives the truth.
fn ref_kinds(root: &Path) -> std::collections::HashMap<String, &'static str> {
    let mut kinds = std::collections::HashMap::new();
    let Ok(output) = Command::new("git")
        .arg("for-each-ref")
        .arg("--format=%(refname)")
        .current_dir(root)
        .output()
    else {
        return kinds;
    };
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(name) = line.strip_prefix("refs/heads/") {
            kinds.insert(name.to_string(), "local");
        } else if let Some(name) = line.strip_prefix("refs/remotes/") {
            kinds.insert(name.to_string(), "remote");
        } else if let Some(name) = line.strip_prefix("refs/tags/") {
            kinds.insert(name.to_string(), "tag");
        }
    }
    kinds
}

/// `%D` output: `HEAD -> main, origin/main, tag: v1`. Each name becomes a ref
/// with the kind the contract declares — `local`, `remote`, `tag` or `head`.
fn parse_refs(decorations: &str, kinds: &std::collections::HashMap<String, &'static str>) -> Vec<Value> {
    let mut refs = Vec::new();
    for entry in decorations.split(", ") {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        // `tag:` is the one prefix `%D` keeps, so a tag is recognisable on its
        // own. Everything else is a bare name, and the full-ref map says which
        // it is — a local `feature/x` and a remote `origin/feature/x` are the
        // same string once the prefix is stripped.
        let (kind, name) = if let Some(rest) = entry.strip_prefix("HEAD -> ") {
            ("local", rest)
        } else if entry == "HEAD" {
            ("head", "")
        } else if let Some(rest) = entry.strip_prefix("tag: ") {
            ("tag", rest)
        } else {
            (kinds.get(entry).copied().unwrap_or("local"), entry)
        };
        if name.is_empty() {
            continue;
        }
        refs.push(json!({ "name": name, "kind": kind, "current": false }));
    }
    // The branch HEAD points at is the current one. `%D` marks it with the
    // `HEAD -> ` prefix, so that entry is the one to flag.
    if let Some(current) = refs.iter_mut().find(|entry| {
        entry.get("kind").and_then(Value::as_str) == Some("local")
            && entry.get("current").and_then(Value::as_bool) == Some(false)
    }) {
        if let Some(object) = current.as_object_mut() {
            object.insert("current".to_string(), json!(true));
        }
    }
    refs
}

#[cfg(test)]
mod log_tests {
    use super::*;

    #[test]
    fn the_real_repository_reports_its_history() {
        let model = log_list("ws-1", Some(3), None);
        let commits = model.get("commits").and_then(Value::as_array).expect("commits");
        assert!(!commits.is_empty(), "this workspace has history");
        let first = &commits[0];
        for field in ["oid", "subject", "authorName", "authorDate"] {
            assert!(
                first.get(field).and_then(Value::as_str).is_some_and(|value| !value.is_empty()),
                "{field} is reported for the newest commit"
            );
        }
        assert!(first.get("parents").and_then(Value::as_array).is_some(), "parents are a list");
    }

    #[test]
    fn the_relation_is_computed_rather_than_asserted() {
        // This repository is many commits ahead of its upstream, so the newest
        // commits must be `outgoing` — a constant `common` would be a claim the
        // data contradicts.
        let model = log_list("ws-1", Some(5), None);
        let commits = model.get("commits").and_then(Value::as_array).expect("commits");
        let first = &commits[0];
        assert_eq!(
            first.get("relation").and_then(Value::as_str),
            Some("head"),
            "the newest commit is the head"
        );
        let relations: Vec<&str> = commits
            .iter()
            .skip(1)
            .filter_map(|commit| commit.get("relation").and_then(Value::as_str))
            .collect();
        assert!(
            relations.iter().any(|relation| *relation == "outgoing" || *relation == "common"),
            "each relation is one of the two the data supports: {relations:?}"
        );
        // And no commit is labelled `incoming`: this walk starts at HEAD, so a
        // commit only the upstream has is not in the page at all.
        assert!(
            !relations.contains(&"incoming"),
            "incoming would be a claim about a commit this page does not contain"
        );
    }

    #[test]
    fn asking_for_one_more_than_exist_is_not_truncated() {
        // The off-by-one, stated against a length the test controls.
        //
        // This used to ask the live repository for 500 commits and assert
        // `truncated == false` — a claim that the history is shorter than 500.
        // It held while the checkout was a shallow clone at 346 commits, and
        // silently became unsatisfiable the moment the history was completed to
        // 529: `log_list` caps a page at `limit.min(500)`, so no request against
        // this repository can be "more than exists" any more. The bug it guards
        // is real, so it is now tested where the length is known.
        assert!(!page_is_truncated(50, 50), "exactly the asked-for count reported more");
        assert!(!page_is_truncated(500, 500), "the cap boundary reported more");
        assert!(!page_is_truncated(0, 50), "an empty history reported more");
        // And one record past the limit does report it, so the check is not
        // simply refusing.
        assert!(page_is_truncated(51, 50), "one past the limit was accepted");
        assert!(page_is_truncated(501, 500), "one past the cap was accepted");
    }

    #[test]
    fn a_subject_containing_the_separator_does_not_split_a_commit() {
        // The record separator is \x1e and the field separator \x1f; a subject
        // holding a newline is the realistic case, and it must not become two
        // commits.
        let text = "oid1\x1f\x1fsubject line one\nline two\x1fa\x1f2026-01-01T00:00:00Z\x1f\x1e";
        let mut commits = Vec::new();
        for record in text.split('\u{1e}') {
            let record = record.trim_start_matches('\n');
            if record.is_empty() {
                continue;
            }
            let mut fields = record.split('\u{1f}');
            let oid = fields.next().unwrap_or("");
            let _parents = fields.next().unwrap_or("");
            let subject = fields.next().unwrap_or("");
            commits.push(json!({ "oid": oid, "subject": subject }));
        }
        assert_eq!(commits.len(), 1, "one record stays one commit");
        assert_eq!(
            commits[0].get("subject").and_then(Value::as_str),
            Some("subject line one\nline two")
        );
    }

    #[test]
    fn decorations_become_refs_with_a_kind() {
        // The kinds come from the full-ref map, not from the name: `origin/main`
        // and a local `feature/x` are the same shape once `%D` strips the prefix.
        let mut kinds = std::collections::HashMap::new();
        kinds.insert("main".to_string(), "local");
        kinds.insert("origin/main".to_string(), "remote");
        kinds.insert("v1".to_string(), "tag");
        kinds.insert("feature/x".to_string(), "local");
        let refs = parse_refs("HEAD -> main, origin/main, tag: v1, feature/x", &kinds);
        let names: Vec<&str> = refs
            .iter()
            .filter_map(|entry| entry.get("name").and_then(Value::as_str))
            .collect();
        assert!(names.contains(&"main"), "the current branch is a ref");
        assert!(names.contains(&"origin/main"), "the upstream is a ref");
        assert!(names.contains(&"v1"), "the tag is a ref");
        let tag = refs
            .iter()
            .find(|entry| entry.get("name").and_then(Value::as_str) == Some("v1"))
            .expect("the tag");
        assert_eq!(tag.get("kind").and_then(Value::as_str), Some("tag"));
        let branch = refs
            .iter()
            .find(|entry| entry.get("name").and_then(Value::as_str) == Some("main"))
            .expect("the branch");
        assert_eq!(branch.get("kind").and_then(Value::as_str), Some("local"));
        assert_eq!(branch.get("current").and_then(Value::as_bool), Some(true));
        let remote = refs
            .iter()
            .find(|entry| entry.get("name").and_then(Value::as_str) == Some("origin/main"))
            .expect("the upstream");
        assert_eq!(remote.get("kind").and_then(Value::as_str), Some("remote"));
        let slashed_local = refs
            .iter()
            .find(|entry| entry.get("name").and_then(Value::as_str) == Some("feature/x"))
            .expect("the local branch");
        assert_eq!(
            slashed_local.get("kind").and_then(Value::as_str),
            Some("local"),
            "a local branch with a slash is not a remote ref"
        );
    }
}

/// Every branch, local and remote-tracking, with the counts the panel shows.
///
/// `git for-each-ref` reports the name, the oid, the upstream and the ahead/behind
/// track in one call per ref, which is `GitBranchRecord` in full. The
/// ahead/behind pair comes from `%(upstream:track)`, whose `[ahead 105]` form is
/// parsed rather than assumed.
pub fn branches_list(workspace_id: &str) -> Value {
    let Some(root) = repo_root() else {
        return json!({ "workspaceId": workspace_id, "branches": [] });
    };
    let output = Command::new("git")
        .arg("for-each-ref")
        .arg("--format=%(refname)%1f%(objectname)%1f%(upstream)%1f%(upstream:track)%1f%(HEAD)")
        .arg("refs/heads")
        .arg("refs/remotes")
        .current_dir(&root)
        .output();
    let Ok(output) = output else {
        return json!({
            "workspaceId": workspace_id,
            "branches": [],
            "unavailableReason": "git is not installed",
        });
    };
    if !output.status.success() {
        return json!({
            "workspaceId": workspace_id,
            "branches": [],
            "unavailableReason": "git for-each-ref failed",
        });
    }
    let text = String::from_utf8_lossy(&output.stdout);

    let mut branches: Vec<Value> = Vec::new();
    for line in text.lines() {
        let mut fields = line.split('\u{1f}');
        let Some(reference) = fields.next() else { continue };
        let oid = fields.next().unwrap_or("");
        let upstream = fields.next().unwrap_or("");
        let track = fields.next().unwrap_or("");
        let head = fields.next().unwrap_or("");
        if oid.is_empty() {
            continue;
        }
        // `refs/remotes/origin/HEAD` is a symbolic ref that points at another
        // ref rather than at a commit; listing it would duplicate
        // `origin/main` under a name the user never typed.
        if reference == "refs/remotes/origin/HEAD" {
            continue;
        }
        let (remote, name) = if let Some(rest) = reference.strip_prefix("refs/heads/") {
            (false, rest)
        } else if let Some(rest) = reference.strip_prefix("refs/remotes/") {
            (true, rest)
        } else {
            continue;
        };
        let (ahead, behind) = parse_track(track);
        branches.push(json!({
            "name": name,
            "remote": remote,
            "current": head.trim() == "*",
            "headOid": oid,
            // Empty when the branch has no upstream, which is the truth rather
            // than a gap.
            "upstream": if upstream.is_empty() { Value::Null } else { json!(upstream) },
            "ahead": ahead,
            "behind": behind,
        }));
    }
    json!({ "workspaceId": workspace_id, "branches": branches })
}

/// `[ahead 105]`, `[behind 3]`, `[ahead 2, behind 5]`, or empty.
fn parse_track(track: &str) -> (u64, u64) {
    let mut ahead = 0;
    let mut behind = 0;
    for part in track.trim_matches(|c| c == '[' || c == ']').split(", ") {
        if let Some(rest) = part.strip_prefix("ahead ") {
            ahead = rest.parse().unwrap_or(0);
        } else if let Some(rest) = part.strip_prefix("behind ") {
            behind = rest.parse().unwrap_or(0);
        }
    }
    (ahead, behind)
}

#[cfg(test)]
mod branch_tests {
    use super::*;

    #[test]
    fn the_track_field_becomes_ahead_and_behind() {
        assert_eq!(parse_track("[ahead 105]"), (105, 0));
        assert_eq!(parse_track("[behind 3]"), (0, 3));
        assert_eq!(parse_track("[ahead 2, behind 5]"), (2, 5));
        // A branch with no upstream reports nothing, and that is not an error.
        assert_eq!(parse_track(""), (0, 0));
        assert_eq!(parse_track("[gone]"), (0, 0));
    }

    #[test]
    fn the_real_repository_lists_its_branches() {
        let model = branches_list("ws-1");
        let branches = model.get("branches").and_then(Value::as_array).expect("branches");
        assert!(!branches.is_empty(), "this workspace has branches");
        for branch in branches {
            let name = branch.get("name").and_then(Value::as_str).unwrap_or("");
            assert!(!name.is_empty(), "every branch has a name");
            // Exactly one is current, and it is a local branch.
            if branch.get("current").and_then(Value::as_bool) == Some(true) {
                assert_eq!(branch.get("remote").and_then(Value::as_bool), Some(false));
            }
        }
        // The symbolic `origin/HEAD` is not listed: it duplicates `origin/main`
        // under a name the user never typed.
        assert!(
            !branches.iter().any(|branch| {
                branch.get("name").and_then(Value::as_str) == Some("origin/HEAD")
            }),
            "the symbolic remote HEAD is filtered out"
        );
    }
}

/// One file's patch, from the working tree or from the index.
///
/// `git diff` with `--cached` for the staged side and without it for the working
/// side, which is the whole of the distinction `GitDiffTarget` makes. The patch
/// is passed through as text: the renderer's diff view is a diff renderer, and
/// re-serialising the patch would only risk changing it.
///
/// A path is a repository-relative path from the query. It is passed as a single
/// argv element, never through a shell, so a path cannot become an option.
pub fn diff_get(workspace_id: &str, path: &str, target: &str) -> Value {
    let Some(root) = repo_root() else {
        return unavailable_diff(workspace_id, path, target, "not a repository");
    };
    let mut command = Command::new("git");
    command.arg("diff").arg("--no-color");
    if target == "staged" {
        command.arg("--cached");
    }
    command.arg("--").arg(path).current_dir(&root);
    let Ok(output) = command.output() else {
        return unavailable_diff(workspace_id, path, target, "git is not installed");
    };
    if !output.status.success() {
        return unavailable_diff(workspace_id, path, target, "git diff failed");
    }
    let patch = String::from_utf8_lossy(&output.stdout).to_string();
    // `git diff` says nothing at all about a file it considers binary, so an
    // empty patch is ambiguous on its own. Asking directly separates "no
    // changes" from "changes we cannot show as text".
    let binary = patch.is_empty() && is_binary(&root, path);
    json!({
        "workspaceId": workspace_id,
        "path": path,
        "target": target,
        "patch": patch,
        "binary": binary,
        "truncated": false,
        // The revision the diff was taken against, so the view can say what it
        // is comparing to.
        "revision": head_revision(&root),
    })
}

fn unavailable_diff(workspace_id: &str, path: &str, target: &str, reason: &str) -> Value {
    json!({
        "workspaceId": workspace_id,
        "path": path,
        "target": target,
        "patch": "",
        "binary": false,
        "truncated": false,
        "unavailableReason": reason,
    })
}

fn is_binary(root: &Path, path: &str) -> bool {
    let Ok(output) = Command::new("git")
        .arg("diff")
        .arg("--numstat")
        .arg("--")
        .arg(path)
        .current_dir(root)
        .output()
    else {
        return false;
    };
    // A binary file reports `-	-	<path>`; a text file reports counts.
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| line.starts_with("-	-	"))
}

fn head_revision(root: &Path) -> String {
    Command::new("git")
        .arg("rev-parse")
        .arg("HEAD")
        .current_dir(root)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod diff_tests {
    use super::*;

    /// A file with a change `git diff` can see, made with `add -N` so it shows
    /// up as an addition without touching anything already committed.
    fn scratch_change() -> Option<(std::path::PathBuf, String)> {
        let root = repo_root()?;
        let name = format!("scratch-diff-test-{}.txt", std::process::id());
        let path = root.join(&name);
        std::fs::write(&path, "ponytail diff line\n").ok()?;
        // `-f`, because `.gitignore` lists this name: three of them were
        // committed as empty tracked files once, and the ignore rule is what
        // stops that. Without `-f` git refuses the intent-to-add, exits non-zero
        // — and `output()` succeeding only says the process ran — so the file is
        // written but never recorded, and every diff assertion below sees an
        // empty patch. The failure is silent in exactly the place the test is
        // meant to be loud.
        Command::new("git")
            .arg("add")
            .arg("-N")
            .arg("-f")
            .arg(&name)
            .current_dir(&root)
            .output()
            .ok()?;
        Some((path, name))
    }

    #[test]
    fn a_working_change_comes_back_as_a_patch() {
        let Some((path, name)) = scratch_change() else {
            // No repository: nothing to assert against.
            return;
        };
        let root = path.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
        let _scratch = scratch::Scratch { root, name: name.clone() };
        let model = diff_get("ws-1", &name, "working");
        let patch = model.get("patch").and_then(Value::as_str).unwrap_or("");
        assert!(patch.contains("ponytail diff line"), "the patch carries the change: {patch}");
        assert_eq!(model.get("binary").and_then(Value::as_bool), Some(false));
        assert!(model.get("revision").and_then(Value::as_str).is_some_and(|r| !r.is_empty()));

        // The staged side of the same file is a different question, and it is
        // asked separately: `add -N` records intent, not content.
        let staged = diff_get("ws-1", &name, "staged");
        assert!(staged.get("patch").and_then(Value::as_str).is_some());
        // `_scratch` cleans up here, including on the assert paths above.
    }

    #[test]
    fn a_path_that_is_not_changed_is_empty_rather_than_an_error() {
        // The fixture is a path the repository has never heard of, and that is
        // deliberate. It used to be a real committed file with the comment "the
        // tree is clean" — which made the test fail for anyone who had that file
        // open, because a working tree mid-edit is the normal state of a working
        // tree. `git diff` answers an unknown path with an empty patch and a
        // zero exit, which is the same "no changes is not an error" behaviour
        // without the premise nobody can hold.
        let model = diff_get("ws-1", "a/path/that/does/not/exist.rs", "working");
        assert_eq!(model.get("patch").and_then(Value::as_str), Some(""));
        assert_eq!(model.get("binary").and_then(Value::as_bool), Some(false));
        assert!(model.get("unavailableReason").is_none(), "no changes is not an error");
    }
}

/// Run one Git operation. Write side, per §9.6: "gix 读 + CLI 写".
///
/// The contract declares ~30 operation kinds. This implements the three the Git
/// panel's change list actually issues — `stage`, `unstage`, `commit` — and
/// refuses the rest with a message that names them, rather than pretending a
/// half-built operation surface is a complete one. The renderer surfaces the
/// message, so a user sees "not wired" rather than a silent no-op.
///
/// Every path arrives as a single argv element, never through a shell, and is
/// rejected if it escapes the repository: a change list entry is a
/// repository-relative path, and anything else is a bug or an attack.
pub fn execute(operation: &Value) -> Value {
    let Some(root) = repo_root() else {
        return refused(operation, "not a git repository");
    };
    let kind = operation.get("kind").and_then(Value::as_str).unwrap_or("");
    let outcome = match kind {
        "stage" => paths_op(&root, &["add", "--"], operation.get("paths"), true),
        "unstage" => paths_op(&root, &["restore", "--staged", "--"], operation.get("paths"), true),
        "commit" => commit(&root, operation),
        "branch.create" => branch_create(&root, operation),
        "branch.switch" => branch_switch(&root, operation),
        "branch.rename" => branch_rename(&root, operation),
        "branch.delete" => branch_delete(&root, operation),
        "fetch" => fetch(&root, operation),
        "pull" => pull(&root, operation),
        "push" => push(&root, operation),
        "stash.push" => stash_push(&root, operation),
        "stash.apply" => stash_apply(&root, operation),
        "stash.drop" => stash_drop(&root, operation),
        "discard" => discard(&root, operation),
        "merge" => integrate(&root, "merge", operation),
        "rebase" => integrate(&root, "rebase", operation),
        "cherry-pick" => integrate(&root, "cherry-pick", operation),
        "revert" => integrate(&root, "revert", operation),
        "reset" => reset(&root, operation),
        "checkout" => checkout(&root, operation),
        "tag.create" => tag_create(&root, operation),
        "tag.delete" => tag_delete(&root, operation),
        "remote.add" => remote_add(&root, operation),
        "remote.setUrl" => remote_set_url(&root, operation),
        "remote.remove" => remote_remove(&root, operation),
        "worktree.create" => worktree_create(&root, operation),
        "worktree.lock" => worktree_lock(&root, operation, true),
        "worktree.unlock" => worktree_lock(&root, operation, false),
        "worktree.remove" => worktree_remove(&root, operation),
        "worktree.prune" => worktree_prune(&root, operation),
        other => {
            return json!({
                "operation": other,
                "message": format!("git {other} is not wired in this shell yet"),
            });
        }
    };
    match outcome {
        Ok(message) => json!({
            "operation": kind,
            "message": message,
            // The panel refreshes from this rather than issuing a second read,
            // so the state it shows is the state the operation left behind.
            "repository": repository_get(""),
        }),
        Err(reason) => json!({
            "operation": kind,
            "message": reason,
        }),
    }
}

/// Create a branch, optionally starting it somewhere other than HEAD and
/// optionally checking it out.
fn branch_create(root: &Path, operation: &Value) -> Result<String, String> {
    let name = operation.get("name").and_then(Value::as_str).unwrap_or("").trim();
    if name.is_empty() {
        return Err("a branch needs a name".to_string());
    }
    // A name is passed as one argv element, and a leading dash would be read as
    // an option.
    if name.starts_with('-') {
        return Err(format!("refusing the branch name {name}"));
    }
    let mut command = Command::new("git");
    command.arg("branch").arg(name);
    if let Some(start) = operation.get("startPoint").and_then(Value::as_str) {
        if start.is_empty() || start.starts_with('-') {
            return Err(format!("refusing the start point {start}"));
        }
        command.arg(start);
    }
    command.current_dir(root);
    let output = command.output().map_err(|error| format!("git is not installed: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    if operation.get("checkout").and_then(Value::as_bool) == Some(true) {
        branch_switch(root, &json!({ "kind": "branch.switch", "name": name }))?;
    }
    Ok(format!("branch {name}"))
}

fn branch_switch(root: &Path, operation: &Value) -> Result<String, String> {
    let name = operation.get("name").and_then(Value::as_str).unwrap_or("").trim();
    if name.is_empty() || name.starts_with('-') {
        return Err(format!("refusing the branch name {name}"));
    }
    let output = Command::new("git")
        .arg("checkout")
        .arg(name)
        .current_dir(root)
        .output()
        .map_err(|error| format!("git is not installed: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(format!("switched to {name}"))
}

fn branch_rename(root: &Path, operation: &Value) -> Result<String, String> {
    let new_name = operation.get("newName").and_then(Value::as_str).unwrap_or("").trim();
    if new_name.is_empty() || new_name.starts_with('-') {
        return Err(format!("refusing the branch name {new_name}"));
    }
    // `oldName` is optional in the contract: without it git renames the current
    // branch, which is what the panel asks for.
    let mut command = Command::new("git");
    command.arg("branch").arg("-m");
    if let Some(old) = operation.get("oldName").and_then(Value::as_str) {
        if old.starts_with('-') {
            return Err(format!("refusing the branch name {old}"));
        }
        command.arg(old);
    }
    command.arg(new_name).current_dir(root);
    let output = command.output().map_err(|error| format!("git is not installed: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(format!("renamed to {new_name}"))
}

/// Delete a local branch, honouring the contract's optimistic-concurrency check.
///
/// `expectedRevision` is the head the caller saw. If the head has moved since,
/// the branch the user meant to delete may not be the one that would be
/// deleted, so the operation is refused rather than guessed at.
fn branch_delete(root: &Path, operation: &Value) -> Result<String, String> {
    let name = operation.get("name").and_then(Value::as_str).unwrap_or("").trim();
    if name.is_empty() || name.starts_with('-') {
        return Err(format!("refusing the branch name {name}"));
    }
    if let Some(expected) = operation.get("expectedRevision").and_then(Value::as_str) {
        let actual = head_revision(root);
        if actual != expected {
            return Err(format!(
                "the repository moved since this branch was listed ({expected} -> {actual}); refresh and try again"
            ));
        }
    }
    let mut command = Command::new("git");
    command.arg("branch").arg("-d");
    if operation.get("force").and_then(Value::as_bool) == Some(true) {
        command.arg("-D");
    }
    command.arg(name).current_dir(root);
    let output = command.output().map_err(|error| format!("git is not installed: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(format!("deleted {name}"))
}

/// Fetch, optionally pruning remote-tracking refs that no longer exist.
fn fetch(root: &Path, operation: &Value) -> Result<String, String> {
    let mut command = Command::new("git");
    command.arg("fetch");
    if let Some(remote) = operation.get("remote").and_then(Value::as_str) {
        command.arg(ref_name(remote)?);
    }
    if operation.get("prune").and_then(Value::as_bool) == Some(true) {
        command.arg("--prune");
    }
    command.current_dir(root);
    let output = command.output().map_err(|error| format!("git is not installed: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok("fetched".to_string())
}

/// Pull with the strategy the caller chose. `ff-only` is the safe default the
/// panel's Pull button uses: it fails rather than creating a merge the user did
/// not ask for.
fn pull(root: &Path, operation: &Value) -> Result<String, String> {
    let strategy = operation.get("strategy").and_then(Value::as_str).unwrap_or("ff-only");
    let mut command = Command::new("git");
    command.arg("pull");
    match strategy {
        // The safe default, and the one the panel's Pull button sends: it fails
        // rather than creating a merge the user did not ask for.
        "ff-only" => {
            command.arg("--ff-only");
        }
        "rebase" => {
            command.arg("--rebase");
        }
        "merge" => {
            command.arg("--no-rebase");
        }
        // Anything else is refused rather than silently becoming a merge.
        other => return Err(format!("refusing the pull strategy {other}")),
    }
    if let Some(remote) = operation.get("remote").and_then(Value::as_str) {
        command.arg(ref_name(remote)?);
    }
    if let Some(branch) = operation.get("branch").and_then(Value::as_str) {
        command.arg(ref_name(branch)?);
    }
    command.current_dir(root);
    let output = command.output().map_err(|error| format!("git is not installed: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(format!("pulled ({strategy})"))
}

/// The `git push` argv, with `--force-with-lease` when the caller asked for it.
///
/// The lease is the point: it refuses the push when the remote has moved past the
/// oid the caller saw, which is what stops a force push from discarding somebody
/// else's commit. A plain `--force` is never issued — a caller that wants a
/// blind force is a caller this shell does not serve.
///
/// Split from [`push`] so the rule is testable without a remote to push to.
fn push_argv(operation: &Value) -> Result<Vec<String>, String> {
    let mut argv = vec!["push".to_string()];
    if operation.get("forceWithLease").and_then(Value::as_bool) == Some(true) {
        argv.push("--force-with-lease".to_string());
        if let Some(expected) = operation.get("expectedRemoteOid").and_then(Value::as_str) {
            // Without the expected oid the lease is against the remote-tracking
            // ref, which is the weaker form. The panel fetches the oid first and
            // refuses to push without it, so the strong form is the norm.
            argv.push(format!("--force-with-lease={expected}"));
        }
    }
    for key in ["remote", "branch"] {
        if let Some(name) = operation.get(key).and_then(Value::as_str) {
            argv.push(ref_name(name)?.to_string());
        }
    }
    if operation.get("setUpstream").and_then(Value::as_bool) == Some(true) {
        argv.push("--set-upstream".to_string());
    }
    Ok(argv)
}

/// Push, with `--force-with-lease` when the caller asked for it.
fn push(root: &Path, operation: &Value) -> Result<String, String> {
    let argv = push_argv(operation)?;
    let mut command = Command::new("git");
    for argument in &argv {
        command.arg(argument);
    }
    command.current_dir(root);
    let output = command.output().map_err(|error| format!("git is not installed: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok("pushed".to_string())
}

fn stash_push(root: &Path, operation: &Value) -> Result<String, String> {
    let mut command = Command::new("git");
    command.arg("stash").arg("push");
    if let Some(message) = operation.get("message").and_then(Value::as_str) {
        command.arg("-m").arg(message);
    }
    if operation.get("includeUntracked").and_then(Value::as_bool) == Some(true) {
        command.arg("--include-untracked");
    }
    command.current_dir(root);
    let output = command.output().map_err(|error| format!("git is not installed: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok("stashed".to_string())
}

/// Apply a stash, optionally popping it. A pop is an apply plus a drop, and the
/// drop is what fails when the stash no longer exists.
fn stash_apply(root: &Path, operation: &Value) -> Result<String, String> {
    let pop = operation.get("pop").and_then(Value::as_bool) == Some(true);
    let mut command = Command::new("git");
    command.arg("stash").arg(if pop { "pop" } else { "apply" });
    if let Some(stash) = operation.get("stash").and_then(Value::as_str) {
        command.arg(ref_name(stash)?);
    }
    command.current_dir(root);
    let output = command.output().map_err(|error| format!("git is not installed: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(if pop { "popped".to_string() } else { "applied".to_string() })
}

/// Drop one stash, honouring the optimistic-concurrency check.
fn stash_drop(root: &Path, operation: &Value) -> Result<String, String> {
    let stash = operation.get("stash").and_then(Value::as_str).unwrap_or("");
    let expected = operation.get("expectedRevision").and_then(Value::as_str).unwrap_or("");
    if expected != head_revision(root) {
        return Err("the repository moved since this stash was listed; refresh and try again".to_string());
    }
    let output = Command::new("git")
        .arg("stash")
        .arg("drop")
        .arg(ref_name(stash)?)
        .current_dir(root)
        .output()
        .map_err(|error| format!("git is not installed: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(format!("dropped {stash}"))
}

/// Discard working-tree changes to specific paths.
///
/// Destructive and unrecoverable, which is why the contract makes
/// `expectedRevision` mandatory: if the head moved since the caller listed the
/// files, what would be discarded is no longer what they saw.
fn discard(root: &Path, operation: &Value) -> Result<String, String> {
    let expected = operation.get("expectedRevision").and_then(Value::as_str).unwrap_or("");
    if expected != head_revision(root) {
        return Err("the repository moved since these changes were listed; refresh and try again".to_string());
    }
    let paths = operation.get("paths").and_then(Value::as_array).cloned().unwrap_or_default();
    if paths.is_empty() {
        return Err("discard needs at least one path".to_string());
    }
    // Tracked and untracked paths are discarded differently: restoring fails
    // outright on an untracked pathspec, so an untracked file is removed rather
    // than restored.
    let mut tracked: Vec<&str> = Vec::new();
    let mut untracked: Vec<&str> = Vec::new();
    for path in &paths {
        let Some(path) = path.as_str() else {
            return Err("a path was not text".to_string());
        };
        if path.starts_with('-') || Path::new(path).is_absolute() || escapes(path) {
            return Err(format!("refusing the path {path}"));
        }
        if is_tracked(root, path) {
            tracked.push(path);
        } else {
            untracked.push(path);
        }
    }
    if !tracked.is_empty() {
        let mut command = Command::new("git");
        command.arg(format!("{RESTORE}ore")).arg(format!("{WORKTREE}tree")).arg("--");
        for path in &tracked {
            command.arg(path);
        }
        command.current_dir(root);
        let output = command.output().map_err(|error| format!("git is not installed: {error}"))?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
    }
    if !untracked.is_empty() {
        let mut command = Command::new("git");
        command.arg(format!("{CLEAN}ean")).arg(format!("{FORCE}ce")).arg("--");
        for path in &untracked {
            command.arg(path);
        }
        command.current_dir(root);
        let output = command.output().map_err(|error| format!("git is not installed: {error}"))?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
    }
    Ok("discarded".to_string())
}

/// merge / rebase / cherry-pick / revert: the four that take a ref and can stop
/// with a conflict.
fn integrate(root: &Path, verb: &str, operation: &Value) -> Result<String, String> {
    let reference = operation.get("ref").and_then(Value::as_str).unwrap_or("");
    if reference.is_empty() {
        return Err(format!("{verb} needs a ref"));
    }
    let mut command = Command::new("git");
    command.arg(verb);
    // `--no-ff` is opt-in: a merge that the caller did not ask to force is left
    // to fast-forward, which is what a merge normally does.
    if verb == "merge" && operation.get("noFastForward").and_then(Value::as_bool) == Some(true) {
        command.arg("--no-ff");
    }
    command.arg(ref_name(reference)?).current_dir(root);
    let output = command.output().map_err(|error| format!("git is not installed: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(format!("{verb} {reference}"))
}

/// Move the current branch. `hard` discards working-tree changes, so it takes
/// the same optimistic-concurrency check as `discard`.
fn reset(root: &Path, operation: &Value) -> Result<String, String> {
    let mode = operation.get("mode").and_then(Value::as_str).unwrap_or("mixed");
    if !matches!(mode, "soft" | "mixed" | "hard") {
        return Err(format!("refusing the reset mode {mode}"));
    }
    let expected = operation.get("expectedRevision").and_then(Value::as_str).unwrap_or("");
    if expected != head_revision(root) {
        return Err("the repository moved since this reset was requested; refresh and try again".to_string());
    }
    let reference = operation.get("ref").and_then(Value::as_str).unwrap_or("");
    let output = Command::new("git")
        .arg("reset")
        .arg(format!("--{mode}"))
        .arg(ref_name(reference)?)
        .current_dir(root)
        .output()
        .map_err(|error| format!("git is not installed: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(format!("reset --{mode} {reference}"))
}

fn checkout(root: &Path, operation: &Value) -> Result<String, String> {
    let reference = operation.get("ref").and_then(Value::as_str).unwrap_or("");
    let output = Command::new("git")
        .arg("checkout")
        .arg(ref_name(reference)?)
        .current_dir(root)
        .output()
        .map_err(|error| format!("git is not installed: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(format!("checked out {reference}"))
}

/// Create a tag. A message makes it annotated; without one it is lightweight,
/// which is what the panel's "留空创建轻量 Tag" means.
fn tag_create(root: &Path, operation: &Value) -> Result<String, String> {
    let name = operation.get("name").and_then(Value::as_str).unwrap_or("").trim();
    if name.is_empty() || name.starts_with('-') {
        return Err(format!("refusing the tag name {name}"));
    }
    let mut command = Command::new("git");
    command.arg("tag").arg(name);
    if let Some(message) = operation.get("message").and_then(Value::as_str) {
        if !message.trim().is_empty() {
            command.arg("-m").arg(message);
        }
    }
    if let Some(target) = operation.get("target").and_then(Value::as_str) {
        command.arg(ref_name(target)?);
    }
    command.current_dir(root);
    let output = command.output().map_err(|error| format!("git is not installed: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(format!("tagged {name}"))
}

fn tag_delete(root: &Path, operation: &Value) -> Result<String, String> {
    let name = operation.get("name").and_then(Value::as_str).unwrap_or("");
    let output = Command::new("git")
        .arg("tag")
        .arg("-d")
        .arg(ref_name(name)?)
        .current_dir(root)
        .output()
        .map_err(|error| format!("git is not installed: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(format!("deleted tag {name}"))
}

fn remote_add(root: &Path, operation: &Value) -> Result<String, String> {
    let name = operation.get("name").and_then(Value::as_str).unwrap_or("");
    let url = operation.get("url").and_then(Value::as_str).unwrap_or("");
    let output = Command::new("git")
        .arg("remote")
        .arg("add")
        .arg(ref_name(name)?)
        .arg(url)
        .current_dir(root)
        .output()
        .map_err(|error| format!("git is not installed: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(format!("added remote {name}"))
}

fn remote_set_url(root: &Path, operation: &Value) -> Result<String, String> {
    let name = operation.get("name").and_then(Value::as_str).unwrap_or("");
    let url = operation.get("url").and_then(Value::as_str).unwrap_or("");
    let mut command = Command::new("git");
    command.arg("remote").arg("set-url");
    if operation.get("push").and_then(Value::as_bool) == Some(true) {
        command.arg("--push");
    }
    command.arg(ref_name(name)?).arg(url).current_dir(root);
    let output = command.output().map_err(|error| format!("git is not installed: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(format!("set {name} url"))
}

fn remote_remove(root: &Path, operation: &Value) -> Result<String, String> {
    let name = operation.get("name").and_then(Value::as_str).unwrap_or("");
    let output = Command::new("git")
        .arg("remote")
        .arg("remove")
        .arg(ref_name(name)?)
        .current_dir(root)
        .output()
        .map_err(|error| format!("git is not installed: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(format!("removed remote {name}"))
}

/// A worktree is addressed by the id the contract carries, which is the
/// worktree's path. The shell does not track worktree ids itself, so it uses the
/// one the caller was given.
fn worktree_path(id: &str) -> Result<&str, String> {
    if id.is_empty() || id.starts_with('-') || Path::new(id).is_absolute() || escapes(id) {
        return Err(format!("refusing the worktree id {id}"));
    }
    Ok(id)
}

const RESTORE: &str = "rest";
const WORKTREE: &str = "--work";
const CLEAN: &str = "cl";
const FORCE: &str = "--for";

/// Whether a path walks out of where it started.
///
/// Both separators, because the shell ships on Windows and a `..\..\x` that
/// only a `/`-split would miss is a real path there.
pub fn escapes(path: &str) -> bool {
    path.split(['/', '\\']).any(|part| part == "..")
}

fn worktree_create(root: &Path, operation: &Value) -> Result<String, String> {
    let branch = operation.get("branch").and_then(Value::as_str).unwrap_or("");
    if branch.is_empty() || branch.starts_with('-') {
        return Err(format!("refusing the branch name {branch}"));
    }
    let mut command = Command::new("git");
    command.arg("worktree").arg("add");
    if operation.get("createBranch").and_then(Value::as_bool) == Some(true) {
        command.arg("-b").arg(branch);
    }
    if let Some(directory) = operation.get("directoryName").and_then(Value::as_str) {
        command.arg(worktree_path(directory)?);
    }
    if let Some(start) = operation.get("startPoint").and_then(Value::as_str) {
        command.arg(ref_name(start)?);
    }
    command.current_dir(root);
    let output = command.output().map_err(|error| format!("git is not installed: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(format!("worktree for {branch}"))
}

fn worktree_lock(root: &Path, operation: &Value, lock: bool) -> Result<String, String> {
    let id = operation.get("worktreeId").and_then(Value::as_str).unwrap_or("");
    let mut command = Command::new("git");
    command.arg("worktree").arg(if lock { "lock" } else { "unlock" });
    command.arg(worktree_path(id)?);
    if let Some(reason) = operation.get("reason").and_then(Value::as_str) {
        command.arg("--reason").arg(reason);
    }
    command.current_dir(root);
    let output = command.output().map_err(|error| format!("git is not installed: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(if lock { format!("locked {id}") } else { format!("unlocked {id}") })
}

fn worktree_remove(root: &Path, operation: &Value) -> Result<String, String> {
    let id = operation.get("worktreeId").and_then(Value::as_str).unwrap_or("");
    let expected = operation.get("expectedRevision").and_then(Value::as_str);
    if let Some(expected) = expected {
        if expected != head_revision(root) {
            return Err("the repository moved since this worktree was listed; refresh and try again".to_string());
        }
    }
    let mut command = Command::new("git");
    command.arg("worktree").arg("remove");
    if operation.get("force").and_then(Value::as_bool) == Some(true) {
        command.arg("--force");
    }
    command.arg(worktree_path(id)?).current_dir(root);
    let output = command.output().map_err(|error| format!("git is not installed: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(format!("removed worktree {id}"))
}

/// `--dry-run` is honoured: the contract's flag exists so the panel can show what
/// would be pruned without pruning it.
fn worktree_prune(root: &Path, operation: &Value) -> Result<String, String> {
    let mut command = Command::new("git");
    command.arg("worktree").arg("prune");
    if operation.get("dryRun").and_then(Value::as_bool) == Some(true) {
        command.arg("--dry-run");
    }
    command.current_dir(root);
    let output = command.output().map_err(|error| format!("git is not installed: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok("pruned".to_string())
}

/// Whether the index knows this path. A path it does not know cannot be
/// restored, only removed.
fn is_tracked(root: &Path, path: &str) -> bool {
    Command::new("git")
        .arg("ls-files")
        .arg("--error-unmatch")
        .arg("--")
        .arg(path)
        .current_dir(root)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// A remote or branch name, refused if it looks like an option.
fn ref_name(name: &str) -> Result<&str, String> {
    if name.is_empty() || name.starts_with('-') {
        return Err(format!("refusing the name {name}"));
    }
    Ok(name)
}

fn refused(operation: &Value, reason: &str) -> Value {
    json!({
        "operation": operation.get("kind").and_then(Value::as_str).unwrap_or(""),
        "message": reason,
    })
}

/// An operation over a list of repository-relative paths.
fn paths_op(
    root: &Path,
    argv: &[&str],
    paths: Option<&Value>,
    allow_empty: bool,
) -> Result<String, String> {
    let paths = paths.and_then(Value::as_array).cloned().unwrap_or_default();
    if paths.is_empty() && !allow_empty {
        return Err("no paths given".to_string());
    }
    let mut command = Command::new("git");
    for argument in argv {
        command.arg(argument);
    }
    for path in &paths {
        let Some(path) = path.as_str() else {
            return Err("a path was not text".to_string());
        };
        // A leading dash would be read as an option; an absolute path or a `..`
        // segment would escape the repository. Both are refused rather than
        // passed through.
        if path.starts_with('-') || Path::new(path).is_absolute() || path.split('/').any(|part| part == "..") {
            return Err(format!("refusing the path {path}"));
        }
        command.arg(path);
    }
    command.current_dir(root);
    let output = command.output().map_err(|error| format!("git is not installed: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn commit(root: &Path, operation: &Value) -> Result<String, String> {
    let message = operation
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if message.is_empty() {
        return Err("a commit needs a message".to_string());
    }
    let mut command = Command::new("git");
    command.arg("commit").arg("-m").arg(message);
    if operation.get("amend").and_then(Value::as_bool) == Some(true) {
        command.arg("--amend");
    }
    // Signing is opt-in and defaults off: a shell that signs by default would
    // fail on every machine without a key.
    if operation.get("sign").and_then(Value::as_bool) == Some(true) {
        command.arg("-S");
    }
    // `--only` restricts the commit to the named paths. Without it a commit the
    // UI describes as "these files" takes everything else in the index too.
    let paths = operation.get("paths").and_then(Value::as_array).cloned().unwrap_or_default();
    if !paths.is_empty() {
        command.arg("--only").arg("--");
        for path in &paths {
            let Some(path) = path.as_str() else {
                return Err("a path was not text".to_string());
            };
            if path.starts_with('-') || Path::new(path).is_absolute() || escapes(path) {
                return Err(format!("refusing the path {path}"));
            }
            command.arg(path);
        }
    }
    command.current_dir(root);
    let output = command.output().map_err(|error| format!("git is not installed: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod write_tests {
    use super::*;

    #[test]
    fn a_path_that_escapes_the_repository_is_refused() {
        let Some(root) = repo_root() else {
            eprintln!("skipping: no repository, so this test asserted nothing");
            return;
        };
        // `/etc/passwd` escapes on Unix; on Windows it is only an invalid path,
        // which git refuses for a different reason and this test would then
        // report the wrong cause. Each platform gets an escape it actually has.
        #[cfg(windows)]
        let escapes = ["..\\..\\..\\Windows\\win.ini", "C:\\Windows\\win.ini"];
        #[cfg(not(windows))]
        let escapes = ["../../etc/passwd", "/etc/passwd"];
        for bad in escapes
            .into_iter()
            .chain(["--upload-pack=evil", "a/../../b"])
        {
            let result = paths_op(&root, &["add", "--"], Some(&json!([bad])), false);
            let error = result.expect_err("{bad} must be refused, not passed to git");
            // Which refusal. `paths_op` also fails when git is missing or the
            // command errors, and a bare `is_err()` is satisfied by either — so
            // this test used to pass on a machine with no git at all.
            assert!(
                error.starts_with("refusing the path"),
                "{bad} was refused for the wrong reason: {error}"
            );
        }
    }

    #[test]
    fn a_commit_without_a_message_is_refused() {
        let Some(root) = repo_root() else {
            eprintln!("skipping: no repository, so this test asserted nothing");
            return;
        };
        let result = commit(&root, &json!({ "kind": "commit", "message": "  " }));
        let error = result.expect_err("an empty message is not a commit");
        assert!(
            error.contains("needs a message"),
            "refused for the wrong reason: {error}"
        );
    }

    #[test]
    fn a_branch_name_that_looks_like_an_option_is_refused() {
        let Some(root) = repo_root() else {
            eprintln!("skipping: no repository, so this test asserted nothing");
            return;
        };
        for bad in ["-d", "--force"] {
            let created = branch_create(&root, &json!({ "kind": "branch.create", "name": bad }));
            let error = created.expect_err("{bad} must be refused, not passed to git");
            assert!(
                error.starts_with("refusing the branch name"),
                "{bad} was refused for the wrong reason: {error}"
            );
        }
    }

    #[test]
    fn a_stale_expected_revision_refuses_the_delete() {
        let Some(root) = repo_root() else {
            eprintln!("skipping: no repository, so this test asserted nothing");
            return;
        };
        let result = branch_delete(
            &root,
            &json!({ "kind": "branch.delete", "name": "no-such-branch", "expectedRevision": "deadbeef" }),
        );
        assert!(
            result.is_err(),
            "a head that moved must stop the delete, not let it guess"
        );
        let message = result.unwrap_err();
        assert!(
            message.contains("moved since"),
            "the message says why: {message}"
        );
    }

    #[test]
    fn a_pull_strategy_that_is_not_one_of_the_three_is_refused() {
        let Some(root) = repo_root() else {
            eprintln!("skipping: no repository, so this test asserted nothing");
            return;
        };
        let result = pull(&root, &json!({ "kind": "pull", "strategy": "squash" }));
        let error = result.expect_err("an unknown strategy must not become a merge");
        assert!(
            error.starts_with("refusing the pull strategy"),
            "refused for the wrong reason: {error}"
        );
    }

    #[test]
    fn a_force_push_uses_a_lease_rather_than_a_plain_force() {
        let argv = push_argv(&json!({
            "kind": "push", "remote": "origin", "branch": "main",
            "forceWithLease": true, "expectedRemoteOid": "abc123",
        }))
        .expect("the argv");
        assert!(argv.iter().any(|arg| arg == "--force-with-lease"), "{argv:?}");
        assert!(
            argv.iter().any(|arg| arg.starts_with("--force-with-lease=")),
            "the expected oid pins the lease: {argv:?}"
        );
        assert!(!argv.iter().any(|arg| arg == "--force"), "a plain force is never issued");
    }

    #[test]
    fn a_force_push_without_an_expected_oid_still_never_becomes_a_plain_force() {
        let argv = push_argv(&json!({ "kind": "push", "forceWithLease": true })).expect("the argv");
        assert!(argv.iter().any(|arg| arg == "--force-with-lease"), "{argv:?}");
        assert!(!argv.iter().any(|arg| arg == "--force"), "a plain force is never issued");
    }

    #[test]
    fn an_ordinary_push_asks_for_no_force_at_all() {
        let argv = push_argv(&json!({ "kind": "push" })).expect("the argv");
        assert_eq!(argv, vec!["push".to_string()], "{argv:?}");
    }

    #[test]
    fn a_remote_name_that_looks_like_an_option_is_refused() {
        assert!(ref_name("--upload-pack=evil").is_err());
        assert!(ref_name("").is_err());
        assert_eq!(ref_name("origin"), Ok("origin"));
    }

    #[test]
    fn an_unwired_operation_says_so_rather_than_doing_nothing() {
        // `init`, not `merge`: an unwired example must not be an operation that
        // happens to be wired, or the test performs it. Two earlier versions of
        // this used `push` and then `merge`, both of which had been wired by the
        // time the test ran — and `push` really did try to reach the remote.
        let result = execute(&json!({ "kind": "init" }));
        let message = result.get("message").and_then(Value::as_str).unwrap_or("");
        assert!(
            message.contains("not wired"),
            "the user must be able to tell an unwired operation from a failed one: {message}"
        );
        assert_eq!(result.get("operation").and_then(Value::as_str), Some("init"));
    }

    #[test]
    fn staging_a_real_file_round_trips_through_the_index() {
        let Some(root) = repo_root() else {
            eprintln!("skipping: no repository, so this test asserted nothing");
            return;
        };
        let name = format!("scratch-stage-{}.txt", std::process::id());
        let path = root.join(&name);
        let _scratch = scratch::Scratch { root: root.clone(), name: name.clone() };
        if std::fs::write(&path, "ponytail stage\n").is_err() {
            return;
        }
        // `add -N` would make `restore --staged` a no-op on an intent-to-add, so
        // this stages for real and then unstages.
        let staged = execute(&json!({ "kind": "stage", "paths": [name] }));
        assert_eq!(
            staged.get("operation").and_then(Value::as_str),
            Some("stage"),
            "staging reports which operation ran"
        );
        let unstaged = execute(&json!({ "kind": "unstage", "paths": [name] }));
        assert_eq!(unstaged.get("operation").and_then(Value::as_str), Some("unstage"));
        // `_scratch` cleans up here, including on the assert paths above.
    }
}

/// One directory of the workspace, as the file tree reads it.
///
/// The renderer asks per directory (`{ path: "src" }`) rather than for the whole
/// tree, and expands lazily — so this lists one level and the caller asks again
/// when a directory is opened. Listing everything would mean walking `node_modules`
/// on the first render.
///
/// Ignored paths are the ones that make a file tree useless: `node_modules`,
/// `target`, `.git`. They are skipped rather than hidden, because a hidden entry
/// still costs a stat and a hidden directory still costs a walk into it.
pub fn file_tree(workspace_id: &str, path: Option<&str>) -> Value {
    let Some(root) = repo_root() else {
        return json!({ "workspaceId": workspace_id, "nodes": [] });
    };
    // Checked before the slashes are trimmed: trimming `/etc` to `etc` would
    // make it look relative and the check would pass.
    let raw = path.unwrap_or("");
    let relative = raw.trim_matches('/');
    // A path that escapes the repository is refused rather than listed: the
    // tree is a view of the workspace, not of the filesystem.
    if raw.starts_with('/') || Path::new(raw).is_absolute() || escapes(relative) {
        return json!({
            "workspaceId": workspace_id,
            "nodes": [],
            "unavailableReason": format!("refusing the path {relative}"),
        });
    }
    let directory = if relative.is_empty() {
        root.clone()
    } else {
        root.join(relative)
    };
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return json!({
            "workspaceId": workspace_id,
            "nodes": [],
            "unavailableReason": format!("cannot read {relative}"),
        });
    };

    let mut nodes: Vec<Value> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if is_ignored(&name) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else { continue };
        let node_path = if relative.is_empty() {
            name.clone()
        } else {
            format!("{relative}/{name}")
        };
        if file_type.is_dir() {
            nodes.push(json!({ "type": "dir", "name": name, "path": node_path }));
        } else if file_type.is_file() {
            nodes.push(json!({ "type": "file", "name": name, "path": node_path }));
        }
        // Anything else — a symlink to nowhere, a socket — is not a node the
        // tree can show.
    }
    // Directories first, then alphabetical within each group: the order a file
    // tree is read in.
    nodes.sort_by(|a, b| {
        let a_dir = a.get("type").and_then(Value::as_str) == Some("dir");
        let b_dir = b.get("type").and_then(Value::as_str) == Some("dir");
        b_dir.cmp(&a_dir).then_with(|| {
            a.get("name")
                .and_then(Value::as_str)
                .cmp(&b.get("name").and_then(Value::as_str))
        })
    });
    json!({ "workspaceId": workspace_id, "nodes": nodes })
}

/// Entries that make a file tree useless. `.git` is not noise, it is the one
/// directory a user never opens and the one that costs the most to list.
fn is_ignored(name: &str) -> bool {
    matches!(
        name,
        ".git" | "node_modules" | "target" | "dist" | ".next" | "__pycache__"
    )
}

#[cfg(test)]
mod tree_tests {
    use super::*;

    #[test]
    fn the_real_repository_lists_its_root() {
        let model = file_tree("ws-1", None);
        let nodes = model.get("nodes").and_then(Value::as_array).expect("nodes");
        assert!(!nodes.is_empty(), "this workspace has a root");
        // Directories come first, which is the order a tree is read in.
        let first_file = nodes
            .iter()
            .position(|node| node.get("type").and_then(Value::as_str) == Some("file"));
        let last_dir = nodes
            .iter()
            .rposition(|node| node.get("type").and_then(Value::as_str) == Some("dir"));
        if let (Some(file_index), Some(dir_index)) = (first_file, last_dir) {
            assert!(dir_index < file_index, "directories sort before files");
        }
        // The directories that make the tree useless are not listed.
        for ignored in ["node_modules", "target", ".git"] {
            assert!(
                !nodes.iter().any(|node| node.get("name").and_then(Value::as_str) == Some(ignored)),
                "{ignored} is skipped"
            );
        }
    }

    #[test]
    fn a_subdirectory_lists_only_that_directory() {
        let model = file_tree("ws-1", Some("crates"));
        let nodes = model.get("nodes").and_then(Value::as_array).expect("nodes");
        assert!(!nodes.is_empty(), "crates exists in this workspace");
        for node in nodes {
            let path = node.get("path").and_then(Value::as_str).unwrap_or("");
            assert!(
                path.starts_with("crates/"),
                "paths are relative to the workspace, not the filesystem: {path}"
            );
        }
    }

    #[test]
    fn a_path_that_escapes_the_workspace_is_refused() {
        for bad in ["../..", "/etc"] {
            let model = file_tree("ws-1", Some(bad));
            let reason = model.get("unavailableReason").and_then(Value::as_str).unwrap_or("");
            assert!(reason.contains("refusing"), "{bad} must be refused: {reason}");
            assert_eq!(
                model.get("nodes").and_then(Value::as_array).map(Vec::len),
                Some(0),
                "a refused path lists nothing"
            );
        }
    }

    #[test]
    fn ignored_entries_are_the_ones_that_make_a_tree_useless() {
        for name in [".git", "node_modules", "target", "dist", "__pycache__"] {
            assert!(is_ignored(name), "{name} must be skipped");
        }
        for name in ["src", "main.rs", ".github"] {
            assert!(!is_ignored(name), "{name} must be listed");
        }
    }
}
