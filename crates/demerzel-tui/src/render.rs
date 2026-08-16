//! Frame rendering: the transcript pane above, the input line below.
//!
//! Pure functions over interface state, so tests drive them through
//! ratatui's `TestBackend` without a terminal.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use crate::input::Input;
use crate::transcript::{Kind, Transcript};
use crate::wrap::{display_width, wrap};

/// The input prompt. The bench probe's shutdown path waits for these
/// bytes on the output stream, so the spelling is load-bearing.
pub(crate) const PROMPT: &str = "dmz> ";

/// What one frame shows.
pub(crate) struct View<'a> {
    pub transcript: &'a mut Transcript,
    pub input: &'a Input,
    /// An approval question replaces the input prompt.
    pub approval: Option<&'a str>,
}

/// Draw one frame. Returns the transcript viewport height, the page
/// unit for scroll keys.
pub(crate) fn draw(frame: &mut Frame, view: View<'_>) -> u16 {
    let area = frame.area();
    if area.height == 0 || area.width == 0 {
        return 0;
    }
    let input_row = Rect {
        y: area.y + area.height - 1,
        height: 1,
        ..area
    };
    let transcript_area = Rect {
        height: area.height - 1,
        ..area
    };

    draw_transcript(frame, transcript_area, view.transcript);
    match view.approval {
        Some(question) => {
            let line = Line::styled(question, Style::default().add_modifier(Modifier::BOLD));
            frame.render_widget(Paragraph::new(line), input_row);
        }
        None => draw_input(frame, input_row, view.input),
    }
    transcript_area.height
}

fn style(kind: Kind) -> Style {
    match kind {
        Kind::Text | Kind::Status => Style::default(),
        Kind::Reasoning | Kind::Notice => Style::default().add_modifier(Modifier::DIM),
        Kind::Error => Style::default().fg(Color::Red),
    }
}

/// Wrap and show the tail of the transcript. Only the rows the
/// viewport and the scroll offset need are wrapped, so a long session
/// pays for its window, not its history.
fn draw_transcript(frame: &mut Frame, area: Rect, transcript: &mut Transcript) {
    if area.height == 0 {
        return;
    }
    let height = usize::from(area.height);
    let want = height + transcript.scroll;

    // Bottom-first rows until the viewport and the offset are covered.
    let mut rows: Vec<(Kind, &str)> = Vec::new();
    'collect: for entry in transcript.entries().rev() {
        for row in wrap(&entry.text, area.width).into_iter().rev() {
            rows.push((entry.kind, row));
            if rows.len() >= want {
                break 'collect;
            }
        }
    }

    let scroll = transcript.scroll.min(rows.len().saturating_sub(height));
    let lines: Vec<Line<'_>> = rows
        .iter()
        .skip(scroll)
        .take(height)
        .rev()
        .map(|(kind, row)| Line::styled(*row, style(*kind)))
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
    transcript.scroll = scroll;
}

/// The prompt and the buffer, windowed so the cursor stays visible.
fn draw_input(frame: &mut Frame, area: Rect, input: &Input) {
    let shown: String = input.text().chars().filter_map(displayed).collect();
    let before_cursor: usize = input.text()[..input.cursor()]
        .chars()
        .filter_map(displayed)
        .map(|ch| display_width(ch.encode_utf8(&mut [0u8; 4])))
        .sum();

    let width = usize::from(area.width);
    let cursor_col = display_width(PROMPT) + before_cursor;
    // Skip columns from the left until the cursor fits on the row.
    let skip = cursor_col.saturating_sub(width.saturating_sub(1));
    let text = format!("{PROMPT}{shown}");
    let visible = skip_columns(&text, skip);

    frame.render_widget(Paragraph::new(visible), area);
    let x = area.x + (cursor_col - skip) as u16;
    frame.set_cursor_position((x.min(area.x + area.width - 1), area.y));
}

/// The printable stand-in for one input char.
fn displayed(ch: char) -> Option<char> {
    match ch {
        '\n' => Some('⏎'),
        '\t' => Some('⇥'),
        ch if ch.is_control() => None,
        ch => Some(ch),
    }
}

/// Drop `skip` display columns from the front.
fn skip_columns(text: &str, skip: usize) -> &str {
    if skip == 0 {
        return text;
    }
    let mut cols = 0usize;
    for (index, ch) in text.char_indices() {
        if cols >= skip {
            return &text[index..];
        }
        cols += display_width(ch.encode_utf8(&mut [0u8; 4]));
    }
    ""
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(width: u16, height: u16, view: View<'_>) -> (Vec<String>, u16) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut rows = 0;
        terminal
            .draw(|frame| {
                rows = draw(frame, view);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let lines = (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect();
        (lines, rows)
    }

    #[test]
    fn a_fresh_screen_shows_the_prompt() {
        let mut transcript = Transcript::new();
        let input = Input::new();
        let (lines, rows) = render(
            20,
            4,
            View {
                transcript: &mut transcript,
                input: &input,
                approval: None,
            },
        );
        assert_eq!(rows, 3);
        assert_eq!(lines[3].trim_end(), "dmz> ".trim_end());
    }

    #[test]
    fn the_transcript_tail_sticks_to_the_bottom_of_the_pane() {
        let mut transcript = Transcript::new();
        for i in 0..10 {
            transcript.push_line(Kind::Text, &format!("line {i}"));
        }
        let input = Input::new();
        let (lines, _) = render(
            20,
            4,
            View {
                transcript: &mut transcript,
                input: &input,
                approval: None,
            },
        );
        assert_eq!(lines[0].trim_end(), "line 7");
        assert_eq!(lines[2].trim_end(), "line 9");
    }

    #[test]
    fn scroll_moves_the_window_and_clamps_at_the_top() {
        let mut transcript = Transcript::new();
        for i in 0..10 {
            transcript.push_line(Kind::Text, &format!("line {i}"));
        }
        transcript.scroll = 3;
        let input = Input::new();
        let (lines, _) = render(
            20,
            4,
            View {
                transcript: &mut transcript,
                input: &input,
                approval: None,
            },
        );
        assert_eq!(lines[2].trim_end(), "line 6");

        transcript.scroll = 100;
        let (lines, _) = render(
            20,
            4,
            View {
                transcript: &mut transcript,
                input: &input,
                approval: None,
            },
        );
        assert_eq!(lines[0].trim_end(), "line 0");
        assert_eq!(transcript.scroll, 7);
    }

    #[test]
    fn long_entries_wrap_inside_the_pane() {
        let mut transcript = Transcript::new();
        transcript.push_line(Kind::Text, "alpha beta gamma");
        let input = Input::new();
        let (lines, _) = render(
            8,
            4,
            View {
                transcript: &mut transcript,
                input: &input,
                approval: None,
            },
        );
        assert_eq!(lines[0].trim_end(), "alpha");
        assert_eq!(lines[1].trim_end(), "beta");
        assert_eq!(lines[2].trim_end(), "gamma");
    }

    #[test]
    fn a_long_input_windows_around_the_cursor() {
        let mut transcript = Transcript::new();
        let mut input = Input::new();
        input.set(&"abcdefghij".repeat(3));
        let (lines, _) = render(
            10,
            2,
            View {
                transcript: &mut transcript,
                input: &input,
                approval: None,
            },
        );
        // The tail is visible; the head scrolled off with the prompt.
        assert!(lines[1].contains("j"), "{:?}", lines[1]);
        assert!(!lines[1].contains("dmz>"));
    }

    #[test]
    fn an_approval_question_replaces_the_prompt() {
        let mut transcript = Transcript::new();
        let input = Input::new();
        let (lines, _) = render(
            40,
            2,
            View {
                transcript: &mut transcript,
                input: &input,
                approval: Some("approve bash? [y]es / [a]lways / [N]o"),
            },
        );
        assert_eq!(lines[1].trim_end(), "approve bash? [y]es / [a]lways / [N]o");
    }
}
