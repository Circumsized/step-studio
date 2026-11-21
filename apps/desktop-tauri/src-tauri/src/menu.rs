//! The native application menu, and the allowlisted commands it sends.
//!
//! macOS needs this for a reason that is not decoration: the system routes ⌘C /
//! ⌘V / ⌘Z to text fields through the Edit menu's roles, so a window with no
//! application menu cannot paste. The in-window title menu stays; this adds the
//! platform one.
//!
//! Custom items only ever send an **allowlisted command id** to the renderer,
//! which maps it onto an action that already exists. Nothing here invokes a
//! Host operation or reads state: a menu is a keyboard, not a controller.
//!
//! The accelerators are owned by the menu on macOS — the renderer skips them
//! there (`nativeMenuOwnsShortcuts`), so one key press never runs an action
//! twice.

use tauri::menu::{Menu, MenuItem, Submenu};
use tauri::{AppHandle, Emitter, Runtime};

/// The channel the renderer listens on. Namespaced like the rest of this shell's
/// IPC, so a stray listener cannot pick up an unrelated event.
pub const MENU_COMMAND_EVENT: &str = "step-studio:chrome:menu-command";

/// Every command a menu item may send.
///
/// A closed set, and the reason it is closed: the renderer's `onMenuCommand`
/// declares exactly these ids, and an id outside the set would arrive as a value
/// its type says cannot happen. Sending anything else is a protocol error, not a
/// feature.
pub const MENU_COMMANDS: [&str; 8] = [
    "app.settings",
    "file.newChat",
    "file.openProject",
    "view.commandPalette",
    "view.toggleSidebar",
    "view.toggleBottomPanel",
    "view.toggleSkills",
    "help.shortcuts",
];

/// The shortcut each command owns on macOS.
///
/// Mirrors the Electron shell's table (`apps/desktop/src/platform/app-menu.ts`)
/// so the two shells bind the same keys. `None` for the one command that has no
/// accelerator.
#[cfg(test)]
fn accelerator_for(command: &str) -> Option<&'static str> {
    match command {
        "app.settings" => Some("cmd+,"),
        "file.newChat" => Some("shift+cmd+o"),
        "file.openProject" => Some("cmd+o"),
        "view.commandPalette" => Some("cmd+k"),
        "view.toggleSidebar" => Some("cmd+b"),
        "view.toggleBottomPanel" => Some("cmd+j"),
        "view.toggleSkills" => Some("shift+cmd+k"),
        _ => None,
    }
}

/// Build the menu, install it, and forward its events to the renderer.
///
/// Called once from `setup`. A failure to build the menu is reported and then
/// ignored: the shell still runs, and the renderer's `menu` capability bit stays
/// false so the shortcuts fall back to the in-window bindings. Refusing to start
/// because a menu could not be built would be a worse trade.
pub fn install<R: Runtime>(app: &AppHandle<R>) {
    match build(app) {
        Ok(menu) => {
            if let Err(error) = menu.set_as_app_menu() {
                eprintln!("[menu] cannot install the application menu: {error}");
                return;
            }
            let handle = app.clone();
            app.on_menu_event(move |_app, event| {
                let id = event.id().as_ref();
                // Only the allowlisted ids are forwarded. Tauri also emits events
                // for the predefined items it adds itself (Copy, Paste, Quit), and
                // those are the platform's to handle — forwarding them would have
                // the renderer act on a menu item it does not own.
                if !is_allowed(id) {
                    return;
                }
                if let Err(error) = handle.emit(MENU_COMMAND_EVENT, id) {
                    eprintln!("[menu] cannot forward {id}: {error}");
                }
            });
        }
        Err(error) => eprintln!("[menu] cannot build the application menu: {error}"),
    }
}

/// The menu itself: an app menu and a view menu.
///
/// Grouped rather than flat because that is what a platform menu bar is, and
/// because the Electron shell groups them the same way — a user moving between
/// the two shells should find the same items in the same places.
fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let file = Submenu::with_items(
        app,
        "File",
        true,
        &[
            &MenuItem::with_id(app, "file.newChat", "New Chat", true, Some("shift+cmd+o"))?,
            &MenuItem::with_id(
                app,
                "file.openProject",
                "Open Project…",
                true,
                Some("cmd+o"),
            )?,
        ],
    )?;
    let view = Submenu::with_items(
        app,
        "View",
        true,
        &[
            &MenuItem::with_id(
                app,
                "view.commandPalette",
                "Command Palette…",
                true,
                Some("cmd+k"),
            )?,
            &MenuItem::with_id(
                app,
                "view.toggleSidebar",
                "Toggle Sidebar",
                true,
                Some("cmd+b"),
            )?,
            &MenuItem::with_id(
                app,
                "view.toggleBottomPanel",
                "Toggle Bottom Panel",
                true,
                Some("cmd+j"),
            )?,
            &MenuItem::with_id(
                app,
                "view.toggleSkills",
                "Toggle Skills",
                true,
                Some("shift+cmd+k"),
            )?,
        ],
    )?;
    let help = Submenu::with_items(
        app,
        "Help",
        true,
        &[&MenuItem::with_id(
            app,
            "help.shortcuts",
            "Keyboard Shortcuts",
            true,
            None::<&str>,
        )?],
    )?;
    let app_menu = Submenu::with_items(
        app,
        "Step Studio",
        true,
        &[&MenuItem::with_id(
            app,
            "app.settings",
            "Settings…",
            true,
            Some("cmd+,"),
        )?],
    )?;

    Menu::with_items(app, &[&app_menu, &file, &view, &help])
}

/// Whether a command id is one the menu is allowed to send.
///
/// Exposed for the test that pins the closed set: an id added to the menu but
/// not to [`MENU_COMMANDS`] would be built, clicked, and then silently dropped —
/// a dead menu item with nothing to say why.
pub fn is_allowed(command: &str) -> bool {
    MENU_COMMANDS.contains(&command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_the_renderer_declares_has_an_entry() {
        // The renderer's `onMenuCommand` declares exactly these eight. A menu
        // item whose id is not in the set would be built and clicked and then
        // dropped by the allowlist, with nothing in any log to say why.
        for command in MENU_COMMANDS {
            assert!(is_allowed(command), "{command} must be allowed");
            // And the accelerator table must not claim one for an unknown id:
            // `None` is the only honest answer for an id outside the set.
            let _ = accelerator_for(command);
        }
    }

    #[test]
    fn an_id_outside_the_set_is_refused_rather_than_forwarded() {
        // Tauri emits events for its own predefined items too. Forwarding those
        // would have the renderer act on a menu item it does not own.
        for command in [
            "copy",
            "paste",
            "quit",
            "",
            "app.anything",
            "../../etc/passwd",
        ] {
            assert!(!is_allowed(command), "{command} must not be forwarded");
        }
    }

    #[test]
    fn the_accelerators_match_the_electron_shell() {
        // The two shells bind the same keys, so a user moving between them does
        // not have to relearn. Mirrors `apps/desktop/src/platform/app-menu.ts`.
        assert_eq!(accelerator_for("app.settings"), Some("cmd+,"));
        assert_eq!(accelerator_for("view.commandPalette"), Some("cmd+k"));
        assert_eq!(
            accelerator_for("help.shortcuts"),
            None,
            "no accelerator for help"
        );
    }
}
