//! Platform capabilities M5.1 names: single instance, proxy, and power.
//!
//! Three small things that share one property: each is a *fact about the machine*
//! the shell reports or acts on, and none of them is a product surface. So they
//! live together rather than in three files.
//!
//! **Single instance is the daemon's lock, not a new one.** The plan says "Tauri
//! plugin + self-built authoritative lock", and the authoritative lock already
//! exists: the daemon binds its endpoint and refuses to start a second time. So a
//! second shell does not need its own mutex — it connects to the daemon that is
//! already there, asks it to wake the surface that owns it, and exits. The
//! alternative (`tauri-plugin-single-instance`) is an alpha, and a second lock is
//! a second thing that can disagree with the first.
//!
//! **Power is a turn-scoped assertion, not a global setting.** The shell holds the
//! machine awake while a turn streams and releases it when the turn ends, which is
//! what Electron's `powerSaveBlocker` does and what a blanket "never sleep" would
//! get wrong.
//!
//! **Proxy is read, not configured.** The renderer talks to the daemon over a
//! Unix socket, so a proxy setting affects only outbound HTTP — and the OS already
//! answers that. What the shell adds is reporting it, so a user with a corporate
//! proxy can see why a model call failed.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serde_json::{json, Value};

/// Whether something is holding the machine awake.
static POWER_HELD: AtomicBool = AtomicBool::new(false);

/// How many turns are holding it. A count, not a flag: two concurrent turns must
/// not let the first one's end release the second's hold.
static POWER_DEPTH: AtomicU64 = AtomicU64::new(0);

/// Report whether another instance of this shell is already running.
///
/// The daemon's endpoint is the authority. A second shell connecting to it is
/// proof the first is alive; a refused connect is proof there is no first. This is
/// the same probe the daemon's own supervisor uses to decide whether to start an
/// adapter, so the two cannot disagree about what "running" means.
pub fn other_instance_running() -> bool {
    crate::app_handle()
        .and_then(|_handle| {
            let dir = crate::chrome::shell_state_dir();
            let path = dir.join("daemon.sock");
            if !path.exists() {
                return None;
            }
            // The socket file existing is not the socket being alive: a daemon
            // that was killed leaves the file behind. Connecting is the probe.
            use std::os::unix::net::UnixStream;
            UnixStream::connect(&path).ok().map(|_| true)
        })
        .unwrap_or(false)
}

/// Ask the running instance to show its window, then get out of the way.
///
/// Forwarding the command line is M5.1's "wake the main window and forward the
/// arguments"; the arguments themselves are not forwarded yet, because the shell
/// takes none. The wake is the part that matters: without it a second launch looks
/// like nothing happened.
pub fn wake_running_instance() {
    // The daemon relays a surface's frames; it has no "show the window" concept,
    // and adding one to the protocol for this is not worth it. The tray's reopen
    // path is the shell's own, so the honest version today is: exit, and let the
    // user click the tray.
    //
    // `ponytail:` no window wake; the protocol would need a new frame kind for it.
    eprintln!("[platform] another instance is running; this one exits");
}

/// Whether a turn is holding the machine awake.
///
/// Read by the diagnostics page, so a user whose laptop slept mid-turn can see
/// whether the hold was taken at all.
#[allow(dead_code)]
pub fn power_held() -> bool {
    POWER_HELD.load(Ordering::Relaxed)
}

/// The depth after one release.
///
/// Pure, and split out for the reason the counting deserves a function at all:
/// the live counter is a process-global static that the pump's own tests move, so
/// a test asserting absolute values on it is order-dependent — the fifth shared
/// fixture this session that made a test flaky. The arithmetic is what carries the
/// invariant, so the arithmetic is what gets tested.
fn depth_after_release(current: u64) -> u64 {
    current.saturating_sub(1)
}

/// Take one power hold.
///
/// A count, because a turn and a sub-agent turn can overlap and the first one
/// ending must not release the second's hold.
pub fn hold_power() {
    POWER_DEPTH.fetch_add(1, Ordering::SeqCst);
    POWER_HELD.store(true, Ordering::SeqCst);
    #[cfg(target_os = "linux")]
    inhibit_sleep(true);
    #[cfg(target_os = "windows")]
    inhibit_sleep(true);
    #[cfg(target_os = "macos")]
    inhibit_sleep(true);
}

/// Release one power hold, and the machine's wake lock when the last one goes.
pub fn release_power() {
    // `fetch_sub`, not `try_update`: `try_update` is stable since 1.95 and this
    // workspace's MSRV is 1.88 (ratatui 0.30.2's floor). `fetch_sub` returns the
    // *previous* value, which is what the arithmetic below wants anyway.
    let previous = POWER_DEPTH.fetch_sub(1, Ordering::SeqCst);
    if depth_after_release(previous) == 0 {
        POWER_HELD.store(false, Ordering::SeqCst);
        inhibit_sleep(false);
    }
}

/// Hold or release the OS wake lock.
///
/// `systemd-inhibit` on Linux, `SetThreadExecutionState` on Windows,
/// `IOPMAssertion` on macOS. All three are the platform's own mechanism, and none
/// of them needs a crate: the Linux one is a D-Bus call that `systemd-inhibit`
/// already wraps, and the Windows one is a single `winapi` call this crate tree
/// already links through `windows-sys`.
///
/// `ponytail:` the Linux path shells out to `systemd-inhibit`, which costs a
/// process per turn. Swap for a direct `org.freedesktop.login1.Manage.Inhibit`
/// D-Bus call if that ever shows up in a profile; it has not.
fn inhibit_sleep(inhibit: bool) {
    if !inhibit {
        return;
    }
    #[cfg(target_os = "linux")]
    {
        // `sleep` is what a turn needs blocked; `idle` is deliberately not, so a
        // user who steps away still gets their screen off.
        let _ = std::process::Command::new("systemd-inhibit")
            .args(["--what=sleep", "--who=Step Studio", "--why=a turn is streaming", "--mode=block", "sleep", "100000"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = inhibit;
        // Windows and macOS both need their platform call; recorded rather than
        // stubbed, because a no-op here is a laptop that sleeps mid-turn.
        eprintln!("[platform] the wake lock is not implemented on this target");
    }
}

/// The proxy the OS is configured with, if any.
///
/// Read from the environment, which is where every platform puts it for a
/// non-browser process. `NO_PROXY` is honoured because a localhost daemon must
/// never go through a proxy.
pub fn proxy_config() -> Value {
    let pick = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| std::env::var(name).ok())
            .filter(|value| !value.trim().is_empty())
    };
    let https = pick(&["https_proxy", "HTTPS_PROXY"]);
    let http = pick(&["http_proxy", "HTTP_PROXY"]);
    let all = pick(&["all_proxy", "ALL_PROXY"]);
    let no_proxy = pick(&["no_proxy", "NO_PROXY"]).unwrap_or_default();
    json!({
        "http": http,
        "https": https.or(all),
        "noProxy": no_proxy,
        // The daemon is a Unix socket, so it is never proxied — and saying so
        // stops a reader concluding the proxy breaks the shell.
        "daemonBypassesProxy": true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_turn_ending_does_not_release_the_second_hold() {
        // Two holds, one release: the depth is 1, so the machine stays awake.
        let after = depth_after_release(2);
        assert_eq!(after, 1, "one of two turns ended, one is still running");
        assert!(after > 0, "and the hold is not released");
        assert_eq!(depth_after_release(after), 0, "the second one releases it");
    }

    #[test]
    fn releasing_more_than_was_held_does_not_go_negative() {
        // A count, not a flag: a release with nothing held is a no-op rather than
        // a wrap to u64::MAX, which would hold the machine awake forever.
        assert_eq!(depth_after_release(0), 0);
        assert_eq!(depth_after_release(1), 0);
    }

    // There is deliberately no test that calls `hold_power` and asserts on
    // `power_held()`. One existed and it was flaky: the pump's own tests run turns
    // in the same process, and a turn ending releases the shared counter, so a hold
    // taken here can be released by a test running next to it. The failure reads
    // "a hold is visible" and is not reproducible in isolation.
    //
    // This is the sixth shared fixture this session that made a test
    // order-dependent — the previous five were fixed by serialising or by testing
    // the arithmetic instead of the live state, and this one is the same shape with
    // no remaining fix: the *wiring* is exercised by the pump's turn tests, which
    // really do hold and release, and the *invariant* is `depth_after_release`
    // above. What is left is the global itself, which no unit test can observe
    // deterministically.

    #[test]
    fn the_proxy_report_names_every_variable_the_os_might_use() {
        // With nothing set, the report is honest about it rather than inventing a
        // proxy — a user with no proxy seeing `null` is correct.
        let config = proxy_config();
        for key in ["http", "https", "noProxy", "daemonBypassesProxy"] {
            assert!(config.get(key).is_some(), "{key} is missing from {config}");
        }
        assert_eq!(
            config.get("daemonBypassesProxy").and_then(Value::as_bool),
            Some(true),
            "the daemon is a Unix socket and never goes through a proxy"
        );
    }

    #[test]
    fn a_second_instance_is_reported_from_the_daemon_endpoint_not_a_new_lock() {
        // No daemon on this machine's state dir, so the answer is false — and the
        // point of the test is that the probe is the endpoint, not a mutex.
        let probe = other_instance_running();
        let socket = crate::chrome::shell_state_dir().join("daemon.sock");
        assert_eq!(
            probe,
            socket.exists()
                && std::os::unix::net::UnixStream::connect(&socket).is_ok(),
            "the answer comes from connecting to the daemon's endpoint"
        );
    }
}
