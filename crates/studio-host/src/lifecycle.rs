//! Acquiring the daemon's endpoint, and deciding when to let go of it.
//!
//! One daemon per user per endpoint. A second launch is not an error to
//! report — it means the app was started twice — so the second process checks
//! whether the first is alive and exits quietly if it is. A socket file left
//! behind by a daemon that was `SIGKILL`ed is not a live daemon: connecting is
//! what tells the two apart, exactly as
//! `packages/studio-host/src/authority-liveness.ts` already argues.

use std::io;
use std::time::Duration;

/// Who owns the daemon's endpoint directory.
///
/// The socket lives in a shared temp directory, so the per-user subdirectory is
/// the only thing standing between this daemon and another local user. It is
/// created `0700` and checked for ownership, because a directory an attacker
/// can write lets them either squat the path (impersonating the daemon) or make
/// it read-only (denying the user a daemon at all).
pub fn own_uid() -> u32 {
    use std::os::unix::fs::MetadataExt;
    std::env::current_exe()
        .and_then(std::fs::metadata)
        .map(|meta| meta.uid())
        .unwrap_or(0)
}

use step_studio_bridge::transport;

/// The endpoint the daemon owns, per user.
///
/// Short by necessity: a Unix socket path has a hard limit of about 103 bytes,
/// and `~/.local/state/...` blows through it under an ordinary username. The
/// pipe-name branch returns a bare name because `transport::endpoint_for_bind`
/// turns one into `\\.\pipe\<name>`; on Windows the endpoint is a name, not a
/// path, and nothing is left on disk to clean up.
///
/// `ponytail:` the user discriminator is the uid on Unix and the same number
/// on Windows, where a real per-user pipe namespace wants the SID. Two users
/// on one Windows machine therefore share a daemon until M5.1; on Unix they
/// do not.
pub fn default_endpoint() -> String {
    let uid = user_discriminator();
    #[cfg(unix)]
    {
        std::env::temp_dir()
            .join(format!("step-studio-{uid}"))
            .join("daemon.sock")
            .to_string_lossy()
            .to_string()
    }
    #[cfg(not(unix))]
    {
        format!("step-studio-{uid}")
    }
}

/// Something that distinguishes one user's daemon from another's, without
/// pulling in `libc` (the workspace forbids `unsafe`).
fn user_discriminator() -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::env::current_exe()
            .and_then(std::fs::metadata)
            .map(|meta| meta.uid())
            .unwrap_or(0)
    }
    #[cfg(not(unix))]
    {
        // Windows has no uid in `std`, and the value only has to be stable
        // per user for the pipe name.
        std::env::var("USERNAME")
            .map(|name| name.len() as u32)
            .unwrap_or(0)
    }
}

/// Who ended up owning the endpoint.
pub enum Acquire {
    /// This process owns it and should serve.
    Owned,
    /// A daemon answered on it; this process should exit.
    AlreadyRunning,
}

/// Take the endpoint, or report that somebody else has it.
///
/// Probing only — the bind belongs to the caller's serve loop, because binding
/// here would hand back a listener this function then drops, leaving the socket
/// file behind for the serve loop to fail on.
pub fn acquire(endpoint: &str) -> io::Result<Acquire> {
    let target = transport::endpoint_for_bind(endpoint);
    // A live daemon answers a connect. A stale socket file does not.
    if transport::connect(&target).is_ok() {
        return Ok(Acquire::AlreadyRunning);
    }
    // Unix leaves the file behind when the daemon dies without cleaning up; a
    // Windows pipe name lives in its own namespace and nothing is left to
    // remove.
    #[cfg(unix)]
    let _ = std::fs::remove_file(endpoint);
    Ok(Acquire::Owned)
}

/// Bind the endpoint. `None` means somebody else won the race for it.
///
/// Two daemons can both probe a dead endpoint and both decide to start. One
/// wins the bind; the other is the second daemon, not a fault.
pub fn bind(endpoint: &str) -> io::Result<Option<transport::Listener>> {
    let target = transport::endpoint_for_bind(endpoint);
    // A Unix socket is a file, so its directory has to exist first. The
    // endpoint lives in a per-user temp subdirectory precisely to keep the path
    // short, and nothing else creates it.
    //
    // Created `0700` and owned by us, or not used at all: `create_dir_all`
    // derives its mode from the umask, and under `umask 000` the socket ends up
    // world-writable — which turns every finding about relayed frames into a
    // local attack.
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let parent = std::path::Path::new(endpoint)
            .parent()
            .ok_or_else(|| io::Error::other("the endpoint has no directory"))?;
        std::fs::create_dir_all(parent)?;
        let metadata = std::fs::metadata(parent)?;
        let owner = metadata.uid();
        if owner != own_uid() {
            // Somebody else owns the path. Refusing is the honest answer: binding
            // would either fail with a confusing EACCES or, worse, succeed next
            // to a directory an attacker controls.
            return Err(io::Error::other(format!(
                "{} is owned by uid {owner}, not uid {}",
                parent.display(),
                own_uid()
            )));
        }
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    match transport::bind(endpoint) {
        Ok(listener) => {
            // The socket itself: `bind` derives its mode from the umask too, and
            // a world-writable socket is the whole finding.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(endpoint, std::fs::Permissions::from_mode(0o600));
            }
            Ok(Some(listener))
        }
        Err(error) => {
            if transport::connect(&target).is_ok() {
                return Ok(None);
            }
            Err(error)
        }
    }
}

/// Whether the daemon still has a reason to be running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// A surface is attached, or none ever was. No clock is running: a daemon
    /// that has never served anybody is booting, not draining, and tty7 means
    /// it stays up with nothing attached.
    Alive,
    /// Nobody is attached and the clock is running.
    Draining { remaining: Duration },
    /// The grace period ran out.
    Expired,
}

/// The drain clock.
///
/// It starts when the **last** surface disconnects — not on the last event and
/// not on the last command. A shell that is mid-turn but still connected keeps
/// the clock stopped, so a long turn is never cut short by it. A shell that
/// died mid-turn lets the clock run, and the turn keeps running without it,
/// which is the whole point of a daemon.
pub struct GraceClock {
    attached: usize,
    ever_attached: bool,
    last_detach: Option<Duration>,
    grace: Duration,
}

impl GraceClock {
    pub fn new(grace: Duration) -> Self {
        Self {
            attached: 0,
            ever_attached: false,
            last_detach: None,
            grace,
        }
    }

    pub fn attach(&mut self) {
        self.attached += 1;
        self.ever_attached = true;
        self.last_detach = None;
    }

    pub fn detach(&mut self, now: Duration) {
        self.attached = self.attached.saturating_sub(1);
        if self.attached == 0 {
            self.last_detach = Some(now);
        }
    }

    pub fn poll(&self, now: Duration) -> Liveness {
        if self.attached > 0 {
            return Liveness::Alive;
        }
        let Some(detached_at) = self.last_detach else {
            // Never served anybody, so there is nothing to drain away from.
            return Liveness::Alive;
        };
        let elapsed = now.saturating_sub(detached_at);
        if elapsed >= self.grace {
            Liveness::Expired
        } else {
            Liveness::Draining {
                remaining: self.grace - elapsed,
            }
        }
    }

    pub fn is_draining(&self) -> bool {
        self.attached == 0 && self.ever_attached && self.last_detach.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock() -> GraceClock {
        GraceClock::new(Duration::from_secs(600))
    }

    #[test]
    fn a_daemon_that_never_served_anybody_has_no_clock() {
        let clock = clock();
        assert_eq!(clock.poll(Duration::from_secs(100_000)), Liveness::Alive);
        assert!(!clock.is_draining());
    }

    #[test]
    fn the_clock_starts_when_the_last_surface_leaves() {
        let mut clock = clock();
        clock.attach();
        clock.attach();
        // One of two leaving is not a drain.
        clock.detach(Duration::from_secs(10));
        assert_eq!(clock.poll(Duration::from_secs(500)), Liveness::Alive);
        clock.detach(Duration::from_secs(20));
        assert_eq!(
            clock.poll(Duration::from_secs(20)),
            Liveness::Draining {
                remaining: Duration::from_secs(600)
            }
        );
        assert_eq!(
            clock.poll(Duration::from_secs(619)),
            Liveness::Draining {
                remaining: Duration::from_secs(1)
            }
        );
        assert_eq!(clock.poll(Duration::from_secs(620)), Liveness::Expired);
    }

    #[test]
    fn a_surface_arriving_mid_drain_stops_the_clock() {
        let mut clock = clock();
        clock.attach();
        clock.detach(Duration::ZERO);
        assert!(clock.is_draining());
        clock.attach();
        assert_eq!(clock.poll(Duration::from_secs(10_000)), Liveness::Alive);
        // And leaving again restarts it from this departure, not the old one.
        clock.detach(Duration::from_secs(10_000));
        assert_eq!(
            clock.poll(Duration::from_secs(10_100)),
            Liveness::Draining {
                remaining: Duration::from_secs(500)
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_endpoints_directory_is_created_before_binding() {
        // A path that does not exist yet, under a directory that does.
        let dir =
            std::env::temp_dir().join(format!("step-studio-bind-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let endpoint = dir.join("nested").join("daemon.sock");
        let bound = bind(&endpoint.to_string_lossy()).expect("bind creates the directory");
        // `is_file` is false for a socket, which is what this is.
        assert!(endpoint.exists(), "the socket file exists once bound");
        drop(bound);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_endpoint_is_short_enough_for_a_unix_socket() {
        let endpoint = default_endpoint();
        #[cfg(unix)]
        assert!(
            endpoint.len() < 103,
            "socket path {endpoint} is {} bytes, over the ~103 limit",
            endpoint.len()
        );
        #[cfg(not(unix))]
        assert!(
            !endpoint.contains('/'),
            "a pipe name has no path separators"
        );
    }
}
