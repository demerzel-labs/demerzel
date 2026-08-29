//! Terminal ownership: raw mode, the alternate screen, mouse capture,
//! and the guarantee that all of it reverts on every exit path,
//! panics included.

use std::io::{Stdout, Write as _};
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::crossterm::{cursor, execute};

/// The keyboard-enhancement flags the interface asks for: keys with
/// modifiers arrive as their own codes, so Shift-Enter reads apart
/// from Enter. Plain keys keep their legacy bytes. A terminal
/// without the protocol ignores the request, and the interface
/// sends it without a query: a query costs a round trip at boot and
/// waits out a timeout on a terminal that stays silent. The flags
/// are pushed on the alternate screen and popped before the
/// interface leaves it; the protocol keeps one stack per screen.
const KEYBOARD_FLAGS: KeyboardEnhancementFlags =
    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES;

/// Mouse reporting for buttons and the wheel, with SGR coordinates:
/// modes 1000 and 1006. Not any-motion reporting (mode 1003), which
/// would wake the loop on every pointer move.
const MOUSE_ON: &[u8] = b"\x1b[?1000h\x1b[?1006h";

/// Whether the mouse is captured, so restore releases it only then:
/// the release sequence must not reach a terminal whose mouse mode
/// belongs to someone else.
static MOUSE: AtomicBool = AtomicBool::new(false);

/// Whether the keyboard flags are pushed, so restore pops them once:
/// a second restore runs on the main screen, whose stack belongs to
/// the shell that started the interface.
static KEYBOARD: AtomicBool = AtomicBool::new(false);

/// The live terminal. Dropping it restores the caller's screen.
pub struct Guard {
    pub terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Guard {
    /// Enter raw mode and the alternate screen, and capture the mouse
    /// when `mouse` is set. The first call installs a panic hook that
    /// restores the terminal before the panic prints.
    pub fn enter(mouse: bool) -> std::io::Result<Self> {
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
            restore();
            return Err(error);
        }
        KEYBOARD.store(true, Ordering::Relaxed);
        if let Err(error) = execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(KEYBOARD_FLAGS)
        ) {
            restore();
            return Err(error);
        }
        if mouse {
            MOUSE.store(true, Ordering::Relaxed);
            let mut stdout = std::io::stdout();
            if let Err(error) = stdout.write_all(MOUSE_ON).and_then(|()| stdout.flush()) {
                restore();
                return Err(error);
            }
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

/// Release the mouse when it was captured, pop the keyboard flags
/// when they were pushed, leave the alternate screen and raw mode.
/// Safe to call twice: the panic hook and the guard's drop both run
/// on a panic, and the second call sends nothing that belongs to
/// the screen underneath.
fn restore() {
    if MOUSE.swap(false, Ordering::Relaxed) {
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
    }
    if KEYBOARD.swap(false, Ordering::Relaxed) {
        let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(
        std::io::stdout(),
        DisableBracketedPaste,
        LeaveAlternateScreen,
        cursor::Show
    );
    let _ = disable_raw_mode();
}
