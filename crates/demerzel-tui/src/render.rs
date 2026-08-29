//! Frame rendering: the transcript pane above, the progress line
//! while a turn runs, the input box, the completion popup over the
//! rows above the box, and the status bar at the bottom.
//!
//! Functions over interface state, driven in tests through ratatui's
//! `TestBackend` without a terminal. The write-backs: `draw` lays out
//! the blocks it shows and clamps the transcript's scroll to the rows
//! that exist; it lays out the editor's rows and moves its box to
//! the cursor.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use unicode_width::UnicodeWidthChar as _;

use crate::PROMPT;
use crate::editor::{CommandSpec, Editor, displayed};
use crate::markdown::Face;
use crate::theme::{Palette, Role};
use crate::transcript::{Kind, Transcript};
use crate::wrap::display_width;

/// Rows the input box grows to before it scrolls inside itself.
const MAX_INPUT_ROWS: u16 = 8;

/// Rows the completion popup shows at most; more matches scroll
/// inside it.
const MAX_POPUP_ROWS: usize = 8;

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
    pub editor: &'a mut Editor,
    pub status: &'a Status,
    /// The theme at the terminal's color depth.
    pub palette: &'a Palette,
    /// An approval question replaces the input box.
    pub approval: Option<&'a str>,
    /// The progress line, shown under the transcript while a turn
    /// runs.
    pub progress: Option<&'a str>,
}

/// Draw one frame. Returns the transcript viewport height, the page
/// unit for scroll keys.
pub(crate) fn draw(frame: &mut Frame, view: View<'_>) -> u16 {
    let area = frame.area();
    if area.height == 0 || area.width == 0 {
        return 0;
    }
    // The status bar takes the bottom row when the screen has room
    // for all three panes; a two-row screen keeps transcript and
    // input. The input box sits above the bar and grows with its
    // rows up to the cap, and the transcript keeps one row at least.
    // The progress line takes one more row above the box when it
    // shows and the screen has a fourth row.
    let bar_rows = u16::from(area.height >= 3);
    let progress_rows = u16::from(view.progress.is_some() && area.height >= 4);
    let box_cap = (area.height - bar_rows - progress_rows)
        .saturating_sub(1)
        .clamp(1, MAX_INPUT_ROWS);
    let (text_width, prefix) = input_columns(area.width);
    let editor_rows = view.editor.sync(text_width);
    let input_rows = match view.approval {
        Some(_) => 1,
        None => u16::try_from(editor_rows)
            .unwrap_or(u16::MAX)
            .clamp(1, box_cap),
    };
    let input_area = Rect {
        y: area.y + area.height - input_rows - bar_rows,
        height: input_rows,
        ..area
    };
    let transcript_area = Rect {
        height: area.height - input_rows - bar_rows - progress_rows,
        ..area
    };

    let palette = view.palette;
    draw_transcript(frame, transcript_area, view.transcript, palette);
    if let Some(progress) = view.progress
        && progress_rows > 0
    {
        let progress_row = Rect {
            y: input_area.y - 1,
            height: 1,
            ..area
        };
        let style = paint(Style::default(), palette.color(Role::ProgressLine));
        draw_line(frame, progress_row, progress, style);
    }
    if bar_rows > 0 {
        let bar_row = Rect {
            y: area.y + area.height - 1,
            height: 1,
            ..area
        };
        draw_status(frame, bar_row, view.status, palette);
    }
    match view.approval {
        Some(question) => {
            let style = paint(
                Style::default().add_modifier(Modifier::BOLD),
                palette.color(Role::Warning),
            );
            draw_line(frame, input_area, question, style);
        }
        None => {
            draw_input(frame, input_area, view.editor, editor_rows, prefix, palette);
            let above = Rect {
                height: input_area.y - area.y,
                ..area
            };
            draw_popup(frame, above, view.editor, prefix, palette);
        }
    }
    transcript_area.height
}

/// The text columns of the input box and whether the prompt shows.
/// The prompt takes its columns from the first row, and every other
/// row indents by as much, unless the screen is too narrow for
/// both. One column stays free at the right, so the cursor can sit
/// past the last char of a full row. The text keeps two columns
/// wherever the screen has them, so a wide char always has a row
/// that holds it; at two columns the free column goes first.
fn input_columns(width: u16) -> (u16, bool) {
    let prompt_cols = u16::try_from(display_width(PROMPT)).unwrap_or(u16::MAX);
    if width >= prompt_cols + 3 {
        (width - prompt_cols - 1, true)
    } else {
        (width.saturating_sub(1).max(2).min(width.max(1)), false)
    }
}

/// One row on the bar's ground — the theme's, or reversed when the
/// theme sets none: model, mode, session, token totals, cut to the
/// width from the right.
fn draw_status(frame: &mut Frame, area: Rect, status: &Status, palette: &Palette) {
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

    let width = usize::from(area.width);
    let (mut shown, used) = fit_row(segments, width);
    // Pad to the full width so the whole row reads as one bar.
    shown.extend(std::iter::repeat_n(' ', width - used));
    // With one of the two colors the row reverses onto it: the
    // terminal's own background reads on the ground, or on the text
    // color, whatever it is.
    let style = match (palette.color(Role::StatusBar), palette.color(Role::Text)) {
        (Some(ground), Some(text)) => Style::default().bg(ground).fg(text),
        (Some(ground), None) => Style::default().fg(ground).add_modifier(Modifier::REVERSED),
        (None, text) => paint(Style::default().add_modifier(Modifier::REVERSED), text),
    };
    frame.render_widget(Paragraph::new(Line::styled(shown, style)), area);
}

/// One row of text cut to the width: the progress line, the
/// approval question.
fn draw_line(frame: &mut Frame, area: Rect, text: &str, style: Style) {
    let (shown, _) = fit_row([text], usize::from(area.width));
    frame.render_widget(Paragraph::new(Line::styled(shown, style)), area);
}

/// The columns of `parts` that fit in `width`, joined, and the
/// columns used. The walk stops at the width, so this frame-rate
/// path never copies a part whole — an oversized model name or tool
/// name costs its visible columns, nothing more. Control characters
/// drop: a name from config, from `/model`, or from the model must
/// never put an escape byte in the frame, the transcript's rule.
fn fit_row<'a>(parts: impl IntoIterator<Item = &'a str>, width: usize) -> (String, usize) {
    let mut shown = String::with_capacity(width.min(512) * 2);
    let mut used = 0usize;
    'parts: for part in parts {
        for ch in part.chars() {
            if ch.is_control() {
                continue;
            }
            let cols = ch.width().unwrap_or(0);
            if used + cols > width {
                break 'parts;
            }
            shown.push(ch);
            used += cols;
        }
    }
    (shown, used)
}

/// `style` with `color` as its foreground. A role with no color
/// paints the terminal's own foreground, so `none` on a nested role
/// — inline code inside colored text, a keyword inside a fence —
/// takes the enclosing color away, as the config contract says.
fn paint(style: Style, color: Option<ratatui::style::Color>) -> Style {
    style.fg(color.unwrap_or(ratatui::style::Color::Reset))
}

/// A block's own style: the attribute the kind carries, and the
/// role's color.
fn style(kind: Kind, palette: &Palette) -> Style {
    match kind {
        Kind::Prompt => paint(
            Style::default().add_modifier(Modifier::BOLD),
            palette.color(Role::Prompt),
        ),
        Kind::Text | Kind::Tool => paint(Style::default(), palette.color(Role::Text)),
        Kind::Reasoning | Kind::Notice => paint(
            Style::default().add_modifier(Modifier::DIM),
            palette.color(Role::Dim),
        ),
        Kind::Error => paint(Style::default(), palette.color(Role::Error)),
    }
}

/// A face over the block's own style. Each class keeps its attribute
/// — bold, italic, underline, dim — so the faces read apart with no
/// color at all; the theme adds the color of the class's role. The
/// more specific class paints last: a keyword inside a fence takes
/// the keyword's color over the fence's.
fn face_style(kind: Kind, face: Face, palette: &Palette) -> Style {
    let mut style = style(kind, palette);
    if face.has(Face::HEADING) {
        style = paint(
            style.add_modifier(Modifier::BOLD),
            palette.color(Role::Heading),
        );
    }
    if face.has(Face::STRONG) {
        style = style.add_modifier(Modifier::BOLD);
    }
    if face.has(Face::TITLE) {
        style = paint(
            style.add_modifier(Modifier::BOLD),
            palette.color(Role::Accent),
        );
    }
    if face.has(Face::EMPHASIS) {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if face.has(Face::STRIKE) {
        style = style.add_modifier(Modifier::CROSSED_OUT);
    }
    if face.has(Face::LINK) {
        style = paint(
            style.add_modifier(Modifier::UNDERLINED),
            palette.color(Role::Link),
        );
    }
    if face.has(Face::URL) || face.has(Face::MARKER) || face.has(Face::MUTED) {
        style = paint(style.add_modifier(Modifier::DIM), palette.color(Role::Dim));
    }
    if face.has(Face::CODE) {
        let role = if face.has(Face::FENCE) {
            Role::CodeBlock
        } else {
            Role::InlineCode
        };
        style = paint(style, palette.color(role));
    }
    if face.has(Face::KEYWORD) {
        style = paint(style, palette.color(Role::Keyword));
    }
    if face.has(Face::STRING) {
        style = paint(style, palette.color(Role::String));
    }
    if face.has(Face::NUMBER) {
        style = paint(style, palette.color(Role::Number));
    }
    if face.has(Face::COMMENT) {
        // With no color of its own a comment also dims, so it still
        // reads apart from the code.
        let color = palette.color(Role::Comment);
        style = paint(style, color);
        if color.is_none() {
            style = style.add_modifier(Modifier::DIM);
        }
    }
    if face.has(Face::SUCCESS) {
        style = paint(style, palette.color(Role::Success));
    }
    if face.has(Face::WARNING) {
        style = paint(style, palette.color(Role::Warning));
    }
    if face.has(Face::FAILURE) {
        style = paint(style, palette.color(Role::Error));
    }
    if face.has(Face::DIFF_ADD) {
        style = paint(style, palette.color(Role::DiffAdd));
    }
    if face.has(Face::DIFF_REMOVE) {
        style = paint(style, palette.color(Role::DiffRemove));
    }
    style
}

/// Show the tail of the transcript, or the browsed window above it.
/// Only the blocks the viewport and the scroll need lay out, and a
/// block lays out once per width, so a long session pays for its
/// window, not its history.
fn draw_transcript(frame: &mut Frame, area: Rect, transcript: &mut Transcript, palette: &Palette) {
    if area.height == 0 {
        return;
    }
    let height = usize::from(area.height);
    transcript.begin_frame(area.width, area.height);
    let want = height.saturating_add(transcript.scroll());

    // Bottom-first: lay out blocks until the viewport and the offset
    // are covered.
    let mut visited = 0usize;
    let mut avail = 0usize;
    for block in transcript.blocks_mut().rev() {
        block.sync(area.width);
        visited += 1;
        avail += block.rows();
        if avail >= want {
            break;
        }
    }
    transcript.clamp_scroll(avail.saturating_sub(height));
    let scroll = transcript.scroll();

    // Rows from the bottom: past the offset, then one viewport.
    let mut lines: Vec<Line<'_>> = Vec::with_capacity(height.min(avail));
    let mut skip = scroll;
    'collect: for block in transcript.blocks().rev().take(visited) {
        let rows = block.rows();
        if rows <= skip {
            skip -= rows;
            continue;
        }
        for index in (0..rows - skip).rev() {
            if lines.len() >= height {
                break 'collect;
            }
            if block.plain() {
                lines.push(Line::styled(block.row(index), style(block.kind, palette)));
            } else {
                let mut spans = Vec::new();
                block.spans(index, |face, text| {
                    spans.push(Span::styled(text, face_style(block.kind, face, palette)));
                });
                lines.push(Line::from(spans));
            }
        }
        skip = 0;
    }
    lines.reverse();
    frame.render_widget(Paragraph::new(lines), area);

    if scroll > 0 {
        draw_indicator(frame, area, scroll, palette);
    }
}

/// Browse mode: the rows below the viewport and the key back to
/// follow mode, on the viewport's last row at the right.
fn draw_indicator(frame: &mut Frame, area: Rect, below: usize, palette: &Palette) {
    let text = format!(" ↓ {below} below · End ");
    let Ok(width) = u16::try_from(display_width(&text)) else {
        return;
    };
    if width > area.width {
        return;
    }
    let corner = Rect {
        x: area.x + area.width - width,
        y: area.y + area.height - 1,
        width,
        height: 1,
    };
    let line = Line::styled(text, highlight(palette));
    frame.render_widget(Paragraph::new(line), corner);
}

/// The style of a highlighted row — the browse indicator, the
/// selected completion: reversed, on the accent when the theme has
/// one.
fn highlight(palette: &Palette) -> Style {
    paint(
        Style::default().add_modifier(Modifier::REVERSED),
        palette.color(Role::Accent),
    )
}

/// The input box: the editor's rows from the one the box scrolled
/// to, the prompt on the first row of the buffer, an indent on the
/// rest. The layout is a cache the editor keeps, so a frame walks
/// the rows it shows and a large paste costs its re-layout once.
/// `count` is the row count the caller's `sync` returned.
fn draw_input(
    frame: &mut Frame,
    area: Rect,
    editor: &mut Editor,
    count: usize,
    prefix: bool,
    palette: &Palette,
) {
    let width = usize::from(area.width);
    let prefix_cols = if prefix { display_width(PROMPT) } else { 0 };
    let height = usize::from(area.height);
    let top = editor.scroll_to_cursor(height);
    let text_style = paint(Style::default(), palette.color(Role::Text));
    let prompt_style = paint(Style::default(), palette.color(Role::Prompt));
    let mut lines = Vec::with_capacity(height);
    for row in top..count.min(top + height) {
        let mut text = String::with_capacity(width * 4);
        let mut used = 0usize;
        let mut spans = Vec::with_capacity(2);
        if prefix {
            if row == 0 {
                spans.push(Span::styled(PROMPT, prompt_style));
            } else {
                text.extend(std::iter::repeat_n(' ', prefix_cols));
            }
            used = prefix_cols;
        }
        for ch in editor.row(row).chars() {
            let Some(shown) = displayed(ch) else { continue };
            let cols = shown.width().unwrap_or(0);
            if used + cols > width {
                break;
            }
            text.push(shown);
            used += cols;
        }
        spans.push(Span::styled(text, text_style));
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), area);

    let cursor_y = editor.cursor_row().saturating_sub(top).min(height - 1);
    let cursor_x = (prefix_cols + editor.cursor_col()).min(width.saturating_sub(1));
    frame.set_cursor_position((area.x + cursor_x as u16, area.y + cursor_y as u16));
}

/// The completion popup, over the rows above the input box: one row
/// per matching command, the usage and the help, the selected one
/// reversed. It sits at the prompt's text column. A popup taller
/// than its cap scrolls to keep the selection in view.
fn draw_popup(frame: &mut Frame, above: Rect, editor: &Editor, prefix: bool, palette: &Palette) {
    let matches: Vec<&CommandSpec> = editor.matches().collect();
    let rows = matches
        .len()
        .min(MAX_POPUP_ROWS)
        .min(usize::from(above.height));
    if rows == 0 {
        return;
    }
    let selected = editor.selected().min(matches.len() - 1);
    let first = selected.saturating_sub(rows - 1);
    let x = if prefix {
        u16::try_from(display_width(PROMPT)).unwrap_or(0)
    } else {
        0
    };
    let usage_cols = matches
        .iter()
        .map(|command| display_width(command.usage()))
        .max()
        .unwrap_or(0);
    let popup_cols = matches
        .iter()
        .map(|command| usage_cols + 4 + display_width(command.help()))
        .max()
        .unwrap_or(0);
    let width = u16::try_from(popup_cols)
        .unwrap_or(u16::MAX)
        .min(above.width.saturating_sub(x));
    if width == 0 {
        return;
    }
    let area = Rect {
        x: above.x + x,
        y: above.y + above.height - rows as u16,
        width,
        height: rows as u16,
    };
    let mut lines = Vec::with_capacity(rows);
    let width_cols = usize::from(width);
    for (index, command) in matches.iter().enumerate().skip(first).take(rows) {
        // The usage column, padded to the widest usage, then the
        // help; the row fills the popup width.
        let (mut text, mut used) = fit_row([" ", command.usage()], width_cols);
        let pad = (usage_cols + 3).saturating_sub(used).min(width_cols - used);
        text.extend(std::iter::repeat_n(' ', pad));
        used += pad;
        let (help, help_used) = fit_row([command.help(), " "], width_cols - used);
        text.push_str(&help);
        used += help_used;
        text.extend(std::iter::repeat_n(' ', width_cols - used));
        let style = if index == selected {
            highlight(palette)
        } else {
            paint(
                Style::default().add_modifier(Modifier::DIM),
                palette.color(Role::Dim),
            )
        };
        lines.push(Line::styled(text, style));
    }
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{Color, Depth, Theme};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn palette() -> Palette {
        Palette::new(&Theme::default(), Depth::TrueColor)
    }

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
        let mut transcript = Transcript::new(false);
        let mut editor = Editor::new(Vec::new());
        let status = status();
        let palette = palette();
        let (lines, rows) = render(
            30,
            4,
            View {
                transcript: &mut transcript,
                editor: &mut editor,
                status: &status,
                palette: &palette,
                approval: None,
                progress: None,
            },
        );
        assert_eq!(rows, 2);
        assert_eq!(lines[2].trim_end(), "dmz> ".trim_end());
        assert!(lines[3].contains("test-model · ask"), "{:?}", lines[3]);
    }

    #[test]
    fn the_status_bar_names_the_session_and_the_totals() {
        let mut transcript = Transcript::new(false);
        let mut editor = Editor::new(Vec::new());
        let status = Status {
            session: Some("s-9".into()),
            input_tokens: 12,
            output_tokens: 5,
            cached_tokens: 2,
            ..status()
        };
        let palette = palette();
        let (lines, _) = render(
            60,
            3,
            View {
                transcript: &mut transcript,
                editor: &mut editor,
                status: &status,
                palette: &palette,
                approval: None,
                progress: None,
            },
        );
        assert!(
            lines[2].contains("test-model · ask · s-9 · 12 in · 5 out · 2 cached"),
            "{:?}",
            lines[2]
        );
    }

    #[test]
    fn a_narrow_bar_cuts_from_the_right() {
        let mut transcript = Transcript::new(false);
        let mut editor = Editor::new(Vec::new());
        let status = status();
        let palette = palette();
        let (lines, _) = render(
            12,
            3,
            View {
                transcript: &mut transcript,
                editor: &mut editor,
                status: &status,
                palette: &palette,
                approval: None,
                progress: None,
            },
        );
        assert_eq!(lines[2], " test-model ");
    }

    #[test]
    fn a_two_row_screen_drops_the_bar_and_keeps_the_prompt() {
        let mut transcript = Transcript::new(false);
        transcript.push_line(Kind::Text, "tail");
        let mut editor = Editor::new(Vec::new());
        let status = status();
        let palette = palette();
        let (lines, rows) = render(
            20,
            2,
            View {
                transcript: &mut transcript,
                editor: &mut editor,
                status: &status,
                palette: &palette,
                approval: None,
                progress: None,
            },
        );
        assert_eq!(rows, 1);
        assert_eq!(lines[0].trim_end(), "tail");
        assert_eq!(lines[1].trim_end(), "dmz> ".trim_end());
    }

    #[test]
    fn the_transcript_tail_sticks_to_the_bottom_of_the_pane() {
        let mut transcript = Transcript::new(false);
        for i in 0..10 {
            transcript.push_line(Kind::Text, &format!("line {i}"));
        }
        let mut editor = Editor::new(Vec::new());
        let status = status();
        let palette = palette();
        let (lines, _) = render(
            20,
            4,
            View {
                transcript: &mut transcript,
                editor: &mut editor,
                status: &status,
                palette: &palette,
                approval: None,
                progress: None,
            },
        );
        assert_eq!(lines[0].trim_end(), "line 8");
        assert_eq!(lines[1].trim_end(), "line 9");
    }

    #[test]
    fn scroll_moves_the_window_and_clamps_at_the_top() {
        let mut transcript = Transcript::new(false);
        for i in 0..10 {
            transcript.push_line(Kind::Text, &format!("line {i}"));
        }
        let mut editor = Editor::new(Vec::new());
        let status = status();
        let palette = palette();
        // The first frame sets the width; a scroll before it would
        // reset with the width change.
        render(
            40,
            4,
            View {
                transcript: &mut transcript,
                editor: &mut editor,
                status: &status,
                palette: &palette,
                approval: None,
                progress: None,
            },
        );
        transcript.scroll_up(3);
        let (lines, _) = render(
            40,
            4,
            View {
                transcript: &mut transcript,
                editor: &mut editor,
                status: &status,
                palette: &palette,
                approval: None,
                progress: None,
            },
        );
        // The indicator sits at the right of the bottom row.
        assert!(lines[1].starts_with("line 6"), "{:?}", lines[1]);

        transcript.scroll_up(100);
        let (lines, _) = render(
            40,
            4,
            View {
                transcript: &mut transcript,
                editor: &mut editor,
                status: &status,
                palette: &palette,
                approval: None,
                progress: None,
            },
        );
        assert_eq!(lines[0].trim_end(), "line 0");
        assert_eq!(transcript.scroll(), 8);
    }

    #[test]
    fn browse_mode_shows_the_rows_below_and_holds_still_as_rows_arrive() {
        let mut transcript = Transcript::new(false);
        for i in 0..10 {
            transcript.push_line(Kind::Text, &format!("line {i}"));
        }
        let mut editor = Editor::new(Vec::new());
        let status = status();
        let palette = palette();
        let mut view = |transcript: &mut Transcript| {
            render(
                30,
                4,
                View {
                    transcript,
                    editor: &mut editor,
                    status: &status,
                    palette: &palette,
                    approval: None,
                    progress: None,
                },
            )
            .0
        };
        // Follow mode: no indicator, the tail at the bottom.
        let lines = view(&mut transcript);
        assert_eq!(lines[1].trim_end(), "line 9");
        assert!(!lines[1].contains("below"));

        transcript.scroll_up(3);
        let lines = view(&mut transcript);
        assert_eq!(lines[0].trim_end(), "line 5");
        assert!(lines[1].starts_with("line 6"), "{:?}", lines[1]);
        assert!(lines[1].contains("3 below"), "{:?}", lines[1]);

        // Rows that arrive below the window leave it in place and
        // count on the indicator.
        transcript.push_line(Kind::Text, "line 10");
        transcript.push_delta(
            Kind::Text,
            "line 11
line 12",
        );
        let lines = view(&mut transcript);
        assert_eq!(lines[0].trim_end(), "line 5");
        assert!(lines[1].contains("6 below"), "{:?}", lines[1]);

        // End: the tail again, no indicator.
        transcript.follow();
        let lines = view(&mut transcript);
        assert_eq!(lines[1].trim_end(), "line 12");
        assert!(!lines[1].contains("below"));
    }

    #[test]
    fn long_entries_wrap_inside_the_pane() {
        let mut transcript = Transcript::new(false);
        transcript.push_line(Kind::Text, "alpha beta gamma");
        let mut editor = Editor::new(Vec::new());
        let status = status();
        let palette = palette();
        let (lines, _) = render(
            8,
            5,
            View {
                transcript: &mut transcript,
                editor: &mut editor,
                status: &status,
                palette: &palette,
                approval: None,
                progress: None,
            },
        );
        assert_eq!(lines[0].trim_end(), "alpha");
        assert_eq!(lines[1].trim_end(), "beta");
        assert_eq!(lines[2].trim_end(), "gamma");
    }

    /// Draw one frame and return the rows, the cursor, and the
    /// buffer, for the input-box tests.
    fn render_cursor(
        width: u16,
        height: u16,
        editor: &mut Editor,
    ) -> (Vec<String>, (u16, u16), ratatui::buffer::Buffer) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut transcript = Transcript::new(false);
        let status = status();
        let palette = palette();
        terminal
            .draw(|frame| {
                draw(
                    frame,
                    View {
                        transcript: &mut transcript,
                        editor,
                        status: &status,
                        palette: &palette,
                        approval: None,
                        progress: None,
                    },
                );
            })
            .unwrap();
        let position = terminal.get_cursor_position().unwrap();
        let buffer = terminal.backend().buffer().clone();
        let lines = (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect();
        (lines, (position.x, position.y), buffer)
    }

    #[test]
    fn a_long_input_wraps_into_the_box_and_the_box_scrolls_to_the_cursor() {
        let mut editor = Editor::new(Vec::new());
        editor.set(&"abcdefghij".repeat(3));
        // A two-row screen keeps a one-row box: the row with the
        // cursor shows, indented, without the prompt.
        let (lines, cursor, _) = render_cursor(10, 2, &mut editor);
        assert_eq!(lines[1], "     ij   ");
        assert_eq!(cursor, (7, 1));
        // A tall screen shows every row under the cap, the prompt
        // on the first.
        let (lines, cursor, _) = render_cursor(10, 12, &mut editor);
        assert_eq!(lines[3], "dmz> abcd ");
        assert_eq!(lines[4], "     efgh ");
        assert_eq!(lines[10], "     ij   ");
        assert_eq!(cursor, (7, 10));
        // Past the cap the box scrolls: the cursor at the top row
        // brings the first rows back.
        editor.home();
        for _ in 0..7 {
            editor.up();
        }
        let (lines, cursor, _) = render_cursor(10, 8, &mut editor);
        assert_eq!(lines[1], "dmz> abcd ");
        assert_eq!(lines[6], "     abcd ");
        assert_eq!(cursor, (5, 1));
    }

    #[test]
    fn a_multi_line_input_shows_each_line_on_its_row() {
        let mut editor = Editor::new(Vec::new());
        editor.set("one\ntwo");
        let (lines, cursor, _) = render_cursor(20, 4, &mut editor);
        assert_eq!(lines[1].trim_end(), "dmz> one");
        assert_eq!(lines[2].trim_end(), "     two");
        assert_eq!(cursor, (8, 2));
        assert!(lines[3].contains("test-model"), "{:?}", lines[3]);
        // A one-row screen keeps the row with the cursor.
        let (lines, cursor, _) = render_cursor(20, 1, &mut editor);
        assert_eq!(lines[0].trim_end(), "     two");
        assert_eq!(cursor, (8, 0));
    }

    #[test]
    fn a_wide_char_at_the_box_edge_keeps_the_cursor_aligned() {
        let mut editor = Editor::new(Vec::new());
        editor.set("日本日本日");
        // Four text columns: two wide chars per row, the fifth on a
        // row of its own, the cursor after it.
        let (_, cursor, buffer) = render_cursor(10, 2, &mut editor);
        assert_eq!(cursor, (7, 1));
        assert_eq!(buffer[(5u16, 1u16)].symbol(), "日");
    }

    #[test]
    fn a_wide_char_shows_on_a_two_column_screen() {
        let mut editor = Editor::new(Vec::new());
        editor.set("日");
        let (lines, cursor, _) = render_cursor(2, 2, &mut editor);
        assert_eq!(lines[1].trim_end(), "日");
        assert_eq!(cursor, (1, 1));
        // A screen too narrow for the prompt and two text columns
        // drops the prompt.
        let (lines, cursor, _) = render_cursor(7, 2, &mut editor);
        assert_eq!(lines[1].trim_end(), "日");
        assert_eq!(cursor, (2, 1));
    }

    #[test]
    fn the_input_box_growing_holds_the_browsed_top_row() {
        let mut terminal = Terminal::new(TestBackend::new(30, 8)).unwrap();
        let mut transcript = Transcript::new(false);
        for i in 0..20 {
            transcript.push_line(Kind::Text, &format!("line {i:02}"));
        }
        let mut editor = Editor::new(Vec::new());
        let status = status();
        let palette = palette();
        let mut top_line = |transcript: &mut Transcript, editor: &mut Editor| {
            terminal
                .draw(|frame| {
                    draw(
                        frame,
                        View {
                            transcript,
                            editor,
                            status: &status,
                            palette: &palette,
                            approval: None,
                            progress: None,
                        },
                    );
                })
                .unwrap();
            let buffer = terminal.backend().buffer().clone();
            (0..30u16)
                .map(|x| buffer[(x, 0u16)].symbol())
                .collect::<String>()
        };
        top_line(&mut transcript, &mut editor);
        transcript.scroll_up(5);
        let before = top_line(&mut transcript, &mut editor);
        assert_eq!(before.trim_end(), "line 09");
        // Two more editor rows take two transcript rows; the top row
        // stays.
        editor.set("one\ntwo\nthree");
        let after = top_line(&mut transcript, &mut editor);
        assert_eq!(after.trim_end(), "line 09");
        assert_eq!(transcript.scroll(), 7);
    }

    #[test]
    fn the_popup_lists_the_matches_above_the_box_with_one_selected() {
        let commands = vec![
            CommandSpec::new("/help", "list the commands"),
            CommandSpec::new("/model [name]", "show or switch the session model"),
        ];
        let mut editor = Editor::new(commands);
        editor.set("/");
        let (lines, cursor, buffer) = render_cursor(60, 6, &mut editor);
        assert_eq!(cursor, (6, 4));
        assert_eq!(lines[4].trim_end(), "dmz> /");
        // One row per match: the usage, then the help.
        assert!(lines[2].contains("/help"), "{:?}", lines[2]);
        assert!(lines[2].contains("list the commands"), "{:?}", lines[2]);
        assert!(lines[3].contains("/model [name]"), "{:?}", lines[3]);
        assert!(
            lines[3].contains("show or switch the session model"),
            "{:?}",
            lines[3]
        );
        let selected = buffer[(6u16, 2u16)].style();
        let other = buffer[(6u16, 3u16)].style();
        assert!(selected.add_modifier.contains(Modifier::REVERSED));
        assert!(!other.add_modifier.contains(Modifier::REVERSED));
        // The selection moves; a narrower match set shrinks the popup.
        editor.select_next();
        let (_, _, buffer) = render_cursor(60, 6, &mut editor);
        assert!(
            buffer[(6u16, 3u16)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        editor.insert_char('m');
        let (lines, _, _) = render_cursor(60, 6, &mut editor);
        assert!(lines[3].contains("/model [name]"), "{:?}", lines[3]);
        assert!(!lines[2].contains("/help"), "{:?}", lines[2]);
        // A dismissed popup leaves the rows to the transcript.
        editor.dismiss();
        let (lines, _, _) = render_cursor(60, 6, &mut editor);
        assert!(!lines[3].contains("/model"), "{:?}", lines[3]);
    }

    #[test]
    fn markdown_rows_carry_their_faces_into_the_frame() {
        let mut terminal = Terminal::new(TestBackend::new(30, 5)).unwrap();
        let mut transcript = Transcript::new(true);
        transcript.push_delta(Kind::Text, "# Title\n\nplain `code`");
        let mut editor = Editor::new(Vec::new());
        let status = status();
        let palette = palette();
        terminal
            .draw(|frame| {
                draw(
                    frame,
                    View {
                        transcript: &mut transcript,
                        editor: &mut editor,
                        status: &status,
                        palette: &palette,
                        approval: None,
                        progress: None,
                    },
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        // The mark, the heading, and the code each read apart from
        // the plain text; which attribute is the theme's call.
        let plain = buffer[(0u16, 2u16)].style();
        assert_eq!(buffer[(0u16, 0u16)].symbol(), "#");
        assert_ne!(buffer[(0u16, 0u16)].style(), plain);
        assert_eq!(buffer[(2u16, 0u16)].symbol(), "T");
        assert_ne!(buffer[(2u16, 0u16)].style(), plain);
        assert_ne!(buffer[(2u16, 0u16)].style(), buffer[(0u16, 0u16)].style());
        assert_eq!(buffer[(6u16, 2u16)].symbol(), "c");
        assert_ne!(buffer[(6u16, 2u16)].style(), plain);
    }

    /// Draw one frame of a formatted reply and a card over `palette`
    /// and return the buffer.
    fn themed_buffer(palette: &Palette) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(40, 6)).unwrap();
        let mut transcript = Transcript::new(true);
        transcript.push_delta(Kind::Text, "# Title\n\nplain `code`");
        transcript.push_line(Kind::Error, "failed");
        let mut editor = Editor::new(Vec::new());
        let status = status();
        terminal
            .draw(|frame| {
                draw(
                    frame,
                    View {
                        transcript: &mut transcript,
                        editor: &mut editor,
                        status: &status,
                        palette,
                        approval: None,
                        progress: None,
                    },
                );
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn a_theme_paints_the_roles_and_no_color_keeps_the_attributes() {
        use ratatui::style::Color as TermColor;
        let mut theme = Theme::dark();
        theme.set(Role::Heading, Some(Color::Rgb(1, 2, 3)));
        theme.set(Role::InlineCode, Some(Color::Indexed(6)));
        let colored = themed_buffer(&Palette::new(&theme, Depth::TrueColor));
        // Rows: title, blank, plain `code`, error, prompt, bar.
        let heading = colored[(2u16, 0u16)].style();
        assert_eq!(heading.fg, Some(TermColor::Rgb(1, 2, 3)));
        assert!(heading.add_modifier.contains(Modifier::BOLD));
        assert_eq!(colored[(6u16, 2u16)].style().fg, Some(TermColor::Cyan));
        assert_eq!(
            colored[(0u16, 3u16)].style().fg,
            Some(TermColor::Rgb(0xe3, 0x70, 0x70))
        );
        // The bar takes the theme's ground instead of reversing.
        let bar = colored[(1u16, 5u16)].style();
        assert_eq!(bar.bg, Some(TermColor::Rgb(0x36, 0x3e, 0x4e)));
        assert!(!bar.add_modifier.contains(Modifier::REVERSED));
        // `none` on a nested role takes the enclosing color away.
        theme.set(Role::InlineCode, None);
        let reset = themed_buffer(&Palette::new(&theme, Depth::TrueColor));
        assert_eq!(reset[(6u16, 2u16)].style().fg, Some(TermColor::Reset));
        assert_ne!(reset[(0u16, 2u16)].style().fg, Some(TermColor::Reset));
        theme.set(Role::InlineCode, Some(Color::Indexed(6)));
        // A text color with no ground reverses onto the text color.
        theme.set(Role::StatusBar, None);
        let bar = themed_buffer(&Palette::new(&theme, Depth::TrueColor))[(1u16, 5u16)].style();
        assert_eq!(bar.fg, Some(TermColor::Rgb(0xd0, 0xd8, 0xe4)));
        assert!(bar.add_modifier.contains(Modifier::REVERSED));
        theme.set(Role::StatusBar, Theme::dark().get(Role::StatusBar));
        // A ground with no text color reverses onto the ground.
        theme.set(Role::Text, None);
        let bar = themed_buffer(&Palette::new(&theme, Depth::TrueColor))[(1u16, 5u16)].style();
        assert_eq!(bar.fg, Some(TermColor::Rgb(0x36, 0x3e, 0x4e)));
        assert!(bar.add_modifier.contains(Modifier::REVERSED));
        theme.set(Role::Text, Theme::dark().get(Role::Text));
        // The prompt takes its role's color.
        assert_eq!(
            colored[(0u16, 4u16)].style().fg,
            Some(TermColor::Rgb(0x6f, 0xa8, 0xdc))
        );

        // Under 256 colors an RGB value lands on a palette index.
        let indexed = themed_buffer(&Palette::new(&theme, Depth::Ansi256));
        assert!(matches!(
            indexed[(0u16, 3u16)].style().fg,
            Some(TermColor::Indexed(16..=255))
        ));

        // No color: the attributes carry the faces alone, and the bar
        // reverses again.
        // A cell with no color reads as the terminal's own.
        let plain = themed_buffer(&Palette::new(&theme, Depth::None));
        let heading = plain[(2u16, 0u16)].style();
        assert_eq!(heading.fg, Some(TermColor::Reset));
        assert!(heading.add_modifier.contains(Modifier::BOLD));
        assert_eq!(plain[(6u16, 2u16)].style().fg, Some(TermColor::Reset));
        assert_eq!(plain[(0u16, 3u16)].style().fg, Some(TermColor::Reset));
        assert!(
            plain[(1u16, 5u16)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn the_progress_line_sits_above_the_input_and_takes_a_transcript_row() {
        let mut transcript = Transcript::new(false);
        for i in 0..5 {
            transcript.push_line(Kind::Text, &format!("line {i}"));
        }
        let mut editor = Editor::new(Vec::new());
        let status = status();
        let palette = palette();
        let (lines, rows) = render(
            30,
            5,
            View {
                transcript: &mut transcript,
                editor: &mut editor,
                status: &status,
                palette: &palette,
                approval: None,
                progress: Some("⠋ 3s · bash\x1b[2J"),
            },
        );
        assert_eq!(rows, 2);
        assert_eq!(lines[1].trim_end(), "line 4");
        assert_eq!(lines[2].trim_end(), "⠋ 3s · bash[2J");
        assert_eq!(lines[3].trim_end(), "dmz>");
        assert!(lines[4].contains("test-model · ask"), "{:?}", lines[4]);
        // A three-row screen keeps the bar and drops the line.
        let (lines, rows) = render(
            30,
            3,
            View {
                transcript: &mut transcript,
                editor: &mut editor,
                status: &status,
                palette: &palette,
                approval: None,
                progress: Some("⠋ 3s · bash"),
            },
        );
        assert_eq!(rows, 1);
        assert_eq!(lines[1].trim_end(), "dmz>");
        assert!(lines[2].contains("test-model"), "{:?}", lines[2]);
    }

    #[test]
    fn an_approval_question_replaces_the_prompt() {
        let mut transcript = Transcript::new(false);
        let mut editor = Editor::new(Vec::new());
        let status = status();
        let palette = palette();
        let (lines, _) = render(
            40,
            2,
            View {
                transcript: &mut transcript,
                editor: &mut editor,
                status: &status,
                palette: &palette,
                approval: Some("approve ba\x1bsh? [y]es / [a]lways / [N]o"),
                progress: None,
            },
        );
        // The tool name is model-controlled: no escape reaches the
        // row.
        assert_eq!(lines[1].trim_end(), "approve bash? [y]es / [a]lways / [N]o");
    }
}
