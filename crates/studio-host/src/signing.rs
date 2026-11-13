//! Signed update manifests, per §12.4: "ed25519 清单 + 公钥 bake + 版本签入防降级".
//!
//! A manifest names the artifact, its version and its digest. The daemon verifies
//! the signature against a public key compiled into the binary before it will
//! replace anything, and refuses a version at or below the one it is running —
//! otherwise a downgrade is an attack that replays an older, validly-signed
//! manifest.
//!
//! The signing key never exists in this crate: only the verifying key is baked
//! in, and signing happens on the release machine. That separation is the whole
//! point — a daemon that can sign can also be made to sign anything.

use std::path::Path;

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

/// The public key the daemon verifies against.
///
/// A placeholder until a real release key is minted and baked at build time; the
/// tests below mint their own. `pub` so a build script can replace it.
pub const VERIFYING_KEY_BYTES: [u8; 32] = [0u8; 32];

/// What a manifest asserts, and what the signature covers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    /// Artifact version, compared as an integer to refuse a downgrade.
    pub version: u64,
    /// Which artifact this is, so a manifest for one thing cannot install another.
    pub artifact: String,
    /// Hex-encoded SHA-256 of the artifact bytes.
    pub sha256: String,
    /// Where to fetch it. Never trusted: the digest is what decides.
    pub url: String,
}

/// A manifest plus the signature over its canonical encoding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedManifest {
    pub manifest: Manifest,
    /// Hex-encoded ed25519 signature.
    pub signature: String,
}

/// Why a manifest was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    /// The signature does not verify against the baked key.
    BadSignature,
    /// The manifest is malformed — a missing field, a non-hex digest.
    Malformed(String),
    /// The version is not newer than what is running.
    NotNewer { offered: u64, running: u64 },
    /// The artifact's digest does not match the manifest.
    DigestMismatch { expected: String, actual: String },
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadSignature => write!(f, "the manifest signature does not verify"),
            Self::Malformed(detail) => write!(f, "malformed manifest: {detail}"),
            Self::NotNewer { offered, running } => {
                write!(f, "version {offered} is not newer than the running {running}")
            }
            Self::DigestMismatch { expected, actual } => {
                write!(f, "artifact digest {actual} does not match the manifest's {expected}")
            }
        }
    }
}

impl std::error::Error for Rejection {}

/// The bytes a signature covers: the manifest's canonical JSON.
///
/// Canonical because a signature over a re-serializable form is not a signature
/// over anything in particular — two encodings of the same manifest would both
/// have to verify, and only one of them was signed.
pub fn signing_bytes(manifest: &Manifest) -> Vec<u8> {
    serde_json::to_vec(manifest).unwrap_or_default()
}

/// Sign a manifest. Release-side only; the daemon never calls this.
pub fn sign(manifest: &Manifest, key: &SigningKey) -> SignedManifest {
    let signature = key.sign(&signing_bytes(manifest));
    SignedManifest {
        manifest: manifest.clone(),
        signature: hex(&signature.to_bytes()),
    }
}

/// Verify a signed manifest against `verifying`, and refuse anything that is
/// not newer than `running_version`.
pub fn verify(
    signed: &SignedManifest,
    verifying: &VerifyingKey,
    running_version: u64,
) -> Result<Manifest, Rejection> {
    let Ok(signature_bytes) = hex_decode(&signed.signature) else {
        return Err(Rejection::Malformed("signature is not hex".to_string()));
    };
    let Ok(signature) = Signature::from_slice(&signature_bytes) else {
        return Err(Rejection::Malformed("signature is not 64 bytes".to_string()));
    };
    verifying
        .verify(&signing_bytes(&signed.manifest), &signature)
        .map_err(|_| Rejection::BadSignature)?;

    if signed.manifest.version <= running_version {
        return Err(Rejection::NotNewer {
            offered: signed.manifest.version,
            running: running_version,
        });
    }
    Ok(signed.manifest.clone())
}

/// Check an artifact's bytes against a verified manifest's digest.
pub fn check_digest(manifest: &Manifest, artifact: &[u8]) -> Result<(), Rejection> {
    use sha2::{Digest as _, Sha256};
    let actual = hex(&Sha256::digest(artifact));
    if actual != manifest.sha256 {
        return Err(Rejection::DigestMismatch {
            expected: manifest.sha256.clone(),
            actual,
        });
    }
    Ok(())
}

/// Read an artifact from disk and check it against a verified manifest.
pub fn check_artifact(manifest: &Manifest, path: &Path) -> Result<(), Rejection> {
    let bytes = std::fs::read(path).map_err(|error| Rejection::Malformed(format!("read artifact: {error}")))?;
    check_digest(manifest, &bytes)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hex_decode(text: &str) -> Result<Vec<u8>, Rejection> {
    if text.len() % 2 != 0 {
        return Err(Rejection::Malformed("odd-length hex".to_string()));
    }
    (0..text.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&text[index..index + 2], 16)
                .map_err(|_| Rejection::Malformed("not hex".to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keypair() -> (SigningKey, VerifyingKey) {
        // Deterministic, so a failure is reproducible.
        let signing = SigningKey::from_bytes(&[7u8; 32]);
        let verifying = signing.verifying_key();
        (signing, verifying)
    }

    /// A manifest whose digest is the real one for `[0u8; 32]`, so the matching
    /// case is a real match rather than a placeholder that happens to differ.
    fn manifest() -> Manifest {
        use sha2::{Digest as _, Sha256};
        Manifest {
            version: 2,
            artifact: "step-studio-host".to_string(),
            sha256: hex(&Sha256::digest([0u8; 32])),
            url: "https://example.invalid/host-2".to_string(),
        }
    }

    #[test]
    fn a_signed_manifest_verifies() {
        let (signing, verifying) = keypair();
        let signed = sign(&manifest(), &signing);
        let verified = verify(&signed, &verifying, 1).expect("verifies");
        assert_eq!(verified, manifest());
    }

    #[test]
    fn a_manifest_signed_by_another_key_is_refused() {
        let (_, verifying) = keypair();
        // A different signer over the same bytes.
        let other = SigningKey::from_bytes(&[9u8; 32]);
        let signed = sign(&manifest(), &other);
        assert_eq!(
            verify(&signed, &verifying, 1),
            Err(Rejection::BadSignature),
            "a signature from another key must not verify"
        );
    }

    #[test]
    fn a_tampered_field_invalidates_the_signature() {
        let (signing, verifying) = keypair();
        let mut signed = sign(&manifest(), &signing);
        // The digest is the field an attacker would change: point the manifest
        // at a different artifact while keeping the original signature.
        signed.manifest.sha256 = hex(&[0xcd; 32]);
        assert_eq!(verify(&signed, &verifying, 1), Err(Rejection::BadSignature));
    }

    #[test]
    fn a_downgrade_is_refused_even_though_it_is_validly_signed() {
        // The attack this refuses: replay an older manifest. Its signature is
        // perfectly good, which is exactly why the version check has to exist
        // separately from the signature check.
        let (signing, verifying) = keypair();
        let mut older = manifest();
        older.version = 1;
        let signed = sign(&older, &signing);
        assert_eq!(
            verify(&signed, &verifying, 1),
            Err(Rejection::NotNewer { offered: 1, running: 1 })
        );
        // And the same version as what is running is also refused.
        let mut same = manifest();
        same.version = 5;
        let signed = sign(&same, &signing);
        assert_eq!(
            verify(&signed, &verifying, 5),
            Err(Rejection::NotNewer { offered: 5, running: 5 })
        );
    }

    #[test]
    fn an_artifact_whose_digest_does_not_match_is_refused() {
        let manifest = manifest();
        let mut artifact = vec![0u8; 32];
        assert!(check_digest(&manifest, &artifact).is_ok());
        artifact[0] = 1;
        assert!(matches!(
            check_digest(&manifest, &artifact),
            Err(Rejection::DigestMismatch { .. })
        ));
    }

    #[test]
    fn a_signature_that_is_not_hex_is_malformed_rather_than_bad() {
        // The two refusals mean different things to whoever reads the log: a
        // malformed manifest is a packaging bug, a bad signature is an attack.
        let (_, verifying) = keypair();
        let mut signed = sign(&manifest(), &SigningKey::from_bytes(&[7u8; 32]));
        signed.signature = "not hex".to_string();
        assert!(matches!(
            verify(&signed, &verifying, 1),
            Err(Rejection::Malformed(_))
        ));
    }
}
