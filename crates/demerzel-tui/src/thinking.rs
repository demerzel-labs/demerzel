//! Thinking blocks: one reasoning summary segment each. A closed
//! block is one row — the state and the summary's first line. An
//! open block shows the whole summary. The block keeps the summary
//! either way, so a key opens and closes it after the fact.

use std::time::{Duration, Instant};

use crate::cards::{one_line, write_duration};

/// Bytes of the summary's first line on a closed block.
const LINE_BYTES: usize = 240;

/// Bytes a block keeps free above its summary for the header row and
/// the cut mark, so the render stays under the block cap.
pub(crate) const SUMMARY_RESERVE: usize = 256;

/// How a thinking block starts: the `thinking` config key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Thinking {
    /// One row: the state and the summary's first line.
    #[default]
    Closed,
    /// The whole summary.
    Open,
    /// No block at all.
    Hidden,
}

impl Thinking {
    const ALL: [Thinking; 3] = [Thinking::Closed, Thinking::Open, Thinking::Hidden];

    /// The names the config key takes, for messages.
    pub const NAMES: &str = "closed, open, hidden";

    pub fn as_str(self) -> &'static str {
        match self {
            Thinking::Closed => "closed",
            Thinking::Open => "open",
            Thinking::Hidden => "hidden",
        }
    }
}

impl std::str::FromStr for Thinking {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Thinking::ALL
            .into_iter()
            .find(|state| state.as_str() == value)
            .ok_or_else(|| format!("must be one of {}, not `{value}`", Thinking::NAMES))
    }
}

impl std::fmt::Display for Thinking {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug)]
enum State {
    /// Deltas still arrive.
    Live,
    Done(Duration),
    /// The turn ended before the segment did.
    Interrupted,
}

/// One reasoning segment: its summary and its state, rendered into
/// its block's text on demand.
#[derive(Debug)]
pub(crate) struct Thought {
    summary: String,
    /// The summary hit its cap; the rest dropped.
    cut: bool,
    open: bool,
    state: State,
    started: Instant,
    /// The text needs a render before the next layout.
    stale: bool,
}

impl Thought {
    pub fn new(open: bool, at: Instant) -> Self {
        Self {
            summary: String::new(),
            cut: false,
            open,
            state: State::Live,
            started: at,
            stale: true,
        }
    }

    /// Append streamed summary text, sanitized, up to `cap` bytes of
    /// summary. Leading whitespace on an empty summary drops: an
    /// adapter's paragraph break between segments is not content.
    pub fn push(&mut self, delta: &str, cap: usize) {
        let delta = if self.summary.is_empty() {
            delta.trim_start()
        } else {
            delta
        };
        if delta.is_empty() || self.cut {
            return;
        }
        for ch in delta.chars() {
            if self.summary.len() >= cap {
                self.cut = true;
                break;
            }
            match ch {
                '\t' => self.summary.push_str("    "),
                ch if ch.is_control() && ch != '\n' => {}
                ch => self.summary.push(ch),
            }
        }
        self.stale = true;
    }

    /// The segment ended at `at`.
    pub fn end(&mut self, at: Instant) {
        if matches!(self.state, State::Live) {
            self.state = State::Done(at.saturating_duration_since(self.started));
            self.stale = true;
        }
    }

    /// The turn ended before the segment did.
    pub fn interrupt(&mut self) {
        if matches!(self.state, State::Live) {
            self.state = State::Interrupted;
            self.stale = true;
        }
    }

    pub fn live(&self) -> bool {
        matches!(self.state, State::Live)
    }

    pub fn open(&self) -> bool {
        self.open
    }

    pub fn set_open(&mut self, open: bool) {
        if self.open != open {
            self.open = open;
            self.stale = true;
        }
    }

    pub fn stale(&self) -> bool {
        self.stale
    }

    /// Render the block's text: the header row, then the summary
    /// whole when open, or its first line on the header row when
    /// closed. Clears the stale mark.
    pub fn render(&mut self, out: &mut String) {
        self.stale = false;
        out.clear();
        let glyph = if self.open { '▾' } else { '▸' };
        match self.state {
            State::Live => out.push_str("· thinking"),
            State::Done(duration) => {
                out.push(glyph);
                out.push_str(" thought for ");
                write_duration(out, duration);
            }
            State::Interrupted => {
                out.push(glyph);
                out.push_str(" thinking interrupted");
            }
        }
        if self.summary.is_empty() {
            return;
        }
        if self.open {
            out.push('\n');
            out.push_str(&self.summary);
            if self.cut {
                out.push_str("\n…");
            }
            return;
        }
        let (first, rest) = match self.summary.find('\n') {
            Some(at) => (&self.summary[..at], &self.summary[at + 1..]),
            None => (self.summary.as_str(), ""),
        };
        out.push_str(" · ");
        out.push_str(&one_line(first, LINE_BYTES));
        let more = self.cut || !rest.trim().is_empty();
        if more && !out.ends_with('…') {
            out.push('…');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAP: usize = 1024;

    fn at(secs: u64) -> Instant {
        // A fixed origin, so durations under test are exact.
        static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *ORIGIN.get_or_init(Instant::now) + Duration::from_secs(secs)
    }

    fn text(thought: &mut Thought) -> String {
        let mut out = String::new();
        thought.render(&mut out);
        out
    }

    #[test]
    fn a_closed_block_is_one_row_and_names_its_state() {
        let mut thought = Thought::new(false, at(0));
        thought.push("**Plan** the fix\nthen the rest", CAP);
        assert_eq!(text(&mut thought), "· thinking · **Plan** the fix…");
        thought.end(at(2));
        assert_eq!(text(&mut thought), "▸ thought for 2.0s · **Plan** the fix…");
        assert!(!thought.stale());
    }

    #[test]
    fn an_open_block_shows_the_summary_whole() {
        let mut thought = Thought::new(true, at(0));
        thought.push("one\ntwo", CAP);
        assert_eq!(text(&mut thought), "· thinking\none\ntwo");
        thought.end(at(1));
        assert_eq!(text(&mut thought), "▾ thought for 1.0s\none\ntwo");
        thought.set_open(false);
        assert!(thought.stale());
        assert_eq!(text(&mut thought), "▸ thought for 1.0s · one…");
        thought.set_open(false);
        assert!(!thought.stale(), "a no-op toggle renders nothing");
    }

    #[test]
    fn a_one_line_summary_gets_no_more_mark() {
        let mut thought = Thought::new(false, at(0));
        thought.push("just this\n\n", CAP);
        thought.end(at(0));
        assert_eq!(text(&mut thought), "▸ thought for 0ms · just this");
    }

    #[test]
    fn an_empty_summary_renders_the_header_alone() {
        let mut thought = Thought::new(false, at(0));
        thought.push("\n\n  ", CAP);
        assert_eq!(text(&mut thought), "· thinking");
        thought.interrupt();
        assert_eq!(text(&mut thought), "▸ thinking interrupted");
        thought.end(at(3));
        assert_eq!(
            text(&mut thought),
            "▸ thinking interrupted",
            "an end after the interrupt changes nothing"
        );
    }

    #[test]
    fn the_summary_is_sanitized_and_cut_at_its_cap() {
        let mut thought = Thought::new(true, at(0));
        thought.push("a\x1b[31mb\tc\r\n", CAP);
        assert_eq!(text(&mut thought), "· thinking\na[31mb    c\n");
        let mut thought = Thought::new(true, at(0));
        thought.push(&"x".repeat(2 * CAP), CAP);
        thought.push("more", CAP);
        let out = text(&mut thought);
        assert_eq!(out.len(), "· thinking\n".len() + CAP + "\n…".len());
        assert!(out.ends_with("\n…"));
        thought.set_open(false);
        assert!(
            text(&mut thought).ends_with('…'),
            "a closed cut block shows the mark"
        );
    }

    #[test]
    fn a_long_first_line_is_cut_on_the_closed_row() {
        let mut thought = Thought::new(false, at(0));
        thought.push(&"y".repeat(2 * LINE_BYTES), CAP);
        let out = text(&mut thought);
        assert!(out.len() < LINE_BYTES + 32, "{}", out.len());
        assert!(out.ends_with('…'));
    }

    #[test]
    fn the_config_names_parse_both_ways() {
        for state in Thinking::ALL {
            assert_eq!(state.as_str().parse::<Thinking>(), Ok(state));
        }
        assert!(
            "shown"
                .parse::<Thinking>()
                .unwrap_err()
                .contains("closed, open, hidden")
        );
        assert_eq!(Thinking::default(), Thinking::Closed);
    }
}
