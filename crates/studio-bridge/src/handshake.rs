//! The Studio Hello handshake. The runtime proves it holds the bridge token by
//! answering a challenge; the host verifies before trusting anything else.

use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Protocol version this client speaks. Mirrors STUDIO_PROTOCOL_VERSION.
pub const PROTOCOL_VERSION: u32 = 1;

pub type HmacSha256 = Hmac<Sha256>;

/// HMAC-SHA256(token, challenge + NUL + runtimeInstanceId), base64url.
///
/// The NUL separator is load-bearing: without it a challenge and an instance
/// id that concatenate to the same bytes would produce the same proof.
/// How much randomness a challenge carries.
const CHALLENGE_BYTES: usize = 16;

/// A challenge nobody could have chosen in advance.
///
/// The challenger picks the challenge, which is the normal shape; what was wrong
/// is that both sides picked a *deterministic* one derived from the epoch
/// (`daemon-{epoch}`, `shell-{epoch}`). That still blocks replaying a proof
/// across a daemon restart, which is what its comment claimed, but inside one
/// epoch the proof is a fixed function of (token, instance id, epoch): capture
/// one exchange and it is valid for the rest of that daemon's life. A challenge
/// with no freshness is not a challenge.
///
/// The epoch prefix is kept, so the cross-restart property survives alongside
/// the new per-connection one.
pub fn fresh_challenge(prefix: &str, epoch: u64) -> String {
    let mut bytes = [0u8; CHALLENGE_BYTES];
    // `getrandom` 0.2 spells it `getrandom`; 0.3 renamed it to `fill`.
    getrandom::getrandom(&mut bytes).expect("the OS CSPRNG is available");
    let mut out = String::with_capacity(prefix.len() + 24 + CHALLENGE_BYTES * 2);
    out.push_str(prefix);
    out.push('-');
    out.push_str(&epoch.to_string());
    out.push('-');
    for byte in bytes {
        for nibble in [byte >> 4, byte & 0x0f] {
            out.push(char::from_digit(nibble as u32, 16).expect("4 bits fit a digit"));
        }
    }
    out
}

pub fn challenge_proof(token: &str, challenge: &str, runtime_instance_id: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(token.as_bytes()).expect("HMAC accepts any key length");
    mac.update(challenge.as_bytes());
    mac.update(b"\0");
    mac.update(runtime_instance_id.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

/// What a verified hello establishes about the runtime on the other end.
#[derive(Debug, Clone)]
pub struct HelloResult {
    pub selected_protocol_version: u32,
    pub runtime_version: String,
    pub upstream_version: String,
    pub runtime_instance_id: String,
    pub runtime_epoch: u64,
    pub capability_profile: String,
}

/// Build the `studio.hello` request body.
pub fn hello_request(challenge: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "studio.hello",
        "requestId": crate::frame::next_frame_id(),
        "supportedProtocolVersions": [PROTOCOL_VERSION],
        "requiredProfile": "limited",
        "challenge": challenge,
    })
}

/// The frame a peer sends to prove it holds the token, answering a challenge the
/// other side chose.
///
/// The other half of the handshake. NIST SP 800-63B-4 defines a
/// challenge-response protocol as one where "the verifier sends the claimant a
/// challenge", and RFC 4949 is explicit about what a claimant-chosen challenge
/// buys: at most *data origin authentication*, never *peer entity authentication*,
/// because no *liveness* is established. Before this existed, the daemon signed
/// whatever challenge it was handed, so any peer could pick a value and be given
/// a valid proof for it.
pub fn hello_proof(token: &str, challenge: &str, runtime_instance_id: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "studio.hello.proof",
        "challengeProof": challenge_proof(token, challenge, runtime_instance_id),
    })
}

/// Verify a proof frame against the challenge this side issued.
///
/// Separate from [`verify_hello`] because a proof frame carries only the proof:
/// the identity was already established by the hello it answers, and requiring
/// the full hello shape here would reject a correct answer.
pub fn verify_proof(
    body: &serde_json::Value,
    token: &str,
    challenge: &str,
    runtime_instance_id: &str,
) -> Result<(), String> {
    let expected = challenge_proof(token, challenge, runtime_instance_id);
    let actual = body
        .get("challengeProof")
        .and_then(|v| v.as_str())
        .ok_or("hello.proof is missing challengeProof")?;
    if !same_bytes(expected.as_bytes(), actual.as_bytes()) {
        return Err("challenge proof mismatch".to_string());
    }
    Ok(())
}

/// Verify a hello result against the expected proof and extract its identity.
pub fn verify_hello(
    body: &serde_json::Value,
    token: &str,
    challenge: &str,
) -> Result<HelloResult, String> {
    let instance_id = body
        .get("runtimeInstanceId")
        .and_then(|v| v.as_str())
        .ok_or("hello is missing runtimeInstanceId")?;
    let expected = challenge_proof(token, challenge, instance_id);
    let actual = body
        .get("challengeProof")
        .and_then(|v| v.as_str())
        .ok_or("hello is missing challengeProof")?;
    // Constant-time: `actual != expected` on strings short-circuits on the first
    // differing byte, which leaks how much of a guessed proof was right. Both
    // sides are hex of a fixed-length digest, so the lengths always match when
    // the bytes do.
    if !same_bytes(expected.as_bytes(), actual.as_bytes()) {
        return Err("challenge proof mismatch".to_string());
    }
    let version = body
        .get("selectedProtocolVersion")
        .and_then(|v| v.as_u64())
        .ok_or("hello is missing selectedProtocolVersion")? as u32;
    if version != PROTOCOL_VERSION {
        return Err(format!("unsupported protocol version {version}"));
    }
    Ok(HelloResult {
        selected_protocol_version: version,
        runtime_version: str_field(body, "runtimeVersion")?,
        upstream_version: str_field(body, "upstreamVersion")?,
        runtime_instance_id: instance_id.to_string(),
        runtime_epoch: body.get("runtimeEpoch").and_then(|v| v.as_u64()).unwrap_or(1),
        capability_profile: body
            .get("capabilityManifest")
            .and_then(|m| m.get("profile"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string(),
    })
}

/// Compare two byte strings without leaking where they first differ.
fn same_bytes(expected: &[u8], actual: &[u8]) -> bool {
    if expected.len() != actual.len() {
        return false;
    }
    let mut diff = 0u8;
    for (left, right) in expected.iter().zip(actual.iter()) {
        diff |= left ^ right;
    }
    diff == 0
}

fn str_field(body: &serde_json::Value, key: &str) -> Result<String, String> {
    body.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("hello is missing {key}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A peer that cannot answer the challenge is refused.
    ///
    /// The negative PoC for the mutual-handshake fix: before it, the daemon and
    /// the runtime signed whatever challenge they were handed and dropped their
    /// own verification, so any peer that could open the endpoint picked a value
    /// and was given a valid proof for it. This pins the other direction — a
    /// proof computed with the wrong secret does not verify.
    #[test]
    fn a_proof_computed_with_the_wrong_token_is_refused() {
        let challenge = fresh_challenge("daemon", 1);
        let instance = "step-runtime-1";

        let honest = challenge_proof("the-real-token", &challenge, instance);
        assert!(
            verify_proof(
                &serde_json::json!({ "type": "studio.hello.proof", "challengeProof": honest }),
                "the-real-token",
                &challenge,
                instance,
            )
            .is_ok(),
            "the honest proof was refused"
        );

        // Wrong token, wrong instance, and a proof minted for a different
        // challenge: each is a peer that does not hold the secret.
        for forged in [
            challenge_proof("another-token", &challenge, instance),
            challenge_proof("the-real-token", &challenge, "step-runtime-2"),
            challenge_proof("the-real-token", &fresh_challenge("daemon", 1), instance),
        ] {
            assert!(
                verify_proof(
                    &serde_json::json!({ "type": "studio.hello.proof", "challengeProof": forged }),
                    "the-real-token",
                    &challenge,
                    instance,
                )
                .is_err(),
                "a forged proof was accepted"
            );
        }

        // And a frame that is not a proof at all.
        assert!(
            verify_proof(
                &serde_json::json!({ "type": "studio.request" }),
                "the-real-token",
                &challenge,
                instance,
            )
            .is_err(),
            "a frame with no proof was accepted"
        );
    }

    #[test]
    fn a_challenge_is_fresh_every_time() {
        // The defect this replaces: both sides derived the challenge from the
        // epoch (`daemon-{epoch}`, `shell-{epoch}`), so inside one epoch the proof
        // was a fixed function of (token, instance id, epoch) and one captured
        // exchange stayed valid for the rest of that daemon's life.
        let first = fresh_challenge("shell", 7);
        let second = fresh_challenge("shell", 7);
        assert_ne!(first, second, "two challenges for the same epoch collided");
        // And the epoch still rides along, so a proof minted for a previous daemon
        // does not verify against this one either.
        assert!(first.starts_with("shell-7-"), "the epoch prefix was lost: {first}");
        assert_ne!(fresh_challenge("shell", 7), fresh_challenge("shell", 8));
    }

    #[test]
    fn a_proof_for_one_challenge_does_not_verify_against_another() {
        // The property the deterministic challenge lacked. A verifier that is fed
        // a proof computed for a different challenge must refuse it.
        let token = "a-token";
        let instance = "instance-1";
        let mine = fresh_challenge("shell", 1);
        let theirs = fresh_challenge("shell", 1);
        assert_ne!(mine, theirs, "the fixture needs two distinct challenges");

        let proof_for_mine = challenge_proof(token, &mine, instance);
        let body = serde_json::json!({
            "runtimeInstanceId": instance,
            "challengeProof": proof_for_mine,
            "selectedProtocolVersion": PROTOCOL_VERSION,
            "runtimeVersion": "1",
            "upstreamVersion": "1",
        });
        assert!(
            verify_hello(&body, token, &theirs).is_err(),
            "a proof minted for another challenge was accepted"
        );
        // And the right challenge still verifies, so the check is not just refusing.
        assert!(verify_hello(&body, token, &mine).is_ok());
    }

    #[test]
    fn every_byte_and_the_length_of_the_proof_are_compared() {
        // What this checks: the comparison classifies equal input, every
        // single-byte difference, a length difference, and empty-vs-not
        // correctly. That is correctness, and it is all a functional test can
        // check.
        //
        // What it does NOT check: that the comparison is constant-time. Renamed
        // from `the_proof_comparison_does_not_short_circuit`, which claimed that
        // property — verified false by replacing the body with an early-exit
        // loop, which is functionally identical and left all eight tests green.
        // Constant-time-ness is a property of the implementation and is verified
        // by reading `same_bytes`; no timing assertion belongs in the suite,
        // because timing tests are flaky and a flaky test trains people to
        // ignore red.
        assert!(same_bytes(b"abc", b"abc"));
        for index in 0..b"abc".len() {
            let mut changed = *b"abc";
            changed[index] ^= 1;
            assert!(!same_bytes(b"abc", &changed), "byte {index} was ignored");
        }
        assert!(!same_bytes(b"abc", b"abcd"), "length was ignored");
        assert!(!same_bytes(b"", b"x"));
    }

    /// RFC 4231's HMAC-SHA-256 vectors, test cases 1 and 2.
    ///
    /// These verify the *primitive* against an independent, standards-track
    /// source rather than against itself. The two cases our own construction
    /// cannot express directly (the RFC's longer keys are 0xaa bytes, which are
    /// not valid UTF-8 and so cannot be a `&str` token) are the ones that matter
    /// least: the long-key path is the `hmac` crate's business, and it is the
    /// crate that these vectors check.
    ///
    /// Why bother, when the crate has its own tests: the failure this catches is
    /// not a broken crate, it is a *swap* — a different hash, a different MAC
    /// construction, or a different base64 variant reaching this call site. None
    /// of the suite's other tests would notice, because they all compare
    /// `challenge_proof` against `challenge_proof`.
    #[test]
    fn the_hmac_primitive_matches_rfc_4231() {
        // Test case 1: key = 0x0b x20, data = "Hi There".
        let mut mac = HmacSha256::new_from_slice(&[0x0b; 20]).expect("any key length");
        mac.update(b"Hi There");
        let digest = mac.finalize().into_bytes();
        assert_eq!(
            hex(&digest),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
            "RFC 4231 test case 1"
        );

        // Test case 2: key = "Jefe", data = "what do ya want for nothing?".
        let mut mac = HmacSha256::new_from_slice(b"Jefe").expect("any key length");
        mac.update(b"what do ya want for nothing?");
        let digest = mac.finalize().into_bytes();
        assert_eq!(
            hex(&digest),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
            "RFC 4231 test case 2"
        );
    }

    /// The separator the module doc calls load-bearing, proven load-bearing.
    ///
    /// Without the NUL, a challenge and an instance id that concatenate to the
    /// same bytes produce the same proof, so a runtime could answer a challenge
    /// it was never asked. The doc comment asserted this; nothing tested it.
    #[test]
    fn the_nul_separator_prevents_a_concatenation_ambiguity() {
        let proof = challenge_proof("token", "a", "bc");
        // Same bytes, different split. Equal here would mean the separator is
        // decorative and the ambiguity the doc names is real.
        assert_ne!(
            proof,
            challenge_proof("token", "ab", "c"),
            "a different challenge/instance split produced the same proof"
        );
        // And an instance id that merely starts with the separator must not be
        // able to forge a different pairing either.
        assert_ne!(proof, challenge_proof("token", "a\0b", "c"));
    }

    /// The proof depends on the challenge, not only on the instance id.
    ///
    /// The suite checked instance-id sensitivity and determinism; a proof that
    /// ignored its challenge would pass both.
    #[test]
    fn the_proof_depends_on_the_challenge() {
        assert_ne!(
            challenge_proof("token", "challenge-1", "rt-1"),
            challenge_proof("token", "challenge-2", "rt-1"),
            "a different challenge produced the same proof"
        );
        // And on the token, which is the whole point of a proof.
        assert_ne!(
            challenge_proof("token-a", "c", "rt-1"),
            challenge_proof("token-b", "c", "rt-1"),
            "a different token produced the same proof"
        );
    }

    /// The encoding is base64url without padding, and it round-trips.
    ///
    /// A standard-base64 or padded variant would still be a string, so nothing
    /// else in the suite would catch it — and a peer that decoded with the wrong
    /// variant would fail only on some inputs.
    #[test]
    fn the_encoding_is_base64url_without_padding() {
        let proof = challenge_proof("token", "challenge", "rt-1");
        assert!(!proof.contains('='), "the proof is padded");
        assert!(!proof.contains('+'), "the proof is standard base64, not base64url");
        assert!(!proof.contains('/'), "the proof is standard base64, not base64url");
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&proof)
            .expect("the proof is valid base64url");
        assert_eq!(bytes.len(), 32, "HMAC-SHA256 is 32 bytes");
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}
