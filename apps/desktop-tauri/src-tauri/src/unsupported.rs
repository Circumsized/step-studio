//! The Windows arms of the two features whose transport is a Unix domain socket.
//!
//! Both features talk to the runtime over a socket the runtime creates:
//! computer use reads frames off one, prediction streams turns off another.
//! On Unix that is `UnixStream`. On Windows there is no `std` equivalent, and
//! neither candidate is drop-in: a named pipe has no `set_read_timeout`, which
//! both protocols lean on, and TCP loopback needs a port that is then a local
//! attack surface. So the transport is not ported, and this is what the shell
//! does about it.
//!
//! **It answers, it does not vanish.** Every command these modules register
//! keeps its name and its shape on both platforms, and on Windows each returns
//! the same `{ "ok": false, "message" }` the Unix arm already returns when the
//! socket is gone. The renderer therefore needs no platform branch: the feature
//! reads as ended, with a reason, exactly as it does when the runtime exits
//! underneath it.
//!
//! The alternative — `#[cfg]`-ing the commands out of `generate_handler!` —
//! fails the same call with "command not found", which says nothing about why.
//!
//! Porting the transport is a change here and in the runtime, not a gate: the
//! timeout is the hard part, and `transport::wait_or_stop` in `studio-bridge`
//! is the shape that solves it.

use serde_json::{json, Value};

use crate::chrome::ChromeResult;

/// What a command answers when the feature has no transport here.
fn unavailable(feature: &str) -> ChromeResult<Value> {
    Ok(json!({
        "ok": false,
        "message": format!(
            "{feature} is not available on Windows yet: it reaches the runtime over a \
             Unix domain socket that has not been ported."
        ),
    }))
}

pub mod browser_observation {
    use serde_json::Value;

    use crate::chrome::ChromeResult;

    #[tauri::command]
    pub fn attach_browser_observation(
        _app: tauri::AppHandle,
        _window: tauri::Window,
        _input: Value,
    ) -> ChromeResult<Value> {
        super::unavailable("Computer use")
    }

    #[tauri::command]
    pub fn control_browser_observation(
        _window: tauri::Window,
        _input: Value,
    ) -> ChromeResult<Value> {
        super::unavailable("Computer use")
    }

    #[tauri::command]
    pub fn detach_browser_observation(
        _window: tauri::Window,
        _input: Value,
    ) -> ChromeResult<Value> {
        super::unavailable("Computer use")
    }

    #[tauri::command]
    pub fn read_computer_capture_command(_input: Value) -> ChromeResult<Value> {
        super::unavailable("Computer use")
    }

    /// Nothing is held, so there is nothing to drop.
    pub fn dispose_window(_window: &str) {}
}

pub mod prediction {
    use serde_json::Value;

    use crate::chrome::ChromeResult;

    #[tauri::command]
    pub fn attach_prediction(
        _app: tauri::AppHandle,
        _window: tauri::Window,
        _input: Value,
    ) -> ChromeResult<Value> {
        super::unavailable("Prompt prediction")
    }

    #[tauri::command]
    pub fn prediction_input(_window: tauri::Window, _input: Value) -> ChromeResult<Value> {
        super::unavailable("Prompt prediction")
    }

    #[tauri::command]
    pub fn detach_prediction(_window: tauri::Window, _input: Value) -> ChromeResult<Value> {
        super::unavailable("Prompt prediction")
    }

    #[tauri::command]
    pub fn import_prediction_history(
        _window: tauri::Window,
        _input: Value,
    ) -> ChromeResult<Value> {
        super::unavailable("Prompt prediction")
    }

    /// Nothing is held, so there is nothing to drop.
    pub fn dispose_channel(_window: &str) {}
}
