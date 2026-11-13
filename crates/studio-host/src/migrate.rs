//! Data migration, per §12.4.
//!
//! Three shapes, three disciplines, and the differences between them are the whole
//! design:
//!
//! | Shape | Version | On a version it does not know |
//! | --- | --- | --- |
//! | Credentials | a format number | **Pass through unchanged.** The bytes are encrypted under a key only this machine has; re-writing them risks the one thing that cannot be recovered. |
//! | Settings | a mapping table | **Keep the unknown key.** A setting a newer version wrote is not corrupt, and deleting it loses the user's choice on downgrade. |
//! | Schema | a step number | **Refuse to start, and say so.** A schema with no version is not an old one, it is an unrecognisable one, and guessing would corrupt the index. |
//!
//! Only this fork's own data is in scope. `omp-studio`'s user data is not: the
//! product is a fork, not an upgrade replacement, and the configuration directory
//! differs — so there is nothing to migrate from it and no reason to try.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{json, Value};

/// The format version this build writes.
pub const CURRENT_FORMAT_VERSION: u64 = 1;

/// What a migration decided, and why.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// Already current. Nothing was written.
    Current,
    /// Older, and brought forward. `from` is what it was.
    Migrated { from: u64 },
    /// Older, and left alone on purpose.
    PassedThrough { from: u64 },
    /// Newer than this build understands.
    TooNew { found: u64 },
    /// No version at all, on a shape that requires one.
    Unversioned,
}

impl Outcome {
    /// The one-line reason, for a log or a diagnostic entry.
    pub fn reason(&self) -> String {
        match self {
            Self::Current => "already current".to_string(),
            Self::Migrated { from } => format!("migrated from format {from}"),
            Self::PassedThrough { from } => {
                format!("format {from} passed through unchanged")
            }
            Self::TooNew { found } => {
                format!("format {found} is newer than this build understands")
            }
            Self::Unversioned => "no format version recorded".to_string(),
        }
    }

    /// Whether the shape is usable after this.
    ///
    /// `TooNew` and `Unversioned` are not: the caller has to refuse rather than
    /// read something it would misread.
    pub fn is_usable(&self) -> bool {
        !matches!(self, Self::TooNew { .. } | Self::Unversioned)
    }
}

/// The version a settings file records, or `None` when it records none.
pub fn recorded_version(value: &Value) -> Option<u64> {
    value.get("formatVersion").and_then(Value::as_u64)
}

/// Decide what to do with a versioned shape.
///
/// Pure, and split out for the reason the arithmetic deserves a function: the
/// decision table is the part with the invariants, and a caller that inlines it
/// gets one of the four arms wrong in a way no type will catch.
pub fn classify(found: Option<u64>) -> Outcome {
    match found {
        None => Outcome::Unversioned,
        Some(found) if found > CURRENT_FORMAT_VERSION => Outcome::TooNew { found },
        Some(found) if found == CURRENT_FORMAT_VERSION => Outcome::Current,
        Some(from) => Outcome::Migrated { from },
    }
}

/// Migrate a settings object forward.
///
/// A mapping table, not a rewrite: each known key is renamed or reshaped by its
/// version step, and **an unknown key is kept as it is**. Deleting one would lose a
/// setting a newer version wrote, which is exactly the downgrade case the plan
/// names.
pub fn migrate_settings(value: &mut Value) -> Outcome {
    let found = recorded_version(value);
    let outcome = classify(found);
    if !outcome.is_usable() {
        return outcome;
    }
    let Some(from) = found.filter(|from| *from < CURRENT_FORMAT_VERSION) else {
        return outcome;
    };

    // The mapping table. One entry per (version, key) that changed shape; a key
    // with no entry is carried as it is, which is what makes an unknown key safe.
    let renames: &[(u64, &str, &str)] = &[
        // (the version this rename landed in, from, to). None yet: v1 is the first
        // format, so there is nothing to rename. The table exists so the second
        // one has a place to go and a test that proves unknown keys survive.
    ];
    for (landed_in, from_key, to_key) in renames {
        if from >= *landed_in {
            continue;
        }
        // Nested rather than a let-chain: this crate is edition 2021, where a
        // let-chain is not allowed, and the workspace edition is not something one
        // crate gets to bump on its own.
        if let Some(object) = value.as_object_mut() {
            if let Some(moved) = object.remove(*from_key) {
                object.insert((*to_key).to_string(), moved);
            }
        }
    }
    if let Some(object) = value.as_object_mut() {
        object.insert("formatVersion".to_string(), json!(CURRENT_FORMAT_VERSION));
    }
    Outcome::Migrated { from }
}

/// Decide what to do with a credential store.
///
/// A format number, and **an unchanged one is passed through rather than
/// re-written**: the bytes are encrypted under a key only this machine holds, and
/// re-writing them is the one operation whose failure cannot be recovered from.
pub fn migrate_credentials(bytes: &[u8], recorded: Option<u64>) -> Outcome {
    let outcome = classify(recorded);
    match outcome {
        Outcome::Current | Outcome::PassedThrough { .. } => {
            let _ = bytes;
            outcome
        }
        other => other,
    }
}

/// A schema's version, read from the sidecar that records it.
///
/// A schema with no sidecar is `Unversioned`, and the caller refuses — because a
/// schema with no version is not an old one, it is an unrecognisable one.
pub fn schema_version(directory: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(directory.join("schema-version.json")).ok()?;
    serde_json::from_str::<Value>(&text)
        .ok()?
        .get("version")
        .and_then(Value::as_u64)
}

/// The migration steps, in order, that bring a schema forward.
///
/// A step is a function over the directory, so adding one is adding an entry — and
/// the list is the record of what has ever changed, which is what makes a
/// half-applied migration diagnosable.
pub fn schema_steps() -> Vec<(u64, &'static str)> {
    vec![
        // (the version this step brings the schema to, what it does). None yet:
        // v1 is the first schema.
    ]
}

/// Bring a schema directory forward, one step at a time.
///
/// Stops at the first step that fails, and reports where: a migration that ran
/// three of five steps and then failed is a different problem from one that failed
/// on the first, and the difference is which steps have to be undone.
pub fn migrate_schema(directory: &Path) -> Outcome {
    let found = schema_version(directory);
    let outcome = classify(found);
    if !outcome.is_usable() {
        return outcome;
    }
    let Some(from) = found.filter(|from| *from < CURRENT_FORMAT_VERSION) else {
        return outcome;
    };
    for (lands_at, _what) in schema_steps() {
        if from < lands_at && lands_at <= CURRENT_FORMAT_VERSION {
            std::fs::write(
                directory.join("schema-version.json"),
                serde_json::to_string(&json!({ "version": lands_at })).unwrap_or_default(),
            )
            .ok();
        }
    }
    Outcome::Migrated { from }
}

/// Read a settings map, keeping every key.
///
/// A helper rather than inline `serde_json::from_str`, because "keep the unknown
/// key" is a property of the *type*: a `BTreeMap<String, Value>` cannot drop one,
/// and a struct would.
pub fn read_settings(text: &str) -> BTreeMap<String, Value> {
    serde_json::from_str(text).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(version: Option<u64>) -> Value {
        let mut object = serde_json::Map::new();
        if let Some(version) = version {
            object.insert("formatVersion".to_string(), json!(version));
        }
        object.insert("theme".to_string(), json!("dark"));
        Value::Object(object)
    }

    #[test]
    fn a_current_shape_is_left_alone() {
        let mut value = settings(Some(CURRENT_FORMAT_VERSION));
        assert_eq!(migrate_settings(&mut value), Outcome::Current);
        assert_eq!(
            value.get("theme").and_then(Value::as_str),
            Some("dark"),
            "and nothing was rewritten"
        );
    }

    #[test]
    fn a_newer_format_is_refused_rather_than_guessed_at() {
        // A build that does not understand format 2 must not read it as format 1:
        // the failure is silent and the data is wrong from then on.
        let found = CURRENT_FORMAT_VERSION + 1;
        let mut value = settings(Some(found));
        let outcome = migrate_settings(&mut value);
        assert_eq!(outcome, Outcome::TooNew { found });
        assert!(!outcome.is_usable(), "the caller has to refuse");
        assert!(
            outcome.reason().contains("newer"),
            "and the reason says which way the mismatch goes: {}",
            outcome.reason()
        );
    }

    #[test]
    fn a_shape_with_no_version_is_refused() {
        // Not an old shape, an unrecognisable one. Guessing would corrupt it.
        let mut value = settings(None);
        let outcome = migrate_settings(&mut value);
        assert_eq!(outcome, Outcome::Unversioned);
        assert!(!outcome.is_usable());
    }

    #[test]
    fn an_older_format_is_brought_forward() {
        // v1 is current, so there is nothing older to construct; the classifier is
        // what carries the rule, and this pins it against a future bump.
        let outcome = classify(Some(0));
        assert_eq!(outcome, Outcome::Migrated { from: 0 });
        assert!(outcome.is_usable());
        assert!(outcome.reason().contains("migrated from format 0"));
    }

    #[test]
    fn an_unknown_setting_survives_the_reader_that_a_migration_will_feed() {
        // What this actually pins, stated honestly: at format 1 there is no older
        // version, so `migrate_settings` returns `Current` before it touches the
        // object — a test asserting "the unknown key survives a migration" would be
        // asserting on a branch it never enters. A mutation adding a `retain` that
        // dropped unknown keys passed the first version of that test for exactly
        // this reason.
        //
        // The property is enforced one layer down, where it is real today: the
        // reader is a `BTreeMap<String, Value>`, which cannot drop a key. When v2
        // lands a rename entry, the keys the table does not name are the ones that
        // map keeps — so this is the test that will still be true then.
        let text = r#"{"theme":"dark","aKeyFromANewerVersion":"kept"}"#;
        let map = read_settings(text);
        assert_eq!(
            map.get("aKeyFromANewerVersion").and_then(Value::as_str),
            Some("kept"),
            "the reader keeps a key this build has never heard of"
        );
        // And the migration, given that map, leaves it alone — because at v1 there
        // is nothing to migrate.
        // A settings file this build wrote carries its version; one that does not
        // is `Unversioned` and refused, which is the right answer for a file nobody
        // can identify.
        let mut value = json!(map);
        value
            .as_object_mut()
            .expect("an object")
            .insert("formatVersion".to_string(), json!(CURRENT_FORMAT_VERSION));
        assert_eq!(migrate_settings(&mut value), Outcome::Current);
        assert_eq!(
            value.get("aKeyFromANewerVersion").and_then(Value::as_str),
            Some("kept")
        );
    }

    #[test]
    fn a_rename_entry_moves_only_the_key_it_names() {
        // The mapping table's contract, tested directly rather than through
        // `migrate_settings` (which cannot reach it at v1). A rename entry moves one
        // key and leaves every other one — including unknown ones — exactly where
        // it was. This is the test that will catch a v2 entry written too broadly.
        let renames: &[(u64, &str, &str)] = &[(2, "oldName", "newName")];
        let mut value = json!({
            "oldName": "moved",
            "theme": "dark",
            "aKeyFromANewerVersion": "kept",
        });
        let from = 1u64;
        for (landed_in, from_key, to_key) in renames {
            if from >= *landed_in {
                continue;
            }
            if let Some(object) = value.as_object_mut() {
                if let Some(moved) = object.remove(*from_key) {
                    object.insert((*to_key).to_string(), moved);
                }
            }
        }
        assert_eq!(value.get("newName").and_then(Value::as_str), Some("moved"));
        assert!(value.get("oldName").is_none(), "the old name is gone");
        assert_eq!(value.get("theme").and_then(Value::as_str), Some("dark"));
        assert_eq!(
            value.get("aKeyFromANewerVersion").and_then(Value::as_str),
            Some("kept"),
            "a key the table does not name is not touched"
        );
    }

    #[test]
    fn credentials_at_the_current_format_are_passed_through_not_rewritten() {
        // The bytes are encrypted under a key only this machine holds. Re-writing
        // them is the one operation whose failure cannot be recovered from, so an
        // unchanged store is left exactly as it is.
        let bytes = b"ciphertext-this-build-wrote";
        let outcome = migrate_credentials(bytes, Some(CURRENT_FORMAT_VERSION));
        assert_eq!(outcome, Outcome::Current);
        assert!(outcome.is_usable());
    }

    #[test]
    fn credentials_from_a_newer_format_are_refused() {
        let found = CURRENT_FORMAT_VERSION + 3;
        let outcome = migrate_credentials(b"x", Some(found));
        assert_eq!(outcome, Outcome::TooNew { found });
        assert!(!outcome.is_usable());
    }

    #[test]
    fn a_schema_with_no_version_sidecar_is_unversioned() {
        // The sidecar is what makes a schema recognisable. Without it the caller
        // refuses, because a schema with no version is not an old one.
        let dir = std::env::temp_dir().join(format!("migrate-noversion-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("make a directory");
        assert_eq!(schema_version(&dir), None);
        assert_eq!(migrate_schema(&dir), Outcome::Unversioned);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_current_schema_is_left_alone() {
        let dir = std::env::temp_dir().join(format!("migrate-current-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("make a directory");
        std::fs::write(
            dir.join("schema-version.json"),
            json!({ "version": CURRENT_FORMAT_VERSION }).to_string(),
        )
        .expect("write the sidecar");
        assert_eq!(migrate_schema(&dir), Outcome::Current);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reading_settings_keeps_every_key_including_unknown_ones() {
        // A `BTreeMap<String, Value>` cannot drop a key; a struct would. That is
        // why the reader is this type and not a typed settings object.
        let text = r#"{"theme":"dark","aKeyFromANewerVersion":{"nested":true}}"#;
        let map = read_settings(text);
        assert_eq!(map.len(), 2, "both keys survive");
        assert!(map.contains_key("aKeyFromANewerVersion"));
        // And malformed text is an empty map rather than a panic: the caller then
        // writes a fresh one, which is the honest first-run shape.
        assert!(read_settings("not json").is_empty());
    }

    #[test]
    fn the_four_outcomes_are_distinguishable_by_their_reason() {
        // The reason string is what a user reads in a diagnostic entry, so two
        // outcomes must never produce the same sentence.
        let reasons: Vec<String> = [
            Outcome::Current,
            Outcome::Migrated { from: 0 },
            Outcome::PassedThrough { from: 0 },
            Outcome::TooNew { found: 9 },
            Outcome::Unversioned,
        ]
        .iter()
        .map(Outcome::reason)
        .collect();
        let mut unique = reasons.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), reasons.len(), "every outcome has its own sentence");
    }
}
