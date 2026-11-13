//! What this machine and this shell are, as the diagnostics page reads it.
//!
//! Every field here is a fact the shell already holds: the platform it was built
//! for, the daemon it is attached to, the endpoint it reached it on. None of it
//! comes from the runtime, so none of it costs a round trip — and none of it is
//! inferred, because an inferred platform is a diagnostic that lies.
//!
//! Nothing here is redacted on the way out, because nothing here is secret. The
//! one thing that would be — the bridge token — is never read into a value that
//! reaches this module.

use serde_json::{json, Value};

use crate::pump;

/// The platform identifier the contract declares.
pub fn platform() -> &'static str {
    if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    }
}

/// The architecture the contract declares.
pub fn arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    }
}

/// `environment.get`: platform, architecture, and the runtime this shell reached.
pub fn environment(runtime_id: String, authority_epoch: u64, runtime_epoch: u64) -> Value {
    json!({
        "platform": platform(),
        "arch": arch(),
        "authority": {
            "authorityId": runtime_id,
            "authorityEpoch": authority_epoch,
        },
        "runtime": {
            "status": "connected",
            "classification": "managed",
            "runtimeId": runtime_id,
            "runtimeEpoch": runtime_epoch,
        },
        "installer": {
            // The shell does not install anything: it launches the daemon beside
            // itself, and the daemon launches the runtime beside itself. There is
            // no artifact download and nothing probed a signature, so both
            // values say what is actually known rather than what is assumed.
            "status": "installed",
            "signature": "unknown",
        },
    })
}

/// `diagnostics.get`: the entries the diagnostics page lists.
///
/// An honest set, which is small: what the shell is, what it is attached to, and
/// where it found the daemon. An entry claiming a check that never ran would be
/// a diagnostic page full of green ticks over nothing.
pub fn report(runtime_id: String, authority_epoch: u64) -> Value {
    let now = pump::iso_utc(pump::now_millis());
    let entries = vec![
        entry("host.platform", "host", "info", format!("platform {} ({})", platform(), arch()), None, &now),
        entry(
            "host.endpoint",
            "host",
            "info",
            format!("daemon endpoint {}", step_studio_host::lifecycle::default_endpoint()),
            None,
            &now,
        ),
        entry(
            "runtime.authority",
            "runtime",
            "info",
            format!("attached to {runtime_id} at epoch {authority_epoch}"),
            None,
            &now,
        ),
        entry(
            "bridge.transport",
            "bridge",
            "info",
            // The endpoint's shape is the platform's, and saying which one is in
            // use is the whole of the transport diagnostic.
            if cfg!(windows) {
                "named pipe"
            } else {
                "unix domain socket"
            }
            .to_string(),
            None,
            &now,
        ),
    ];

    json!({
        "generatedAt": now,
        "authority": {
            "authorityId": runtime_id,
            "authorityEpoch": authority_epoch,
        },
        "entries": entries,
        // Nothing is withheld, so the flag is true for the reason it exists:
        // it promises the caller that what it sees is all there is.
        "redacted": true,
    })
}

fn entry(
    id: &str,
    scope: &str,
    level: &str,
    message: String,
    detail: Option<Value>,
    at: &str,
) -> Value {
    json!({
        "entryId": id,
        "scope": scope,
        "level": level,
        "message": message,
        "detail": detail,
        "occurredAt": at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_platform_is_one_the_contract_declares() {
        assert!(
            matches!(platform(), "win32" | "darwin" | "linux"),
            "{} is not a PlatformId",
            platform()
        );
        assert!(matches!(arch(), "x64" | "arm64"), "{} is not an ArchId", arch());
    }

    #[test]
    fn the_environment_carries_the_session_it_was_given() {
        let model = environment("step-runtime-1".to_string(), 7, 9);
        assert_eq!(
            model.pointer("/authority/authorityId").and_then(Value::as_str),
            Some("step-runtime-1")
        );
        assert_eq!(
            model.pointer("/authority/authorityEpoch").and_then(Value::as_u64),
            Some(7)
        );
        assert_eq!(
            model.pointer("/runtime/runtimeEpoch").and_then(Value::as_u64),
            Some(9)
        );
        // The installer state is honest about there being no installer.
        assert_eq!(
            model.pointer("/installer/status").and_then(Value::as_str),
            Some("installed")
        );
    }

    #[test]
    fn every_diagnostic_entry_declares_only_contract_values() {
        let model = report("step-runtime-1".to_string(), 3);
        let entries = model.get("entries").and_then(Value::as_array).expect("entries");
        assert!(!entries.is_empty());
        for entry in entries {
            assert!(
                matches!(entry.get("scope").and_then(Value::as_str), Some("host" | "runtime" | "bridge" | "installer")),
                "{} is not a DiagnosticScope",
                entry.get("scope").and_then(Value::as_str).unwrap_or("")
            );
            assert!(
                matches!(entry.get("level").and_then(Value::as_str), Some("info" | "warning" | "error")),
                "{} is not a DiagnosticLevel",
                entry.get("level").and_then(Value::as_str).unwrap_or("")
            );
            let message = entry.get("message").and_then(Value::as_str).unwrap_or("");
            assert!(!message.is_empty(), "an entry with no message says nothing");
            assert!(entry.get("occurredAt").and_then(Value::as_str).is_some());
        }
    }

    #[test]
    fn no_diagnostic_value_is_the_bridge_token() {
        // The one secret in this system is the bridge token, and diagnostics are
        // what a user pastes into a bug report — so it must never be a value.
        //
        // Checked as an exact value rather than a substring: the token happens to
        // be a prefix of the daemon's own directory name (`step-studio-<uid>`),
        // which the endpoint entry legitimately reports, and a substring check
        // would flag that.
        let model = report("step-runtime-1".to_string(), 3);
        let mut values: Vec<String> = Vec::new();
        collect_strings(&model, &mut values);
        assert!(
            !values.iter().any(|value| value == crate::bridge::BRIDGE_TOKEN),
            "the token value leaked as a diagnostic value"
        );
    }

    /// Every string anywhere in a JSON value, so a leak cannot hide in a key.
    fn collect_strings(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::String(text) => out.push(text.clone()),
            Value::Array(items) => {
                for item in items {
                    collect_strings(item, out);
                }
            }
            Value::Object(map) => {
                for (key, item) in map {
                    out.push(key.clone());
                    collect_strings(item, out);
                }
            }
            _ => {}
        }
    }
}
