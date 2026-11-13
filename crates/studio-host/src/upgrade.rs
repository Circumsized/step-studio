//! The upgrade handoff: how an update tells a running daemon to go away *now*.
//!
//! This is the first third of §12.4's hot-replace sequence. The middle third (the
//! package swap) is [`crate::swap`]; the last third (the first surface starting the
//! new daemon) is the shell's.
//!
//! **A marker file, not a signal.** The plan targets Windows, where `SIGUSR1` does
//! not exist; a marker in the daemon's own state directory works on both and needs
//! no new protocol frame. The watch loop already polls every 250 ms, so "immediately"
//! is 250 ms, which is what the sequence means by it.
//!
//! **The marker names the daemon it is for, and that is the whole design.** An
//! upgrade writes a marker, the daemon it names consumes it and exits, the package
//! is swapped, the new daemon starts. If the named daemon dies *before* consuming —
//! which is the case nobody plans for — the marker is still there, and a new daemon
//! that honoured it would exit on startup. That is a machine whose daemon never
//! boots, caused by an update that half-finished.
//!
//! So the marker carries the daemon's instance id, and only that daemon consumes
//! it. A marker naming somebody else is stale: logged and left, so the operator can
//! see an update did not land.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// The marker's filename, inside the daemon's state directory.
const MARKER: &str = "upgrade-requested.json";

/// Where the marker lives.
pub fn marker_path(state_dir: &Path) -> PathBuf {
    state_dir.join(MARKER)
}

/// Ask the daemon `instance_id` to drain immediately.
///
/// Written by the updater, before it swaps anything. `instance_id` is what makes a
/// stale marker harmless — see the module comment.
pub fn request(state_dir: &Path, instance_id: &str) -> std::io::Result<()> {
    let payload = json!({
        "instanceId": instance_id,
        // Milliseconds, so a reader on another machine can compare it to its own
        // clock without a timezone in the way.
        "requestedAtMs": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as u64)
            .unwrap_or_default(),
    });
    let text = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());
    std::fs::create_dir_all(state_dir)?;
    std::fs::write(marker_path(state_dir), text)
}

/// What a daemon should do about the marker, given its own instance id.
#[derive(Debug, PartialEq)]
pub enum Decision {
    /// No marker at all.
    Absent,
    /// A marker naming this daemon: drain now, with the grace period forced to zero.
    Drain,
    /// A marker naming a daemon that is not this one. Stale — an update that did
    /// not land. Left on disk and reported, never consumed.
    Stale { found: String },
    /// A marker that is not readable as one. Treated as stale rather than as a
    /// drain: a file nobody can parse is not an instruction this daemon gave.
    Unreadable,
}

impl Decision {
    /// Whether the daemon should shut down right now.
    pub fn should_drain(&self) -> bool {
        matches!(self, Self::Drain)
    }

    /// The one-line reason, for the log.
    pub fn reason(&self) -> String {
        match self {
            Self::Absent => "no upgrade requested".to_string(),
            Self::Drain => "an update asked this daemon to drain now".to_string(),
            Self::Stale { found } => {
                format!("an upgrade marker names daemon {found}, which is not this one; leaving it")
            }
            Self::Unreadable => "an upgrade marker is not readable; leaving it".to_string(),
        }
    }
}

/// Read the marker and decide.
///
/// The decision is a pure function of the file's contents and this daemon's id, so
/// it can be tested without a running daemon — and the case that matters (a stale
/// marker) is exactly the one a live test would not produce on demand.
pub fn decide(state_dir: &Path, instance_id: &str) -> Decision {
    let path = marker_path(state_dir);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Decision::Absent;
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return Decision::Unreadable;
    };
    match value.get("instanceId").and_then(Value::as_str) {
        Some(found) if found == instance_id => Decision::Drain,
        Some(found) => Decision::Stale {
            found: found.to_string(),
        },
        None => Decision::Unreadable,
    }
}

/// Consume a marker that named this daemon.
///
/// Called after the decision to drain, so a crash between the two leaves the marker
/// for the *next* daemon — which will read it as stale rather than as an instruction,
/// because it names a different instance. That is the property that makes a
/// half-finished update recoverable.
pub fn consume(state_dir: &Path) {
    let _ = std::fs::remove_file(marker_path(state_dir));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("upgrade-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("make a state directory");
        dir
    }

    const ME: &str = "01a1d10e-e022-7e1e-a16b-305b34783484";
    const OTHER: &str = "01a1d10e-e022-7e1e-a16b-305b34783485";

    #[test]
    fn no_marker_means_no_drain() {
        let dir = state_dir("absent");
        assert_eq!(decide(&dir, ME), Decision::Absent);
        assert!(!decide(&dir, ME).should_drain());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_marker_naming_this_daemon_drains_it() {
        let dir = state_dir("mine");
        request(&dir, ME).expect("requested");
        let decision = decide(&dir, ME);
        assert_eq!(decision, Decision::Drain);
        assert!(decision.should_drain());
        assert!(decision.reason().contains("drain now"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_marker_naming_another_daemon_is_stale_and_is_not_consumed() {
        // The case that makes a half-finished update recoverable: the daemon the
        // update meant to stop died first, and the new one must boot rather than
        // exit. A daemon that honoured a stale marker is a machine whose daemon
        // never starts.
        let dir = state_dir("stale");
        request(&dir, OTHER).expect("requested");
        let decision = decide(&dir, ME);
        assert_eq!(
            decision,
            Decision::Stale {
                found: OTHER.to_string()
            }
        );
        assert!(!decision.should_drain(), "a stale marker must not stop this daemon");
        assert!(decision.reason().contains("not this one"));
        // And it is left on disk, so the operator can see an update did not land.
        assert!(marker_path(&dir).exists(), "a stale marker is evidence, not litter");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_marker_is_not_an_instruction() {
        // A file nobody can parse is not something this daemon asked for. Honouring
        // it would let anything writable to the state directory stop the daemon.
        let dir = state_dir("unreadable");
        std::fs::write(marker_path(&dir), b"not json at all").expect("write");
        assert_eq!(decide(&dir, ME), Decision::Unreadable);
        assert!(!decide(&dir, ME).should_drain());
        // And one with no instance id is the same answer.
        std::fs::write(marker_path(&dir), br#"{"requestedAtMs":1}"#).expect("write");
        assert_eq!(decide(&dir, ME), Decision::Unreadable);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn consuming_removes_only_the_marker_that_was_honoured() {
        let dir = state_dir("consume");
        request(&dir, ME).expect("requested");
        assert!(decide(&dir, ME).should_drain());
        consume(&dir);
        assert!(!marker_path(&dir).exists(), "a honoured marker is gone");
        assert_eq!(decide(&dir, ME), Decision::Absent, "and the next poll sees nothing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_marker_survives_a_crash_between_decide_and_consume_as_stale() {
        // The whole point of naming the daemon: the daemon that read the marker
        // died before consuming it, so the marker is still there — and the *next*
        // daemon has a different id, so it reads the marker as stale rather than as
        // an instruction to exit.
        let dir = state_dir("crash");
        request(&dir, ME).expect("requested");
        // No consume: the daemon died here.
        assert_eq!(decide(&dir, ME), Decision::Drain, "the named daemon would drain");
        assert_eq!(
            decide(&dir, OTHER),
            Decision::Stale {
                found: ME.to_string()
            },
            "and the next one boots"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_four_decisions_are_distinguishable_by_their_reason() {
        let dir = state_dir("reasons");
        request(&dir, OTHER).expect("requested");
        std::fs::write(marker_path(&dir), b"garbage").expect("write");
        let decisions = [
            Decision::Absent,
            Decision::Drain,
            // The stale decision, from a real marker naming another daemon.
            Decision::Stale {
                found: OTHER.to_string(),
            },
            Decision::Unreadable,
        ];
        let reasons: Vec<String> = decisions.iter().map(Decision::reason).collect();
        let mut unique = reasons.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), reasons.len(), "every decision has its own sentence");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
