//! Frame rendering: the transcript pane above, the input line below.
//!
//! Functions over interface state, driven in tests through ratatui's
//! `TestBackend` without a terminal. The one write-back: `draw`
//! clamps the transcript's scroll to the rows that exist.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthChar as _;

use crate::PROMPT;
use crate::input::Input;
use crate::transcript::{Kind, Transcript};
use crate::wrap::{display_width, wrap};

/// What the status bar names: the model, the mode, the open session,
/// and the open session's token totals, counted from this run's
/// start. A session change restarts the totals.
pub(crate) struct Status {
    pub model: String,
    pub mode: String,
    pub session: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
}

/// What one frame shows.
pub(crate) struct View<'a> {
    pub transcript: &'a mut Transcript,
    pub input: &'a Input,
    pub status: &'a Status,
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
    // The status bar takes one row when the screen has room for all
    // three panes; a two-row screen keeps transcript and input.
    let bar_rows = u16::from(area.height >= 3);
    let input_row = Rect {
        y: area.y + area.height - 1,
        height: 1,
        ..area
    };
    let transcript_area = Rect {
        height: area.height - 1 - bar_rows,
        ..area
    };

    draw_transcript(frame, transcript_area, view.transcript);
    if bar_rows > 0 {
        let bar_row = Rect {
            y: area.y + area.height - 2,
            height: 1,
            ..area
        };
        draw_status(frame, bar_row, view.status);
    }
    match view.approval {
        Some(question) => {
            let line = Line::styled(question, Style::default().add_modifier(Modifier::BOLD));
            frame.render_widget(Paragraph::new(line), input_row);
        }
        None => draw_input(frame, input_row, view.input),
    }
    transcript_area.height
}

/// One reversed row: model, mode, session, token totals, cut to the
/// width from the right.
fn draw_status(frame: &mut Frame, area: Rect, status: &Status) {
    let totals = format!(
        " · {} in · {} out · {} cached",
        status.input_tokens, status.output_tokens, status.cached_tokens
    );
    let session = status.session.as_deref().unwrap_or("");
    let separator = if status.session.is_some() { " · " } else { "" };
    let segments = [
        " ",
        status.model.as_str(),
        " · ",
        status.mode.as_str(),
        separator,
        session,
        totals.as_str(),
    ];

    // The walk stops at the width, so this frame-rate path never
    // copies a segment whole — an oversized model name costs its
    // visible columns, nothing more.
    let width = usize::from(area.width);
    let mut shown = String::with_capacity(width * 4);
    let mut used = 0usize;
    'segments: for segment in segments {
        for ch in segment.chars() {
            // The model name comes from config and `/model`; an
            // escape byte must never reach the frame, the
            // transcript's rule.
            if ch.is_control() {
                continue;
            }
            let cols = ch.width().unwrap_or(0);
            if used + cols > width {
                break 'segments;
            }
            shown.push(ch);
            used += cols;
        }
    }
    // Pad to the full width so the whole row reads as one bar.
    shown.extend(std::iter::repeat_n(' ', width - used));
    let line = Line::styled(shown, Style::default().add_modifier(Modifier::REVERSED));
    frame.render_widget(Paragraph::new(line), area);
}

fn style(kind: Kind) -> Style {
    match kind {
        Kind::Prompt => Style::default().add_modifier(Modifier::BOLD),
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
/// Cost is bounded by the window, not the buffer length, so a large
/// paste cannot slow every later frame. Zero-width chars never fill
/// the window; only a long unbroken run of them can extend the walk.
fn draw_input(frame: &mut Frame, area: Rect, input: &Input) {
    let width = usize::from(area.width);
    // One column stays free so the cursor can sit past the last char.
    let avail_before = width.saturating_sub(1);

    // Walk back from the cursor to find the window start.
    let mut before_cols = 0usize;
    let mut window_start = input.cursor();
    for ch in input.text()[..input.cursor()].chars().rev() {
        let cols = displayed(ch).map(|shown| shown.width().unwrap_or(0));
        if let Some(cols) = cols
            && before_cols + cols > avail_before
        {
            break;
        }
        before_cols += cols.unwrap_or(0);
        window_start -= ch.len_utf8();
    }

    // The prompt joins only when the whole prefix fits after it.
    let prompt_cols = display_width(PROMPT);
    let with_prompt = window_start == 0 && prompt_cols + before_cols <= avail_before;
    let mut text = String::with_capacity(width * 4);
    let mut used = 0usize;
    if with_prompt {
        text.push_str(PROMPT);
        used = prompt_cols;
    }
    let mut cursor_x = None;
    for (offset, ch) in input.text()[window_start..].char_indices() {
        if window_start + offset == input.cursor() {
            cursor_x = Some(used);
        }
        let Some(shown) = displayed(ch) else { continue };
        let cols = shown.width().unwrap_or(0);
        if used + cols > width {
            break;
        }
        text.push(shown);
        used += cols;
    }
    let cursor_x = cursor_x.unwrap_or(used).min(width.saturating_sub(1));

    frame.render_widget(Paragraph::new(text), area);
    frame.set_cursor_position((area.x + cursor_x as u16, area.y));
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn status() -> Status {
        Status {
            model: "test-model".into(),
            mode: "ask".into(),
            session: None,
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
        }
    }

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
    fn a_fresh_screen_shows_the_bar_and_the_prompt() {
        let mut transcript = Transcript::new();
        let input = Input::new();
        let status = status();
        let (lines, rows) = render(
            30,
            4,
            View {
                transcript: &mut transcript,
                input: &input,
                status: &status,
                approval: None,
            },
        );
        assert_eq!(rows, 2);
        assert!(lines[2].contains("test-model · ask"), "{:?}", lines[2]);
        assert_eq!(lines[3].trim_end(), "dmz> ".trim_end());
    }

    #[test]
    fn the_status_bar_names_the_session_and_the_totals() {
        let mut transcript = Transcript::new();
        let input = Input::new();
        let status = Status {
            session: Some("s-9".into()),
            input_tokens: 12,
            output_tokens: 5,
            cached_tokens: 2,
            ..status()
        };
        let (lines, _) = render(
            60,
            3,
            View {
                transcript: &mut transcript,
                input: &input,
                status: &status,
                approval: None,
            },
        );
        assert!(
            lines[1].contains("test-model · ask · s-9 · 12 in · 5 out · 2 cached"),
            "{:?}",
            lines[1]
        );
    }

    #[test]
    fn a_narrow_bar_cuts_from_the_right() {
        let mut transcript = Transcript::new();
        let input = Input::new();
        let status = status();
        let (lines, _) = render(
            12,
            3,
            View {
                transcript: &mut transcript,
                input: &input,
                status: &status,
                approval: None,
            },
        );
        assert_eq!(lines[1], " test-model ");
    }

    #[test]
    fn a_two_row_screen_drops_the_bar_and_keeps_the_prompt() {
        let mut transcript = Transcript::new();
        transcript.push_line(Kind::Text, "tail");
        let input = Input::new();
        let status = status();
        let (lines, rows) = render(
            20,
            2,
            View {
                transcript: &mut transcript,
                input: &input,
                status: &status,
                approval: None,
            },
        );
        assert_eq!(rows, 1);
        assert_eq!(lines[0].trim_end(), "tail");
        assert_eq!(lines[1].trim_end(), "dmz> ".trim_end());
    }

    #[test]
    fn the_transcript_tail_sticks_to_the_bottom_of_the_pane() {
        let mut transcript = Transcript::new();
        for i in 0..10 {
            transcript.push_line(Kind::Text, &format!("line {i}"));
        }
        let input = Input::new();
        let status = status();
        let (lines, _) = render(
            20,
            4,
            View {
                transcript: &mut transcript,
                input: &input,
                status: &status,
                approval: None,
            },
        );
        assert_eq!(lines[0].trim_end(), "line 8");
        assert_eq!(lines[1].trim_end(), "line 9");
    }

    #[test]
    fn scroll_moves_the_window_and_clamps_at_the_top() {
        let mut transcript = Transcript::new();
        for i in 0..10 {
            transcript.push_line(Kind::Text, &format!("line {i}"));
        }
        transcript.scroll = 3;
        let input = Input::new();
        let status = status();
        let (lines, _) = render(
            20,
            4,
            View {
                transcript: &mut transcript,
                input: &input,
                status: &status,
                approval: None,
            },
        );
        assert_eq!(lines[1].trim_end(), "line 6");

        transcript.scroll = 100;
        let (lines, _) = render(
            20,
            4,
            View {
                transcript: &mut transcript,
                input: &input,
                status: &status,
                approval: None,
            },
        );
        assert_eq!(lines[0].trim_end(), "line 0");
        assert_eq!(transcript.scroll, 8);
    }

    #[test]
    fn long_entries_wrap_inside_the_pane() {
        let mut transcript = Transcript::new();
        transcript.push_line(Kind::Text, "alpha beta gamma");
        let input = Input::new();
        let status = status();
        let (lines, _) = render(
            8,
            5,
            View {
                transcript: &mut transcript,
                input: &input,
                status: &status,
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
        let status = status();
        let (lines, _) = render(
            10,
            2,
            View {
                transcript: &mut transcript,
                input: &input,
                status: &status,
                approval: None,
            },
        );
        // The tail is visible; the head scrolled off with the prompt.
        assert!(lines[1].contains("j"), "{:?}", lines[1]);
        assert!(!lines[1].contains("dmz>"));
    }

    #[test]
    fn a_wide_char_at_the_window_edge_keeps_the_cursor_aligned() {
        let mut terminal = Terminal::new(TestBackend::new(10, 2)).unwrap();
        let mut transcript = Transcript::new();
        let mut input = Input::new();
        input.set("日本日本日");
        let status = status();
        terminal
            .draw(|frame| {
                draw(
                    frame,
                    View {
                        transcript: &mut transcript,
                        input: &input,
                        status: &status,
                        approval: None,
                    },
                );
            })
            .unwrap();
        // Four wide chars fit before the cursor; the fifth straddles
        // the edge and drops whole, so the cursor sits at column 8.
        let position = terminal.get_cursor_position().unwrap();
        assert_eq!((position.x, position.y), (8, 1));
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0u16, 1u16)].symbol(), "本");
    }

    #[test]
    fn an_approval_question_replaces_the_prompt() {
        let mut transcript = Transcript::new();
        let input = Input::new();
        let status = status();
        let (lines, _) = render(
            40,
            2,
            View {
                transcript: &mut transcript,
                input: &input,
                status: &status,
                approval: Some("approve bash? [y]es / [a]lways / [N]o"),
            },
        );
        assert_eq!(lines[1].trim_end(), "approve bash? [y]es / [a]lways / [N]o");
    }
}
