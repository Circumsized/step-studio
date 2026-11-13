//! Step Studio's TUI surface: the four dashboards §9.4 names.
//!
//! A client of the daemon, exactly like the Tauri shell — it attaches to the
//! Studio Bridge endpoint and holds no child of its own. That is what makes it
//! a *second* surface: the daemon multiplexes (M2.3), so the TUI and the GUI
//! can be attached at once, and each gets its own receipts back.

use std::io::{self, Stdout};
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

fn main() -> Result<(), String> {
    // The daemon's endpoint, derived the same way the daemon derives it. Passing
    // it in is possible but then every caller has to know the rule.
    let endpoint = std::env::args()
        .nth(1)
        .unwrap_or_else(step_studio_host::lifecycle::default_endpoint);

    let mut terminal = restore_on_exit(EnterAlternateScreen)?;
    let outcome = run(&mut terminal, &endpoint);
    // Put the terminal back before reporting anything: an error printed into an
    // alternate screen is an error nobody reads.
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
    let _ = disable_raw_mode();
    drop(terminal);
    outcome
}

/// Enter the alternate screen, and give back whatever restores it if that fails.
fn restore_on_exit(
    _setup: EnterAlternateScreen,
) -> Result<Terminal<CrosstermBackend<Stdout>>, String> {
    enable_raw_mode().map_err(|error| format!("无法进入 raw mode：{error}"))?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).map_err(|error| format!("无法切换屏幕：{error}"))?;
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
            KeyCode::Down | KeyCode::Char('j') => app.select_next(),
            KeyCode::Up | KeyCode::Char('k') => app.select_previous(),
            _ => {}
        }
        app.tick();
    }
}
