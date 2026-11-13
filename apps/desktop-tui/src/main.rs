//! Step Studio's TUI surface: the four dashboards §9.4 names.
//!
//! A client of the daemon, exactly like the Tauri shell — it attaches to the
//! Studio Bridge endpoint and holds no child of its own. That is what makes it
//! a *second* surface: the daemon multiplexes (M2.3), so the TUI and the GUI
//! can be attached at once, and each gets its own receipts back.

use std::io::{self, Stdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use step_studio_tui::app::App;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

/// How long a keypress wait lasts before the tick runs anyway.
///
/// Short enough that the dashboard refreshes on its own; long enough that an
/// idle TUI is not spinning. ratatui redraws only the cells that changed, so a
/// tick with nothing new costs one diff and no bytes.
const POLL: Duration = Duration::from_millis(200);

/// Whether the alternate screen and raw mode are currently ours to undo.
///
/// A flag rather than a `Drop` guard, because the undo has to happen from
/// `main`'s own control flow *and* from a panic hook, and a guard cannot be
/// reached from a hook that is already unwinding.
static TERMINAL_OURS: AtomicBool = AtomicBool::new(false);

/// Put the terminal back the way it was found.
///
/// Idempotent, and safe to call twice: the flag is what makes it so, and the
/// second call is a no-op rather than a second `LeaveAlternateScreen` written to
/// a terminal that is already back.
fn restore_terminal() {
    if !TERMINAL_OURS.swap(false, Ordering::SeqCst) {
        return;
    }
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
    let _ = disable_raw_mode();
}

fn main() -> Result<(), String> {
    // The daemon's endpoint, derived the same way the daemon derives it. Passing
    // it in is possible but then every caller has to know the rule.
    let endpoint = std::env::args()
        .nth(1)
        .unwrap_or_else(step_studio_host::lifecycle::default_endpoint);

    // A panic inside `run` used to unwind with raw mode and the alternate screen
    // still active, leaving the caller's shell with no echo and no line editing
    // — and the message about the panic was written into a screen nobody can
    // see. The hook restores first and says so second.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        previous_hook(info);
    }));

    let mut terminal = match enter_alternate_screen() {
        Ok(terminal) => terminal,
        Err(error) => {
            // Raw mode may already be on when the screen switch fails, so it is
            // undone here rather than assumed away. The caller's shell is not
            // this program's to leave broken.
            restore_terminal();
            let _ = std::panic::take_hook();
            return Err(error);
        }
    };
    let outcome = run(&mut terminal, &endpoint);
    // Put the terminal back before reporting anything: an error printed into an
    // alternate screen is an error nobody reads.
    restore_terminal();
    drop(terminal);
    outcome
}

/// Enter raw mode and the alternate screen, recording that we did.
fn enter_alternate_screen() -> Result<Terminal<CrosstermBackend<Stdout>>, String> {
    enable_raw_mode().map_err(|error| format!("无法进入 raw mode：{error}"))?;
    TERMINAL_OURS.store(true, Ordering::SeqCst);
    let mut stdout = io::stdout();
    if let Err(error) = execute!(stdout, EnterAlternateScreen) {
        return Err(format!("无法切换屏幕：{error}"));
    }
    Terminal::new(CrosstermBackend::new(stdout)).map_err(|error| format!("无法建终端：{error}"))
}

fn run(terminal: &mut Terminal<CrosstermBackend<Stdout>>, endpoint: &str) -> Result<(), String> {
    let mut app = App::attach(endpoint)?;
    loop {
        terminal
            .draw(|frame| app.draw(frame))
            .map_err(|error| format!("绘制失败：{error}"))?;
        if !event::poll(POLL).map_err(|error| format!("读取输入失败：{error}"))? {
            app.tick();
            continue;
        }
        // Only key presses. A release or a repeat would switch dashboards twice
        // for one key on terminals that report both.
        let Ok(Event::Key(key)) = event::read() else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Tab => app.next_dashboard(),
            KeyCode::BackTab => app.previous_dashboard(),
            KeyCode::Down | KeyCode::Char('j') => app.move_down(),
            KeyCode::Up | KeyCode::Char('k') => app.move_up(),
            _ => {}
        }
        app.tick();
    }
}
