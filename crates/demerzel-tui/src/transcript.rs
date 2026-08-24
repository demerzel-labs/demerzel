//! The transcript model: what one session has shown so far, as blocks.
//!
//! A block is one unit of the session: a prompt echo, one streamed
//! model reply segment, one reasoning segment, a tool call, a notice,
//! or an error. Each block owns a layout cache: its wrapped rows at
//! the last width, as byte offsets into its text. The renderer walks
//! blocks from the bottom until the viewport is full, so a long
//! session pays for its window, not its history. A streamed delta
//! touches only the open block, and that block's layout catches up by
//! re-wrapping its last logical line, not the whole block.
//!
//! Two caps bound memory and per-frame work. The block count cap
//! drops the oldest blocks. The per-block byte cap closes a block that
//! grows past it and continues the stream in a new one, so a full
//! re-layout — a width change — never wraps more than one cap of
//! text per block.
//!
//! The view is `scroll`: rows between the bottom of the viewport and
//! the end of the transcript. Zero is follow mode, the view sticks to
//! the bottom. A positive value is browse mode: rows that arrive below
//! the viewport add to it, so the view stays where the user put it,
//! and the same number names the rows below for the indicator.

use std::collections::HashMap;
use std::collections::VecDeque;

use crate::wrap::row_starts;

/// Blocks the transcript keeps. Old blocks beyond the cap drop.
pub const MAX_BLOCKS: usize = 2_000;

/// Byte cap for one block. A streamed block that reaches it closes
/// and the stream continues in a new block; a pushed block splits.
pub const MAX_BLOCK_BYTES: usize = 32 * 1024;

/// What a block is. Kinds map to terminal-native styles until the
/// theme lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The user's submitted line, echoed with the prompt.
    Prompt,
    /// One model reply segment, streamed.
    Text,
    /// One reasoning summary segment, streamed, dim.
    Reasoning,
    /// One tool call: its status line, updated in place.
    Tool,
    /// Interface message, dim: usage, session notes, previews,
    /// approval echoes.
    Notice,
    /// Failure message.
    Error,
}

/// The wrapped rows of one block at one width.
#[derive(Debug, Default)]
struct Layout {
    /// The width the rows hold for; zero before the first layout.
    width: u16,
    /// Byte offset where each row starts. A row ends where the next
    /// row starts, less the newline that ended its logical line.
    rows: Vec<u32>,
    /// Rows of the logical lines before the last one. Appended text
    /// can change only the last line, so these stay across a sync.
    stable: usize,
    /// Byte offset where the last logical line starts.
    tail: usize,
    /// Text arrived since the rows were laid out.
    dirty: bool,
}

/// One unit of the transcript with its layout cache.
#[derive(Debug)]
pub struct Block {
    pub kind: Kind,
    text: String,
    layout: Layout,
}

impl Block {
    fn new(kind: Kind, text: String) -> Self {
        Self {
            kind,
            text,
            layout: Layout::default(),
        }
    }

    #[cfg(test)]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether the rows hold for `width` with no text pending.
    pub(crate) fn clean(&self, width: u16) -> bool {
        self.layout.width == width && !self.layout.dirty
    }

    /// Bring the rows up to date at `width`. Returns the rows the
    /// block gained since its last sync at this width: all of them
    /// for a block laid out for the first time, none for a layout at
    /// a new width, where a row count means nothing.
    pub(crate) fn sync(&mut self, width: u16) -> usize {
        if self.layout.width != width {
            let first = self.layout.width == 0;
            self.layout.width = width;
            self.layout.rows.clear();
            self.lay_out_from(0);
            self.layout.dirty = false;
            return if first { self.layout.rows.len() } else { 0 };
        }
        if !self.layout.dirty {
            return 0;
        }
        let before = self.layout.rows.len();
        self.layout.rows.truncate(self.layout.stable);
        let tail = self.layout.tail;
        self.lay_out_from(tail);
        self.layout.dirty = false;
        self.layout.rows.len().saturating_sub(before)
    }

    /// Wrap the logical lines from byte `from` on and append their
    /// row starts. A trailing newline ends the last line; it opens no
    /// empty one.
    fn lay_out_from(&mut self, from: usize) {
        let text = &self.text;
        let layout = &mut self.layout;
        let mut start = from;
        loop {
            let rest = &text[start..];
            let (line, next) = match rest.find('\n') {
                Some(end) => (&rest[..end], Some(start + end + 1)),
                None => (rest, None),
            };
            if next.is_none() && start == text.len() && start > 0 {
                layout.stable = layout.rows.len();
                layout.tail = start;
                break;
            }
            layout.stable = layout.rows.len();
            layout.tail = start;
            row_starts(line, layout.width, |offset| {
                layout.rows.push((start + offset) as u32);
            });
            match next {
                Some(next) => start = next,
                None => break,
            }
        }
    }

    /// Rows at the synced width.
    pub(crate) fn rows(&self) -> usize {
        self.layout.rows.len()
    }

    /// One row at the synced width.
    pub(crate) fn row(&self, index: usize) -> &str {
        let start = self.layout.rows[index] as usize;
        let end = self
            .layout
            .rows
            .get(index + 1)
            .map_or(self.text.len(), |&next| next as usize);
        let row = &self.text[start..end];
        row.strip_suffix('\n').unwrap_or(row)
    }

    /// Append streamed text. The rows catch up at the next sync.
    /// Returns the overflow past the byte cap, for a new block.
    fn append(&mut self, delta: &str) -> Option<String> {
        push_sanitized(&mut self.text, delta);
        self.layout.dirty = true;
        let overflow = split_at_cap(&mut self.text);
        if overflow.is_some() {
            // The cut can land inside the laid-out tail: the next
            // sync lays the closed block out whole.
            self.relayout_whole();
        }
        overflow
    }

    /// Replace the text whole: a tool line changing state.
    fn set_text(&mut self, text: String) {
        self.text = text;
        self.relayout_whole();
    }

    /// Make the next sync lay every row out again, at the same width,
    /// so it still reports the rows the block gained or lost.
    fn relayout_whole(&mut self) {
        self.layout.stable = 0;
        self.layout.tail = 0;
        self.layout.dirty = true;
    }
}

/// The scrollable session log.
pub struct Transcript {
    blocks: VecDeque<Block>,
    /// Blocks dropped from the front, so tool-block indices stay
    /// stable.
    dropped: usize,
    /// The kind of the last block while it still streams.
    open: Option<Kind>,
    /// Tool call id to the absolute index of its block and the tool
    /// name, kept so an unanswered call can close as interrupted.
    tools: HashMap<String, (usize, String)>,
    /// Rows between the viewport bottom and the transcript end. The
    /// renderer clamps it against the rows that exist.
    scroll: usize,
    /// The width of the last frame; zero before the first.
    width: u16,
}

impl Transcript {
    pub fn new() -> Self {
        Self {
            blocks: VecDeque::new(),
            dropped: 0,
            open: None,
            tools: HashMap::new(),
            scroll: 0,
            width: 0,
        }
    }

    pub fn blocks(&self) -> impl DoubleEndedIterator<Item = &Block> + ExactSizeIterator {
        self.blocks.iter()
    }

    pub(crate) fn blocks_mut(
        &mut self,
    ) -> impl DoubleEndedIterator<Item = &mut Block> + ExactSizeIterator {
        self.blocks.iter_mut()
    }

    /// Append streamed text of `kind`. Consecutive deltas of one kind
    /// extend the open block; a kind switch or a pushed block closes
    /// it. A block that reaches the byte cap closes too, and the
    /// stream continues in a new one.
    pub fn push_delta(&mut self, kind: Kind, delta: &str) {
        if delta.is_empty() {
            return;
        }
        if self.open == Some(kind)
            && let Some(last) = self.blocks.back_mut()
        {
            if let Some(overflow) = last.append(delta) {
                self.push_block(kind, overflow);
            }
        } else {
            let mut text = String::with_capacity(delta.len().min(MAX_BLOCK_BYTES));
            push_sanitized(&mut text, delta);
            self.push_block(kind, text);
        }
        self.open = Some(kind);
    }

    /// Append one whole block. Newlines inside `text` stay: a preview
    /// or a pasted prompt is one block of several lines.
    pub fn push_line(&mut self, kind: Kind, text: &str) {
        self.open = None;
        let mut clean = String::with_capacity(text.len().min(MAX_BLOCK_BYTES));
        push_sanitized(&mut clean, text);
        self.push_block(kind, clean);
    }

    /// One status block for a started tool call, updatable by id.
    pub fn tool_start(&mut self, id: &str, name: &str) {
        self.push_line(Kind::Tool, &format!("· {name} …"));
        let index = self.dropped + self.blocks.len() - 1;
        self.tools.insert(id.to_string(), (index, name.to_string()));
    }

    /// Rewrite the call's block in place. When the block already fell
    /// off the cap, append instead.
    pub fn tool_end(&mut self, id: &str, summary: &str, is_error: bool) {
        let mark = if is_error { "✗" } else { "·" };
        let text = tool_text(&format!("{mark} {summary}"));
        match self
            .tools
            .remove(id)
            .and_then(|(index, _)| index.checked_sub(self.dropped))
        {
            Some(offset) if offset < self.blocks.len() => self.blocks[offset].set_text(text),
            _ => self.push_line(Kind::Tool, &text),
        }
    }

    /// End-of-turn cleanup: close any block that still streams, and
    /// mark every unanswered call as interrupted — an aborted turn
    /// must not leave a pending line that reads like an applied
    /// change. The marker claims interruption, not absence: a
    /// dropped turn can cut an approved call mid-run, so whether the
    /// call changed anything is unknown here.
    pub fn close(&mut self) {
        self.open = None;
        for (index, name) in self.tools.values() {
            if let Some(offset) = index.checked_sub(self.dropped)
                && offset < self.blocks.len()
            {
                // The name is model-controlled: it goes through the
                // same sanitizer and cap as every other write.
                let text = tool_text(&format!("✗ {name}: interrupted"));
                self.blocks[offset].set_text(text);
            }
        }
        self.tools.clear();
    }

    /// Rows between the viewport bottom and the end. Zero is follow
    /// mode.
    pub fn scroll(&self) -> usize {
        self.scroll
    }

    pub fn browsing(&self) -> bool {
        self.scroll > 0
    }

    /// Enter or deepen browse mode by `rows`. The renderer clamps.
    pub fn scroll_up(&mut self, rows: usize) {
        self.scroll = self.scroll.saturating_add(rows);
    }

    pub fn scroll_down(&mut self, rows: usize) {
        self.scroll = self.scroll.saturating_sub(rows);
    }

    /// Return to follow mode: the view sticks to the bottom.
    pub fn follow(&mut self) {
        self.scroll = 0;
    }

    /// Clamp the scroll to the rows above the viewport.
    pub(crate) fn clamp_scroll(&mut self, rows_above: usize) {
        self.scroll = self.scroll.min(rows_above);
    }

    /// Start a frame at `width`: lay out every block that changed since
    /// the last frame, and keep a browsing view anchored by adding the
    /// rows that arrived below it. Blocks that changed form a suffix
    /// — new blocks append, and only the open block streams — so the
    /// walk stops at the first block that is clean. A width change
    /// lays nothing out here: the renderer lays out the blocks it
    /// shows, and the scroll clamps.
    pub(crate) fn begin_frame(&mut self, width: u16) {
        if width != self.width {
            self.width = width;
            return;
        }
        let mut added = 0;
        for block in self.blocks.iter_mut().rev() {
            if block.clean(width) {
                break;
            }
            added += block.sync(width);
        }
        if self.scroll > 0 {
            self.scroll = self.scroll.saturating_add(added);
        }
    }

    /// Append `text` as blocks: one, or several when it exceeds the
    /// byte cap. `text` is sanitized already.
    fn push_block(&mut self, kind: Kind, mut text: String) {
        loop {
            let rest = split_at_cap(&mut text);
            self.blocks.push_back(Block::new(kind, text));
            while self.blocks.len() > MAX_BLOCKS {
                self.blocks.pop_front();
                self.dropped += 1;
            }
            match rest {
                Some(tail) => text = tail,
                None => break,
            }
        }
    }
}

/// A tool status line: sanitized, on one line, inside the cap.
fn tool_text(raw: &str) -> String {
    let mut text = String::new();
    push_sanitized(&mut text, raw);
    let mut text = text.replace('\n', " ");
    let _ = split_at_cap(&mut text);
    text
}

/// When `text` exceeds the cap, cut it at the last char boundary
/// inside the cap and return the tail.
fn split_at_cap(text: &mut String) -> Option<String> {
    if text.len() <= MAX_BLOCK_BYTES {
        return None;
    }
    let mut cut = MAX_BLOCK_BYTES;
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
    use crate::wrap::wrap;

    fn blocks(t: &Transcript) -> Vec<(Kind, &str)> {
        t.blocks().map(|b| (b.kind, b.text())).collect()
    }

    /// Every row of every block, laid out at `width`.
    fn rows(t: &mut Transcript, width: u16) -> Vec<String> {
        let mut out = Vec::new();
        for block in t.blocks_mut() {
            block.sync(width);
            for index in 0..block.rows() {
                out.push(block.row(index).to_string());
            }
        }
        out
    }

    /// The rows a block's text wraps to when laid out from scratch:
    /// the ground truth an incremental layout must match.
    fn expected_rows(text: &str, width: u16) -> Vec<String> {
        let body = text.strip_suffix('\n').unwrap_or(text);
        if text.is_empty() {
            return vec![String::new()];
        }
        body.split('\n')
            .flat_map(|line| wrap(line, width).into_iter().map(str::to_string))
            .collect()
    }

    #[test]
    fn deltas_of_one_kind_extend_the_open_block() {
        let mut t = Transcript::new();
        t.push_delta(Kind::Text, "hel");
        t.push_delta(Kind::Text, "lo\nworld");
        assert_eq!(blocks(&t), vec![(Kind::Text, "hello\nworld")]);
        assert_eq!(rows(&mut t, 80), vec!["hello", "world"]);
    }

    #[test]
    fn a_kind_switch_opens_a_new_block() {
        let mut t = Transcript::new();
        t.push_delta(Kind::Reasoning, "thinking");
        t.push_delta(Kind::Text, "answer");
        assert_eq!(
            blocks(&t),
            vec![(Kind::Reasoning, "thinking"), (Kind::Text, "answer")]
        );
    }

    #[test]
    fn a_trailing_newline_opens_no_empty_row() {
        let mut t = Transcript::new();
        t.push_delta(Kind::Text, "done\n");
        assert_eq!(rows(&mut t, 80), vec!["done"]);
        t.push_delta(Kind::Text, "next");
        assert_eq!(rows(&mut t, 80), vec!["done", "next"]);
        // A blank line inside the text is one empty row.
        t.push_delta(Kind::Text, "\n\nlast");
        assert_eq!(rows(&mut t, 80), vec!["done", "next", "", "last"]);
    }

    #[test]
    fn a_pushed_block_interrupts_a_stream_and_the_stream_reopens() {
        let mut t = Transcript::new();
        t.push_delta(Kind::Text, "strea");
        t.push_line(Kind::Notice, "note");
        t.push_delta(Kind::Text, "med");
        assert_eq!(
            blocks(&t),
            vec![
                (Kind::Text, "strea"),
                (Kind::Notice, "note"),
                (Kind::Text, "med"),
            ]
        );
    }

    #[test]
    fn a_pushed_block_keeps_its_lines_together() {
        let mut t = Transcript::new();
        t.push_line(Kind::Notice, "one\ntwo");
        assert_eq!(t.blocks().len(), 1);
        assert_eq!(rows(&mut t, 80), vec!["one", "two"]);
    }

    #[test]
    fn an_incremental_layout_matches_a_fresh_one() {
        // Deltas of every shape — mid-word, at a space, with one or
        // several newlines, wide chars — at several widths: the rows
        // after each delta must equal the rows of the whole text.
        let text = "the quick brown fox\njumps over the lazy dog 日本語 text\n\nand a blank line\n\
                    then a much longer line that wraps more than once at every width tried\n";
        for width in [5u16, 12, 33, 80] {
            let mut t = Transcript::new();
            let mut whole = String::new();
            let mut cut = 0;
            let mut step = 1;
            while cut < text.len() {
                let mut end = (cut + step).min(text.len());
                while !text.is_char_boundary(end) {
                    end += 1;
                }
                t.push_delta(Kind::Text, &text[cut..end]);
                whole.push_str(&text[cut..end]);
                cut = end;
                step = step % 7 + 1;
                assert_eq!(
                    rows(&mut t, width),
                    expected_rows(&whole, width),
                    "width {width} after {cut} bytes"
                );
            }
        }
    }

    #[test]
    fn a_width_change_lays_the_block_out_again() {
        let mut t = Transcript::new();
        t.push_delta(Kind::Text, "one two three");
        assert_eq!(rows(&mut t, 80), vec!["one two three"]);
        assert_eq!(rows(&mut t, 8), vec!["one two ", "three"]);
        t.push_delta(Kind::Text, " four");
        assert_eq!(rows(&mut t, 8), vec!["one two ", "three ", "four"]);
    }

    #[test]
    fn sync_reports_the_rows_a_delta_added() {
        let mut t = Transcript::new();
        t.push_delta(Kind::Text, "one");
        let block = t.blocks_mut().next_back().unwrap();
        assert_eq!(block.sync(8), 1, "a first layout counts every row");
        assert!(block.clean(8));
        assert_eq!(block.sync(8), 0);
        // A new width lays out again and counts nothing.
        assert_eq!(block.sync(2), 0);
        assert_eq!(block.rows(), 2);
        assert_eq!(block.sync(8), 0);
        t.push_delta(Kind::Text, " two three\nfour");
        let block = t.blocks_mut().next_back().unwrap();
        assert!(!block.clean(8));
        assert_eq!(block.sync(8), 2);
        assert_eq!(block.sync(8), 0);
    }

    #[test]
    fn tool_blocks_update_in_place() {
        let mut t = Transcript::new();
        t.tool_start("call_1", "bash");
        t.push_delta(Kind::Text, "meanwhile");
        t.tool_end("call_1", "bash: ok", false);
        assert_eq!(
            blocks(&t),
            vec![(Kind::Tool, "· bash: ok"), (Kind::Text, "meanwhile")]
        );
    }

    #[test]
    fn a_failed_tool_block_gets_the_error_mark() {
        let mut t = Transcript::new();
        t.tool_start("call_1", "edit");
        t.tool_end("call_1", "stale tag", true);
        assert_eq!(blocks(&t), vec![(Kind::Tool, "✗ stale tag")]);
    }

    #[test]
    fn an_unknown_call_id_appends_instead() {
        let mut t = Transcript::new();
        t.tool_end("ghost", "late result", false);
        assert_eq!(blocks(&t), vec![(Kind::Tool, "· late result")]);
    }

    #[test]
    fn a_tool_update_lays_its_rows_out_again() {
        let mut t = Transcript::new();
        t.tool_start("call_1", "bash");
        assert_eq!(rows(&mut t, 80), vec!["· bash …"]);
        t.tool_end("call_1", "bash: exit 0 after a long summary", false);
        assert_eq!(
            rows(&mut t, 12),
            vec!["· bash: ", "exit 0 ", "after a ", "long summary"]
        );
    }

    #[test]
    fn the_cap_drops_old_blocks_and_tool_updates_survive() {
        let mut t = Transcript::new();
        t.tool_start("call_1", "bash");
        for i in 0..MAX_BLOCKS {
            t.push_line(Kind::Text, &format!("line {i}"));
        }
        // The status block fell off the front; the update appends.
        t.tool_end("call_1", "bash: ok", false);
        assert_eq!(t.blocks().len(), MAX_BLOCKS);
        let last = t.blocks().last().unwrap();
        assert_eq!(last.text(), "· bash: ok");
    }

    #[test]
    fn a_block_at_the_byte_cap_closes_and_the_stream_continues() {
        let mut t = Transcript::new();
        t.push_delta(Kind::Text, &"x".repeat(MAX_BLOCK_BYTES));
        t.push_delta(Kind::Text, "overflow");
        let all: Vec<_> = t.blocks().collect();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].text().len(), MAX_BLOCK_BYTES);
        assert_eq!(all[1].text(), "overflow");
        // The closed block lays out whole; the rows rejoin its text.
        let rows = rows(&mut t, 100);
        assert_eq!(rows.concat().len(), MAX_BLOCK_BYTES + "overflow".len());
    }

    #[test]
    fn oversized_blocks_split_at_the_cap_on_every_path() {
        // One pushed block, as a pasted prompt echoes it.
        let mut t = Transcript::new();
        t.push_line(Kind::Prompt, &"é".repeat(MAX_BLOCK_BYTES));
        assert!(t.blocks().len() >= 2);
        let joined: String = t.blocks().map(|b| b.text()).collect();
        assert_eq!(joined, "é".repeat(MAX_BLOCK_BYTES));
        for block in t.blocks() {
            assert!(block.text().len() <= MAX_BLOCK_BYTES);
        }

        // One giant delta onto an open block.
        let mut t = Transcript::new();
        t.push_delta(Kind::Text, "start");
        t.push_delta(Kind::Text, &"y".repeat(2 * MAX_BLOCK_BYTES));
        for block in t.blocks() {
            assert!(block.text().len() <= MAX_BLOCK_BYTES);
        }
        let joined: String = t.blocks().map(|b| b.text()).collect();
        assert_eq!(joined.len(), "start".len() + 2 * MAX_BLOCK_BYTES);
    }

    #[test]
    fn close_marks_unanswered_calls_as_interrupted_and_forgets_them() {
        let mut t = Transcript::new();
        t.tool_start("call_1", "bash");
        t.close();
        // The pending block claims interruption, never absence: the
        // call may have run partway before the turn dropped.
        assert_eq!(blocks(&t), vec![(Kind::Tool, "✗ bash: interrupted")]);
        // A late end for a forgotten id appends; it rewrites nothing.
        t.tool_end("call_1", "late", false);
        assert_eq!(t.blocks().len(), 2);
    }

    #[test]
    fn the_interrupted_marker_sanitizes_a_hostile_tool_name() {
        let mut t = Transcript::new();
        t.tool_start("call_1", "ba\x1b[2Jsh\nSECOND");
        t.close();
        let last = t.blocks().last().unwrap();
        assert_eq!(last.text(), "✗ ba[2Jsh SECOND: interrupted");
    }

    #[test]
    fn close_leaves_answered_calls_alone() {
        let mut t = Transcript::new();
        t.tool_start("call_1", "bash");
        t.tool_end("call_1", "bash: ok", false);
        t.close();
        assert_eq!(blocks(&t), vec![(Kind::Tool, "· bash: ok")]);
    }

    #[test]
    fn control_characters_never_reach_a_block() {
        let mut t = Transcript::new();
        t.push_delta(Kind::Text, "a\x1b[31mred\tdone\r");
        assert_eq!(blocks(&t), vec![(Kind::Text, "a[31mred    done")]);
        let mut t = Transcript::new();
        t.push_line(Kind::Notice, "a\x1b[31mred\tdone\r");
        assert_eq!(blocks(&t), vec![(Kind::Notice, "a[31mred    done")]);
    }

    #[test]
    fn a_browsing_view_stays_put_when_rows_arrive() {
        let mut t = Transcript::new();
        for i in 0..10 {
            t.push_line(Kind::Text, &format!("line {i}"));
        }
        // The first frame at a width lays out what it shows; the
        // test lays out everything, as a full-height frame would.
        t.begin_frame(40);
        rows(&mut t, 40);
        t.scroll_up(4);
        // Rows that arrive below the viewport deepen the scroll by
        // their count, so the rows on screen do not move.
        t.push_line(Kind::Text, "one more");
        t.push_delta(Kind::Text, "and a stream\nof two rows");
        t.begin_frame(40);
        assert_eq!(t.scroll(), 7);
        // A delta that adds no row changes nothing.
        t.push_delta(Kind::Text, " still");
        t.begin_frame(40);
        assert_eq!(t.scroll(), 7);
        // A width change leaves the scroll to the renderer's clamp.
        t.push_line(Kind::Text, "after a resize");
        t.begin_frame(20);
        assert_eq!(t.scroll(), 7);
    }

    #[test]
    fn follow_mode_ignores_arrivals_and_end_returns_to_it() {
        let mut t = Transcript::new();
        t.push_line(Kind::Text, "first");
        t.begin_frame(40);
        t.push_line(Kind::Text, "second");
        t.begin_frame(40);
        assert_eq!(t.scroll(), 0);
        assert!(!t.browsing());
        t.scroll_up(3);
        assert!(t.browsing());
        t.scroll_down(1);
        assert_eq!(t.scroll(), 2);
        t.follow();
        assert!(!t.browsing());
    }
}
