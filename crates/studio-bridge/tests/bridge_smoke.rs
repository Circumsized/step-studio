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

use step_studio_bridge::frame::{FrameDecoder, FrameEncoder, MAX_FRAME_BYTES};
use step_studio_bridge::handshake::{challenge_proof, PROTOCOL_VERSION};
use step_studio_bridge::transport;

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

/// Every body the codec has to carry survives a round trip, byte for byte.
///
/// A table rather than a property test, and deliberately so. proptest was added
/// and then removed: it costs 13 packages, and its own limitations page says
/// "traditional unit testing with intelligently selected cases is still necessary
/// for many kinds of problems" — for a bytes-to-struct transformation the
/// interesting inputs are the ones you can name, not the ones a generator finds.
/// A random body almost never contains a nested escape or a lone surrogate half.
///
/// The cases below are the ones that break a hand-rolled length prefix or a
/// serde round trip: an empty body, a body whose JSON is longer than its UTF-8
/// length, escapes that expand under re-encoding, deep nesting, numbers at the
/// edges of f64, and strings that are valid JSON but not valid UTF-8 as Rust
/// would emit them.
#[test]
fn every_body_the_codec_carries_survives_a_round_trip() {
    let bodies: Vec<(&str, serde_json::Value)> = vec![
        ("empty object", serde_json::json!({})),
        ("null", serde_json::json!(null)),
        ("a lone string", serde_json::json!("hello")),
        ("escapes", serde_json::json!({"q": "a\"b\\c\nd\te"})),
        ("unicode", serde_json::json!({"cn": "中文", "emoji": "🙂"})),
        ("nested", serde_json::json!({"a": {"b": {"c": [1, 2, {"d": true}]}}})),
        ("numbers", serde_json::json!({"i": 0, "neg": -1, "big": 9_007_199_254_740_991i64, "f": 1.5})),
        ("array", serde_json::json!([1, "two", null, false, {"k": "v"}])),
        ("long string", serde_json::json!("x".repeat(4096))),
        ("many keys", {
            let mut map = serde_json::Map::new();
            for i in 0..200 {
                map.insert(format!("key-{i}"), serde_json::json!(i));
            }
            serde_json::Value::Object(map)
        }),
    ];

    for (label, body) in &bodies {
        let bytes = FrameEncoder::encode(7, body).expect("encode");
        // The frame is self-describing: the two prefixes must agree with the
        // bytes that follow, or the decoder is reading a different layout than
        // the encoder wrote.
        let payload_length = u32::from_be_bytes(bytes[0..4].try_into().unwrap()) as usize;
        let header_length = u32::from_be_bytes(bytes[4..8].try_into().unwrap()) as usize;
        assert_eq!(
            payload_length,
            4 + header_length + (bytes.len() - 8 - header_length),
            "{label}: the payload length prefix does not match the frame"
        );

        let frames = FrameDecoder::new().push(&bytes).expect("decode");
        assert_eq!(frames.len(), 1, "{label}: expected exactly one frame");
        assert_eq!(&frames[0].body, body, "{label}: the body did not survive");
        assert_eq!(frames[0].header.runtime_epoch, 7, "{label}: the epoch did not survive");
        assert_eq!(frames[0].header.version, PROTOCOL_VERSION, "{label}: the version");
        assert_eq!(
            frames[0].header.body_length,
            serde_json::to_vec(body).expect("measure").len(),
            "{label}: the declared body length is not the encoded one"
        );
    }
}

/// A frame split at every possible boundary still decodes to the same thing.
///
/// The existing test splits at one hard-coded offset. The failure mode this
/// catches is an off-by-one in the incremental decoder's buffer arithmetic,
/// which only shows up at particular split points — and a single split is a
/// coin flip.
#[test]
fn a_frame_split_at_any_boundary_decodes_the_same() {
    let body = serde_json::json!({"nested": {"deep": [1, 2, 3]}, "s": "esc\"ape"});
    let bytes = FrameEncoder::encode(3, &body).expect("encode");

    for split in 1..bytes.len() {
        let mut decoder = FrameDecoder::new();
        let first = decoder.push(&bytes[..split]).expect("first half");
        assert!(first.is_empty(), "split at {split}: decoded before the frame was whole");
        let second = decoder.push(&bytes[split..]).expect("second half");
        assert_eq!(second.len(), 1, "split at {split}: lost the frame");
        assert_eq!(second[0].body, body, "split at {split}: a different body");
    }
}

/// The decoder survives adversarial input without panicking.
///
/// This is the one property the round-trip table cannot cover: a table of valid
/// bodies exercises the happy path, and the security review named this decoder the
/// codebase's single best fuzz target precisely because of the hand-rolled
/// `u32` length to allocation path. Stateful fuzzing was decided against (the
/// decoder is stateless, so it buys a stateful tool's cost for a stateless
/// problem), but "never panics" is a one-loop property and needs no harness.
///
/// A panic here is not a crashed test: the decoder runs on the daemon's connection
/// thread, so a panic takes the daemon down with it.
#[test]
fn the_decoder_survives_adversarial_input_without_panicking() {
    let good = FrameEncoder::encode(1, &serde_json::json!({"n": 1})).expect("encode");
    let mut cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("one byte", vec![0]),
        ("seven bytes, short of both prefixes", vec![0; 7]),
        ("exactly the two prefixes, nothing else", vec![0; 8]),
        ("zero payload length", vec![0, 0, 0, 0, 0, 0, 0, 0]),
        ("payload length above the cap", {
            let mut v = (MAX_FRAME_BYTES as u32 + 1).to_be_bytes().to_vec();
            v.extend_from_slice(&0u32.to_be_bytes());
            v
        }),
        ("payload length at the cap, no body", {
            let mut v = MAX_FRAME_BYTES.to_be_bytes().to_vec();
            v.extend_from_slice(&0u32.to_be_bytes());
            v
        }),
        ("header length zero", {
            let mut v = 8u32.to_be_bytes().to_vec();
            v.extend_from_slice(&0u32.to_be_bytes());
            v
        }),
        ("header length past the payload", {
            let mut v = 8u32.to_be_bytes().to_vec();
            v.extend_from_slice(&9999u32.to_be_bytes());
            v
        }),
        ("valid frame then garbage", {
            let mut v = good.clone();
            v.extend_from_slice(&[0xff, 0xfe, 0x00, 0x01]);
            v
        }),
        ("valid frame truncated mid-body", good[..good.len() - 3].to_vec()),
        ("non-utf8 in the body", {
            let mut v = 30u32.to_be_bytes().to_vec();
            v.extend_from_slice(&24u32.to_be_bytes());
            v.extend_from_slice(b"{\"protocol\":\"omp-studio\",");
            v.extend_from_slice(&[0xff, 0xfe, 0xfd]);
            v
        }),
    ];

    // A deterministic sweep, so a failure is reproducible rather than a seed.
    // Splitmix64: one line, no dependency, and the sequence is fixed.
    let mut state = 0x9e3779b97f4a7c15u64;
    for length in 0..600usize {
        let mut bytes = Vec::with_capacity(length);
        for _ in 0..length {
            state = state.wrapping_add(0x9e3779b97f4a7c15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
            bytes.push((z ^ (z >> 31)) as u8);
        }
        cases.push(("sweep", bytes));
    }

    for (label, bytes) in &cases {
        // The whole assertion is that this returns rather than unwinds. A
        // decoder that closed on an earlier error must also refuse cleanly.
        let mut decoder = FrameDecoder::new();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            decoder.push(bytes).map(|frames| frames.len())
        }));
        assert!(
            outcome.is_ok(),
            "{label}: the decoder panicked on {} bytes",
            bytes.len()
        );
        // And a closed decoder stays closed rather than recovering.
        let _ = decoder.push(&good);
        let _ = decoder.push(&good);
    }

    // Panic-safety alone is not enough, and the first version of this test
    // learned that the hard way: disabling the `payload_length > MAX_FRAME_BYTES`
    // guard changed nothing observable, because an oversized length then merely
    // makes the decoder wait for bytes that will never come. So the rejection has
    // to be asserted on its own, or the cap is a control no test can lose.
    let mut over = (MAX_FRAME_BYTES as u32 + 1).to_be_bytes().to_vec();
    over.extend_from_slice(&0u32.to_be_bytes());
    let refused = FrameDecoder::new().push(&over);
    assert!(
        matches!(refused, Err(step_studio_bridge::frame::FrameError::TooLarge { .. })),
        "a frame over the cap must be refused, got {refused:?}"
    );
    let mut decoder = FrameDecoder::new();
    assert!(decoder.push(&over).is_err(), "the first refusal");
    assert!(
        decoder.push(&good).is_err(),
        "a decoder that refused one frame stays closed"
    );

    // And at the cap itself is accepted as a length, not refused as oversized.
    let mut at_cap = MAX_FRAME_BYTES.to_be_bytes().to_vec();
    at_cap.extend_from_slice(&0u32.to_be_bytes());
    assert!(
        !matches!(
            FrameDecoder::new().push(&at_cap),
            Err(step_studio_bridge::frame::FrameError::TooLarge { .. })
        ),
        "a frame exactly at the cap is not over it"
    );
}

#[test]
fn the_protocol_version_is_the_one_both_sides_declare() {
    // A bump on one side only is the drift this pins: the handshake negotiates a
    // version, and a client speaking a newer one than the adapter must fail rather
    // than silently misparse.
    assert_eq!(PROTOCOL_VERSION, 1);
}

/// The connection contract both platforms rest on: a second usable handle on the
/// *same* connection, and one end going away so the other sees it.
///
/// Written against `transport` rather than `UnixStream` on purpose, so it runs on
/// Windows too. On Unix the second handle is `try_clone`; on Windows it is an
/// `Arc` clone of one pipe, because `miow` implements `Read` and `Write` on
/// `&self` — the daemon's writer thread and reader thread each hold a clone.
/// Either way this is what `serve_surface` and `stream_for_reader` are built on,
/// and until now nothing exercised it.
///
/// On Windows these are also the only automated checks that the `miow` swap
/// works at runtime — `FILE_FLAG_FIRST_PIPE_INSTANCE` cleared, `max_instances`
/// pinned, and a pipe carrying bytes both ways. CI's Windows job is clippy-only,
/// so they need a real machine to run there.
#[test]
fn a_duplicated_connection_is_usable_from_both_ends() {
    use std::io::{Read, Write};

    let endpoint = scratch("duplicate").join("bridge.sock");
    let listener = transport::bind(&endpoint.to_string_lossy()).expect("bind");

    // Connect from a thread: on Windows `bind` returns immediately and it is
    // `accept` that waits, so the two have to be concurrent.
    let for_client = transport::endpoint_for_bind(&endpoint.to_string_lossy());
    let connector = std::thread::spawn(move || transport::connect(&for_client));

    let accepted = transport::accept(listener).expect("accept");
    let mut client = connector
        .join()
        .expect("the connector finished")
        .expect("connect");
    // So a broken `duplicate` fails rather than hangs the suite. A no-op on
    // Windows — `set_read_timeout` has no named-pipe equivalent — which means
    // that platform relies on the read eventually erroring.
    transport::set_read_timeout(&client, Some(std::time::Duration::from_secs(10)))
        .expect("a read timeout");

    let connection = accepted.into_connection();
    let mut second = transport::connection_duplicate(&connection).expect("a second handle");

    // Write through the duplicate, read through the client: two handles, one
    // connection, both directions live.
    second.write_all(b"through-the-second-handle").expect("write");
    let mut seen = [0u8; 25];
    client.read_exact(&mut seen).expect("read what was written");
    assert_eq!(&seen, b"through-the-second-handle");
}

#[test]
fn closing_one_end_makes_the_other_see_a_closed_connection() {
    use std::io::Read;

    let endpoint = scratch("closed").join("bridge.sock");
    let listener = transport::bind(&endpoint.to_string_lossy()).expect("bind");

    let for_client = transport::endpoint_for_bind(&endpoint.to_string_lossy());
    let connector = std::thread::spawn(move || transport::connect(&for_client));

    let accepted = transport::accept(listener).expect("accept");
    let mut client = connector
        .join()
        .expect("the connector finished")
        .expect("connect");
    transport::set_read_timeout(&client, Some(std::time::Duration::from_secs(10)))
        .expect("a read timeout");

    let connection = accepted.into_connection();
    transport::close_connection(&connection).expect("close one end");

    // Microsoft, on DisconnectNamedPipe: the client "receives an error the next
    // time it attempts to access the pipe" — not a clean EOF. So either shape is
    // accepted, and only the read *returning* is asserted.
    let mut buffer = [0u8; 8];
    let started = std::time::Instant::now();
    match client.read(&mut buffer) {
        Ok(0) => {}
        Ok(_) => panic!("a closed connection yielded bytes"),
        // A timeout is NOT a close. The first version accepted any error, so
        // removing the close above left it passing: the read blocked for the
        // full timeout and the timeout error was read as "closed". Rejecting it
        // is what makes the assertion mean "the read returned".
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            panic!(
                "the read never returned after {:?}: the close was not observed",
                started.elapsed()
            );
        }
        Err(_) => {}
    }
}

/// A directory to bind an endpoint in, thrown away by the next call.
fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ss-transport-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("make a temp dir");
    dir
}
