//! Terminal ownership: raw mode, the alternate screen, and the
//! guarantee that both revert on every exit path, panics included.

use std::io::Stdout;
use std::sync::Once;

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::crossterm::{cursor, execute};

/// The live terminal. Dropping it restores the caller's screen.
pub struct Guard {
    pub terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Guard {
    /// Enter raw mode and the alternate screen. The first call installs
    /// a panic hook that restores the terminal before the panic prints.
    pub fn enter() -> std::io::Result<Self> {
        static HOOK: Once = Once::new();
        HOOK.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                restore();
                previous(info);
            }));
        });

        enable_raw_mode()?;
        if let Err(error) = execute!(
            std::io::stdout(),
            EnterAlternateScreen,
            EnableBracketedPaste
        ) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        match Terminal::new(CrosstermBackend::new(std::io::stdout())) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                restore();
                Err(error)
            }
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        restore();
    }
}

/// Leave the alternate screen and raw mode. Safe to call twice.
fn restore() {
    let _ = execute!(
        std::io::stdout(),
        DisableBracketedPaste,
        LeaveAlternateScreen,
        cursor::Show
    );
    let _ = disable_raw_mode();
}
