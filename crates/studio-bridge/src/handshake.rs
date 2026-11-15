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
    if actual != expected {
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

fn str_field(body: &serde_json::Value, key: &str) -> Result<String, String> {
    body.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("hello is missing {key}"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
