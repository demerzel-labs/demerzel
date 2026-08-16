//! The transcript model: what one session has shown so far.
//!
//! The transcript stores logical lines, not wrapped rows. The renderer
//! wraps the visible tail; scrolling counts wrapped rows from the
//! bottom. The line count is capped so a long session cannot grow
//! memory without bound; the cap drops the oldest lines.

use std::collections::HashMap;
use std::collections::VecDeque;

/// Logical lines the transcript keeps. Old lines beyond the cap drop.
const MAX_LINES: usize = 5_000;

/// How a line renders. Kinds map to terminal-native styles in M2;
/// theming is M3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Model text.
    Text,
    /// Reasoning summary, dim.
    Reasoning,
    /// One tool call, updated in place when the call ends.
    Status,
    /// Interface message, dim: usage, session notes, approval echoes.
    Notice,
    /// Failure message.
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub kind: Kind,
    pub text: String,
}

/// The scrollable session log.
pub struct Transcript {
    entries: VecDeque<Entry>,
    /// Lines dropped from the front, so tool-line indices stay stable.
    dropped: usize,
    /// The kind of the last line while it still streams.
    open: Option<Kind>,
    /// Tool call id to the absolute index of its status line.
    tool_lines: HashMap<String, usize>,
    /// Wrapped rows scrolled up from the bottom. The renderer clamps
    /// it against the real row count.
    pub scroll: usize,
}

impl Transcript {
    pub fn new() -> Self {
        Self {
            entries: VecDeque::new(),
            dropped: 0,
            open: None,
            tool_lines: HashMap::new(),
            scroll: 0,
        }
    }

    pub fn entries(&self) -> impl DoubleEndedIterator<Item = &Entry> + ExactSizeIterator {
        self.entries.iter()
    }

    /// Append streamed text of `kind`. Consecutive deltas of one kind
    /// extend the same line; a kind switch or a full line closes it.
    pub fn push_delta(&mut self, kind: Kind, delta: &str) {
        if delta.is_empty() {
            return;
        }
        if self.open != Some(kind) {
            self.open = None;
        }
        let closes = delta.ends_with('\n');
        let mut parts: Vec<&str> = delta.split('\n').collect();
        if closes {
            // The trailing newline ends the last line; it opens no
            // empty one.
            parts.pop();
        }
        for (index, part) in parts.iter().enumerate() {
            if index == 0
                && self.open == Some(kind)
                && let Some(last) = self.entries.back_mut()
            {
                push_sanitized(&mut last.text, part);
            } else {
                self.push_entry(kind, part);
            }
        }
        self.open = if closes { None } else { Some(kind) };
    }

    /// Append whole lines. A newline inside `text` splits it.
    pub fn push_line(&mut self, kind: Kind, text: &str) {
        self.open = None;
        for part in text.split('\n') {
            self.push_entry(kind, part);
        }
    }

    /// One status line for a started tool call, updatable by id.
    pub fn tool_start(&mut self, id: &str, name: &str) {
        self.push_line(Kind::Status, &format!("· {name} …"));
        let index = self.dropped + self.entries.len() - 1;
        self.tool_lines.insert(id.to_string(), index);
    }

    /// Rewrite the call's status line in place. When the line already
    /// fell off the cap, append instead.
    pub fn tool_end(&mut self, id: &str, summary: &str, is_error: bool) {
        let mark = if is_error { "✗" } else { "·" };
        let mut text = String::new();
        push_sanitized(&mut text, &format!("{mark} {summary}"));
        // A summary with newlines still lands on one status line.
        let text = text.replace('\n', " ");
        match self
            .tool_lines
            .remove(id)
            .and_then(|index| index.checked_sub(self.dropped))
        {
            Some(offset) if offset < self.entries.len() => {
                self.entries[offset].text = text;
            }
            _ => self.push_line(Kind::Status, &text),
        }
    }

    /// Close any line that still streams.
    pub fn close(&mut self) {
        self.open = None;
    }

    fn push_entry(&mut self, kind: Kind, text: &str) {
        let mut clean = String::with_capacity(text.len());
        push_sanitized(&mut clean, text);
        self.entries.push_back(Entry { kind, text: clean });
        while self.entries.len() > MAX_LINES {
            self.entries.pop_front();
            self.dropped += 1;
        }
    }
}

/// Append `text` with control characters neutralized: tabs become
/// spaces, the rest drop. An escape byte must never reach the frame.
fn push_sanitized(out: &mut String, text: &str) {
    for ch in text.chars() {
        if ch == '\t' {
            out.push_str("    ");
        } else if ch == '\n' || !ch.is_control() {
            out.push(ch);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(t: &Transcript) -> Vec<(Kind, &str)> {
        t.entries().map(|e| (e.kind, e.text.as_str())).collect()
    }

    #[test]
    fn deltas_of_one_kind_extend_the_open_line() {
        let mut t = Transcript::new();
        t.push_delta(Kind::Text, "hel");
        t.push_delta(Kind::Text, "lo\nworld");
        assert_eq!(
            lines(&t),
            vec![(Kind::Text, "hello"), (Kind::Text, "world")]
        );
    }

    #[test]
    fn a_kind_switch_closes_the_open_line() {
        let mut t = Transcript::new();
        t.push_delta(Kind::Reasoning, "thinking");
        t.push_delta(Kind::Text, "answer");
        assert_eq!(
            lines(&t),
            vec![(Kind::Reasoning, "thinking"), (Kind::Text, "answer")]
        );
    }

    #[test]
    fn a_trailing_newline_closes_the_line() {
        let mut t = Transcript::new();
        t.push_delta(Kind::Text, "done\n");
        t.push_delta(Kind::Text, "next");
        assert_eq!(lines(&t), vec![(Kind::Text, "done"), (Kind::Text, "next")]);
    }

    #[test]
    fn a_pushed_line_interrupts_a_stream_and_the_stream_reopens() {
        let mut t = Transcript::new();
        t.push_delta(Kind::Text, "strea");
        t.push_line(Kind::Notice, "note");
        t.push_delta(Kind::Text, "med");
        assert_eq!(
            lines(&t),
            vec![
                (Kind::Text, "strea"),
                (Kind::Notice, "note"),
                (Kind::Text, "med"),
            ]
        );
    }

    #[test]
    fn tool_lines_update_in_place() {
        let mut t = Transcript::new();
        t.tool_start("call_1", "bash");
        t.push_delta(Kind::Text, "meanwhile");
        t.tool_end("call_1", "bash: ok", false);
        assert_eq!(
            lines(&t),
            vec![(Kind::Status, "· bash: ok"), (Kind::Text, "meanwhile")]
        );
    }

    #[test]
    fn a_failed_tool_line_gets_the_error_mark() {
        let mut t = Transcript::new();
        t.tool_start("call_1", "edit");
        t.tool_end("call_1", "stale tag", true);
        assert_eq!(lines(&t), vec![(Kind::Status, "✗ stale tag")]);
    }

    #[test]
    fn an_unknown_call_id_appends_instead() {
        let mut t = Transcript::new();
        t.tool_end("ghost", "late result", false);
        assert_eq!(lines(&t), vec![(Kind::Status, "· late result")]);
    }

    #[test]
    fn the_cap_drops_old_lines_and_tool_updates_survive() {
        let mut t = Transcript::new();
        t.tool_start("call_1", "bash");
        for i in 0..MAX_LINES {
            t.push_line(Kind::Text, &format!("line {i}"));
        }
        // The status line fell off the front; the update appends.
        t.tool_end("call_1", "bash: ok", false);
        assert_eq!(t.entries().len(), MAX_LINES);
        let last = t.entries().last().unwrap();
        assert_eq!(last.text, "· bash: ok");
    }

    #[test]
    fn control_characters_never_reach_an_entry() {
        let mut t = Transcript::new();
        t.push_delta(Kind::Text, "a\x1b[31mred\tdone\r");
        assert_eq!(lines(&t), vec![(Kind::Text, "a[31mred    done")]);
    }
}
