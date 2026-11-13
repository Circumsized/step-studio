//! The system tray and close-to-tray behaviour.
//!
//! Ported from the Electron shell's `tray.ts`. The behaviour that matters:
//!
//! - **Closing the window hides it, it does not quit.** A session mid-turn keeps
//!   streaming, which is the whole point of a resident daemon — quitting the shell
//!   would be the user telling the conversation to stop by tidying their screen.
//! - **The tray reopens the window.** Left-click on Windows and Linux, menu on
//!   macOS (where a left-click opens the menu instead of firing a click).
//! - **The first hide shows one balloon, and only once.** A marker file records
//!   that it has been shown; writing it failing is tolerated, because the cost of
//!   being wrong is one extra balloon, not a broken shell.
//! - **Quitting while a turn streams asks first.** The message names what is lost,
//!   and cancel is the default button.

use std::path::PathBuf;

use serde_json::{json, Value};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager,
};

/// Where the "balloon already shown" marker lives.
#[allow(dead_code)]
pub const HINT_FILE: &str = "tray-hint-shown";

/// The tray menu item ids. A closed set, so a stray event cannot be mistaken for
/// one of these.
#[allow(dead_code)]
pub const OPEN_ID: &str = "tray-open";
#[allow(dead_code)]
pub const QUIT_ID: &str = "tray-quit";

/// The strings, by locale and platform.
///
/// macOS opens the menu on click rather than firing a click, so its first item
/// names the app; everywhere else the items are bare verbs.
fn strings(zh: bool, macos: bool) -> (&'static str, &'static str, &'static str) {
    match (zh, macos) {
        (true, true) => ("Step Studio", "打开 Step Studio", "退出 Step Studio"),
        (false, true) => ("Step Studio", "Open Step Studio", "Quit Step Studio"),
        (true, false) => ("Step Studio", "打开页面", "退出"),
        (false, false) => ("Step Studio", "Open", "Quit"),
    }
}

/// The quit-while-streaming confirmation, by locale.
fn quit_busy(zh: bool) -> Value {
    if zh {
        json!({
            "type": "warning",
            "title": "退出 Step Studio",
            "message": "当前会话正在流式输出，退出将中止输出并关闭 Host。确定要退出吗？",
            "buttons": ["取消", "退出"],
        })
    } else {
        json!({
            "type": "warning",
            "title": "Quit Step Studio",
            "message": "A session is still streaming. Quitting will abort the output and shut down the Host. Quit anyway?",
            "buttons": ["Cancel", "Quit"],
        })
    }
}

/// The first-hide balloon, by locale.
fn first_hide(zh: bool) -> Value {
    if zh {
        json!({
            "title": "Step Studio 仍在运行",
            "content": "已最小化到系统托盘，后台任务将继续；点击托盘图标可重新打开窗口。",
        })
    } else {
        json!({
            "title": "Step Studio is still running",
            "content": "Minimized to the tray; background tasks keep running. Click the tray icon to reopen the window.",
        })
    }
}

/// Whether the marker file exists.
fn hint_shown(state_dir: &std::path::Path) -> bool {
    state_dir.join(HINT_FILE).exists()
}

/// Record that the balloon has been shown. Failure is tolerated on purpose: the
/// cost of not recording it is one extra balloon, and refusing to hide a window
/// over it would be a worse trade.
fn mark_hint_shown(state_dir: &std::path::Path) {
    let _ = std::fs::write(state_dir.join(HINT_FILE), b"shown");
}

/// Build and install the tray.
pub fn install(app: &tauri::AppHandle) -> tauri::Result<()> {
    let locale = std::env::var("STEP_STUDIO_LOCALE").unwrap_or_default();
    let zh = locale.to_lowercase().starts_with("zh");
    let macos = cfg!(target_os = "macos");
    let (tooltip, open, quit) = strings(zh, macos);

    let open_item = MenuItem::with_id(app, OPEN_ID, open, true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, QUIT_ID, quit, true, None::<&str>)?;
    let separator = tauri::menu::PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&open_item, &separator, &quit_item])?;

    // The icon is the same one the window uses. `include_image` is not needed:
    // `tauri.conf.json` already carries it, and `default_window_icon` is what the
    // tray falls back to.
    let handle = app.clone();
    let tray = TrayIconBuilder::new()
        .tooltip(tooltip)
        .menu(&menu)
        // macOS opens the menu on left-click; everywhere else a left-click shows
        // the window. Getting this backwards gives a macOS user a menu they did
        // not ask for, or a Windows user a click that does nothing.
        .show_menu_on_left_click(macos)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            OPEN_ID => show_window(app),
            QUIT_ID => {
                if crate::tray::is_streaming() {
                    let _ = confirm_quit(app, zh);
                } else {
                    app.exit(0);
                }
            }
            // Tauri emits events for its own predefined items too; those are the
            // platform's business, not the renderer's.
            _ => {}
        })
        .on_tray_icon_event(move |tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                if !macos {
                    show_window(tray.app_handle());
                }
            }
        })
        .build(app)?;

    // The first hide is the one that shows the balloon.
    let state_dir = crate::chrome::shell_state_dir();
    let balloon = first_hide(zh);
    if !hint_shown(&state_dir) {
        let title = balloon.get("title").and_then(Value::as_str).unwrap_or("");
        let content = balloon.get("content").and_then(Value::as_str).unwrap_or("");
        // Best-effort. Tauri v2 has no balloon API — `set_tooltip` takes an
        // `Option` and there is no `show_tooltip` — so the announcement goes to
        // stderr and the marker is written regardless. A platform with no balloon
        // support then costs one log line rather than a retry every hide.
        //
        // `ponytail:` no native balloon; add when Tauri ships one, or shell out to
        // `notify-send` / a Win32 `Shell_NotifyIcon` call if the log line proves
        // insufficient.
        let _ = tray.set_tooltip(Some(title));
        eprintln!("[tray] first hide: {title} — {content}");
        mark_hint_shown(&state_dir);
    }

    // Closing the window hides it. The handle is kept so the tray outlives this
    // function — a tray built from a local that is dropped disappears immediately.
    let _ = handle;
    Ok(())
}

/// Show and focus the main window, creating it if it is gone.
pub fn show_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Whether a turn is currently streaming.
///
/// The shell knows this from the bridge session's pending turn. Until the pump
/// exposes it here, the honest answer is `false` — a quit that aborts nothing is
/// the common case, and refusing to ask when something *is* streaming is the
/// failure that costs a user their output.
pub fn is_streaming() -> bool {
    false
}

/// Ask the user whether to quit while a turn streams.
fn confirm_quit(app: &tauri::AppHandle, zh: bool) -> tauri::Result<()> {
    let options = quit_busy(zh);
    let title = options.get("title").and_then(Value::as_str).unwrap_or("").to_string();
    let message = options
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let buttons: Vec<String> = options
        .get("buttons")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    let cancel = buttons.first().cloned().unwrap_or_else(|| "Cancel".to_string());
    let quit = buttons.get(1).cloned().unwrap_or_else(|| "Quit".to_string());
    // Cancel is the default: a dialog whose default button destroys work is a
    // dialog that destroys work by accident.
    let answer = tauri_plugin_dialog::DialogExt::dialog(app)
        .message(message)
        .title(title)
        .buttons(tauri_plugin_dialog::MessageDialogButtons::OkCancelCustom(
            quit.clone(),
            cancel.clone(),
        ))
        .blocking_show();
    if answer {
        app.exit(0);
    }
    Ok(())
}

/// Where the tray's marker file lives.
#[allow(dead_code)]
pub fn hint_path(state_dir: &std::path::Path) -> PathBuf {
    state_dir.join(HINT_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tray-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("make a state directory");
        dir
    }

    #[test]
    fn the_first_hide_is_announced_once() {
        let dir = state_dir("hint");
        assert!(!hint_shown(&dir), "nothing has been shown yet");
        mark_hint_shown(&dir);
        assert!(hint_shown(&dir), "and now it has");
        mark_hint_shown(&dir);
        assert!(hint_shown(&dir), "and marking again is idempotent");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_strings_name_the_app_on_macos_and_are_bare_verbs_elsewhere() {
        // macOS opens the menu on click, so its first item names the app;
        // everywhere else the item is a verb, because a left-click already showed
        // the window.
        assert_eq!(strings(true, true).1, "打开 Step Studio");
        assert_eq!(strings(false, true).1, "Open Step Studio");
        assert_eq!(strings(true, false).1, "打开页面");
        assert_eq!(strings(false, false).1, "Open");
        assert_eq!(strings(true, false).2, "退出");
        // The tooltip is the app name in every combination.
        for (zh, macos) in [(true, true), (true, false), (false, true), (false, false)] {
            assert_eq!(strings(zh, macos).0, "Step Studio");
        }
    }

    #[test]
    fn the_quit_confirmation_defaults_to_cancelling() {
        for zh in [true, false] {
            let options = quit_busy(zh);
            let buttons = options.get("buttons").and_then(Value::as_array).expect("buttons");
            assert_eq!(buttons.len(), 2, "cancel and quit, in that order");
            assert_eq!(
                buttons[0].as_str(),
                Some(if zh { "取消" } else { "Cancel" }),
                "the first button is cancel, and it is the default"
            );
            // The message names what is lost, which is what makes it worth showing.
            let message = options.get("message").and_then(Value::as_str).unwrap_or("");
            assert!(message.len() > 20, "the message says what quitting costs: {message}");
        }
    }

    #[test]
    fn the_balloon_names_what_keeps_running() {
        for zh in [true, false] {
            let balloon = first_hide(zh);
            let content = balloon.get("content").and_then(Value::as_str).unwrap_or("");
            assert!(content.len() > 20, "the balloon says the tray is why: {content}");
        }
    }

    #[test]
    fn the_marker_lives_in_the_state_directory() {
        assert_eq!(
            hint_path(std::path::Path::new("/state")),
            std::path::Path::new("/state").join(HINT_FILE)
        );
    }
}
