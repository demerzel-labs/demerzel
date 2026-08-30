//! Frame rendering: the transcript pane above, the progress line
//! while a turn runs, the input box in its frame with the status on
//! the frame's bottom edge, the completion popup over the rows
//! above the box, and, on a screen too small for the frame, the
//! status bar at the bottom.
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
use ratatui::widgets::{Block as Bordered, BorderType, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthChar as _;

use crate::PROMPT;
use crate::editor::{CommandSpec, Editor, displayed};
use crate::markdown::Face;
use crate::select;
use crate::theme::{Palette, Role};
use crate::transcript::{Block, Kind, Transcript};
use crate::wrap::display_width;

/// Rows the input box grows to before it scrolls inside itself.
const MAX_INPUT_ROWS: u16 = 8;

/// Rows the completion popup shows at most; more matches scroll
/// inside it.
const MAX_POPUP_ROWS: usize = 8;

/// Screen size from which the input box gets its frame: a box with
/// rounded corners whose bottom edge carries the status — the model,
/// the mode, the effort, the totals — between line segments. A
/// smaller screen keeps every row and column for content and shows
/// the status as a bar.
const FRAMED_HEIGHT: u16 = 8;
const FRAMED_WIDTH: u16 = 16;

/// Rows the frame takes: the box's top edge, a blank row under the
/// input, and the bottom edge with the status.
const FRAME_ROWS: u16 = 3;

/// Columns the frame takes at each side: the box's edge.
const FRAME_INSET: u16 = 1;

/// What the status names: the model, the mode, the reasoning
/// effort, and the open session's token totals, counted from this
/// run's start. A session change restarts the totals. The session's
/// id is a `/session` reply, not a segment.
pub(crate) struct Status {
    pub model: String,
    pub mode: String,
    /// The effort's name; empty shows no segment.
    pub effort: String,
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
        view.transcript.clear_shown();
        return 0;
    }
    // The status bar takes the bottom row when the screen has room
    // for all three panes; a two-row screen keeps transcript and
    // input. The input box sits above the bar and grows with its
    // rows up to the cap, and the transcript keeps one row at least.
    // The progress line takes one more row above the box when it
    // shows and the screen has a fourth row. On a screen of eight
    // rows and sixteen columns or more the box gets its frame, a
    // box with rounded corners, and the status moves onto its
    // bottom edge; the bar row goes, and the progress line gets a
    // blank row above it, apart from the reply.
    let framed = area.height >= FRAMED_HEIGHT && area.width >= FRAMED_WIDTH;
    let bar_rows = u16::from(!framed && area.height >= 3);
    let frame_rows = if framed { FRAME_ROWS } else { 0 };
    let inset = if framed { FRAME_INSET } else { 0 };
    let progress_rows = match view.progress {
        Some(_) if framed => 2,
        Some(_) if area.height >= 4 => 1,
        _ => 0,
    };
    let box_cap = (area.height - bar_rows - progress_rows - frame_rows)
        .saturating_sub(1)
        .clamp(1, MAX_INPUT_ROWS);
    let (text_width, prefix) = input_columns(area.width - 2 * inset);
    let editor_rows = view.editor.sync(text_width);
    let input_rows = match view.approval {
        Some(_) => 1,
        None => u16::try_from(editor_rows)
            .unwrap_or(u16::MAX)
            .clamp(1, box_cap),
    };
    // From the bottom: the bar, or the box's bottom edge and the
    // blank row above it when framed, then the input rows.
    let input_area = Rect {
        x: area.x + inset,
        y: area.y + area.height - bar_rows - frame_rows.saturating_sub(1) - input_rows,
        width: area.width - 2 * inset,
        height: input_rows,
    };
    let transcript_area = Rect {
        height: area.height - input_rows - bar_rows - progress_rows - frame_rows,
        ..area
    };

    let palette = view.palette;
    draw_transcript(frame, transcript_area, view.transcript, palette);
    if framed {
        let frame_area = Rect {
            y: input_area.y - 1,
            height: input_rows + FRAME_ROWS,
            ..area
        };
        draw_frame(frame, frame_area, view.status, palette);
    }
    if let Some(progress) = view.progress
        && progress_rows > 0
    {
        let progress_row = Rect {
            y: input_area.y - 1 - u16::from(framed),
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
            // The popup sits above the box's top edge, at the text
            // column of the prompt.
            let above = Rect {
                height: input_area.y - area.y - u16::from(framed),
                ..area
            };
            let text_x = inset
                + if prefix {
                    u16::try_from(display_width(PROMPT)).unwrap_or(0)
                } else {
                    0
                };
            if let Some(popup) = draw_popup(frame, above, view.editor, text_x, palette) {
                // The popup hides the cells under it: a press there
                // must not select what it covers.
                let y = usize::from(popup.y - area.y);
                let x = usize::from(popup.x - area.x);
                view.transcript.cover(
                    y..y + usize::from(popup.height),
                    x..x + usize::from(popup.width),
                );
            }
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
/// theme sets none: model, mode, effort, token totals, cut to the
/// width from the right. The small-screen form of the status.
fn draw_status(frame: &mut Frame, area: Rect, status: &Status, palette: &Palette) {
    let totals = format!(
        " · {} in · {} out · {} cached",
        status.input_tokens, status.output_tokens, status.cached_tokens
    );
    let (effort_sep, effort_label) = if status.effort.is_empty() {
        ("", "")
    } else {
        (" · ", EFFORT_LABEL)
    };
    let segments = [
        " ",
        status.model.as_str(),
        " · ",
        status.mode.as_str(),
        effort_sep,
        effort_label,
        status.effort.as_str(),
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

/// The frame around the input box: a dim box with rounded corners,
/// the status on its bottom edge. The top and the sides come from
/// the border widget, which writes the cells with no allocation;
/// the bottom edge is its own line. The input rows draw inside
/// afterwards.
fn draw_frame(frame: &mut Frame, area: Rect, status: &Status, palette: &Palette) {
    let style = paint(
        Style::default().add_modifier(Modifier::DIM),
        palette.color(Role::Dim),
    );
    let sides = Bordered::new()
        .borders(Borders::TOP | Borders::LEFT | Borders::RIGHT)
        .border_type(BorderType::Rounded)
        .border_style(style);
    frame.render_widget(sides, area);
    let bottom = Rect {
        y: area.y + area.height - 1,
        height: 1,
        ..area
    };
    frame.render_widget(
        Paragraph::new(status_edge(status, usize::from(area.width), palette)),
        bottom,
    );
}

/// The label before the effort on the status, so the level reads
/// apart from the mode next to it.
const EFFORT_LABEL: &str = "effort ";

/// The box's bottom edge with the status on it: the model, the
/// mode, the effort, and the token totals, each between line
/// segments, cut to the width from the right; the corners stay. The
/// segments take the dim role's color, so they read apart from the
/// typed text and follow the theme; with no color they dim like the
/// lines.
fn status_edge(status: &Status, width: usize, palette: &Palette) -> Line<'static> {
    let dim = paint(
        Style::default().add_modifier(Modifier::DIM),
        palette.color(Role::Dim),
    );
    let text = match palette.color(Role::Dim) {
        Some(color) => Style::default().fg(color),
        None => dim,
    };
    let totals = format!(
        "{} in · {} out · {} cached",
        status.input_tokens, status.output_tokens, status.cached_tokens
    );
    // Each segment is a label and its text; an empty text is no
    // segment.
    let segments = [
        ("", status.model.as_str()),
        ("", status.mode.as_str()),
        (EFFORT_LABEL, status.effort.as_str()),
        ("", totals.as_str()),
    ];
    // The caller frames a screen of sixteen columns or more, so the
    // corners and the first line always fit.
    debug_assert!(width >= usize::from(FRAMED_WIDTH));
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(12);
    // The right corner keeps the last column.
    let budget = width.saturating_sub(1);
    spans.push(Span::styled("╰─", dim));
    let mut used = 2usize;
    for (label, segment) in segments {
        if segment.is_empty() {
            continue;
        }
        // One column stays for the line after the segment.
        let room = budget.saturating_sub(used + 1);
        let (piece, cols) = fit_row([" ", label, segment, " "], room);
        // The leading space always fits: a piece of one column
        // holds none of the segment, and the line fills the rest.
        if cols <= 1 {
            break;
        }
        spans.push(Span::styled(piece, text));
        spans.push(Span::styled("─", dim));
        used += cols + 1;
    }
    if used < budget {
        spans.push(Span::styled("─".repeat(budget - used), dim));
    }
    spans.push(Span::styled("╯", dim));
    Line::from(spans)
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
fn fit_row<const N: usize>(parts: [&str; N], width: usize) -> (String, usize) {
    let bytes: usize = parts.iter().map(|part| part.len()).sum();
    let mut shown = String::with_capacity(bytes.min(width.saturating_mul(4)));
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
        transcript.clear_shown();
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

    // Rows from the bottom: past the offset, then one viewport. The
    // row map records the block and row on each viewport row, for
    // the mouse.
    let mut shown = transcript.take_shown();
    shown.clear();
    let selection = transcript.selection();
    let selected = highlight(palette);
    let mut lines: Vec<Line<'_>> = Vec::with_capacity(height.min(avail));
    let mut skip = scroll;
    let dropped = transcript.dropped();
    'collect: for (offset, block) in transcript.blocks().enumerate().rev().take(visited) {
        let rows = block.rows();
        if rows <= skip {
            skip -= rows;
            continue;
        }
        let absolute = dropped + offset;
        for index in (0..rows - skip).rev() {
            if lines.len() >= height {
                break 'collect;
            }
            shown.push((absolute, index));
            let columns = selection.and_then(|selection| selection.columns(absolute, index));
            if let Some((from, to)) = columns {
                lines.push(selected_line(block, index, from, to, selected, palette));
            } else if block.plain() {
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
    shown.reverse();
    frame.render_widget(Paragraph::new(lines), area);
    transcript.set_shown(shown);

    if scroll > 0
        && let Some(corner) = draw_indicator(frame, area, scroll, palette)
    {
        // The indicator hides the cells under it, like the popup,
        // and a press on it is the key it names.
        let y = usize::from(corner.y - area.y);
        let x = usize::from(corner.x - area.x);
        transcript.set_indicator(y, x..x + usize::from(corner.width));
    }
}

/// One row with the selection over columns `from..to`: the row's
/// spans, split at the window's edges, the part inside in the
/// highlight style. A selected row costs a column split and up to
/// two spans more than a plain one.
fn selected_line<'a>(
    block: &'a Block,
    index: usize,
    from: usize,
    to: usize,
    selected: Style,
    palette: &Palette,
) -> Line<'a> {
    let mut spans = Vec::with_capacity(3);
    let mut col = 0usize;
    let mut push = |face: Face, text: &'a str| {
        let style = face_style(block.kind, face, palette);
        let (start, end) = select::window(text, from, to, &mut col);
        if start > 0 {
            spans.push(Span::styled(&text[..start], style));
        }
        if end > start {
            spans.push(Span::styled(&text[start..end], selected));
        }
        if end < text.len() {
            spans.push(Span::styled(&text[end..], style));
        }
    };
    if block.plain() {
        push(Face::PLAIN, block.row(index));
    } else {
        block.spans(index, push);
    }
    Line::from(spans)
}

/// Browse mode: the rows below the viewport and the key back to
/// follow mode, on the viewport's last row at the right. Returns
/// the cells it took, when it drew.
fn draw_indicator(frame: &mut Frame, area: Rect, below: usize, palette: &Palette) -> Option<Rect> {
    let text = format!(" ↓ {below} below · End ");
    let Ok(width) = u16::try_from(display_width(&text)) else {
        return None;
    };
    if width > area.width {
        return None;
    }
    let corner = Rect {
        x: area.x + area.width - width,
        y: area.y + area.height - 1,
        width,
        height: 1,
    };
    let line = Line::styled(text, highlight(palette));
    frame.render_widget(Paragraph::new(line), corner);
    Some(corner)
}

/// The style of a highlighted row — the browse indicator, the
/// selected completion, the drag selection: reversed, on the accent
/// when the theme has one.
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
/// reversed. It sits at `text_x`, the prompt's text column. A popup
/// taller than its cap scrolls to keep the selection in view.
/// Returns the area the popup took, when it drew.
fn draw_popup(
    frame: &mut Frame,
    above: Rect,
    editor: &Editor,
    text_x: u16,
    palette: &Palette,
) -> Option<Rect> {
    let matches: Vec<&CommandSpec> = editor.matches().collect();
    let rows = matches
        .len()
        .min(MAX_POPUP_ROWS)
        .min(usize::from(above.height));
    if rows == 0 {
        return None;
    }
    let selected = editor.selected().min(matches.len() - 1);
    let first = selected.saturating_sub(rows - 1);
    let x = text_x;
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
        return None;
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
    Some(area)
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
            effort: "high".into(),
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
        assert_eq!(lines[2].trim_end(), "❯");
        assert!(lines[3].contains("test-model · ask"), "{:?}", lines[3]);
    }

    #[test]
    fn the_status_bar_names_the_totals() {
        let mut transcript = Transcript::new(false);
        let mut editor = Editor::new(Vec::new());
        let status = Status {
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
            lines[2].contains("test-model · ask · effort high · 12 in · 5 out · 2 cached"),
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
        assert_eq!(lines[1].trim_end(), "❯");
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
        assert_eq!(lines[1], "  ij      ");
        assert_eq!(cursor, (4, 1));
        // A tall screen shows every row under the cap, the prompt
        // on the first. Ten columns are too few for the frame.
        let (lines, cursor, _) = render_cursor(10, 12, &mut editor);
        assert_eq!(lines[6], "❯ abcdefg ");
        assert_eq!(lines[7], "  hijabcd ");
        assert_eq!(lines[10], "  ij      ");
        assert_eq!(cursor, (4, 10));
        // Past the cap the box scrolls: the cursor at the top row
        // brings the first rows back.
        editor.home();
        for _ in 0..7 {
            editor.up();
        }
        let (lines, cursor, _) = render_cursor(10, 8, &mut editor);
        assert_eq!(lines[2], "❯ abcdefg ");
        assert_eq!(lines[6], "  ij      ");
        assert_eq!(cursor, (2, 2));
    }

    #[test]
    fn a_multi_line_input_shows_each_line_on_its_row() {
        let mut editor = Editor::new(Vec::new());
        editor.set("one\ntwo");
        let (lines, cursor, _) = render_cursor(20, 4, &mut editor);
        assert_eq!(lines[1].trim_end(), "❯ one");
        assert_eq!(lines[2].trim_end(), "  two");
        assert_eq!(cursor, (5, 2));
        assert!(lines[3].contains("test-model"), "{:?}", lines[3]);
        // A one-row screen keeps the row with the cursor.
        let (lines, cursor, _) = render_cursor(20, 1, &mut editor);
        assert_eq!(lines[0].trim_end(), "  two");
        assert_eq!(cursor, (5, 0));
    }

    #[test]
    fn a_wide_char_at_the_box_edge_keeps_the_cursor_aligned() {
        let mut editor = Editor::new(Vec::new());
        editor.set("日本日本日");
        // Seven text columns: three wide chars on the first row,
        // two on the second, the cursor after them.
        let (_, cursor, buffer) = render_cursor(10, 2, &mut editor);
        assert_eq!(cursor, (6, 1));
        assert_eq!(buffer[(4u16, 1u16)].symbol(), "日");
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
        let (lines, cursor, _) = render_cursor(4, 2, &mut editor);
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
        assert_eq!(before.trim_end(), "line 11");
        // Two more editor rows take two transcript rows; the top row
        // stays.
        editor.set("one\ntwo\nthree");
        // The pane is one row now, so the indicator shares it.
        let after = top_line(&mut transcript, &mut editor);
        assert!(after.starts_with("line 11"), "{after:?}");
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
        assert_eq!(cursor, (3, 4));
        assert_eq!(lines[4].trim_end(), "❯ /");
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
        assert_eq!(lines[3].trim_end(), "❯");
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
        assert_eq!(lines[1].trim_end(), "❯");
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

    /// Draw one frame of `transcript` at `width` by `height` with an
    /// empty editor and return the rows and the buffer.
    fn frame_of(
        transcript: &mut Transcript,
        width: u16,
        height: u16,
    ) -> (Vec<String>, ratatui::buffer::Buffer) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut editor = Editor::new(Vec::new());
        let status = status();
        let palette = palette();
        terminal
            .draw(|frame| {
                draw(
                    frame,
                    View {
                        transcript,
                        editor: &mut editor,
                        status: &status,
                        palette: &palette,
                        approval: None,
                        progress: None,
                    },
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let lines = (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();
        (lines, buffer)
    }

    fn reversed(buffer: &ratatui::buffer::Buffer, x: u16, y: u16) -> bool {
        buffer[(x, y)]
            .style()
            .add_modifier
            .contains(Modifier::REVERSED)
    }

    /// A transcript with one of each block the copy rules touch: a
    /// prompt echo, a formatted reply with a fence, a card with
    /// output, and a notice.
    fn copy_fixture() -> Transcript {
        let mut transcript = Transcript::new(true);
        transcript.push_line(Kind::Prompt, &format!("{PROMPT}hello"));
        transcript.push_delta(Kind::Text, "Done.\n\n```rust\nfn main() {}\n```\n");
        transcript.tool_start("c1", "bash");
        transcript.tool_preview("c1", "bash $ ls");
        transcript.tool_end(
            "c1",
            "bash",
            crate::cards::Outcome {
                summary: Some("listed"),
                is_error: false,
                output: "a.rs\nb.rs".to_string(),
                output_cut: false,
                at: std::time::Instant::now(),
            },
        );
        transcript.push_line(Kind::Notice, "· 10 in");
        transcript
    }

    #[test]
    fn a_drag_reverses_the_span_and_the_release_copies_it_without_the_marks() {
        let mut transcript = copy_fixture();
        let (lines, _) = frame_of(&mut transcript, 40, 17);
        assert_eq!(
            &lines[..11],
            &[
                "❯ hello",
                "Done.",
                "",
                "╭─ rust",
                "│ fn main() {}",
                "╰─",
                "✓ bash $ ls",
                "  a.rs",
                "  b.rs",
                "  listed",
                "· 10 in",
            ]
        );
        // The press starts on the echo, the pointer ends on the
        // notice's fourth column.
        assert!(!transcript.select_press(0, 0));
        assert!(transcript.select_drag(3, 10));
        let (_, buffer) = frame_of(&mut transcript, 40, 17);
        assert!(reversed(&buffer, 0, 0));
        assert!(reversed(&buffer, 2, 4));
        assert!(reversed(&buffer, 3, 10));
        assert!(!reversed(&buffer, 4, 10));
        assert!(!reversed(&buffer, 0, 11));
        // The copy: no prompt, no fence frame, no card header or
        // indent, the last row through the pointer.
        let copied = transcript.select_release().unwrap();
        assert!(!copied.cut);
        assert_eq!(
            copied.text,
            "hello\nDone.\n\nfn main() {}\na.rs\nb.rs\nlisted\n· 10"
        );
        // The selection stays on the screen after the release.
        let (_, buffer) = frame_of(&mut transcript, 40, 17);
        assert!(reversed(&buffer, 0, 1));

        // A span inside one row: the columns under the drag, both
        // ends inclusive, whichever way the drag went.
        assert!(transcript.select_press(4, 1));
        assert!(transcript.select_drag(2, 1));
        let (_, buffer) = frame_of(&mut transcript, 40, 17);
        assert!(!reversed(&buffer, 1, 1));
        assert!(reversed(&buffer, 2, 1));
        assert!(reversed(&buffer, 4, 1));
        assert!(!reversed(&buffer, 5, 1));
        assert!(!reversed(&buffer, 0, 0));
        assert_eq!(transcript.select_release().unwrap().text, "ne.");

        // A drag onto a blank row of the pane runs to the last row's
        // end; a drag that starts on a fence row skips the frame.
        transcript.select_press(0, 4);
        transcript.select_drag(0, 11);
        assert_eq!(
            transcript.select_release().unwrap().text,
            "fn main() {}\na.rs\nb.rs\nlisted\n· 10 in"
        );
        // A span over marks alone — the card's header — yields no
        // text, so nothing reaches the clipboard.
        transcript.select_press(0, 6);
        transcript.select_drag(9, 6);
        assert_eq!(transcript.select_release().unwrap().text, "");

        // A click selects nothing and clears the highlight.
        assert!(transcript.select_press(3, 1));
        assert!(transcript.select_release().is_none());
        let (_, buffer) = frame_of(&mut transcript, 40, 17);
        assert!(!reversed(&buffer, 3, 1));
        // A press off the rows clears too.
        transcript.select_press(0, 2);
        transcript.select_drag(0, 3);
        assert!(transcript.select_press(0, 14));
        assert!(transcript.select_release().is_none());
    }

    #[test]
    fn blank_rows_inside_a_fence_a_quote_and_a_card_copy_as_blank_lines() {
        let mut transcript = Transcript::new(true);
        transcript.push_delta(
            Kind::Text,
            "```rust\nfn a() {}\n\nfn b() {}\n```\n\n> one\n>\n> two\n\n---\n\n| a | b |\n|---|---|\n| 1 | 2 |\n",
        );
        let outcome = |output: &str| crate::cards::Outcome {
            summary: Some("listed"),
            is_error: false,
            output: output.to_string(),
            output_cut: false,
            at: std::time::Instant::now(),
        };
        transcript.tool_start("c1", "bash");
        transcript.tool_end("c1", "bash", outcome("a\n\nb\nc"));
        transcript.tool_start("c2", "bash");
        transcript.tool_end("c2", "bash", outcome("a\nb\nc\nd\ne\nf\ng\nh"));
        let (lines, _) = frame_of(&mut transcript, 40, 40);
        // The pane is the rows above the frame, the box, and the bar.
        let last = lines[..36]
            .iter()
            .rposition(|line| !line.is_empty())
            .unwrap();
        assert!(lines[last].contains("Ctrl-O"), "{:?}", lines[last]);
        transcript.select_press(0, 0);
        transcript.select_drag(39, last as u16);
        let text = transcript.select_release().unwrap().text;
        // The fence keeps its blank line; the quote keeps its bar's
        // blank line; the rule adds no line and leaves its two gap
        // rows; the table keeps its cells apart and drops its head
        // rule; the first card keeps its blank output line; the
        // second card's tail keeps its result line and drops the
        // hint.
        assert_eq!(
            text,
            "fn a() {}\n\nfn b() {}\n\none\n\ntwo\n\n\na  b\n1  2\n\
             a\n\nb\nc\nlisted\nd\ne\nf\ng\nh\nlisted"
        );
    }

    #[test]
    fn a_card_hint_wrapped_onto_its_own_row_adds_no_line() {
        let mut transcript = Transcript::new(false);
        transcript.tool_start("c1", "bash");
        let summary = "s".repeat(30);
        transcript.tool_end(
            "c1",
            "bash",
            crate::cards::Outcome {
                summary: Some(&summary),
                is_error: false,
                output: "a\nb\nc\nd\ne\nf\ng\nh".to_string(),
                output_cut: false,
                at: std::time::Instant::now(),
            },
        );
        transcript.push_line(Kind::Notice, "· 10 in");
        let (lines, _) = frame_of(&mut transcript, 40, 14);
        // The footer wraps: the hint lands on a row of its own.
        assert!(lines[7].contains("Ctrl-O"), "{:?}", lines);
        assert!(!lines[7].contains(&summary), "{:?}", lines);
        transcript.select_press(0, 0);
        transcript.select_drag(10, 8);
        let text = transcript.select_release().unwrap().text;
        assert!(text.ends_with(&format!("{summary}\n· 10 in")), "{text:?}");
    }

    #[test]
    fn a_block_that_changed_since_the_frame_takes_no_press_or_drag() {
        let mut transcript = Transcript::new(false);
        transcript.push_line(Kind::Text, "line 0");
        transcript.push_delta(Kind::Text, "one\ntwo");
        frame_of(&mut transcript, 30, 6);
        // A delta lands; the frame is still pending. The streamed
        // block's rows on the screen are stale, the line above is
        // not.
        transcript.push_delta(Kind::Text, "\nthree");
        assert!(!transcript.select_press(0, 1));
        assert!(transcript.selection().is_none());
        transcript.select_press(0, 0);
        assert!(!transcript.select_drag(2, 2));
        assert!(transcript.select_drag(2, 0));
        assert_eq!(transcript.select_release().unwrap().text, "lin");
        // The frame draws; the block reads as shown again.
        frame_of(&mut transcript, 30, 6);
        transcript.select_press(0, 1);
        transcript.select_drag(4, 3);
        assert_eq!(transcript.select_release().unwrap().text, "one\ntwo\nthree");
        // A card that waits for its render is pending too.
        transcript.tool_start("c1", "bash");
        frame_of(&mut transcript, 30, 12);
        transcript.tool_end(
            "c1",
            "bash",
            crate::cards::Outcome {
                summary: None,
                is_error: false,
                output: String::new(),
                output_cut: false,
                at: std::time::Instant::now(),
            },
        );
        transcript.select_press(0, 4);
        assert!(transcript.selection().is_none());
    }

    #[test]
    fn a_drag_below_the_pane_leaves_the_head_and_a_blank_cell_inside_it_takes_the_row_end() {
        let mut transcript = Transcript::new(false);
        transcript.push_line(Kind::Text, "line 0");
        transcript.push_line(Kind::Text, "line 1");
        // An eight-row screen: four pane rows, then the box with
        // its frame.
        frame_of(&mut transcript, 30, 8);
        transcript.select_press(0, 0);
        assert!(transcript.select_drag(2, 0));
        // Rows 4 to 7 are the box and its frame: the head stays.
        for y in 4..8 {
            assert!(!transcript.select_drag(9, y));
        }
        assert_eq!(transcript.select_release().unwrap().text, "lin");
        // Row 2 is blank inside the pane: the last row, to its end.
        transcript.select_press(0, 0);
        assert!(transcript.select_drag(0, 2));
        assert_eq!(transcript.select_release().unwrap().text, "line 0\nline 1");
    }

    #[test]
    fn the_browse_indicator_covers_its_cells() {
        let mut transcript = Transcript::new(false);
        for i in 0..10 {
            transcript.push_line(Kind::Text, &format!("line {i} of the transcript"));
        }
        frame_of(&mut transcript, 40, 4);
        transcript.scroll_up(3);
        let (lines, _) = frame_of(&mut transcript, 40, 4);
        assert!(lines[1].contains("3 below"), "{:?}", lines[1]);
        // The indicator sits at the right of the pane's last row: a
        // drag through it selects nothing under it.
        transcript.select_press(0, 1);
        assert!(!transcript.select_drag(38, 1));
        assert!(transcript.select_drag(5, 1));
        assert_eq!(transcript.select_release().unwrap().text, "line 6");
        // A press on it is the End key: back to follow mode.
        assert!(transcript.select_press(38, 1));
        assert!(transcript.selection().is_none());
        assert!(!transcript.browsing());
        assert!(transcript.select_release().is_none());
        let (lines, _) = frame_of(&mut transcript, 40, 4);
        assert_eq!(lines[1], "line 9 of the transcript");
    }

    #[test]
    fn a_tall_screen_frames_the_input_box() {
        let mut transcript = Transcript::new(false);
        transcript.push_line(Kind::Text, "line 0");
        let mut editor = Editor::new(Vec::new());
        let status = status();
        let palette = palette();
        let (lines, rows) = render(
            20,
            8,
            View {
                transcript: &mut transcript,
                editor: &mut editor,
                status: &status,
                palette: &palette,
                approval: None,
                progress: Some("⠋ 3s · bash"),
            },
        );
        // The pane, a blank row, the progress line, the box's top,
        // the input row, a blank row, the box's bottom edge with the
        // status.
        assert_eq!(rows, 2);
        assert_eq!(lines[0].trim_end(), "line 0");
        assert_eq!(lines[2].trim(), "");
        assert_eq!(lines[3].trim_end(), "⠋ 3s · bash");
        assert_eq!(lines[4], "╭──────────────────╮");
        assert_eq!(lines[5], format!("│❯{}│", " ".repeat(17)));
        assert_eq!(lines[6], format!("│{}│", " ".repeat(18)));
        assert!(lines[7].starts_with("╰─ test-model ─"), "{:?}", lines[7]);
        assert!(lines[7].ends_with('╯'), "{:?}", lines[7]);
        assert_eq!(lines[7].chars().count(), 20);
        // A seven-row screen keeps the packed layout.
        let (lines, rows) = render(
            20,
            7,
            View {
                transcript: &mut transcript,
                editor: &mut editor,
                status: &status,
                palette: &palette,
                approval: None,
                progress: None,
            },
        );
        assert_eq!(rows, 5);
        assert_eq!(lines[5].trim_end(), "❯");
        assert!(!lines.iter().any(|line| line.contains('─')));
    }

    #[test]
    fn the_status_sits_on_the_box_edge_between_line_segments() {
        let mut transcript = Transcript::new(false);
        let mut editor = Editor::new(Vec::new());
        let status = Status {
            input_tokens: 12,
            output_tokens: 5,
            cached_tokens: 2,
            ..status()
        };
        let palette = palette();
        let (lines, _) = render(
            70,
            8,
            View {
                transcript: &mut transcript,
                editor: &mut editor,
                status: &status,
                palette: &palette,
                approval: None,
                progress: None,
            },
        );
        let edge = &lines[7];
        let head = "╰─ test-model ─ ask ─ effort high ─ 12 in · 5 out · 2 cached ─";
        assert!(edge.starts_with(head), "{edge:?}");
        assert!(edge.ends_with('╯'), "{edge:?}");
        assert_eq!(edge.chars().count(), 70);
        assert!(
            edge[head.len()..]
                .trim_end_matches('╯')
                .chars()
                .all(|ch| ch == '─'),
            "{edge:?}"
        );
        // A narrow box cuts the segments from the right and keeps
        // its corners.
        let (lines, _) = render(
            18,
            8,
            View {
                transcript: &mut transcript,
                editor: &mut editor,
                status: &status,
                palette: &palette,
                approval: None,
                progress: None,
            },
        );
        // The mode has no room: the line fills it, and no empty
        // slot shows.
        assert_eq!(lines[7], "╰─ test-model ───╯");
    }

    #[test]
    fn a_narrow_pane_keeps_the_prompt_out_of_the_copy() {
        for width in [4u16, 5, 7] {
            let mut transcript = Transcript::new(false);
            transcript.push_line(Kind::Prompt, &format!("{PROMPT}hello"));
            transcript.push_line(Kind::Text, "x");
            frame_of(&mut transcript, width, 12);
            transcript.select_press(0, 0);
            transcript.select_drag(width - 1, 7);
            let text = transcript.select_release().unwrap().text;
            assert!(!text.contains('❯'), "width {width}: {text:?}");
            assert!(!text.starts_with('\n'), "width {width}: {text:?}");
            assert_eq!(text.replace('\n', ""), "hellox", "width {width}: {text:?}");
        }
    }

    #[test]
    fn a_press_on_the_popup_selects_nothing_under_it() {
        let commands = vec![
            CommandSpec::new("/help", "list the commands"),
            CommandSpec::new("/model [name]", "show or switch the session model"),
        ];
        let mut transcript = Transcript::new(false);
        for i in 0..5 {
            transcript.push_line(Kind::Text, &format!("line {i}"));
        }
        let mut editor = Editor::new(commands);
        editor.set("/");
        let mut terminal = Terminal::new(TestBackend::new(60, 7)).unwrap();
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
        // Rows 3 and 4 hold the popup from column 5; row 2 is
        // transcript, and so are the columns left of the popup.
        assert!(!transcript.select_press(6, 3));
        assert!(transcript.select_release().is_none());
        transcript.select_press(0, 2);
        // A drag onto a covered cell leaves the head where it is.
        assert!(!transcript.select_drag(6, 3));
        assert!(transcript.select_drag(5, 2));
        assert_eq!(transcript.select_release().unwrap().text, "line 2");
        // The cells left of the popup stay the transcript's.
        transcript.select_press(0, 3);
        transcript.select_drag(1, 3);
        assert_eq!(transcript.select_release().unwrap().text, "li");
    }

    #[test]
    fn the_selection_maps_through_the_browsed_window() {
        let mut transcript = Transcript::new(false);
        for i in 0..10 {
            transcript.push_line(Kind::Text, &format!("line {i}"));
        }
        frame_of(&mut transcript, 30, 4);
        transcript.scroll_up(3);
        let (lines, _) = frame_of(&mut transcript, 30, 4);
        assert_eq!(lines[0], "line 5");
        transcript.select_press(0, 0);
        transcript.select_drag(5, 1);
        assert_eq!(transcript.select_release().unwrap().text, "line 5\nline 6");
        // Rows that arrive below leave the selection in place.
        transcript.push_line(Kind::Text, "line 10");
        transcript.push_delta(Kind::Text, "streamed");
        let (_, buffer) = frame_of(&mut transcript, 30, 4);
        assert!(reversed(&buffer, 0, 0));
        assert!(transcript.selection().is_some());
        // A width change takes the rows away, and the selection.
        frame_of(&mut transcript, 31, 4);
        assert!(transcript.selection().is_none());
    }

    #[test]
    fn a_change_under_the_selection_clears_it() {
        let mut transcript = Transcript::new(false);
        transcript.push_line(Kind::Text, "line 0");
        transcript.tool_start("c1", "bash");
        transcript.push_line(Kind::Text, "line 2");
        transcript.tool_start("c2", "read");
        // A pane tall enough for every row, so no row shifts.
        frame_of(&mut transcript, 30, 12);
        // The span covers the first card, not the second.
        transcript.select_press(0, 0);
        transcript.select_drag(3, 2);
        let outcome = || crate::cards::Outcome {
            summary: None,
            is_error: false,
            output: String::new(),
            output_cut: false,
            at: std::time::Instant::now(),
        };
        transcript.tool_end("c2", "read", outcome());
        assert!(transcript.selection().is_some());
        transcript.tool_end("c1", "bash", outcome());
        assert!(transcript.selection().is_none());

        // A stream into the selected block clears it.
        transcript.push_delta(Kind::Text, "open");
        let (lines, _) = frame_of(&mut transcript, 30, 12);
        assert_eq!(lines[6], "open");
        transcript.select_press(0, 0);
        transcript.select_drag(2, 6);
        transcript.push_delta(Kind::Text, " more");
        assert!(transcript.selection().is_none());

        // The expand key changes every card.
        frame_of(&mut transcript, 30, 12);
        transcript.select_press(0, 0);
        transcript.select_drag(2, 1);
        transcript.set_expanded(true);
        assert!(transcript.selection().is_none());
    }

    #[test]
    fn the_copy_stops_at_the_cap_and_says_so() {
        let mut transcript = Transcript::new(false);
        let line = "x".repeat(99);
        let mut text = String::new();
        for _ in 0..800 {
            text.push_str(&line);
            text.push('\n');
        }
        transcript.push_line(Kind::Text, &text);
        frame_of(&mut transcript, 100, 10);
        transcript.select_press(0, 4);
        transcript.scroll_up(usize::MAX);
        frame_of(&mut transcript, 100, 10);
        transcript.select_drag(0, 0);
        let copied = transcript.select_release().unwrap();
        assert!(copied.cut);
        assert!(copied.text.len() <= crate::select::COPY_CAP);
        assert!(copied.text.starts_with(&line));
    }
}
