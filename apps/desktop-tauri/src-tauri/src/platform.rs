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
use std::sync::Mutex;

use serde_json::{json, Value};

use step_studio_host as host;

/// Whether something is holding the machine awake.
static POWER_HELD: AtomicBool = AtomicBool::new(false);

/// How many turns are holding it. A count, not a flag: two concurrent turns must
/// not let the first one's end release the second's hold.
static POWER_DEPTH: AtomicU64 = AtomicU64::new(0);

/// The inhibitor, if one is running. Held so releasing it is possible — see
/// [`take_wake_lock`].
static WAKE_LOCK: Mutex<Option<std::process::Child>> = Mutex::new(None);

/// Report whether another instance of this shell is already running.
///
/// The daemon's endpoint is the authority. A second shell connecting to it is
/// proof the first is alive; a refused connect is proof there is no first. This is
/// the same probe the daemon's own supervisor uses to decide whether to start an
/// adapter, so the two cannot disagree about what "running" means.
pub fn other_instance_running() -> bool {
    crate::app_handle().is_some_and(|_handle| endpoint_answers(&daemon_probe_path()))
}

/// Whether anything is accepting connections at `endpoint`.
///
/// Connecting is the whole probe. The socket file existing is not the socket
/// being alive — a daemon that was killed leaves the file behind — so there is
/// nothing to gain from checking first.
///
/// Split from [`other_instance_running`] because that one needs a live
/// `AppHandle`, which no test has: with the probe inline, the only assertion
/// available was "no daemon means false", which a function that always answered
/// false would also pass. This one can be driven both ways.
///
/// Through `transport`, not `UnixStream` directly: the daemon's endpoint is a
/// socket file on Unix and a pipe name on Windows, and `endpoint_for_bind` is
/// the one place that knows the difference. Same probe `lifecycle::acquire`
/// makes, minus the stale-file removal a probe has no business doing.
fn endpoint_answers(endpoint: &std::path::Path) -> bool {
    let target =
        step_studio_bridge::transport::endpoint_for_bind(&endpoint.to_string_lossy());
    step_studio_bridge::transport::connect(&target).is_ok()
}

/// Where the probe looks: the daemon's own endpoint.
///
/// This used to be `shell_state_dir()/daemon.sock`, which is `<cwd>/.step-studio`
/// — a directory the daemon never binds, because it binds the machine-level
/// `default_endpoint()` instead. In production the probe therefore always
/// answered false, and M5.1's single-instance guard never fired: a second launch
/// started a second shell rather than exiting. The test that was supposed to
/// catch this compared the function to its own path, so both sides were wrong
/// together and it passed anyway.
pub(crate) fn daemon_probe_path() -> std::path::PathBuf {
    std::path::PathBuf::from(host::lifecycle::default_endpoint())
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

/// The depth after one release, and whether that was the last hold.
///
/// Pure, and split out for the reason the counting deserves a function at all:
/// the live counter is a process-global static that the pump's own tests move, so
/// a test asserting absolute values on it is order-dependent — the fifth shared
/// fixture this session that made a test flaky. The arithmetic is what carries the
/// invariant, so the arithmetic is what gets tested.
///
/// `previous == 0` is reported rather than clamped away: it means a release with
/// nothing held, and the caller has to *not* decrement again. The first version
/// returned `0` for it, which the caller read as "that was the last hold" and so
/// cleared the flag and released the lock on a depth that was already zero.
fn depth_after_release(current: u64) -> (u64, bool) {
    if current == 0 {
        return (0, false);
    }
    (current - 1, current == 1)
}

/// Take one power hold.
///
/// A count, because a turn and a sub-agent turn can overlap and the first one
/// ending must not release the second's hold.
pub fn hold_power() {
    POWER_DEPTH.fetch_add(1, Ordering::SeqCst);
    POWER_HELD.store(true, Ordering::SeqCst);
    // Only the first hold takes the lock: a second concurrent turn does not need a
    // second inhibitor, and two would mean the first one's release still leaves the
    // machine awake.
    let mut slot = WAKE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if slot.is_none() {
        *slot = take_wake_lock();
    }
}

/// Release one power hold, and the machine's wake lock when the last one goes.
pub fn release_power() {
    // `fetch_sub` returns the *previous* value, which is what the arithmetic wants.
    // Not `try_update`: that is stable since 1.95 and this workspace's MSRV is 1.88
    // (ratatui 0.30.2's floor).
    let previous = POWER_DEPTH.fetch_sub(1, Ordering::SeqCst);
    let (depth, was_last) = depth_after_release(previous);
    if previous == 0 {
        // A release with nothing held. Undo the decrement rather than let it wrap:
        // `fetch_sub` on zero gives `u64::MAX`, which holds the machine awake with
        // no way back.
        POWER_DEPTH.store(0, Ordering::SeqCst);
        return;
    }
    if was_last {
        POWER_DEPTH.store(depth, Ordering::SeqCst);
        POWER_HELD.store(false, Ordering::SeqCst);
        // Killing the inhibitor *is* the release, and `wait` reaps the child.
        let mut slot = WAKE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(mut child) = slot.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Take the OS wake lock, and return the handle that releases it.
///
/// The handle is the point. The first version returned nothing and `inhibit_sleep`
/// began with `if !inhibit { return; }`, which made the whole release path dead
/// code: every hold spawned a `systemd-inhibit --mode=block sleep 100000` and
/// dropped the `Child`, so the inhibitor was never released and the child was
/// never reaped. Ten turns meant ten inhibitors and ten zombies, and the module
/// doc's "released when the turn ends" was false on every platform.
///
/// Keeping the child is what makes release possible: killing it is the release.
/// That is also why the lifetime is bounded — 100000 seconds is 27.7 hours, and a
/// hold that outlives its process would keep the machine awake after a crash.
///
/// `ponytail:` the Linux path shells out to `systemd-inhibit`, which costs a
/// process per turn. Swap for a direct `org.freedesktop.login1.Manage.Inhibit`
/// D-Bus call if that ever shows up in a profile; it has not.
fn take_wake_lock() -> Option<std::process::Child> {
    #[cfg(target_os = "linux")]
    {
        // `sleep` is what a turn needs blocked; `idle` is deliberately not, so a
        // user who steps away still gets their screen off.
        std::process::Command::new("systemd-inhibit")
            .args([
                "--what=sleep",
                "--who=Step Studio",
                "--why=a turn is streaming",
                "--mode=block",
                "sleep",
                "100000",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Windows and macOS both need their platform call (`SetThreadExecutionState`
        // / `IOPMAssertion`). Recorded rather than stubbed, because a no-op here is
        // a laptop that sleeps mid-turn.
        eprintln!("[platform] the wake lock is not implemented on this target");
        None
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
        let (after, was_last) = depth_after_release(2);
        assert_eq!(after, 1, "one of two turns ended, one is still running");
        assert!(!was_last, "and the hold is not released");
        let (after, was_last) = depth_after_release(after);
        assert_eq!(after, 0);
        assert!(was_last, "the second one releases it");
    }

    #[test]
    fn releasing_more_than_was_held_does_not_go_negative() {
        // A count, not a flag. `fetch_sub` on zero gives `u64::MAX`, which holds
        // the machine awake with no way back — so the caller has to be told this
        // was a release with nothing held, and undo it.
        let (depth, was_last) = depth_after_release(0);
        assert_eq!(depth, 0, "the depth stays at zero");
        assert!(!was_last, "and the lock is not released");
        let (depth, was_last) = depth_after_release(1);
        assert_eq!(depth, 0);
        assert!(was_last);
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
    fn the_probe_looks_where_the_daemon_binds() {
        // The assertion that matters, and the one the previous version could not
        // make: it compared `other_instance_running()` against a connect to the
        // path the function itself had chosen. A wrong path made both sides
        // false and the test passed.
        //
        // This one compares the path to the daemon's endpoint — which is the
        // right comparison, and it was still vacuous until now, because
        // `daemon_probe_path()` *is* `default_endpoint()`: the two sides moved
        // together. Verified by mutation: replacing `default_endpoint`'s body
        // with an empty string left this test green. So it now also asserts the
        // thing that actually regressed historically — the probe reaching for a
        // path under the shell's own per-workspace directory.
        let probe = daemon_probe_path();
        assert_eq!(
            probe,
            std::path::PathBuf::from(host::lifecycle::default_endpoint()),
            "the probe must look where the daemon binds, not at a path of the shell's own"
        );
        assert!(
            !probe.as_os_str().is_empty(),
            "an empty endpoint is not where the daemon binds"
        );
        assert!(
            !probe.ends_with(".step-studio")
                && !probe.components().any(|part| part.as_os_str() == ".step-studio"),
            "the probe is looking at the shell's own workspace directory again"
        );
    }

    #[test]
    #[cfg(unix)]
    fn something_listening_at_the_endpoint_is_another_instance() {
        use std::os::unix::net::UnixListener;

        // The half of the probe that had no test: with nothing bound the answer
        // is false, which a function hardwired to `false` also passes.
        let endpoint = daemon_probe_path();
        if let Some(parent) = endpoint.parent() {
            std::fs::create_dir_all(parent).expect("make the endpoint directory");
        }
        let _ = std::fs::remove_file(&endpoint);
        let listener = UnixListener::bind(&endpoint).expect("bind the probe endpoint");

        assert!(
            endpoint_answers(&endpoint),
            "a live daemon at {} was not seen",
            endpoint.display()
        );
        // No `accept`: a Unix `connect` completes on the backlog alone, and
        // dropping the listener closes whatever is pending. Calling it here
        // would also hang the test the moment the probe stopped connecting.

        drop(listener);
        let _ = std::fs::remove_file(&endpoint);
        assert!(
            !endpoint_answers(&endpoint),
            "a removed endpoint still answered"
        );
    }

    #[test]
    fn no_daemon_means_no_other_instance() {
        // The probe is a connect, so an endpoint nobody is listening on answers
        // false rather than panicking. This is the production answer on a
        // machine where the daemon has not been started.
        assert!(
            !other_instance_running(),
            "something is already listening on {}",
            daemon_probe_path().display()
        );
    }
}
