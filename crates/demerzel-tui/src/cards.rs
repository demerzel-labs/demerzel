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
//! The interface keeps a capped tail of each output, so a card at the
//! cap costs half a block of output next to its text. The expand
//! toggle shows the kept output whole.
//!
//! The progress line names what the turn is doing: a spinner, the
//! elapsed time, and the phase. Its text is a pure function of the
//! elapsed time, so the frame timer animates it with no state.

use std::time::{Duration, Instant};

use crate::markdown::Face;
use crate::transcript::push_sanitized;

/// Bytes of one tool output the interface keeps: the tail.
pub const MAX_OUTPUT_BYTES: usize = 16 * 1024;

/// Output lines a collapsed card shows, from the end.
const TAIL_LINES: usize = 5;

/// Bytes of one collapsed tail line; the rest is cut. One long line —
/// minified data, a progress bar written with carriage returns —
/// must not fill the screen when the card is collapsed.
const TAIL_LINE_BYTES: usize = 240;

/// Bytes of a tool name or a summary on a card. Both come from
/// outside the interface; the cap keeps the card inside its block.
const LINE_BYTES: usize = 512;

/// The key that expands and collapses the cards, named in the hint.
const EXPAND_KEY: &str = "Ctrl-O";

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

    /// The footer word when the tool gave no summary.
    fn word(self) -> &'static str {
        match self {
            State::Running => "running",
            State::Pending => "pending",
            State::Done => "done",
            State::Error => "error",
            State::Declined => "declined",
            State::Interrupted => "interrupted",
        }
    }
}

/// What a finished call reports.
pub struct Outcome<'a> {
    pub summary: Option<&'a str>,
    pub is_error: bool,
    /// The kept output, sanitized by the card.
    pub output: &'a str,
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
    output_cut: bool,
    expanded: bool,
    /// The text needs a render before the next layout.
    pub(crate) stale: bool,
    /// Byte offsets from the last render, for the faces: the header
    /// end and the output span.
    header_end: usize,
    output_span: (usize, usize),
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
            output_cut: false,
            expanded,
            stale: true,
            header_end: 0,
            output_span: (0, 0),
        }
    }

    /// The call dispatches: the duration counts from here.
    pub fn run(&mut self, at: Instant) {
        self.started = Some(at);
    }

    /// The driver's preview of the call. Its first line joins the
    /// header, less the tool name when the line starts with it; the
    /// rest is the body.
    pub fn set_preview(&mut self, preview: &str) {
        let mut clean = String::with_capacity(preview.len());
        push_sanitized(&mut clean, preview);
        let (first, rest) = match clean.find('\n') {
            Some(cut) => (&clean[..cut], Some(&clean[cut + 1..])),
            None => (clean.as_str(), None),
        };
        let first = first
            .strip_prefix(self.name.as_str())
            .and_then(|rest| rest.strip_prefix(' '))
            .unwrap_or(first);
        self.input = Some(first.to_string());
        self.body = rest.filter(|rest| !rest.is_empty()).map(str::to_string);
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
            let mut clean = String::with_capacity(preview.len());
            push_sanitized(&mut clean, preview);
            self.body = Some(clean);
        }
        self.stale = true;
    }

    /// The question is answered.
    pub fn answered(&mut self, declined: bool) {
        self.state = if declined {
            State::Declined
        } else {
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
            self.output.clear();
            push_sanitized(&mut self.output, outcome.output);
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

    /// Render the card into `out`, replacing its content.
    pub fn render(&mut self, out: &mut String) {
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
                out.push_str("\n  ");
                out.push_str(line);
            }
        }

        let output_start = out.len();
        let mut hidden = 0usize;
        if !self.output.is_empty() && matches!(self.state, State::Done | State::Error) {
            if self.expanded {
                if self.output_cut {
                    out.push_str("\n  … earlier output not kept");
                }
                for line in self.output.lines() {
                    out.push_str("\n  ");
                    out.push_str(line);
                }
            } else {
                let total = self.output.lines().count();
                hidden = total.saturating_sub(TAIL_LINES);
                for line in self.output.lines().skip(hidden) {
                    out.push_str("\n  ");
                    let cut = line_cut(line, TAIL_LINE_BYTES);
                    out.push_str(&line[..cut]);
                    if cut < line.len() {
                        out.push('…');
                    }
                }
            }
        }
        self.output_span = (output_start, out.len());

        if !matches!(self.state, State::Running | State::Pending) {
            out.push_str("\n  ");
            if let Some(duration) = self.duration
                && self.state != State::Declined
            {
                out.push_str(&duration_text(duration));
                out.push_str(" · ");
            }
            match &self.summary {
                Some(summary) if self.state != State::Declined => out.push_str(summary),
                _ => out.push_str(self.state.word()),
            }
            if hidden > 0 {
                let word = if hidden == 1 { "line" } else { "lines" };
                out.push_str(&format!(" · {hidden} more {word} · {EXPAND_KEY}"));
            }
        }
    }

    /// The face changes over the last render, clamped to `len`: the
    /// header strong, the output dim.
    pub fn faces(&self, len: usize, out: &mut Vec<(u32, Face)>) {
        let header_end = self.header_end.min(len);
        if header_end > 0 {
            out.push((0, Face::STRONG));
            out.push((header_end as u32, Face::PLAIN));
        }
        let (start, end) = (self.output_span.0.min(len), self.output_span.1.min(len));
        if end > start {
            // The output can start where the header ends: one change
            // there, not two.
            if out.last().is_some_and(|&(at, _)| at as usize == start) {
                out.pop();
            }
            out.push((start as u32, Face::MARKER));
            out.push((end as u32, Face::PLAIN));
        }
    }
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
    for ch in raw.chars() {
        if text.len() >= cap {
            text.push('…');
            break;
        }
        match ch {
            '\n' | '\t' => text.push(' '),
            ch if ch.is_control() => {}
            ch => text.push(ch),
        }
    }
    text
}

/// The tail of `output` inside the byte cap, and whether it is a cut.
pub fn output_tail(output: &str) -> (&str, bool) {
    if output.len() <= MAX_OUTPUT_BYTES {
        return (output, false);
    }
    let mut start = output.len() - MAX_OUTPUT_BYTES;
    while !output.is_char_boundary(start) {
        start += 1;
    }
    (&output[start..], true)
}

/// A duration the way the card shows it.
pub fn duration_text(duration: Duration) -> String {
    let millis = duration.as_millis();
    if millis < 1_000 {
        format!("{millis}ms")
    } else if millis < 60_000 {
        format!("{:.1}s", duration.as_secs_f64())
    } else {
        let seconds = duration.as_secs();
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    }
}

/// The progress line: the spinner, the elapsed time, and the phase.
pub fn progress_text(elapsed: Duration, phase: &str) -> String {
    let frame = (elapsed.as_millis() / SPINNER_MS) as usize % SPINNER.len();
    let seconds = elapsed.as_secs();
    let elapsed = if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    };
    format!("{} {elapsed} · {phase}", SPINNER[frame])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(card: &mut Card) -> String {
        let mut out = String::new();
        card.render(&mut out);
        out
    }

    fn finish(card: &mut Card, output: &str, is_error: bool) {
        let start = Instant::now();
        card.run(start);
        card.end(Outcome {
            summary: Some("bash: exit 0"),
            is_error,
            output,
            output_cut: false,
            at: start + Duration::from_millis(1_234),
        });
    }

    #[test]
    fn a_card_renders_its_header_body_tail_and_footer() {
        let mut card = Card::new("bash", false);
        assert_eq!(text(&mut card), "· bash");
        card.set_preview("$ cargo fmt\n  cargo test");
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
    fn a_cut_output_says_so_when_expanded() {
        let mut card = Card::new("bash", true);
        let start = Instant::now();
        card.run(start);
        card.end(Outcome {
            summary: None,
            is_error: false,
            output: "tail",
            output_cut: true,
            at: start,
        });
        assert_eq!(
            text(&mut card),
            "✓ bash\n  … earlier output not kept\n  tail\n  0ms · done"
        );
    }

    #[test]
    fn the_states_render_their_glyph_and_word() {
        let mut card = Card::new("bash", false);
        card.set_preview("$ rm -r x");
        card.run(Instant::now());
        card.pending(None);
        assert_eq!(text(&mut card), "? bash $ rm -r x");
        card.answered(true);
        assert_eq!(text(&mut card), "⊘ bash $ rm -r x\n  declined");
        // The result of a declined call changes nothing: the refusal
        // is the result.
        card.end(Outcome {
            summary: None,
            is_error: true,
            output: "the user declined the bash call",
            output_cut: false,
            at: Instant::now(),
        });
        assert_eq!(text(&mut card), "⊘ bash $ rm -r x\n  declined");

        let mut card = Card::new("bash", false);
        card.pending(Some("{\n  \"cmd\": \"x\"\n}"));
        assert_eq!(text(&mut card), "? bash\n  {\n    \"cmd\": \"x\"\n  }");
        card.answered(false);
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
    fn the_faces_mark_the_header_and_the_output() {
        let mut card = Card::new("bash", false);
        finish(&mut card, "out", false);
        let shown = text(&mut card);
        let mut faces = Vec::new();
        card.faces(shown.len(), &mut faces);
        let header = "✓ bash".len() as u32;
        let output_end = "✓ bash\n  out".len() as u32;
        assert_eq!(
            faces,
            vec![
                (0, Face::STRONG),
                (header, Face::MARKER),
                (output_end, Face::PLAIN),
            ]
        );
        card.set_preview("$ true\n  more");
        let shown = text(&mut card);
        let mut faces = Vec::new();
        card.faces(shown.len(), &mut faces);
        assert_eq!(faces.len(), 4);
        assert_eq!(faces[1], ("✓ bash $ true".len() as u32, Face::PLAIN));
        assert_eq!(
            faces[2],
            ("✓ bash $ true\n    more".len() as u32, Face::MARKER)
        );
        // Offsets clamp to a text the transcript cut at its cap.
        let mut faces = Vec::new();
        card.faces(4, &mut faces);
        assert!(faces.iter().all(|&(at, _)| at <= 4));
    }

    #[test]
    fn names_and_summaries_are_one_sanitized_line_inside_a_cap() {
        let mut card = Card::new("ba\x1b[2Jsh\nSECOND", false);
        assert_eq!(text(&mut card), "· ba[2Jsh SECOND");
        let long = "n".repeat(2 * LINE_BYTES);
        let mut card = Card::new(&long, false);
        let shown = text(&mut card);
        assert!(shown.len() < LINE_BYTES + 16, "{}", shown.len());
        assert!(shown.ends_with('…'));
        card.run(Instant::now());
        card.end(Outcome {
            summary: Some(&long),
            is_error: false,
            output: "a\x1bb\tc",
            output_cut: false,
            at: Instant::now(),
        });
        let shown = text(&mut card);
        assert!(shown.contains("\n  ab    c\n"), "{shown}");
        assert!(shown.len() < 2 * LINE_BYTES + 64, "{}", shown.len());
    }

    #[test]
    fn the_output_tail_keeps_the_cap_at_a_char_boundary() {
        let short = "x".repeat(MAX_OUTPUT_BYTES);
        assert_eq!(output_tail(&short), (short.as_str(), false));
        let long = format!("{}é{}", "a".repeat(100), "b".repeat(MAX_OUTPUT_BYTES - 1));
        let (tail, cut) = output_tail(&long);
        assert!(cut);
        assert!(tail.len() <= MAX_OUTPUT_BYTES);
        assert!(tail.starts_with('b'));
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
