//! The shell's session surface: what Step has run, and which project is active.
//!
//! The store reading itself lives in `step-studio-sessions`, because the TUI is
//! a second surface of the daemon and reads the same store — two copies of that
//! reader would drift, and the drift would be silent, each rendering a confident
//! and differently-wrong session list. What is left here is the part that is
//! about *this shell*: the active-project registry, and the directory picker.

use std::path::Path;

use serde_json::{json, Value};

/// One page of session history.
pub fn list(limit: Option<u64>, workspace_id: Option<&str>, status: Option<&str>) -> Value {
    step_studio_sessions::list(limit, workspace_id, status)
}

/// The sub-agent sessions Step keeps on disk.
pub fn agents(session_id: Option<&str>) -> Value {
    step_studio_sessions::agents(session_id)
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
    let last = text.lines().rev().find(|line| !line.trim().is_empty())?;
    serde_json::from_str::<Value>(last)
        .ok()?
        .get("timestamp")
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    let Some(store) = step_studio_sessions::store_dir() else {
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
        let Some(project) = step_studio_sessions::project_path(&path) else {
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

#[cfg(test)]
mod project_tests {
    use super::*;

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

/// Open the OS directory picker and adopt whatever it returns.
///
/// Cancellation is an error rather than an empty list, matching the reference
/// implementation (the Electron Host's workspace adapter, which throws
/// `WorkspacePickCancelledError`). The renderer keys off the failure: an empty
/// list would look like success with nothing selected, and the create-project
/// flow would report "the chosen folder was not registered as a project" for a
/// user who simply closed the dialog.
///
/// Blocking is correct here: the pump is a background thread, and the dialog
/// plugin documents this variant as the one for non-main-thread callers. The
/// pump stalls for the length of the dialog, which is what a picker is for.
///
/// `name` is accepted and ignored. The shell has no workspace registry to
/// persist a display-name override in, so the row is named after the directory
/// the way `open` names one. Carrying the create-project flow's typed name
/// needs that registry first.
// ponytail: no display-name override; add when a persisted workspace registry lands
pub fn pick(name: Option<&str>) -> Result<Value, (String, String)> {
    let _ = name;
    let Some(handle) = crate::app_handle() else {
        return Err((
            "CAPABILITY_UNAVAILABLE".to_string(),
            "workspace.pick is not available: no app handle".to_string(),
        ));
    };
    let picked = tauri_plugin_dialog::DialogExt::dialog(handle)
        .file()
        .set_title("选择项目文件夹")
        .blocking_pick_folder();
    // A content URI (Android) has no filesystem path, so there is nothing to
    // adopt; treating it as a cancellation would be a silent wrong answer.
    let Some(path) = picked.as_ref().and_then(|folder| folder.as_path()) else {
        return Err((
            "UNAVAILABLE".to_string(),
            "directory picker cancelled".to_string(),
        ));
    };
    Ok(open(&path.to_string_lossy()))
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

    #[test]
    fn a_pick_that_cannot_open_is_an_error_not_an_empty_list() {
        // The contract this pins: the renderer tells "the user closed the
        // picker" apart from "there are no projects" by the receipt's status,
        // so a pick that cannot run must fail. Returning `{"workspaces":[]}`
        // instead would look like success with nothing selected, and the
        // create-project flow would report the chosen folder as unregistered
        // for a user who only pressed Escape.
        //
        // A test binary has no app handle, which is the same shape: the dialog
        // is unreachable, so this is the unreachable arm rather than the cancel
        // arm. Both must be `Err`.
        let outcome = pick(None);
        let (code, message) = outcome.expect_err("an unreachable picker is not a successful pick");
        assert!(!code.is_empty(), "the code is what the renderer matches on");
        assert!(!message.is_empty(), "and the message is what it shows");
    }
}
