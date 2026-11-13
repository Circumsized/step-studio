//! The daemon's own process tree.
//!
//! This is the only place the process tree can come from. The daemon is started
//! with `process_group(0)` — its own session — so it is reparented to init when
//! the shell exits, which is exactly what makes a shell restart survivable. The
//! consequence is that a shell walking `/proc` for itself never sees the daemon,
//! the adapter, or the CLI behind it. So the tree has to be reported by the
//! process that owns it.
//!
//! Unix reads `/proc`. Windows has no equivalent in `std`, and the toolhelp
//! snapshot API it would need is not reachable without `unsafe` — so there the
//! tree is the daemon plus whatever children it can name, and nothing more. The
//! distinction is in the read model rather than hidden: a process the platform
//! cannot describe is absent, and the caller is told the tree is partial.

use serde_json::{json, Value};

/// Unix milliseconds, for the read model's `generatedAt`.
pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// RFC 3339 in UTC, second precision.
pub fn iso_utc(millis: u64) -> String {
    let secs = millis / 1000;
    let days = secs / 86_400;
    let time_of_day = secs % 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time_of_day / 3600,
        (time_of_day % 3600) / 60,
        time_of_day % 60
    )
}

/// Days since the epoch to a calendar date, Howard Hinnant's algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (year + if month <= 2 { 1 } else { 0 }, month, day)
}

/// What one process in the tree is.
///
/// Derived from the parent chain rather than from a name: the daemon is the
/// root, its direct child is the adapter, and everything below that is the CLI
/// driving the model. Naming a process from its executable would be a guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Daemon,
    Adapter,
    Cli,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Self::Daemon => "daemon",
            Self::Adapter => "adapter",
            Self::Cli => "cli",
        }
    }
}

/// One process, as the panel reads it.
#[derive(Debug, Clone)]
pub struct Process {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    pub role: Role,
    /// Resident set size in bytes, where the platform reports it.
    pub rss_bytes: Option<u64>,
}

/// The tree the daemon can see, rooted at itself.
pub fn tree() -> Vec<Process> {
    let me = std::process::id();
    let mut processes = vec![Process {
        pid: me,
        parent_pid: parent_of(me),
        role: Role::Daemon,
        rss_bytes: rss_of(me),
    }];
    // Breadth-first from the daemon, so the depth that decides the role is
    // already known when a process is reached.
    let mut frontier = vec![me];
    let mut depth = 0;
    while !frontier.is_empty() && depth < 8 {
        let mut next = Vec::new();
        for parent in &frontier {
            for child in children_of(*parent) {
                if processes.iter().any(|process| process.pid == child) {
                    continue;
                }
                let role = if depth == 0 { Role::Adapter } else { Role::Cli };
                processes.push(Process {
                    pid: child,
                    parent_pid: Some(*parent),
                    role,
                    rss_bytes: rss_of(child),
                });
                next.push(child);
            }
        }
        frontier = next;
        depth += 1;
    }
    processes
}

/// The read model the contract asks for.
pub fn read_model() -> Value {
    let processes = tree();
    let total = processes
        .iter()
        .filter_map(|process| process.rss_bytes)
        .sum::<u64>();
    json!({
        "generatedAt": iso_utc(now_millis()),
        "selfPid": std::process::id(),
        "processes": processes
            .iter()
            .map(|process| {
                json!({
                    "pid": process.pid,
                    "parentPid": process.parent_pid,
                    "role": process.role.as_str(),
                    "rssBytes": process.rss_bytes,
                })
            })
            .collect::<Vec<_>>(),
        "totalRssBytes": total,
        // Where the platform could not describe a process, its rss is absent and
        // the total is short. Saying so is better than a total that looks exact.
        "complete": cfg!(unix),
    })
}

/// Whether `/proc` is the source, which is the only place this is exact.
pub fn is_exact() -> bool {
    cfg!(unix)
}

#[cfg(unix)]
fn parent_of(pid: u32) -> Option<u32> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    parse_status_field(&status, "PPid:").map(|ppid| ppid as u32)
}

#[cfg(unix)]
fn children_of(pid: u32) -> Vec<u32> {
    // `/proc/<pid>/task/<tid>/children` is the kernel's own answer, so a process
    // that forked and has not yet exec'd is still found.
    let Ok(task) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return Vec::new();
    };
    let mut found: Vec<u32> = Vec::new();
    for entry in task.flatten() {
        let Ok(text) = std::fs::read_to_string(entry.path().join("children")) else {
            continue;
        };
        for field in text.split_whitespace() {
            if let Ok(child) = field.parse::<u32>() {
                if !found.contains(&child) {
                    found.push(child);
                }
            }
        }
    }
    found
}

#[cfg(unix)]
fn rss_of(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    // `VmRSS:  6772 kB` — kilobytes, so scaled to bytes.
    parse_status_field(&status, "VmRSS:").map(|kb| kb * 1024)
}

#[cfg(unix)]
fn parse_status_field(status: &str, key: &str) -> Option<u64> {
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            return rest
                .split_whitespace()
                .next()
                .and_then(|value| value.parse().ok());
        }
    }
    None
}

/// No toolhelp snapshot without `unsafe`, so only the daemon itself can be
/// described. The children it spawned are known by pid from `Child`, but their
/// own children are not reachable.
#[cfg(not(unix))]
fn parent_of(_pid: u32) -> Option<u32> {
    None
}

#[cfg(not(unix))]
fn children_of(_pid: u32) -> Vec<u32> {
    Vec::new()
}

#[cfg(not(unix))]
fn rss_of(_pid: u32) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tree_is_rooted_at_the_daemon() {
        let processes = tree();
        assert!(
            !processes.is_empty(),
            "the daemon itself is always in the tree"
        );
        let root = &processes[0];
        assert_eq!(root.pid, std::process::id(), "the root is this process");
        assert_eq!(root.role, Role::Daemon);
    }

    #[test]
    fn a_spawned_child_is_named_the_adapter() {
        // The role comes from the depth in the tree, not from the executable's
        // name — a guess about what a binary is would be wrong the moment it is
        // renamed.
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn a child");
        let processes = tree();
        let found = processes
            .iter()
            .find(|process| process.pid == child.id())
            .expect("the spawned child is in the tree");
        assert_eq!(
            found.role,
            Role::Adapter,
            "a direct child of the daemon is the adapter"
        );
        assert_eq!(found.parent_pid, Some(std::process::id()));

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn a_grandchild_is_named_the_cli() {
        // Two commands, not one: `sh -c 'sleep 30'` execs the single command in
        // place, so there is no grandchild at all and the test would be vacuous.
        // A shell with two things to do has to fork for one of them.
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("sleep 30; sleep 30")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn a shell");
        // The shell has to fork before there is a grandchild to find; walking
        // immediately after the spawn races it. The first child is there at once,
        // which is why the adapter test needs no wait.
        std::thread::sleep(std::time::Duration::from_millis(300));
        let processes = tree();
        assert!(
            processes.iter().any(|process| process.role == Role::Cli),
            "something below the direct child is the CLI"
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn the_read_model_carries_every_process_and_a_total() {
        let model = read_model();
        assert_eq!(
            model.get("selfPid").and_then(Value::as_u64),
            Some(u64::from(std::process::id()))
        );
        let processes = model
            .get("processes")
            .and_then(Value::as_array)
            .expect("processes");
        assert!(!processes.is_empty());
        for process in processes {
            assert!(process.get("pid").and_then(Value::as_u64).is_some());
            assert!(process.get("role").and_then(Value::as_str).is_some());
        }
        // The total is the sum of what each process reported, so it is short by
        // exactly what the platform could not describe — never invented.
        let summed: u64 = processes
            .iter()
            .filter_map(|process| process.get("rssBytes").and_then(Value::as_u64))
            .sum();
        assert_eq!(
            model.get("totalRssBytes").and_then(Value::as_u64),
            Some(summed)
        );
    }

    #[test]
    fn the_model_says_whether_the_platform_could_describe_everything() {
        let model = read_model();
        // On Unix `/proc` is the source and the tree is exact; elsewhere it is
        // not, and the flag says so rather than letting a short total look whole.
        assert_eq!(
            model.get("complete").and_then(Value::as_bool),
            Some(cfg!(unix))
        );
    }
}
