//! Restart policy for a supervised child.
//!
//! The child is the runtime adapter: a separate process that can die on its
//! own — the user kills `step`, it segfaults, the machine sleeps. The daemon
//! must bring it back, but not forever and not fast enough to burn a core.
//!
//! Three rules, in the order they matter:
//!
//! 1. A child that *lived* is restarted at once. A crash after a healthy run
//!    is an incident, not a broken start; waiting would only add downtime.
//! 2. A child that died inside the grace period is a failed start, and each
//!    one doubles the wait up to a cap, so a broken adapter costs milliseconds
//!    of CPU instead of a spinning restart loop.
//! 3. Enough failed starts and the supervisor stops. It reports the runtime as
//!    unavailable and waits for an explicit retry rather than trying forever.
//!
//! Times are monotonic offsets from the supervisor's creation rather than
//! `Instant`s, so the whole policy is a pure function of what it was told:
//! the tests need no sleeping and there is no clock trait to thread through.
//!
//! `ponytail:` no jitter. One supervised child cannot form a thundering herd,
//! and `std` has no RNG. Add full jitter when M2.3 fans out to N adapters,
//! which is the first point where lockstep retries are actually possible.

use std::collections::VecDeque;
use std::time::Duration;

/// The tunables, in one struct so a number is changed once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Uptime at or above which a restart counts as a healthy start. A child
    /// that dies sooner is a failed start.
    ///
    /// 60 s is the hot window the plan already uses elsewhere, so there is one
    /// number rather than two that mean the same thing.
    pub grace: Duration,
    /// Delay before the first retry of a failed start.
    pub base_delay: Duration,
    /// Ceiling on the growing delay.
    pub max_delay: Duration,
    /// Failed starts, inside `window`, after which the supervisor stops.
    pub max_failures: u32,
    /// Window failed starts are counted in; older ones stop counting.
    pub window: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            grace: Duration::from_secs(60),
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
            max_failures: 3,
            window: Duration::from_secs(600),
        }
    }
}

/// What the supervisor wants the loop to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// Start the child now.
    Spawn,
    /// Wait this much longer, then ask again.
    Wait(Duration),
    /// The supervisor has stopped. Do not start anything until [`Supervisor::retry`].
    Halted,
}

/// One supervised child's restart policy.
pub struct Supervisor {
    policy: Policy,
    /// Offsets at which failed starts happened, oldest first.
    failures: VecDeque<Duration>,
    /// Earliest offset the next spawn may happen at.
    gate: Duration,
    /// Set once the failure budget is spent; cleared only by [`Supervisor::retry`].
    halted: bool,
    /// Restarts where the child had lived, for the status line. Not a budget:
    /// a healthy child that dies is restarted as often as it takes.
    healthy_restarts: u64,
    /// Spends of the failure budget, for the status line.
    failed_starts: u64,
}

impl Supervisor {
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            failures: VecDeque::new(),
            gate: Duration::ZERO,
            halted: false,
            healthy_restarts: 0,
            failed_starts: 0,
        }
    }

    /// What to do at monotonic offset `now`.
    ///
    /// Half-open is deliberately not a timer. A halted supervisor stays halted
    /// until somebody asks for a retry, because the only honest probe is a
    /// caller that wants the runtime — a shell attaching is that caller.
    pub fn poll(&mut self, now: Duration) -> Next {
        if self.halted {
            return Next::Halted;
        }
        if now < self.gate {
            return Next::Wait(self.gate - now);
        }
        Next::Spawn
    }

    /// Report that the child exited after `uptime`.
    pub fn on_exit(&mut self, uptime: Duration, now: Duration) {
        if uptime >= self.policy.grace {
            // It ran. Whatever killed it was not a broken start, so the budget
            // buys nothing here and the restart is immediate.
            self.healthy_restarts += 1;
            self.failures.clear();
            self.gate = now;
            return;
        }
        self.failed_starts += 1;
        self.prune(now);
        self.failures.push_back(now);
        let attempt = self.failures.len() as u32;
        if attempt >= self.policy.max_failures {
            self.halted = true;
            return;
        }
        self.gate = now + self.delay_for(attempt);
    }

    /// Spend the failure budget on one more attempt.
    ///
    /// The caller is whoever wants the runtime back: a shell attaching, or an
    /// operator. Clearing the failures rather than decrementing them means a
    /// probe that survives the grace period closes the breaker outright, and
    /// one that does not gets the full budget again rather than one try.
    pub fn retry(&mut self, now: Duration) {
        self.halted = false;
        self.failures.clear();
        self.gate = now;
    }

    pub fn is_halted(&self) -> bool {
        self.halted
    }

    pub fn healthy_restarts(&self) -> u64 {
        self.healthy_restarts
    }

    pub fn failed_starts(&self) -> u64 {
        self.failed_starts
    }

    /// Drop failures the window has carried out of relevance.
    fn prune(&mut self, now: Duration) {
        while let Some(front) = self.failures.front() {
            if now.saturating_sub(*front) > self.policy.window {
                self.failures.pop_front();
            } else {
                break;
            }
        }
    }

    /// `base * 2^(attempt-1)`, capped. The shift is clamped because a
    /// `saturating_mul` on an already-capped value would otherwise be the only
    /// thing standing between a long run and an overflow panic.
    fn delay_for(&self, attempt: u32) -> Duration {
        let shift = attempt.saturating_sub(1).min(16);
        self.policy
            .base_delay
            .saturating_mul(1u32 << shift)
            .min(self.policy.max_delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn policy() -> Policy {
        Policy {
            grace: Duration::from_secs(5),
            base_delay: ms(100),
            max_delay: ms(1_000),
            max_failures: 3,
            window: Duration::from_secs(60),
        }
    }

    #[test]
    fn a_child_that_lived_is_restarted_at_once() {
        let mut supervisor = Supervisor::new(policy());
        supervisor.on_exit(Duration::from_secs(30), Duration::ZERO);
        // No wait, and the budget is untouched.
        assert_eq!(supervisor.poll(Duration::ZERO), Next::Spawn);
        assert_eq!(supervisor.healthy_restarts(), 1);
        assert_eq!(supervisor.failed_starts(), 0);
    }

    #[test]
    fn a_healthy_run_clears_the_failure_budget() {
        let mut supervisor = Supervisor::new(policy());
        supervisor.on_exit(Duration::ZERO, Duration::ZERO);
        supervisor.on_exit(Duration::ZERO, ms(100));
        // Two fast failures, then one that lived. The next fast failure must
        // start the budget over rather than finish it.
        supervisor.on_exit(Duration::from_secs(30), ms(200));
        supervisor.on_exit(Duration::ZERO, ms(300));
        assert_eq!(supervisor.poll(ms(300)), Next::Wait(ms(100)));
        assert!(!supervisor.is_halted());
    }

    #[test]
    fn fast_failures_back_off_exponentially_and_cap() {
        // A budget of five so the whole curve is observable before the halt.
        let mut supervisor = Supervisor::new(Policy {
            max_failures: 7,
            ..policy()
        });
        let mut now = Duration::ZERO;
        // Doubling, then the cap: 100 << 4 is 1600, so from the fifth failure
        // on the ceiling is what applies.
        for expected in [ms(100), ms(200), ms(400), ms(800), ms(1_000), ms(1_000)] {
            supervisor.on_exit(Duration::ZERO, now);
            assert_eq!(supervisor.poll(now), Next::Wait(expected));
            now += expected;
        }
    }

    #[test]
    fn enough_fast_failures_halt_the_supervisor() {
        let mut supervisor = Supervisor::new(policy());
        for step in 0..3 {
            supervisor.on_exit(Duration::ZERO, ms(500 * step));
        }
        assert!(supervisor.is_halted());
        assert_eq!(supervisor.poll(ms(10_000)), Next::Halted);
        assert_eq!(supervisor.failed_starts(), 3);
    }

    #[test]
    fn a_halted_supervisor_stays_halted_until_asked() {
        let mut supervisor = Supervisor::new(policy());
        for step in 0..3 {
            supervisor.on_exit(Duration::ZERO, ms(500 * step));
        }
        // Time alone does not revive it: there is no half-open timer, because
        // the only honest probe is a caller that wants the runtime.
        assert_eq!(supervisor.poll(Duration::from_secs(3_600)), Next::Halted);
        supervisor.retry(Duration::from_secs(3_600));
        assert_eq!(supervisor.poll(Duration::from_secs(3_600)), Next::Spawn);
    }

    #[test]
    fn a_probe_that_dies_gets_the_full_budget_again() {
        let mut supervisor = Supervisor::new(policy());
        for step in 0..3 {
            supervisor.on_exit(Duration::ZERO, ms(500 * step));
        }
        supervisor.retry(ms(1_000));
        // The probe failed immediately, but the budget was cleared, so this is
        // attempt one of three rather than an instant re-halt.
        supervisor.on_exit(Duration::ZERO, ms(1_000));
        assert!(!supervisor.is_halted());
        assert_eq!(supervisor.poll(ms(1_000)), Next::Wait(ms(100)));
    }

    #[test]
    fn failures_older_than_the_window_stop_counting() {
        let mut supervisor = Supervisor::new(policy());
        supervisor.on_exit(Duration::ZERO, Duration::ZERO);
        // Just outside the 60 s window: the first failure is gone, so this is
        // attempt one and the budget is not spent.
        supervisor.on_exit(Duration::ZERO, Duration::from_secs(61));
        assert!(!supervisor.is_halted());
        assert_eq!(
            supervisor.poll(Duration::from_secs(61)),
            Next::Wait(ms(100))
        );
    }

    #[test]
    fn the_delay_caps_rather_than_overflowing() {
        let mut supervisor = Supervisor::new(Policy {
            base_delay: Duration::from_secs(30),
            max_delay: Duration::from_secs(30),
            max_failures: 100,
            ..policy()
        });
        for step in 0..40 {
            supervisor.on_exit(Duration::ZERO, Duration::from_secs(step));
        }
        assert!(!supervisor.is_halted());
        assert_eq!(
            supervisor.poll(Duration::from_secs(39)),
            Next::Wait(Duration::from_secs(30))
        );
    }
}
