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

/// Byte cap for one logical line. The renderer wraps a visible line
/// from its start, so an uncapped line grows every frame's cost.
const MAX_ENTRY_BYTES: usize = 8 * 1024;

/// How a line renders. Kinds map to terminal-native styles in M2;
/// theming is M3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The user's submitted line, echoed with the prompt.
    Prompt,
    /// Model text.
    Text,
    /// Reasoning summary, dim.
    Reasoning,
    /// A tool-call line or an approval header.
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
    /// Tool call id to the absolute index of its status line and the
    /// tool name, kept so an unanswered call can close as
    /// interrupted.
    tool_lines: HashMap<String, (usize, String)>,
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
    /// A line that reaches the byte cap closes too, so one endless
    /// stream line cannot grow render cost without bound.
    pub fn push_delta(&mut self, kind: Kind, delta: &str) {
        if delta.is_empty() {
            return;
        }
        if self.open != Some(kind) {
            self.open = None;
        }
        // The trailing newline ends the last line; it opens no empty
        // one.
        let closes = delta.ends_with('\n');
        let body = delta.strip_suffix('\n').unwrap_or(delta);
        for (index, part) in body.split('\n').enumerate() {
            if index == 0
                && self.open == Some(kind)
                && let Some(last) = self.entries.back_mut()
            {
                push_sanitized(&mut last.text, part);
                let tail = split_at_cap(&mut last.text);
                if let Some(tail) = tail {
                    self.push_entry(kind, &tail);
                }
            } else {
                self.push_entry(kind, part);
            }
        }
        let full = self
            .entries
            .back()
            .is_some_and(|last| last.text.len() >= MAX_ENTRY_BYTES);
        self.open = if closes || full { None } else { Some(kind) };
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
        self.tool_lines
            .insert(id.to_string(), (index, name.to_string()));
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
            .and_then(|(index, _)| index.checked_sub(self.dropped))
        {
            Some(offset) if offset < self.entries.len() => {
                self.entries[offset].text = text;
            }
            _ => self.push_line(Kind::Status, &text),
        }
    }

    /// End-of-turn cleanup: close any line that still streams, and
    /// mark every unanswered call as interrupted — an aborted turn
    /// must not leave a pending line that reads like an applied
    /// change. The marker claims interruption, not absence: a
    /// dropped turn can cut an approved call mid-run, so whether the
    /// call changed anything is unknown here.
    pub fn close(&mut self) {
        self.open = None;
        for (index, name) in self.tool_lines.values() {
            if let Some(offset) = index.checked_sub(self.dropped)
                && offset < self.entries.len()
            {
                // The name is model-controlled: sanitize and cap it
                // the way every other entry write does.
                let mut text = String::new();
                push_sanitized(&mut text, &format!("✗ {name}: interrupted"));
                let mut text = text.replace('\n', " ");
                let _ = split_at_cap(&mut text);
                self.entries[offset].text = text;
            }
        }
        self.tool_lines.clear();
    }

    fn push_entry(&mut self, kind: Kind, text: &str) {
        let mut clean = String::with_capacity(text.len().min(MAX_ENTRY_BYTES));
        push_sanitized(&mut clean, text);
        // A pushed line obeys the same byte cap as a streamed one: a
        // pasted prompt must not make every later frame wrap it whole.
        loop {
            let rest = split_at_cap(&mut clean);
            self.entries.push_back(Entry { kind, text: clean });
            while self.entries.len() > MAX_LINES {
                self.entries.pop_front();
                self.dropped += 1;
            }
            match rest {
                Some(tail) => clean = tail,
                None => break,
            }
        }
    }
}

/// When `text` exceeds the cap, cut it at the last char boundary
/// inside the cap and return the tail.
fn split_at_cap(text: &mut String) -> Option<String> {
    if text.len() <= MAX_ENTRY_BYTES {
        return None;
    }
    let mut cut = MAX_ENTRY_BYTES;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    Some(text.split_off(cut))
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
    fn a_line_at_the_byte_cap_closes_and_the_stream_continues() {
        let mut t = Transcript::new();
        t.push_delta(Kind::Text, &"x".repeat(MAX_ENTRY_BYTES));
        t.push_delta(Kind::Text, "overflow");
        let all: Vec<_> = t.entries().collect();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].text.len(), MAX_ENTRY_BYTES);
        assert_eq!(all[1].text, "overflow");
    }

    #[test]
    fn oversized_lines_split_at_the_cap_on_every_path() {
        // One pushed line, as a pasted prompt echoes it.
        let mut t = Transcript::new();
        t.push_line(Kind::Prompt, &"é".repeat(MAX_ENTRY_BYTES));
        assert!(t.entries().len() >= 2);
        let joined: String = t.entries().map(|e| e.text.as_str()).collect();
        assert_eq!(joined, "é".repeat(MAX_ENTRY_BYTES));
        for entry in t.entries() {
            assert!(entry.text.len() <= MAX_ENTRY_BYTES);
        }

        // One giant delta onto an open line.
        let mut t = Transcript::new();
        t.push_delta(Kind::Text, "start");
        t.push_delta(Kind::Text, &"y".repeat(2 * MAX_ENTRY_BYTES));
        for entry in t.entries() {
            assert!(entry.text.len() <= MAX_ENTRY_BYTES);
        }
    }

    #[test]
    fn close_marks_unanswered_calls_as_interrupted_and_forgets_them() {
        let mut t = Transcript::new();
        t.tool_start("call_1", "bash");
        t.close();
        // The pending line claims interruption, never absence: the
        // call may have run partway before the turn dropped.
        assert_eq!(lines(&t), vec![(Kind::Status, "✗ bash: interrupted")]);
        // A late end for a forgotten id appends; it rewrites nothing.
        t.tool_end("call_1", "late", false);
        assert_eq!(t.entries().len(), 2);
    }

    #[test]
    fn the_interrupted_marker_sanitizes_a_hostile_tool_name() {
        let mut t = Transcript::new();
        t.tool_start("call_1", "ba\x1b[2Jsh\nSECOND");
        t.close();
        let last = t.entries().last().unwrap();
        assert_eq!(last.text, "✗ ba[2Jsh SECOND: interrupted");
    }

    #[test]
    fn close_leaves_answered_calls_alone() {
        let mut t = Transcript::new();
        t.tool_start("call_1", "bash");
        t.tool_end("call_1", "bash: ok", false);
        t.close();
        assert_eq!(lines(&t), vec![(Kind::Status, "· bash: ok")]);
    }

    #[test]
    fn control_characters_never_reach_an_entry() {
        let mut t = Transcript::new();
        t.push_delta(Kind::Text, "a\x1b[31mred\tdone\r");
        assert_eq!(lines(&t), vec![(Kind::Text, "a[31mred    done")]);
    }
}
