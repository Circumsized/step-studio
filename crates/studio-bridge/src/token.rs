//! The bridge token: minted per daemon start, never compiled in.
//!
//! The token used to be the source constant `"step-studio"`, which every
//! installation shared and every reader of the binary could recover. That made
//! the HMAC handshake session-binding rather than authentication: any local
//! process could compute a valid proof. It is now 32 bytes from the OS CSPRNG,
//! written `0600` into the daemon's own state directory by the daemon that
//! mints it, and read from there by every surface.

use std::path::Path;

/// How many random bytes back a token. 256 bits is far past anything a local
/// process can guess inside the connect timeout.
const TOKEN_BYTES: usize = 32;

/// Mint a token: 32 OS-CSPRNG bytes, hex-encoded.
///
/// Hex rather than base64 because hex is what the rest of the bridge already
/// writes into files (`signing.rs` digests, the manifest `sha256` fields), so a
/// token in a log or a diff looks like its neighbours.
pub fn generate_token() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    // `getrandom` 0.2 spells it `getrandom`; 0.3 renamed it to `fill`.
    getrandom::getrandom(&mut bytes).expect("the OS CSPRNG is available");
    let mut out = String::with_capacity(TOKEN_BYTES * 2);
    for byte in bytes {
        for nibble in [byte >> 4, byte & 0x0f] {
            out.push(char::from_digit(nibble as u32, 16).expect("4 bits fit a digit"));
        }
    }
    out
}

/// Read the token a daemon published, or `None` when it cannot be read.
///
/// A missing or empty file is `None` rather than a typed error: every caller's
/// next step is the same — do not connect — and the reason worth showing a user
/// is about the daemon, not about which syscall failed.
///
/// Trims, because a token written with a trailing newline by hand would
/// otherwise never match the minted one.
pub fn read_token_file(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let token = text.trim();
    (!token.is_empty()).then(|| token.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("step-studio-token-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("make a temp dir");
        dir
    }

    #[test]
    fn a_minted_token_is_64_hex_characters() {
        let token = generate_token();
        assert_eq!(token.len(), TOKEN_BYTES * 2, "{token}");
        assert!(
            token
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "not hex: {token}"
        );
    }

    #[test]
    fn two_tokens_do_not_collide() {
        // Not a proof of randomness — a counter would pass too. It catches the
        // failure that actually happens: a generator that ignores its buffer
        // and returns the same zero-filled string every time.
        let first = generate_token();
        let second = generate_token();
        assert_ne!(first, second);
        assert!(!first.bytes().all(|byte| byte == b'0'));
    }

    #[test]
    fn a_token_file_round_trips_and_a_trailing_newline_is_ignored() {
        let dir = scratch("round-trip");
        let path = dir.join("token");
        let token = generate_token();
        std::fs::write(&path, format!("{token}\n")).expect("write the token");
        assert_eq!(read_token_file(&path).as_deref(), Some(token.as_str()));
    }

    #[test]
    fn a_missing_or_blank_token_file_is_none_rather_than_an_error() {
        let dir = scratch("absent");
        assert_eq!(read_token_file(&dir.join("absent")), None);
        let blank = dir.join("blank");
        std::fs::write(&blank, "  \n").expect("write whitespace");
        assert_eq!(read_token_file(&blank), None);
    }
}
