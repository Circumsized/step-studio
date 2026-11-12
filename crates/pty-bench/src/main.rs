//! §14.6's PTY gate: the longest frame under sustained output.
//!
//! The plan defines 满速 as a sustained 10 MB/s, and asks for the longest frame
//! rather than an average — a terminal that is fast on average but stalls for
//! 200 ms once a second is a terminal the user feels. `max` is recorded here and
//! not used as a gate, per the same section's statistics discipline: GC and
//! scheduler noise make a single slow frame normal.
//!
//! This drives the same `portable-pty` the shell uses, so the number is about
//! the PTY path rather than about this program.

use std::io::Read;
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

/// The plan's 满速 definition: a sustained 10 MB of output.
const TOTAL_BYTES: usize = 10 * 1024 * 1024;

fn main() {
    let system = native_pty_system();
    let pair = system
        .openpty(PtySize {
            rows: 50,
            cols: 220,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open a pty");
    let mut child = pair
        .slave
        .spawn_command(producer())
        .expect("spawn the producer");
    // The slave end is the child's; dropping ours is what lets the reader see
    // EOF when the producer exits.
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().expect("clone the reader");
    let started = Instant::now();
    let mut frames: Vec<Duration> = Vec::new();
    let mut received = 0usize;
    let mut chunk = vec![0u8; 64 * 1024];

    while received < TOTAL_BYTES {
        let read_started = Instant::now();
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                // One frame is one read that the caller has to hand to the
                // renderer, so the read's own duration is the frame time.
                frames.push(read_started.elapsed());
                received += read;
            }
            Err(_) => break,
        }
    }
    let elapsed = started.elapsed();
    let _ = child.kill();
    let _ = child.wait();

    frames.sort();
    let percentile = |fraction: f64| -> Duration {
        if frames.is_empty() {
            return Duration::ZERO;
        }
        let index = ((frames.len() as f64 - 1.0) * fraction).round() as usize;
        frames[index]
    };
    let throughput = received as f64 / elapsed.as_secs_f64() / (1024.0 * 1024.0);
    println!(
        "received {received} bytes in {:.2}s ({throughput:.1} MB/s), {} frames",
        elapsed.as_secs_f64(),
        frames.len()
    );
    println!(
        "frame time: p50={:.3}ms p99={:.3}ms p99.9={:.3}ms max={:.3}ms",
        percentile(0.50).as_secs_f64() * 1e3,
        percentile(0.99).as_secs_f64() * 1e3,
        percentile(0.999).as_secs_f64() * 1e3,
        frames.last().copied().unwrap_or_default().as_secs_f64() * 1e3,
    );
}

/// A producer that writes `TOTAL_BYTES` and exits. `head` is used rather than a
/// shell loop so the measurement is about the PTY, not about `sh`'s throughput.
fn producer() -> CommandBuilder {
    let mut command = CommandBuilder::new("head");
    command.arg("-c");
    command.arg(TOTAL_BYTES.to_string());
    command.arg("/dev/zero");
    command
}
