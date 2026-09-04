//! The first-run screen: what a machine without a credential sees
//! before the prompt. The wordmark, the credential state per
//! provider, the login commands, and the key that continues. Enter
//! goes on to the session; a turn there fails with the credential
//! error until a login lands, since the driver builds the provider
//! at the first turn. Ctrl-D ends the run, and Ctrl-C keeps its
//! two-press rule with the hint on the last row.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::QUIT_HINT;
use crate::render::{draw_line, paint};
use crate::theme::{Palette, Role};

/// The first-run screen's text. The driver fills it from the
/// credential probe when no credential resolves for the session's
/// provider; the interface draws the wordmark, the hints, and the
/// key that continues.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct FirstRun {
    /// The session's provider, the one without a credential.
    pub provider: String,
    /// The credential state per provider, one line each, as the
    /// doctor reports it: the provider id, a colon, the state.
    pub credentials: Vec<String>,
    /// The login commands, the session's provider first.
    pub commands: Vec<LoginCommand>,
}

/// One login command on the first-run screen.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct LoginCommand {
    /// The command to type.
    pub command: String,
    /// What the command does, in a few words.
    pub purpose: String,
}

impl LoginCommand {
    pub fn new(command: impl Into<String>, purpose: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            purpose: purpose.into(),
        }
    }
}

/// The wordmark: three rows of box-drawing characters, at most 24
/// columns.
const LOGO: [&str; 3] = [
    "┌┬┐┌─┐┌┬┐┌─┐┬─┐┌─┐┌─┐┬",
    "│││├┤ │││├┤ ├┬┘┌─┘├┤ │",
    "└┴┘└─┘┴ ┴└─┘┴└─└─┘└─┘┴─┘",
];

/// The left margin of every row.
const MARGIN: u16 = 2;

/// Draw the screen: the wordmark, the state, the commands, and the
/// key hint, top down from a one-row margin. A row past the bottom
/// does not draw; a line past the width is cut. `quit_hint` puts the
/// Ctrl-C hint on the last row.
pub(crate) fn draw(frame: &mut Frame, screen: &FirstRun, palette: &Palette, quit_hint: bool) {
    let area = frame.area();
    if area.height == 0 || area.width <= MARGIN {
        return;
    }
    let text = paint(Style::default(), palette.color(Role::Text));
    let dim = paint(Style::default(), palette.color(Role::Dim));
    let accent = paint(
        Style::default().add_modifier(Modifier::BOLD),
        palette.color(Role::Accent),
    );
    let mut rows = Rows {
        frame,
        area,
        next: area.y + 1,
    };
    for row in LOGO {
        rows.push(row, accent);
    }
    rows.push("", text);
    rows.push(
        &format!(
            "No credential for {}, the session's provider.",
            screen.provider
        ),
        text,
    );
    for line in &screen.credentials {
        rows.push(&format!("  {line}"), dim);
    }
    rows.push("", text);
    rows.push("Log in from another terminal:", text);
    let width = screen
        .commands
        .iter()
        .map(|login| login.command.chars().count())
        .max()
        .unwrap_or(0);
    for login in &screen.commands {
        rows.push(
            &format!("  {:<width$}   {}", login.command, login.purpose),
            text,
        );
    }
    rows.push("", text);
    rows.push(
        "Press Enter to continue. A turn fails until a login lands.",
        text,
    );
    if quit_hint {
        let last = Rect {
            x: area.x + MARGIN,
            y: area.y + area.height - 1,
            width: area.width - MARGIN,
            height: 1,
        };
        let warning = paint(Style::default(), palette.color(Role::Warning));
        draw_line(rows.frame, last, QUIT_HINT, warning);
    }
}

/// The rows of the screen, filled top down inside the margin.
struct Rows<'a, 'b> {
    frame: &'a mut Frame<'b>,
    area: Rect,
    next: u16,
}

impl Rows<'_, '_> {
    fn push(&mut self, text: &str, style: Style) {
        if self.next >= self.area.y + self.area.height {
            return;
        }
        let row = Rect {
            x: self.area.x + MARGIN,
            y: self.next,
            width: self.area.width - MARGIN,
            height: 1,
        };
        draw_line(self.frame, row, text, style);
        self.next += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{Depth, Theme};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn screen() -> FirstRun {
        FirstRun {
            provider: "openai-codex".into(),
            credentials: vec![
                "openai-codex: none — run `dmz login`".into(),
                "anthropic: none".into(),
            ],
            commands: vec![
                LoginCommand::new("dmz login", "the subscription"),
                LoginCommand::new("dmz login anthropic", "an API key"),
            ],
        }
    }

    fn rows(width: u16, height: u16, quit_hint: bool) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let palette = Palette::new(&Theme::default(), Depth::TrueColor);
        terminal
            .draw(|frame| draw(frame, &screen(), &palette, quit_hint))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn the_wordmark_is_three_rows_inside_one_width() {
        for row in LOGO {
            assert!(row.chars().count() <= 24, "{row}");
        }
        assert_eq!(LOGO[2].chars().count(), 24);
    }

    #[test]
    fn the_screen_lays_out_top_down_and_aligns_the_commands() {
        let rows = rows(80, 16, false);
        assert_eq!(rows[0], "");
        assert_eq!(rows[1], format!("  {}", LOGO[0]));
        assert_eq!(rows[3], format!("  {}", LOGO[2]));
        assert_eq!(
            rows[5],
            "  No credential for openai-codex, the session's provider."
        );
        assert_eq!(rows[6], "    openai-codex: none — run `dmz login`");
        assert_eq!(rows[9], "  Log in from another terminal:");
        assert_eq!(rows[10], "    dmz login             the subscription");
        assert_eq!(rows[11], "    dmz login anthropic   an API key");
        assert_eq!(
            rows[13],
            "  Press Enter to continue. A turn fails until a login lands."
        );
    }

    #[test]
    fn a_small_screen_cuts_rows_and_columns_and_the_hint_takes_the_last_row() {
        let rows = rows(20, 5, true);
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[1].chars().count(), 20);
        assert_eq!(
            rows[4],
            format!("  {}", QUIT_HINT.chars().take(18).collect::<String>())
        );
        // Nothing to draw into: no panic.
        rows_of_nothing();
    }

    fn rows_of_nothing() {
        let _ = rows(2, 1, true);
        let _ = rows(1, 0, false);
    }
}
