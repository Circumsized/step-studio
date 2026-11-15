//! §12.4's "迁移后凭据可用", against the files a store actually leaves.
//!
//! The unit tests in `src/migrate.rs` pin the decision table in isolation. This
//! file pins what the acceptance case is about: a credential written by one
//! build is still readable after the format version moves — and the way that is
//! achieved is that **nothing was rewritten to achieve it**.
//!
//! Both the credential store and the schema record their version *beside* the
//! data rather than inside it, and for credentials that is load-bearing: the
//! ciphertext is AES-256-GCM under a key held in the OS keystore, so re-writing
//! it means decrypting and re-encrypting under a key that may not be reachable —
//! the one operation whose failure cannot be recovered from. So the version goes
//! in a sidecar, and the blob reaches the codec exactly as it was found.
//!
//! **What is not covered here, and why.** The ciphertext itself. `studio-host`
//! has no cipher and no keystore: the encrypted store lives in the Tauri shell
//! (`service_definitions.rs`), which depends on this crate and not the other way
//! round, so a test in here cannot open one. What is asserted instead is the
//! property the migration has to preserve for the credential to survive — the
//! bytes on disk are the bytes the codec is handed, byte for byte. The keystore
//! path cannot run on this machine either: there is no `org.freedesktop.secrets`
//! on the session bus, which is what `keyring`'s `sync-secret-service` backend
//! talks to. Checked with `busctl --user tree org.freedesktop.secrets`, which
//! answers "The name org.freedesktop.secrets was not provided by any .service
//! files". So the encrypted-store layer is not exercised here, and this file does
//! not claim to.

#![cfg(unix)]

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::json;
use step_studio_host::migrate::{self, Outcome, CURRENT_FORMAT_VERSION};

/// The nonce in front of the ciphertext, per the store's `nonce || ciphertext`.
const NONCE_BYTES: usize = 12;

/// The token a saved configuration carries in its `env` block.
const TOKEN: &str = "step-token-do-not-log";

/// A scratch directory, removed when the test ends.
struct Scratch {
    directory: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let directory =
            std::env::temp_dir().join(format!("migrate-store-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("make a scratch directory");
        Self { directory }
    }

    fn path(&self) -> &Path {
        &self.directory
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// Where a credential store keeps its ciphertext and its version.
///
/// The names are the shell's: `<state>/service-definitions.enc`, with the format
/// number in `service-definitions.enc.version` beside it.
fn blob_path(directory: &Path) -> PathBuf {
    directory.join("service-definitions.enc")
}

fn sidecar_path(directory: &Path) -> PathBuf {
    directory.join("service-definitions.enc.version")
}

/// What a build leaves behind: the blob, and the version recorded beside it.
///
/// `None` is a store written by a build that recorded no version at all.
fn leave_behind(directory: &Path, blob: &[u8], version: Option<u64>) {
    std::fs::write(blob_path(directory), blob).expect("write the store");
    if let Some(version) = version {
        std::fs::write(sidecar_path(directory), version.to_string())
            .expect("write the sidecar");
    }
}

/// What one reload did.
struct Reload {
    verdict: Outcome,
    /// The bytes the codec would open, when the verdict allowed a read at all.
    bytes: Option<Vec<u8>>,
}

/// One reload: decide from the sidecar, and only then open the file.
///
/// This is the caller's discipline rather than the store's — `migrate_credentials`
/// is handed the bytes and answers with a verdict, never with replacements — and
/// the two halves of it are the two things the acceptance case needs. A version
/// this build does not understand is refused *before* the read, so a file it
/// would misread is never opened. A version it does understand is read as it
/// stands, so the ciphertext survives.
fn reload(directory: &Path) -> Reload {
    // The sidecar read the way the shell reads it: text, trimmed, parsed.
    let recorded = std::fs::read_to_string(sidecar_path(directory))
        .ok()
        .and_then(|text| text.trim().parse().ok());
    let verdict = migrate::classify(recorded);
    if !verdict.is_usable() {
        return Reload { verdict, bytes: None };
    }
    let bytes = std::fs::read(blob_path(directory)).expect("read the store");
    let verdict = migrate::migrate_credentials(&bytes, recorded);
    if !verdict.is_usable() {
        return Reload { verdict, bytes: None };
    }
    Reload {
        verdict,
        bytes: Some(bytes),
    }
}

/// A store blob in the shape the shell writes: a nonce, then ciphertext.
///
/// The plaintext behind it is a saved configuration whose `env` block carries the
/// token — but the cipher is AES-256-GCM under a keystore key this crate cannot
/// reach (see the module note), so the payload is a stand-in. It is long enough
/// that a plaintext leak would be findable in it, which is what the assertion in
/// the first test checks.
fn store_blob() -> Vec<u8> {
    let mut blob = vec![0x5a; NONCE_BYTES];
    blob.extend_from_slice(&[0xa7; 40]);
    blob
}

/// A file's identity, for the assertion that matters most in this file.
///
/// The store writes temp-then-rename, so a rewrite moves the inode; an in-place
/// one moves the mtime; either moves the bytes. A test that checked only the
/// bytes would miss the rename, and one that checked only the mtime would miss a
/// rewrite on a filesystem with coarse timestamps.
#[derive(Debug, PartialEq, Eq)]
struct Stamp {
    bytes: Vec<u8>,
    inode: u64,
    modified: SystemTime,
}

fn stamp(path: &Path) -> Stamp {
    let metadata = std::fs::metadata(path).expect("stat the store");
    Stamp {
        bytes: std::fs::read(path).expect("read the store"),
        inode: metadata.ino(),
        modified: metadata.modified().expect("the store has a modification time"),
    }
}

#[test]
fn a_credential_store_at_the_recorded_format_reloads_without_being_rewritten() {
    // The acceptance case: a store this build wrote, reloaded. The token has to
    // come back, and the way it does is that nothing touched the file.
    let scratch = Scratch::new("current");
    let blob = store_blob();
    leave_behind(scratch.path(), &blob, Some(CURRENT_FORMAT_VERSION));
    let before = stamp(&blob_path(scratch.path()));

    let reload = reload(scratch.path());

    assert_eq!(reload.verdict, Outcome::Current);
    assert_eq!(
        reload.bytes.as_deref(),
        Some(blob.as_slice()),
        "the codec is handed the same bytes, so the token inside is the same token"
    );
    assert!(
        !blob
            .windows(TOKEN.len())
            .any(|window| window == TOKEN.as_bytes()),
        "and the token is not on disk in the clear"
    );
    assert_eq!(
        stamp(&blob_path(scratch.path())),
        before,
        "the store was not rewritten"
    );
    assert_eq!(
        std::fs::read_dir(scratch.path())
            .expect("list the store directory")
            .count(),
        2,
        "and nothing else was written beside it"
    );
}

#[test]
fn a_credential_from_an_older_format_is_passed_through_unchanged() {
    // The bump: the sidecar said 0, this build is at 1. There is no credential
    // migration step to run and there never will be — the bytes are encrypted
    // under a key that may not be reachable — so the only thing to do with an
    // older store is leave it exactly as it is, and report that rather than
    // reporting a rewrite that did not happen.
    let scratch = Scratch::new("older");
    let blob = store_blob();
    let from = CURRENT_FORMAT_VERSION - 1;
    leave_behind(scratch.path(), &blob, Some(from));
    let before = stamp(&blob_path(scratch.path()));

    let reload = reload(scratch.path());

    assert_eq!(reload.verdict, Outcome::PassedThrough { from });
    assert!(
        reload.verdict.is_usable(),
        "an older credential is still readable: {}",
        reload.verdict.reason()
    );
    assert_eq!(
        reload.bytes.as_deref(),
        Some(blob.as_slice()),
        "the token survives the version bump"
    );
    assert_eq!(
        stamp(&blob_path(scratch.path())),
        before,
        "and it was not rewritten to catch up"
    );
}

#[test]
fn a_credential_store_from_a_newer_format_is_refused_rather_than_guessed_at() {
    // A build that does not understand format 2 must not read it as format 1:
    // the failure is silent and the token is wrong from then on. Taking the blob
    // away entirely is the sharpest form of the assertion — the refusal has to
    // arrive from the sidecar, before the file is opened at all.
    let scratch = Scratch::new("newer");
    let blob = store_blob();
    let found = CURRENT_FORMAT_VERSION + 1;
    leave_behind(scratch.path(), &blob, Some(found));
    std::fs::remove_file(blob_path(scratch.path())).expect("take the store away");
    let before = stamp(&sidecar_path(scratch.path()));

    let reload = reload(scratch.path());

    assert_eq!(reload.verdict, Outcome::TooNew { found });
    assert!(!reload.verdict.is_usable(), "the caller has to refuse");
    assert!(
        reload.bytes.is_none(),
        "and the store was never read — there was no file left to read"
    );
    assert!(
        reload.verdict.reason().contains("newer"),
        "the reason says which way the mismatch goes: {}",
        reload.verdict.reason()
    );
    assert_eq!(
        stamp(&sidecar_path(scratch.path())),
        before,
        "nothing was rewritten to make it fit"
    );
    assert_eq!(
        std::fs::read_dir(scratch.path())
            .expect("list the store directory")
            .count(),
        1,
        "and no file was written in its place"
    );
}

#[test]
fn a_credential_store_with_no_version_sidecar_is_refused() {
    // Not an old store, an unrecognisable one. Guessing would read it wrong.
    let scratch = Scratch::new("unversioned");
    leave_behind(scratch.path(), &store_blob(), None);

    let reload = reload(scratch.path());

    assert_eq!(reload.verdict, Outcome::Unversioned);
    assert!(!reload.verdict.is_usable());
    assert!(reload.bytes.is_none(), "an unidentifiable store is not read");
}

#[test]
fn a_schema_at_the_recorded_version_is_not_rewritten() {
    // The schema is the shape that *does* write — `migrate_schema` bumps its
    // sidecar a step at a time — so this is where "not rewritten when the version
    // already matched" is a property of the code rather than of the caller. The
    // index is the user's: a rewrite that fails halfway is a different problem
    // from one that never ran.
    let scratch = Scratch::new("schema-current");
    let sidecar = scratch.path().join("schema-version.json");
    std::fs::write(&sidecar, json!({ "version": CURRENT_FORMAT_VERSION }).to_string())
        .expect("write the sidecar");
    let before = stamp(&sidecar);

    let outcome = migrate::migrate_schema(scratch.path());

    assert_eq!(outcome, Outcome::Current);
    assert_eq!(stamp(&sidecar), before, "a matching schema is not rewritten");
}
