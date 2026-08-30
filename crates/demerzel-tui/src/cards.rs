//! Tool cards and the progress line.
//!
//! One card per tool call. The header carries the state glyph, the
//! tool name, and the input line: the first line of the driver's
//! preview. The body is the rest of the preview. The footer is a tail
//! of the output and one line with the duration and the tool's
//! summary. A card renders to text with a few face changes; the
//! transcript lays the text out like any other block and refreshes it
//! when the card changes.
//!
//! The interface keeps a capped tail of each output, sanitized before
//! the cap, so a card at the cap costs half a block of output next to
//! its text. The expand toggle shows the kept output whole.
//!
//! The progress line names what the turn is doing: a spinner, the
//! elapsed time, and the stage. Its text is a pure function of the
//! elapsed time, so the frame timer animates it with no state.
//!
//! This module also owns the text hygiene every outside string goes
//! through before it reaches a block: control characters drop, tabs
//! widen, and a line stays one line.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use crate::markdown::Face;

/// Bytes of one tool output the interface keeps: the tail, after
/// sanitizing.
pub const MAX_OUTPUT_BYTES: usize = 16 * 1024;

/// Bytes of one preview body the card keeps. The driver caps its
/// previews lower; this bounds the public path.
pub const MAX_PREVIEW_BYTES: usize = 8 * 1024;

/// Raw bytes `kept_output` walks at most. A char the sanitizer
/// drops costs no width, so without this bound a control-only
/// prefix would make the walk the output's.
const SCAN_BYTES: usize = 2 * MAX_OUTPUT_BYTES;

/// Output lines a collapsed card shows, from the end.
const TAIL_LINES: usize = 5;

/// Bytes of one collapsed tail line; the rest is cut. One long line —
/// minified data, a progress bar written with carriage returns —
/// must not fill the screen when the card is collapsed.
const TAIL_LINE_BYTES: usize = 240;

/// Bytes of a tool name, an input line, or a summary on a card. All
/// come from outside the interface; the cap keeps the card inside
/// its block.
const LINE_BYTES: usize = 512;

/// Bytes of the tool name on the progress line.
pub const PROGRESS_NAME_BYTES: usize = 80;

/// Bytes a render keeps free under its cap for the footer, so the
/// summary and the hint stay when the output rows are cut.
const FOOTER_RESERVE: usize = LINE_BYTES + 64;

/// The key that expands and collapses the cards, named in the hint.
const EXPAND_KEY: &str = "Ctrl-O";

/// The indent of every card line under the header. The copy rules
/// read it as the frame's columns, through `Block::frame_columns`.
pub(crate) const INDENT: &str = "  ";

/// The spinner frames, one per tick of the progress line.
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Milliseconds one spinner frame holds.
const SPINNER_MS: u128 = 100;

/// Where a call is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Between the call's start and its result: the model streams
    /// the call, then the tool runs.
    Running,
    /// The approval question is up for this call.
    Pending,
    Done,
    Error,
    /// The user refused the call at the question.
    Declined,
    /// The turn ended before the call returned.
    Interrupted,
}

impl State {
    fn glyph(self) -> char {
        match self {
            State::Running => '·',
            State::Pending => '?',
            State::Done => '✓',
            State::Error | State::Interrupted => '✗',
            State::Declined => '⊘',
        }
    }

    /// The face of the state's mark.
    fn face(self) -> Face {
        match self {
            State::Running => Face::PLAIN,
            State::Pending | State::Declined | State::Interrupted => Face::WARNING,
            State::Done => Face::SUCCESS,
            State::Error => Face::FAILURE,
        }
    }

    /// The footer word of an ended call, for a card whose tool gave
    /// no summary. An open call has no footer.
    fn word(self) -> Option<&'static str> {
        match self {
            State::Running | State::Pending => None,
            State::Done => Some("done"),
            State::Error => Some("error"),
            State::Declined => Some("declined"),
            State::Interrupted => Some("interrupted"),
        }
    }
}

/// What a finished call reports.
pub struct Outcome<'a> {
    pub summary: Option<&'a str>,
    pub is_error: bool,
    /// The kept output: sanitized, inside the cap. The card takes it.
    pub output: String,
    /// The kept output is a tail: earlier bytes fell over the cap.
    pub output_cut: bool,
    pub at: Instant,
}

/// One tool call on the screen.
#[derive(Debug)]
pub struct Card {
    name: String,
    state: State,
    /// The first line of the preview, on the header.
    input: Option<String>,
    /// The preview past its first line, or the question's own
    /// preview whole.
    body: Option<String>,
    started: Option<Instant>,
    duration: Option<Duration>,
    summary: Option<String>,
    output: String,
    /// Lines in the output, counted once when it lands.
    output_lines: usize,
    output_cut: bool,
    expanded: bool,
    /// The text needs a render before the next layout.
    stale: bool,
    /// Byte offsets from the last render, for the faces: the header
    /// end, the output span, and the start of the footer's hint.
    header_end: usize,
    output_span: (usize, usize),
    hint_start: usize,
}

impl Card {
    pub fn new(name: &str, expanded: bool) -> Self {
        Self {
            name: one_line(name, LINE_BYTES),
            state: State::Running,
            input: None,
            body: None,
            started: None,
            duration: None,
            summary: None,
            output: String::new(),
            output_lines: 0,
            output_cut: false,
            expanded,
            stale: true,
            header_end: 0,
            output_span: (0, 0),
            hint_start: 0,
        }
    }

    /// The name of the tool, as the header shows it.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether the card changed since its last render.
    pub fn stale(&self) -> bool {
        self.stale
    }

    /// The call dispatches: the duration counts from here.
    pub fn run(&mut self, at: Instant) {
        self.started = Some(at);
    }

    /// The driver's preview of the call. Its first line joins the
    /// header, without the tool name when the line starts with it as
    /// a word; the rest is the body.
    pub fn set_preview(&mut self, preview: &str) {
        let (first, rest) = match preview.find('\n') {
            Some(cut) => (&preview[..cut], Some(&preview[cut + 1..])),
            None => (preview, None),
        };
        let first = first
            .strip_prefix(self.name.as_str())
            .and_then(|rest| {
                if rest.is_empty() {
                    Some(rest)
                } else {
                    rest.strip_prefix(' ')
                }
            })
            .unwrap_or(first);
        self.input = (!first.is_empty()).then(|| one_line(first, LINE_BYTES));
        self.body = rest
            .filter(|rest| !rest.is_empty())
            .map(|rest| capped_text(rest, MAX_PREVIEW_BYTES));
        self.stale = true;
    }

    /// The approval question is up. A preview the question carries
    /// becomes the body when the card has none.
    pub fn pending(&mut self, preview: Option<&str>) {
        self.state = State::Pending;
        if self.input.is_none()
            && self.body.is_none()
            && let Some(preview) = preview
        {
            self.body = Some(capped_text(preview, MAX_PREVIEW_BYTES));
        }
        self.stale = true;
    }

    /// The question is answered at `at`. An approved call runs from
    /// here, so its duration counts from the answer, not from the
    /// question.
    pub fn answered(&mut self, declined: bool, at: Instant) {
        self.state = if declined {
            State::Declined
        } else {
            self.started = Some(at);
            State::Running
        };
        self.stale = true;
    }

    /// The call returned. A declined call keeps its state: its
    /// result is the refusal, and its output says so already.
    pub fn end(&mut self, outcome: Outcome<'_>) {
        if self.state != State::Declined {
            self.state = if outcome.is_error {
                State::Error
            } else {
                State::Done
            };
            self.output = outcome.output;
            self.output_lines = self.output.lines().count();
            self.output_cut = outcome.output_cut;
        }
        self.duration = self
            .started
            .map(|started| outcome.at.saturating_duration_since(started));
        self.summary = outcome
            .summary
            .filter(|summary| !summary.is_empty())
            .map(|summary| one_line(summary, LINE_BYTES));
        self.stale = true;
    }

    /// The turn ended with the call open.
    pub fn interrupt(&mut self) {
        self.state = State::Interrupted;
        self.stale = true;
    }

    pub fn set_expanded(&mut self, expanded: bool) {
        if self.expanded != expanded {
            self.expanded = expanded;
            self.stale = true;
        }
    }

    /// Render the card into `out`, replacing its content, and mark
    /// it fresh. The output rows stop under `cap`, less the footer,
    /// so the footer always lands.
    pub fn render(&mut self, out: &mut String, cap: usize) {
        self.stale = false;
        out.clear();
        out.push(self.state.glyph());
        out.push(' ');
        out.push_str(&self.name);
        if let Some(input) = &self.input {
            out.push(' ');
            out.push_str(input);
        }
        self.header_end = out.len();
        if let Some(body) = &self.body {
            for line in body.lines() {
                push_indented(out, line);
            }
        }

        let output_start = out.len();
        let mut hidden = 0usize;
        if !self.output.is_empty() && matches!(self.state, State::Done | State::Error) {
            if self.expanded {
                if self.output_cut {
                    push_indented(out, "… earlier output not kept");
                }
                let budget = cap.saturating_sub(FOOTER_RESERVE);
                for line in self.output.lines() {
                    if out.len() + line.len() + 1 + INDENT.len() > budget {
                        push_indented(out, "… rows cut at the block cap");
                        break;
                    }
                    push_indented(out, line);
                }
            } else {
                // The tail starts after the newline that ends the
                // hidden lines, found from the end: a trailing
                // newline ends the last line and separates none.
                hidden = self.output_lines.saturating_sub(TAIL_LINES);
                let separators = if self.output.ends_with('\n') {
                    TAIL_LINES
                } else {
                    TAIL_LINES - 1
                };
                let tail = self
                    .output
                    .rmatch_indices('\n')
                    .nth(separators)
                    .map_or(self.output.as_str(), |(at, _)| &self.output[at + 1..]);
                for line in tail.lines() {
                    push_indented(out, "");
                    let cut = line_cut(line, TAIL_LINE_BYTES);
                    out.push_str(&line[..cut]);
                    if cut < line.len() {
                        out.push('…');
                    }
                }
            }
        }
        self.output_span = (output_start, out.len());
        self.hint_start = out.len();

        let Some(word) = self.state.word() else {
            return;
        };
        push_indented(out, "");
        if let Some(duration) = self.duration
            && self.state != State::Declined
        {
            write_duration(out, duration);
            out.push_str(" · ");
        }
        match &self.summary {
            Some(summary) if self.state != State::Declined => out.push_str(summary),
            _ => out.push_str(word),
        }
        // The hint names what the expanded view adds: the hidden
        // lines, and the cut when the kept output is a tail. It is
        // the frame's, not the call's: a mark, so a copy leaves it
        // out.
        self.hint_start = out.len();
        let cut = self.output_cut && !self.expanded;
        if hidden > 0 {
            let plural = if hidden == 1 { "line" } else { "lines" };
            let _ = write!(out, " · {hidden} more {plural}");
        }
        if cut {
            out.push_str(" · output cut");
        }
        if hidden > 0 || cut {
            let _ = write!(out, " · {EXPAND_KEY}");
        }
    }

    /// The face changes over the last render, clamped to `len`: the
    /// state mark in its state's face, the header a title, a `+` or
    /// `-` line of the preview a diff line, the output muted.
    pub fn faces(&self, len: usize, out: &mut Vec<(u32, Face)>) {
        let header_end = self.header_end.min(len);
        if header_end > 0 {
            let mark = self.state.glyph().len_utf8().min(header_end);
            let state = self.state.face();
            out.push((0, Face::TITLE | state));
            if mark < header_end && state != Face::PLAIN {
                out.push((mark as u32, Face::TITLE));
            }
            out.push((header_end as u32, Face::PLAIN));
        }
        if let Some(body) = &self.body {
            // The body renders as a newline, the indent, and each
            // line, from the header's end; the offsets follow that
            // layout.
            let mut at = self.header_end;
            for line in body.lines() {
                let start = at + 1 + INDENT.len();
                at = start + line.len();
                let face = match line.as_bytes() {
                    [b'+', b' ', ..] => Face::DIFF_ADD,
                    [b'-', b' ', ..] => Face::DIFF_REMOVE,
                    _ => continue,
                };
                if start >= len {
                    break;
                }
                out.push((start as u32, face));
                out.push((at.min(len) as u32, Face::PLAIN));
            }
        }
        let (start, end) = (self.output_span.0.min(len), self.output_span.1.min(len));
        if end > start {
            // The output can start where the header ends: one change
            // there, not two.
            if out.last().is_some_and(|&(at, _)| at as usize == start) {
                out.pop();
            }
            out.push((start as u32, Face::MUTED));
            out.push((end as u32, Face::PLAIN));
        }
        if self.hint_start < len {
            out.push((self.hint_start as u32, Face::MARKER));
        }
    }
}

/// Append a newline, the indent, and `line`.
fn push_indented(out: &mut String, line: &str) {
    out.push('\n');
    out.push_str(INDENT);
    out.push_str(line);
}

/// The largest byte count at or under `cap` that ends `line` at a char
/// boundary.
fn line_cut(line: &str, cap: usize) -> usize {
    if line.len() <= cap {
        return line.len();
    }
    let mut cut = cap;
    while !line.is_char_boundary(cut) {
        cut -= 1;
    }
    cut
}

/// `raw` sanitized, on one line, and cut at `cap` bytes with a mark.
pub fn one_line(raw: &str, cap: usize) -> String {
    let mut text = String::with_capacity(raw.len().min(cap));
    push_one_line(&mut text, raw, cap);
    text
}

/// Append `raw` as one line of at most `cap` bytes plus the cut
/// mark, the way [`one_line`] builds one: in place, for a render
/// that runs every frame.
pub fn push_one_line(out: &mut String, raw: &str, cap: usize) {
    let start = out.len();
    for ch in raw.chars() {
        if out.len() - start >= cap {
            out.push('…');
            break;
        }
        match ch {
            '\n' | '\t' => out.push(' '),
            ch if ch.is_control() => {}
            ch => out.push(ch),
        }
    }
}

/// `raw` sanitized and cut at `cap` bytes with a mark on its own
/// line.
fn capped_text(raw: &str, cap: usize) -> String {
    let mut text = String::with_capacity(raw.len().min(cap + 8));
    for ch in raw.chars() {
        if text.len() > cap {
            text.truncate(line_cut(&text, cap));
            text.push_str("\n…");
            break;
        }
        match ch {
            '\t' => text.push_str("    "),
            ch if ch.is_control() && ch != '\n' => {}
            ch => text.push(ch),
        }
    }
    text
}

/// Append `text` with control characters neutralized: tabs become
/// spaces, the rest drop. An escape byte must never reach the frame.
pub fn push_sanitized(out: &mut String, text: &str) {
    for ch in text.chars() {
        if ch == '\t' {
            out.push_str("    ");
        } else if ch == '\n' || !ch.is_control() {
            out.push(ch);
        }
    }
}

/// The tail of `output` the interface keeps: the last chars whose
/// sanitized form fits the byte cap. The walk back from the end
/// measures each char as the sanitizer writes it and stops at
/// `SCAN_BYTES` of raw text, so the work is the kept tail's, not
/// the output's. Returns the tail and whether it is a cut.
pub fn kept_output(output: &str) -> (String, bool) {
    let mut width = 0usize;
    let mut start = output.len();
    for (at, ch) in output.char_indices().rev() {
        let cols = match ch {
            '\t' => 4,
            '\n' => 1,
            ch if ch.is_control() => 0,
            ch => ch.len_utf8(),
        };
        if width + cols > MAX_OUTPUT_BYTES || output.len() - at > SCAN_BYTES {
            break;
        }
        width += cols;
        start = at;
    }
    let mut text = String::with_capacity(width);
    push_sanitized(&mut text, &output[start..]);
    debug_assert_eq!(text.len(), width);
    (text, start > 0)
}

/// Append a duration the way the card shows it.
pub fn write_duration(out: &mut String, duration: Duration) {
    let millis = duration.as_millis();
    let _ = if millis < 1_000 {
        write!(out, "{millis}ms")
    } else if millis < 60_000 {
        write!(out, "{:.1}s", duration.as_secs_f64())
    } else {
        let seconds = duration.as_secs();
        write!(out, "{}m {:02}s", seconds / 60, seconds % 60)
    };
}

/// Write the progress line into `out`: the spinner, the elapsed
/// time, and the stage.
pub fn write_progress(out: &mut String, elapsed: Duration, stage: &str) {
    out.clear();
    let frame = (elapsed.as_millis() / SPINNER_MS) as usize % SPINNER.len();
    let seconds = elapsed.as_secs();
    out.push(SPINNER[frame]);
    let _ = if seconds < 60 {
        write!(out, " {seconds}s · {stage}")
    } else {
        write!(out, " {}m {:02}s · {stage}", seconds / 60, seconds % 60)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cap wide enough for every test but the one that cuts.
    const CAP: usize = 32 * 1024;

    fn text(card: &mut Card) -> String {
        let mut out = String::new();
        card.render(&mut out, CAP);
        out
    }

    fn outcome<'a>(
        summary: Option<&'a str>,
        is_error: bool,
        output: &str,
        at: Instant,
    ) -> Outcome<'a> {
        Outcome {
            summary,
            is_error,
            output: output.to_string(),
            output_cut: false,
            at,
        }
    }

    fn finish(card: &mut Card, output: &str, is_error: bool) {
        let start = Instant::now();
        card.run(start);
        card.end(outcome(
            Some("bash: exit 0"),
            is_error,
            output,
            start + Duration::from_millis(1_234),
        ));
    }

    fn duration_text(duration: Duration) -> String {
        let mut out = String::new();
        write_duration(&mut out, duration);
        out
    }

    fn progress_text(elapsed: Duration, stage: &str) -> String {
        let mut out = String::from("stale");
        write_progress(&mut out, elapsed, stage);
        out
    }

    #[test]
    fn a_card_renders_its_header_body_tail_and_footer() {
        let mut card = Card::new("bash", false);
        assert!(card.stale());
        assert_eq!(text(&mut card), "· bash");
        assert!(!card.stale());
        card.set_preview("$ cargo fmt\n  cargo test");
        assert!(card.stale());
        assert_eq!(text(&mut card), "· bash $ cargo fmt\n    cargo test");
        finish(&mut card, "one\ntwo\n", false);
        assert_eq!(
            text(&mut card),
            "✓ bash $ cargo fmt\n    cargo test\n  one\n  two\n  1.2s · bash: exit 0"
        );
    }

    #[test]
    fn the_input_line_drops_the_tool_name_it_repeats() {
        let mut card = Card::new("edit", false);
        card.set_preview("edit src/main.rs\n@ replace line 3\n+ one");
        assert_eq!(
            text(&mut card),
            "· edit src/main.rs\n  @ replace line 3\n  + one"
        );
        let mut card = Card::new("create", false);
        card.set_preview("create notes.txt");
        assert_eq!(text(&mut card), "· create notes.txt");
        // A first line that is the name alone adds nothing; a line
        // that starts with the name as part of a word keeps it.
        let mut card = Card::new("edit", false);
        card.set_preview("edit\n@ delete line 1");
        assert_eq!(text(&mut card), "· edit\n  @ delete line 1");
        let mut card = Card::new("read", false);
        card.set_preview("readme.md");
        assert_eq!(text(&mut card), "· read readme.md");
    }

    #[test]
    fn a_collapsed_card_shows_the_tail_and_counts_the_rest() {
        let mut card = Card::new("read", false);
        let output: String = (1..=8).map(|i| format!("{i}: line\n")).collect();
        finish(&mut card, &output, false);
        let shown = text(&mut card);
        assert!(
            shown.starts_with("✓ read\n  4: line\n  5: line\n"),
            "{shown}"
        );
        assert!(!shown.contains("3: line"));
        assert!(
            shown.ends_with("bash: exit 0 · 3 more lines · Ctrl-O"),
            "{shown}"
        );
        // The tail seek agrees with the line iterator for every
        // shape: a trailing newline, none, empty lines, fewer lines
        // than the tail.
        for output in [
            "a\nb\nc\nd\ne\nf\ng",
            "a\nb\nc\nd\ne\nf\ng\n",
            "\n\n\n\n\n\n\n",
            "\n\n\n\n\n\n\n\n",
            "a\n\nb\n",
            "one",
            "a\nb\nc\nd\ne",
            "a\nb\nc\nd\ne\n",
        ] {
            let mut card = Card::new("t", false);
            finish(&mut card, output, false);
            let shown = text(&mut card);
            let rows: Vec<&str> = shown.lines().skip(1).map(|row| &row[2..]).collect();
            let total = output.lines().count();
            let expected: Vec<&str> = output
                .lines()
                .skip(total.saturating_sub(TAIL_LINES))
                .collect();
            assert_eq!(&rows[..rows.len() - 1], &expected[..], "{output:?}");
        }
        card.set_expanded(true);
        let shown = text(&mut card);
        assert!(shown.contains("\n  1: line\n"), "{shown}");
        assert!(!shown.contains("more line"));
        // One line hidden reads as a line.
        let mut card = Card::new("read", false);
        let output: String = (1..=6).map(|i| format!("{i}\n")).collect();
        finish(&mut card, &output, false);
        assert!(text(&mut card).ends_with(" · 1 more line · Ctrl-O"));
    }

    #[test]
    fn a_long_tail_line_is_cut_and_the_expanded_view_keeps_it() {
        let mut card = Card::new("bash", false);
        let long = format!("{}é", "x".repeat(TAIL_LINE_BYTES - 1));
        finish(&mut card, &long, false);
        let shown = text(&mut card);
        let line = shown.lines().nth(1).unwrap();
        assert!(line.ends_with("x…"), "{line}");
        assert!(!line.contains('é'));
        card.set_expanded(true);
        assert!(text(&mut card).contains(&long));
    }

    #[test]
    fn a_cut_output_says_so_collapsed_and_expanded() {
        let mut card = Card::new("bash", false);
        let start = Instant::now();
        card.run(start);
        card.end(Outcome {
            summary: None,
            is_error: false,
            output: "tail".to_string(),
            output_cut: true,
            at: start,
        });
        // The kept tail shows whole, so the hint names the cut.
        assert_eq!(
            text(&mut card),
            "✓ bash\n  tail\n  0ms · done · output cut · Ctrl-O"
        );
        card.set_expanded(true);
        assert_eq!(
            text(&mut card),
            "✓ bash\n  … earlier output not kept\n  tail\n  0ms · done"
        );
        // Hidden lines and the cut both show.
        card.set_expanded(false);
        card.end(Outcome {
            summary: None,
            is_error: false,
            output: (1..=7).map(|i| format!("{i}\n")).collect(),
            output_cut: true,
            at: start,
        });
        assert!(
            text(&mut card).ends_with("done · 2 more lines · output cut · Ctrl-O"),
            "{}",
            text(&mut card)
        );
    }

    #[test]
    fn the_output_rows_stop_under_the_cap_and_the_footer_lands() {
        let mut card = Card::new("bash", true);
        let output: String = (0..3_000).map(|i| format!("{i}\n")).collect();
        finish(&mut card, &output, false);
        let mut out = String::new();
        card.render(&mut out, 4_096);
        assert!(out.len() <= 4_096, "{}", out.len());
        assert!(out.contains("\n  … rows cut at the block cap\n"), "{out}");
        assert!(out.ends_with("\n  1.2s · bash: exit 0"), "{out}");
        // Under a cap that fits, nothing is cut.
        card.render(&mut out, CAP);
        assert!(!out.contains("rows cut"));
    }

    #[test]
    fn the_states_render_their_glyph_and_word() {
        let mut card = Card::new("bash", false);
        card.set_preview("$ rm -r x");
        card.run(Instant::now());
        card.pending(None);
        assert_eq!(text(&mut card), "? bash $ rm -r x");
        card.answered(true, Instant::now());
        assert_eq!(text(&mut card), "⊘ bash $ rm -r x\n  declined");
        // The result of a declined call changes nothing: the refusal
        // is the result.
        card.end(outcome(
            None,
            true,
            "the user declined the bash call",
            Instant::now(),
        ));
        assert_eq!(text(&mut card), "⊘ bash $ rm -r x\n  declined");

        let mut card = Card::new("bash", false);
        card.pending(Some("{\n  \"cmd\": \"x\"\n}"));
        assert_eq!(text(&mut card), "? bash\n  {\n    \"cmd\": \"x\"\n  }");
        card.answered(false, Instant::now());
        assert_eq!(text(&mut card), "· bash\n  {\n    \"cmd\": \"x\"\n  }");
        finish(&mut card, "boom", true);
        assert_eq!(
            text(&mut card),
            "✗ bash\n  {\n    \"cmd\": \"x\"\n  }\n  boom\n  1.2s · bash: exit 0"
        );

        let mut card = Card::new("bash", false);
        card.interrupt();
        assert_eq!(text(&mut card), "✗ bash\n  interrupted");
    }

    #[test]
    fn an_approved_call_counts_its_duration_from_the_answer() {
        let start = Instant::now();
        let mut card = Card::new("bash", false);
        card.run(start);
        card.pending(None);
        // The user takes ten seconds; the tool takes one.
        card.answered(false, start + Duration::from_secs(10));
        card.end(outcome(None, false, "", start + Duration::from_secs(11)));
        assert_eq!(text(&mut card), "✓ bash\n  1.0s · done");
    }

    #[test]
    fn the_faces_mark_the_diff_lines_of_a_preview_and_the_state() {
        let mut card = Card::new("edit", false);
        card.set_preview("src/lib.rs\n- old line\n+ new line\n  context");
        let shown = text(&mut card);
        let mut faces = Vec::new();
        card.faces(shown.len(), &mut faces);
        let removed = "· edit src/lib.rs\n  ".len() as u32;
        let added = "· edit src/lib.rs\n  - old line\n  ".len() as u32;
        // A running call's mark has no face of its own, so the
        // header is one entry.
        assert_eq!(
            faces,
            vec![
                (0, Face::TITLE),
                ("· edit src/lib.rs".len() as u32, Face::PLAIN),
                (removed, Face::DIFF_REMOVE),
                (removed + "- old line".len() as u32, Face::PLAIN),
                (added, Face::DIFF_ADD),
                (added + "+ new line".len() as u32, Face::PLAIN),
            ]
        );
        // A cut text keeps the entries inside it.
        let mut faces = Vec::new();
        card.faces(removed as usize + 4, &mut faces);
        assert!(faces.iter().all(|&(at, _)| at <= removed + 4));
        assert!(faces.iter().any(|&(_, face)| face == Face::DIFF_REMOVE));

        // The output starts where the last preview line ends: one
        // change there, and the entries stay in order.
        card.set_preview("src/lib.rs\n+ new line");
        finish(&mut card, "out", false);
        let shown = text(&mut card);
        let mut faces = Vec::new();
        card.faces(shown.len(), &mut faces);
        let added = "✓ edit src/lib.rs\n  ".len() as u32;
        assert_eq!(
            faces,
            vec![
                (0, Face::TITLE | Face::SUCCESS),
                ("✓".len() as u32, Face::TITLE),
                ("✓ edit src/lib.rs".len() as u32, Face::PLAIN),
                (added, Face::DIFF_ADD),
                (added + "+ new line".len() as u32, Face::MUTED),
                (added + "+ new line\n  out".len() as u32, Face::PLAIN),
            ]
        );
        assert!(faces.windows(2).all(|pair| pair[0].0 < pair[1].0));

        finish(&mut card, "", true);
        let shown = text(&mut card);
        let mut faces = Vec::new();
        card.faces(shown.len(), &mut faces);
        assert_eq!(faces[0], (0, Face::TITLE | Face::FAILURE));
    }

    #[test]
    fn the_faces_mark_the_header_and_the_output() {
        let mut card = Card::new("bash", false);
        finish(&mut card, "out", false);
        let shown = text(&mut card);
        let mut faces = Vec::new();
        card.faces(shown.len(), &mut faces);
        let mark = "✓".len() as u32;
        let header = "✓ bash".len() as u32;
        let output_end = "✓ bash\n  out".len() as u32;
        assert_eq!(
            faces,
            vec![
                (0, Face::TITLE | Face::SUCCESS),
                (mark, Face::TITLE),
                (header, Face::MUTED),
                (output_end, Face::PLAIN),
            ]
        );
        card.set_preview("$ true\n  more");
        let shown = text(&mut card);
        let mut faces = Vec::new();
        card.faces(shown.len(), &mut faces);
        assert_eq!(faces.len(), 5);
        assert_eq!(faces[2], ("✓ bash $ true".len() as u32, Face::PLAIN));
        assert_eq!(
            faces[3],
            ("✓ bash $ true\n    more".len() as u32, Face::MUTED)
        );
        // Offsets clamp to a text the transcript cut at its cap.
        let mut faces = Vec::new();
        card.faces(4, &mut faces);
        assert!(faces.iter().all(|&(at, _)| at <= 4));
    }

    #[test]
    fn outside_strings_are_sanitized_and_capped() {
        let mut card = Card::new("ba\x1b[2Jsh\nSECOND", false);
        assert_eq!(text(&mut card), "· ba[2Jsh SECOND");
        let long = "n".repeat(2 * LINE_BYTES);
        let mut card = Card::new(&long, false);
        let shown = text(&mut card);
        assert!(shown.len() < LINE_BYTES + 16, "{}", shown.len());
        assert!(shown.ends_with('…'));
        card.run(Instant::now());
        card.end(outcome(Some(&long), false, "ab    c", Instant::now()));
        let shown = text(&mut card);
        assert!(shown.contains("\n  ab    c\n"), "{shown}");
        assert!(shown.len() < 2 * LINE_BYTES + 64, "{}", shown.len());

        // A preview past its cap keeps its head and marks the cut;
        // its first line is one capped line.
        let mut card = Card::new("bash", false);
        let preview = format!("$ {}\n{}", "x".repeat(2 * LINE_BYTES), "y\n".repeat(8_000));
        card.set_preview(&preview);
        let shown = text(&mut card);
        // Each kept line renders with its indent: at most twice the
        // body cap, plus the header.
        assert!(
            shown.len() < LINE_BYTES + 2 * MAX_PREVIEW_BYTES + 64,
            "{}",
            shown.len()
        );
        assert!(shown.ends_with("\n  …"), "{}", &shown[shown.len() - 8..]);
        let mut card = Card::new("bash", false);
        card.pending(Some(&"z\t\n".repeat(4_000)));
        let shown = text(&mut card);
        assert!(shown.ends_with("\n  …"));
        assert!(!shown.contains('\t'));
    }

    #[test]
    fn the_kept_output_is_sanitized_before_the_cap() {
        let short = "x".repeat(MAX_OUTPUT_BYTES);
        assert_eq!(kept_output(&short), (short.clone(), false));
        let long = format!("{}é{}", "a".repeat(100), "b".repeat(MAX_OUTPUT_BYTES - 1));
        let (tail, cut) = kept_output(&long);
        assert!(cut);
        assert!(tail.len() <= MAX_OUTPUT_BYTES);
        assert!(tail.starts_with('b'));
        // Tabs widen before the cap, so the kept bytes stay inside it.
        let tabs = "\t".repeat(MAX_OUTPUT_BYTES);
        let (tail, cut) = kept_output(&tabs);
        assert!(cut);
        assert_eq!(tail.len(), MAX_OUTPUT_BYTES);
        assert!(
            tail.capacity() <= MAX_OUTPUT_BYTES + 64,
            "{}",
            tail.capacity()
        );
        assert!(tail.bytes().all(|byte| byte == b' '));
        let (tail, cut) = kept_output("a\x1b[31mb\r\n");
        assert_eq!((tail.as_str(), cut), ("a[31mb\n", false));
        // Control characters past the cap count for nothing: they
        // are no cut.
        let controls = format!("\x07\x07{}", "x".repeat(MAX_OUTPUT_BYTES));
        let (tail, cut) = kept_output(&controls);
        assert_eq!((tail.len(), cut), (MAX_OUTPUT_BYTES, false));
        // A control-only prefix past the scan bound is a cut, not a
        // walk over the whole output.
        let prefix = format!("{}{}", "\x07".repeat(4 * MAX_OUTPUT_BYTES), "x".repeat(100));
        let (tail, cut) = kept_output(&prefix);
        assert_eq!((tail.as_str(), cut), ("x".repeat(100).as_str(), true));
        // The walk matches a whole sanitize followed by a tail cut.
        let mixed = "é\tx\x1b\n".repeat(MAX_OUTPUT_BYTES / 4);
        let mut whole = String::new();
        push_sanitized(&mut whole, &mixed);
        let (tail, cut) = kept_output(&mixed);
        assert!(cut);
        assert_eq!(tail.as_str(), &whole[whole.len() - tail.len()..]);
        assert!(tail.len() <= MAX_OUTPUT_BYTES && tail.len() > MAX_OUTPUT_BYTES - 8);
    }

    #[test]
    fn durations_read_in_the_unit_that_fits() {
        assert_eq!(duration_text(Duration::from_millis(7)), "7ms");
        assert_eq!(duration_text(Duration::from_millis(999)), "999ms");
        assert_eq!(duration_text(Duration::from_millis(1_000)), "1.0s");
        assert_eq!(duration_text(Duration::from_millis(59_949)), "59.9s");
        assert_eq!(duration_text(Duration::from_secs(60)), "1m 00s");
        assert_eq!(duration_text(Duration::from_secs(3_725)), "62m 05s");
    }

    #[test]
    fn the_progress_line_animates_from_the_elapsed_time() {
        assert_eq!(
            progress_text(Duration::from_millis(0), "thinking"),
            "⠋ 0s · thinking"
        );
        assert_eq!(
            progress_text(Duration::from_millis(150), "bash"),
            "⠙ 0s · bash"
        );
        assert_eq!(
            progress_text(Duration::from_millis(1_050), "writing"),
            "⠋ 1s · writing"
        );
        assert_eq!(
            progress_text(Duration::from_secs(65), "bash"),
            "⠋ 1m 05s · bash"
        );
    }
}
