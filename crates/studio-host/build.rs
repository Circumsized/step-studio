//! Stamp this build with an identity a newer shell can compare against.
//!
//! §12.4's hot-replace sequence ends with "the first surface starts the new
//! daemon" — but a daemon that was already running when the update landed keeps
//! serving until it drains. A shell that just updated then talks to a daemon
//! from the previous build, and the plan's answer (`plan.md`:585) is to *say so*
//! rather than refuse, because refusing is what makes "updated and now it will
//! not open" (AT-3).
//!
//! The workspace version cannot carry this: every crate shares
//! `version.workspace = true`, so the shell and the daemon always report the
//! same string and a skew between them is invisible. What is needed is something
//! that changes per build.
//!
//! A timestamp, not a counter: a counter is a number somebody has to remember to
//! bump, and the one build where they forget is the one where the skew goes
//! undetected. Its failure mode — a clock that runs backwards between two builds
//! — makes the comparison answer "not older", which is the safe direction, and it
//! is detectable in the log rather than silent.
//!
//! Seconds, not nanoseconds: two builds inside the same second tie, and a tie is
//! correctly read as "not skewed". Sub-second precision would buy nothing and
//! would make the stamped value depend on filesystem timestamp granularity.

use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    // Nothing to watch beyond this file: the stamp is deliberately per-build, not
    // per-source-change, so editing a .rs file must not rewrite it. Cargo's
    // default (rerun when any file in the package changes) would do exactly that.
    println!("cargo:rerun-if-changed=build.rs");
    let built_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    println!("cargo:rustc-env=STEP_STUDIO_BUILD_ID={built_at}");
}
