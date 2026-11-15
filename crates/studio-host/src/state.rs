//! What the daemon must remember across its own restarts.
//!
//! Two things, and only two:
//!
//! - **The epoch.** The daemon mints it, monotonically. If it reset to the
//!   same value on every start, a client could not tell a restarted daemon
//!   from the one it was talking to, and the reducer's epoch rule
//!   (`packages/client/src/reducer.ts:712`) would never fire. That is exactly
//!   what happens today: the shell hardcodes `1` (`bridge.rs:266`), so the
//!   rule is dead code and M2.5's epoch tests would pass while testing
//!   nothing.
//! - **The breaker.** How many times the daemon has failed to start, and when
//!   the window began. Kept in the daemon's own memory this would reset on
//!   every restart — which is the one restart that must not clear it.
//!
//! Writes are a fresh temp file plus `rename`: a crash mid-write leaves either
//! the old file or the new one, never a truncated one. Same discipline as
//! `packages/studio-host/src/session-lease-store.ts:44`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Failed daemon starts, inside one window, that open the breaker.
pub const MAX_DAEMON_FAILURES: u32 = 5;
/// How long a failed start keeps counting.
pub const FAILURE_WINDOW: std::time::Duration = std::time::Duration::from_secs(600);

/// The whole persisted file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DaemonState {
    /// Handed to every client that connects. Bumped on every clean start, so a
    /// reconnecting client can tell a new daemon from the old one.
    pub epoch: u64,
    #[serde(default)]
    pub breaker: Breaker,
}

/// The daemon's own restart budget.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Breaker {
    /// Failed starts inside the current window.
    pub attempts: u32,
    /// Unix seconds at which the current window began. `0` means no window.
    #[serde(default)]
    pub window_started_at: u64,
    /// Why the last start failed, for the surface to show.
    #[serde(default)]
    pub last_error: String,
}

impl Breaker {
    /// Whether a launch is allowed at all.
    ///
    /// There is deliberately no timer that reopens this. Once the budget is
    /// spent the daemon stays down until somebody asks for it, because the
    /// only honest probe is a caller that wants it.
    pub fn is_open(&self) -> bool {
        self.attempts >= MAX_DAEMON_FAILURES
    }

    /// Record a failed start. Returns whether that failure opened the breaker.
    pub fn record_failure(&mut self, now: u64, error: &str) -> bool {
        if self.window_started_at == 0
            || now.saturating_sub(self.window_started_at) > FAILURE_WINDOW.as_secs()
        {
            // Nothing counts any more, so this is the first failure of a new
            // window rather than the sixth of an old one.
            self.attempts = 1;
            self.window_started_at = now;
        } else {
            self.attempts += 1;
        }
        self.last_error = error.to_string();
        self.is_open()
    }

    /// Spend the budget on one more attempt.
    pub fn reset(&mut self) {
        self.attempts = 0;
        self.window_started_at = 0;
        self.last_error.clear();
    }
}

/// Where the daemon keeps its state.
///
/// Per user, not per workspace: one daemon serves every workspace on the
/// machine. The shell's `cwd/.step-studio` (`bridge.rs:252`) is deliberately
/// not reused for this — that is per-workspace and would give one daemon per
/// checkout.
pub fn state_dir() -> PathBuf {
    #[cfg(windows)]
    {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        state_dir_under(&base)
    }
    #[cfg(not(windows))]
    {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join(".local").join("state"))
            })
            .unwrap_or_else(|| PathBuf::from("."));
        state_dir_under(&base)
    }
}

/// The daemon's state directory, under a given base.
///
/// Split from [`state_dir`] so a caller that controls another process's
/// environment can work out where that process will look without repeating the
/// layout: the upgrade drill spawns a daemon with its own `XDG_STATE_HOME` and
/// has to write the marker where *that* daemon reads it. The alternative — the
/// test writing to `cwd/.step-studio` because that is what the daemon used to
/// read — is how the marker came to live in the wrong directory in the first
/// place.
pub fn state_dir_under(base: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        base.join("Step Studio").join("daemon")
    }
    #[cfg(not(windows))]
    {
        base.join("step-studio").join("daemon")
    }
}

fn state_path() -> PathBuf {
    state_dir().join("state.json")
}

/// Read the state, or the initial state when there is no file yet.
///
/// An unreadable or malformed file is treated as absent: the daemon must still
/// start, and a state file it cannot parse is not a reason to refuse to serve.
pub fn load() -> DaemonState {
    let Ok(text) = fs::read_to_string(state_path()) else {
        return DaemonState::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Write the state, replacing whatever was there.
pub fn save(state: &DaemonState) -> io::Result<()> {
    let dir = state_dir();
    fs::create_dir_all(&dir)?;
    let path = state_path();
    // A per-process temp name, so two daemons racing do not clobber each
    // other's half-written file. `create_new` makes the collision loud rather
    // than silent.
    let temp = dir.join(format!("state.json.{}.tmp", std::process::id()));
    let text = serde_json::to_string_pretty(state).unwrap_or_else(|_| "{}".to_string());
    fs::write(&temp, text)?;
    fs::rename(&temp, &path)?;
    Ok(())
}

/// Bump the epoch and persist it. Returns the new value.
pub fn next_epoch() -> u64 {
    let mut state = load();
    state.epoch = state.epoch.saturating_add(1);
    let _ = save(&state);
    state.epoch
}

/// Unix seconds, or 0 if the clock is before the epoch. `0` is the "no window"
/// marker in [`Breaker`], which is safe because it can only mean a clock that
/// is wrong by five decades.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Read-only view for the shell: may it launch a daemon at all?
pub fn breaker_is_open() -> bool {
    load().breaker.is_open()
}

/// The manual retry. Clears the breaker so the next launch is allowed.
pub fn reset_breaker() -> io::Result<()> {
    let mut state = load();
    state.breaker.reset();
    save(&state)
}

/// Exposed for tests, which need a directory they can throw away.
pub fn state_path_for(dir: &Path) -> PathBuf {
    dir.join("state.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_breaker_opens_after_the_budget_and_stays_open() {
        let mut breaker = Breaker::default();
        assert!(!breaker.is_open());
        for step in 0..MAX_DAEMON_FAILURES {
            let opened = breaker.record_failure(1_000 + u64::from(step), "boom");
            assert_eq!(opened, step + 1 >= MAX_DAEMON_FAILURES);
        }
        assert!(breaker.is_open());
        // Time alone does not reopen it.
        assert!(breaker.is_open());
        assert_eq!(breaker.last_error, "boom");
    }

    #[test]
    fn a_failure_outside_the_window_starts_a_new_one() {
        let mut breaker = Breaker::default();
        for step in 0..MAX_DAEMON_FAILURES {
            breaker.record_failure(1_000 + u64::from(step), "boom");
        }
        assert!(breaker.is_open());
        // Well past the window: the old failures no longer count.
        breaker.record_failure(1_000 + FAILURE_WINDOW.as_secs() + 1, "again");
        assert!(!breaker.is_open());
        assert_eq!(breaker.attempts, 1);
    }

    #[test]
    fn the_epoch_survives_a_round_trip_and_only_goes_up() {
        let dir =
            std::env::temp_dir().join(format!("step-studio-state-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("make a temp dir");

        let state = DaemonState {
            epoch: 7,
            ..DaemonState::default()
        };
        let path = state_path_for(&dir);
        fs::write(&path, serde_json::to_string(&state).expect("serialize")).expect("write");
        let read: DaemonState =
            serde_json::from_str(&fs::read_to_string(&path).expect("read")).expect("parse");
        assert_eq!(read.epoch, 7);

        // A file the daemon cannot parse is treated as absent, not fatal.
        fs::write(&path, "{ not json").expect("write garbage");
        let fallback: DaemonState =
            serde_json::from_str(&fs::read_to_string(&path).expect("read")).unwrap_or_default();
        assert_eq!(fallback.epoch, 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_save_replaces_rather_than_appends() {
        let dir =
            std::env::temp_dir().join(format!("step-studio-save-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("make a temp dir");
        let path = state_path_for(&dir);

        let mut state = DaemonState {
            epoch: 1,
            ..DaemonState::default()
        };
        let text = serde_json::to_string_pretty(&state).expect("serialize");
        fs::write(&path, &text).expect("write");

        state.epoch = 2;
        fs::write(
            &path,
            serde_json::to_string_pretty(&state).expect("serialize"),
        )
        .expect("rewrite");
        let read: DaemonState =
            serde_json::from_str(&fs::read_to_string(&path).expect("read")).expect("parse");
        assert_eq!(read.epoch, 2);

        let _ = fs::remove_dir_all(&dir);
    }
}
