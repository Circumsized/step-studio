//! Micro-benchmarks for the three hot paths §14.6 names: frame codec, event
//! projection, and JSONL session parsing.
//!
//! Release only. A debug-mode number is meaningless, and criterion defaults to
//! the profile it is run under, so these are read from `cargo bench`.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use serde_json::json;
use step_studio_bridge::frame::{write_frame, FrameDecoder};
use step_studio_bridge::handshake::{challenge_proof, verify_hello, PROTOCOL_VERSION};

/// A conversation delta: the smallest event that still carries the fields the
/// codec has to serialize, and by volume the one that dominates a turn.
fn delta_event() -> serde_json::Value {
    json!({
        "kind": "conversation.message.delta",
        "sessionId": "01a1d10e-e022-7e1e-a16b-305b34783484",
        "turnId": "01a1d10e-e022-7e1e-a16b-305b34783485",
        "delta": "the quick brown fox jumps over the lazy dog",
    })
}

fn frame_codec(c: &mut Criterion) {
    let event = delta_event();
    c.bench_function("frame/encode", |b| {
        b.iter(|| {
            let mut bytes = Vec::new();
            write_frame(&mut bytes, black_box(7), black_box(&event)).expect("encode");
            black_box(bytes)
        })
    });

    // Decoding is the one that runs per event on the reader thread, so it is
    // measured with a fresh decoder per iteration the way `BridgeSession` uses
    // one per connection.
    let mut encoded = Vec::new();
    write_frame(&mut encoded, 7, &event).expect("encode");
    c.bench_function("frame/decode", |b| {
        b.iter(|| {
            let mut decoder = FrameDecoder::new();
            let frames = decoder.push(black_box(&encoded)).expect("decode");
            black_box(frames)
        })
    });

    // A whole turn's worth of frames arriving in one read, which is what
    // actually happens when the CLI writes faster than the socket drains.
    let mut burst = Vec::new();
    for _ in 0..64 {
        write_frame(&mut burst, 7, &event).expect("encode");
    }
    c.bench_function("frame/decode-burst-64", |b| {
        b.iter(|| {
            let mut decoder = FrameDecoder::new();
            let frames = decoder.push(black_box(&burst)).expect("decode");
            black_box(frames)
        })
    });
}

fn handshake(c: &mut Criterion) {
    let token = "step-studio";
    let challenge = "daemon-12345";
    c.bench_function("handshake/proof", |b| {
        b.iter(|| black_box(challenge_proof(black_box(token), black_box(challenge), "step-runtime-4242")))
    });

    let instance_id = "step-runtime-4242";
    let proof = challenge_proof(token, challenge, instance_id);
    let hello = json!({
        "type": "studio.hello.result",
        "requestId": "daemon-hello",
        "selectedProtocolVersion": PROTOCOL_VERSION,
        "runtimeVersion": "0.1.0",
        "upstreamVersion": "step-code",
        "runtimeInstanceId": instance_id,
        "runtimeEpoch": 12345,
        "capabilityManifest": { "profile": "limited" },
        "challengeProof": proof,
    });
    c.bench_function("handshake/verify", |b| {
        b.iter(|| {
            black_box(verify_hello(black_box(&hello), black_box(token), black_box(challenge)))
                .expect("verify")
        })
    });
}

fn jsonl_parsing(c: &mut Criterion) {
    // The shape `step --mode rpc` emits: one JSON object per line, with the
    // entry_appended flood that dominates a session's early output. Built with
    // `serde_json` rather than a format string so the fixture cannot drift from
    // valid JSON the way a hand-written one does.
    let mut jsonl = String::new();
    for index in 0..512 {
        let entry = json!({
            "type": "entry_appended",
            "entry": {
                "type": "custom",
                "customType": "step.activity",
                "data": {
                    "version": 1,
                    "sessionId": "01a1d10e-e022-7e1e-a16b-305b34783484",
                    "kind": "skill",
                    "stage": "discovered",
                    "resourceId": format!("/home/user/.claude/skills/skill-{index}/SKILL.md"),
                    "name": format!("$skill-{index}"),
                    "runId": format!("discovery:{index}"),
                    "startedAt": 1794508775945u64,
                },
                "id": format!("{index:08x}"),
                "parentId": format!("{index:08x}"),
                "timestamp": "2026-11-12T18:39:35.945Z",
            },
        });
        jsonl.push_str(&serde_json::to_string(&entry).expect("serialize"));
        jsonl.push('\n');
    }
    let bytes = jsonl.as_bytes();

    c.bench_function("jsonl/parse-512-entries", |b| {
        b.iter(|| {
            let mut count = 0usize;
            for line in black_box(bytes).split(|&b| b == b'\n') {
                if line.is_empty() {
                    continue;
                }
                let value: serde_json::Value = serde_json::from_slice(line).expect("parse");
                if value.get("type").is_some() {
                    count += 1;
                }
            }
            black_box(count)
        })
    });
}

criterion_group!(benches, frame_codec, handshake, jsonl_parsing);
criterion_main!(benches);
