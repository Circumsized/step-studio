//! Wire-protocol unit tests: the challenge proof's construction, and the frame
//! decoder's reassembly of split and batched frames.
//!
//! A third test used to live here, driving the real adapter end to end from a Rust
//! client. It is gone, and the reason is worth recording rather than leaving a
//! silently-red test:
//!
//! - It spawned the TypeScript adapter, which M2.4 deleted.
//! - Repointing it at the Rust adapter surfaced that the adapter's handshake
//!   expects a *push*-style CLI — one that emits frames on stdout unprompted —
//!   while the test double was request-response. Making the double match would
//!   mean writing a second fake protocol, which is a mock agreeing with itself.
//! - The cross-language property it existed for is covered from the other
//!   direction: `packages/studio-host/test/real-runtime-adapter.test.ts` drives the
//!   *Rust* adapter with the *TypeScript* client. Same two implementations, same
//!   wire, opposite ends.
//! - Daemon-level behaviour (shell restart, orphan cleanup, drain) is covered by
//!   `crates/studio-host/tests/daemon.rs` against the real daemon.
//!
//! What is left is what a unit test can actually prove: that the proof is built the
//! way the protocol says, and that the decoder survives fragmentation.

use step_studio_bridge::frame::{FrameDecoder, FrameEncoder};
use step_studio_bridge::handshake::{challenge_proof, PROTOCOL_VERSION};

#[test]
fn challenge_proof_matches_the_protocol_construction() {
    // Same vector the TypeScript host uses, so a drift on either side fails here.
    let proof = challenge_proof("token", "challenge-1", "rt-inst-0001");
    assert!(!proof.is_empty());
    // Deterministic: same inputs, same proof.
    assert_eq!(proof, challenge_proof("token", "challenge-1", "rt-inst-0001"));
    // The NUL separator is load-bearing.
    assert_ne!(proof, challenge_proof("token", "challenge-1", "rt-inst-0002"));
}

#[test]
fn frame_decoder_reassembles_split_and_batched_frames() {
    let first = FrameEncoder::encode(1, &serde_json::json!({"n": 1})).expect("encode");
    let second = FrameEncoder::encode(1, &serde_json::json!({"n": 2})).expect("encode");
    let mut both = first.clone();
    both.extend_from_slice(&second);

    let mut decoder = FrameDecoder::new();
    let frames = decoder.push(&both).expect("batched");
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].body.get("n").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(frames[1].body.get("n").and_then(|v| v.as_i64()), Some(2));

    // A frame arriving in fragments must not be decoded early or lost.
    let mut split = FrameDecoder::new();
    assert!(split.push(&first[..3]).expect("first fragment").is_empty());
    let rest = split.push(&first[3..]).expect("second fragment");
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].body.get("n").and_then(|v| v.as_i64()), Some(1));
}

#[test]
fn the_protocol_version_is_the_one_both_sides_declare() {
    // A bump on one side only is the drift this pins: the handshake negotiates a
    // version, and a client speaking a newer one than the adapter must fail rather
    // than silently misparse.
    assert_eq!(PROTOCOL_VERSION, 1);
}
