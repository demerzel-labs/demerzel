//! The transcript model: what one session has shown so far, as blocks.
//!
//! A block is one unit of the session: a prompt echo, one streamed
//! model reply segment, one reasoning segment, a tool call, a notice,
//! or an error. Each block owns a layout cache: its styled rows at
//! the last width. A plain block keeps its rows as byte offsets into
//! its text. A model reply renders as markdown into a buffer of its
//! own, with the rows and the face changes over that buffer. The
//! renderer walks blocks from the bottom until the viewport is full,
//! so a long session pays for its window, not its history. A streamed
//! delta touches only the open block. A plain block's layout catches
//! up from its last row, not from its start: a wrap break depends
//! only on the text before it, so the rows already laid out stay
//! valid. A markdown block renders again whole: a delta can close a
//! fence or an emphasis that opened rows earlier.
//!
//! Two caps bound memory and per-frame work. The block count cap
//! drops the oldest blocks. The per-block byte cap closes a block that
//! grows past it and continues the stream in a new one, so a full
//! re-layout — a width change — never wraps more than one cap of
//! text per block.
//!
//! The view is `scroll`: rows between the bottom of the viewport and
//! the end of the transcript. Zero is follow mode, the view sticks to
//! the bottom. A positive value is browse mode. Rows that arrive or
//! change at or below the top of the viewport add to it, so the row
//! the user reads stays where it is, and the same number names the
//! rows below the viewport for the indicator. A width change returns
//! to follow mode: the rows the number counted no longer exist.

use std::collections::HashMap;
use std::collections::VecDeque;

use crate::markdown::{self, Face, Target};
use crate::wrap::row_starts;

/// Blocks the transcript keeps. Old blocks beyond the cap drop.
const MAX_BLOCKS: usize = 2_000;

/// Byte cap for one block. A streamed block that reaches it closes
/// and the stream continues in a new block; a pushed block splits.
pub const MAX_BLOCK_BYTES: usize = 32 * 1024;

/// Byte cap for the rendered rows of one markdown block. Prefixes,
/// frames, and table padding grow the rows past the source; a block
/// whose render would cross the cap shows as plain text instead.
pub(crate) const MAX_RENDER_BYTES: usize = 4 * MAX_BLOCK_BYTES;

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

/// The rows of one block at one width.
#[derive(Debug, Default)]
struct Layout {
    /// The width the rows hold for; zero before the first layout.
    width: u16,
    /// Byte offset where each row starts, into `rendered` when the
    /// block rendered as markdown, else into the text. A row ends
    /// where the next row starts, less the newline that ended its
    /// logical line.
    rows: Vec<u32>,
    /// Face changes as `(offset, face)`, sorted; a face holds until
    /// the next change. Empty for a plain layout.
    faces: Vec<(u32, Face)>,
    /// The markdown rows' text. `None` for a plain layout.
    rendered: Option<String>,
    /// Plain layout: rows that stay valid when text is appended, all
    /// but the last row, because a wrap break depends only on the
    /// text before it.
    stable: usize,
    /// Plain layout: byte offset where the last row starts; the point
    /// a sync lays out from.
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
    #[cfg(test)]
    fn clean(&self, width: u16) -> bool {
        self.layout.width == width && !self.layout.dirty
    }

    /// Whether the block was laid out at any width.
    fn laid(&self) -> bool {
        self.layout.width != 0
    }

    /// Bring the rows up to date at `width`, a model reply as markdown
    /// when `markdown` is set. The flag holds for a transcript's whole
    /// life, so a block is always laid out one way. Returns the change
    /// in the row count since the last sync at this width: every row
    /// for a block laid out for the first time, zero for a layout at
    /// a new width, where a row count means nothing.
    pub(crate) fn sync(&mut self, width: u16, markdown: bool) -> isize {
        debug_assert!(width > 0, "zero width is the never-laid-out mark");
        let markdown = markdown && self.kind == Kind::Text;
        if self.layout.width != width {
            let first = !self.laid();
            self.layout.width = width;
            self.lay_out_whole(markdown);
            return if first { row_delta(0, self.rows()) } else { 0 };
        }
        if !self.layout.dirty {
            return 0;
        }
        let before = self.rows();
        if markdown {
            self.lay_out_whole(true);
        } else {
            self.layout.rows.truncate(self.layout.stable);
            let tail = self.layout.tail;
            self.lay_out_from(tail);
            self.layout.dirty = false;
        }
        row_delta(before, self.rows())
    }

    /// Lay every row out again: as markdown when asked and the render
    /// stays inside its cap, else as plain text.
    fn lay_out_whole(&mut self, markdown: bool) {
        let layout = &mut self.layout;
        layout.rows.clear();
        layout.faces.clear();
        if markdown {
            let text = layout.rendered.get_or_insert_with(String::new);
            text.clear();
            let target = Target {
                text,
                rows: &mut layout.rows,
                faces: &mut layout.faces,
            };
            if markdown::render(&self.text, layout.width, MAX_RENDER_BYTES, target) {
                layout.dirty = false;
                return;
            }
            layout.rows.clear();
            layout.faces.clear();
        }
        layout.rendered = None;
        self.lay_out_from(0);
        self.layout.dirty = false;
    }

    /// Wrap the text from byte `from` — a row start — and append the
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
            row_starts(line, layout.width, |offset| {
                layout.rows.push((start + offset) as u32);
            });
            layout.stable = layout.rows.len() - 1;
            layout.tail = layout.rows[layout.stable] as usize;
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

    /// The text the rows index: the rendered buffer or the source.
    fn buffer(&self) -> &str {
        self.layout.rendered.as_deref().unwrap_or(&self.text)
    }

    /// One row at the synced width, and where it starts in the buffer.
    fn row_at(&self, index: usize) -> (usize, &str) {
        let buffer = self.buffer();
        let start = self.layout.rows[index] as usize;
        let end = self
            .layout
            .rows
            .get(index + 1)
            .map_or(buffer.len(), |&next| next as usize);
        let row = &buffer[start..end];
        (start, row.strip_suffix('\n').unwrap_or(row))
    }

    /// One row at the synced width.
    pub(crate) fn row(&self, index: usize) -> &str {
        self.row_at(index).1
    }

    /// Whether every row is one span in the block's own style.
    pub(crate) fn plain(&self) -> bool {
        self.layout.faces.is_empty()
    }

    /// One row at the synced width as its spans, in order.
    pub(crate) fn spans<'b>(&'b self, index: usize, mut span: impl FnMut(Face, &'b str)) {
        let (from, row) = self.row_at(index);
        let faces = &self.layout.faces;
        let mut next = faces.partition_point(|&(at, _)| at as usize <= from);
        let mut face = if next == 0 {
            Face::Plain
        } else {
            faces[next - 1].1
        };
        let mut at = 0usize;
        while next < faces.len() && (faces[next].0 as usize) < from + row.len() {
            let (change, new) = faces[next];
            let change = change as usize - from;
            if change > at {
                span(face, &row[at..change]);
            }
            face = new;
            at = change;
            next += 1;
        }
        if at < row.len() || at == 0 {
            span(face, &row[at..]);
        }
    }

    /// Append streamed text. The rows catch up at the next sync.
    /// Returns the overflow past the byte cap, for a new block.
    fn append(&mut self, delta: &str) -> Option<String> {
        push_sanitized(&mut self.text, delta);
        self.layout.dirty = true;
        let overflow = split_at_cap(&mut self.text);
        if overflow.is_some() {
            // The closed block keeps no room for a stream it no
            // longer carries, and its cut can land inside the
            // laid-out tail: the next sync lays it out whole.
            self.text.shrink_to_fit();
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

fn row_delta(before: usize, after: usize) -> isize {
    // Row counts stay far below `isize::MAX`: the byte cap bounds them.
    after as isize - before as isize
}

/// The scrollable session log.
pub struct Transcript {
    /// Render model replies as markdown; off shows the raw text.
    markdown: bool,
    blocks: VecDeque<Block>,
    /// Blocks dropped from the front, so absolute block indices stay
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
    /// The absolute block count at the last frame: blocks past it
    /// arrived since.
    frame_end: usize,
}

impl Transcript {
    pub fn new(markdown: bool) -> Self {
        Self {
            markdown,
            blocks: VecDeque::new(),
            dropped: 0,
            open: None,
            tools: HashMap::new(),
            scroll: 0,
            width: 0,
            frame_end: 0,
        }
    }

    pub fn blocks(&self) -> impl DoubleEndedIterator<Item = &Block> + ExactSizeIterator {
        self.blocks.iter()
    }

    /// Whether model replies render as markdown.
    pub(crate) fn markdown(&self) -> bool {
        self.markdown
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
                self.push_blocks(kind, &overflow);
            }
        } else {
            let mut text = String::with_capacity(delta.len());
            push_sanitized(&mut text, delta);
            self.push_blocks(kind, &text);
        }
        self.open = Some(kind);
    }

    /// Append one whole block. Newlines inside `text` stay: a preview
    /// or a pasted prompt is one block of several lines.
    pub fn push_line(&mut self, kind: Kind, text: &str) {
        self.open = None;
        let mut clean = String::with_capacity(text.len());
        push_sanitized(&mut clean, text);
        self.push_blocks(kind, &clean);
    }

    /// One status block for a started tool call, updatable by id.
    pub fn tool_start(&mut self, id: &str, name: &str) {
        // The name is model-controlled: one line, inside the cap, as
        // every tool-line write.
        self.push_line(Kind::Tool, &tool_text(&format!("· {name} …")));
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

    /// Start a frame on a viewport of `width` by `height`. A width
    /// change returns to follow mode. In browse mode, the walk lays
    /// out the blocks from the end up to the top of the viewport —
    /// the blocks the frame shows or counts, no others — and adds the
    /// rows they gained or lost to the scroll, so the row the user
    /// reads stays where it is. A block above the viewport changes
    /// nothing: its rows sit above the number. A block never laid out
    /// before, and older than the last frame, sat above the viewport
    /// too, so its rows count as old.
    pub(crate) fn begin_frame(&mut self, width: u16, height: u16) {
        if width != self.width {
            self.width = width;
            self.scroll = 0;
        }
        let end = self.dropped + self.blocks.len();
        if self.scroll > 0 {
            let top = self.scroll.saturating_add(usize::from(height));
            let mut walked = 0usize;
            let mut delta = 0isize;
            for (offset, block) in self.blocks.iter_mut().enumerate().rev() {
                if walked >= top {
                    break;
                }
                let new = self.dropped + offset >= self.frame_end;
                let laid = block.laid();
                let before = block.rows();
                let change = block.sync(width, self.markdown);
                if new || laid {
                    delta += change;
                }
                walked += match (new, laid) {
                    (true, _) => 0,
                    (false, true) => before,
                    (false, false) => block.rows(),
                };
            }
            self.scroll = self.scroll.saturating_add_signed(delta);
        }
        self.frame_end = end;
    }

    /// Append sanitized `text` as blocks of at most the byte cap. Each
    /// block gets an allocation of its own size, so a large paste
    /// costs its length once.
    fn push_blocks(&mut self, kind: Kind, text: &str) {
        let mut rest = text;
        loop {
            let (head, tail) = rest.split_at(cap_boundary(rest));
            self.blocks.push_back(Block::new(kind, head.to_string()));
            while self.blocks.len() > MAX_BLOCKS {
                self.blocks.pop_front();
                self.dropped += 1;
            }
            if tail.is_empty() {
                break;
            }
            rest = tail;
        }
    }
}

/// A tool status line: sanitized, on one line, inside the cap.
fn tool_text(raw: &str) -> String {
    let mut text = String::new();
    push_sanitized(&mut text, raw);
    let mut text = text.replace('\n', " ");
    text.truncate(cap_boundary(&text));
    text
}

/// The byte cap, or the last char boundary inside it.
fn cap_boundary(text: &str) -> usize {
    if text.len() <= MAX_BLOCK_BYTES {
        return text.len();
    }
    let mut cut = MAX_BLOCK_BYTES;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    cut
}

/// When `text` exceeds the cap, cut it at the last char boundary
/// inside the cap and return the tail.
fn split_at_cap(text: &mut String) -> Option<String> {
    let cut = cap_boundary(text);
    (cut < text.len()).then(|| text.split_off(cut))
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

    /// A transcript that shows every kind as plain text: the layout
    /// mechanics under test hold for both modes, and plain rows are
    /// the ones that read as their text.
    fn plain() -> Transcript {
        Transcript::new(false)
    }

    /// Every row of every block, laid out at `width`.
    fn rows(t: &mut Transcript, width: u16) -> Vec<String> {
        let markdown = t.markdown();
        let mut out = Vec::new();
        for block in t.blocks_mut() {
            block.sync(width, markdown);
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

    /// A frame as the renderer runs it: the anchor walk, then every
    /// block laid out.
    fn frame(t: &mut Transcript, width: u16, height: u16) {
        t.begin_frame(width, height);
        rows(t, width);
    }

    #[test]
    fn deltas_of_one_kind_extend_the_open_block() {
        let mut t = plain();
        t.push_delta(Kind::Text, "hel");
        t.push_delta(Kind::Text, "lo\nworld");
        assert_eq!(blocks(&t), vec![(Kind::Text, "hello\nworld")]);
        assert_eq!(rows(&mut t, 80), vec!["hello", "world"]);
    }

    #[test]
    fn a_kind_switch_opens_a_new_block() {
        let mut t = plain();
        t.push_delta(Kind::Reasoning, "thinking");
        t.push_delta(Kind::Text, "answer");
        assert_eq!(
            blocks(&t),
            vec![(Kind::Reasoning, "thinking"), (Kind::Text, "answer")]
        );
    }

    #[test]
    fn a_trailing_newline_opens_no_empty_row() {
        let mut t = plain();
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
        let mut t = plain();
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
        let mut t = plain();
        t.push_line(Kind::Notice, "one\ntwo");
        assert_eq!(t.blocks().len(), 1);
        assert_eq!(rows(&mut t, 80), vec!["one", "two"]);
    }

    #[test]
    fn an_incremental_layout_matches_a_fresh_one() {
        // Deltas of every shape — mid-word, at a space, with one or
        // several newlines, wide chars, one line that wraps many
        // times — at several widths: the rows after each delta must
        // equal the rows of the whole text.
        let text = "the quick brown fox\njumps over the lazy dog 日本語 text\n\nand a blank line\n\
                    then a much longer line that wraps more than once at every width tried, \
                    and keeps going without a newline so the layout must resume from a row, \
                    not a line, 日本語の文字 included\n";
        for width in [5u16, 12, 33, 80] {
            let mut t = plain();
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
    fn a_sync_lays_out_from_the_last_row_not_the_last_line() {
        let mut t = plain();
        t.push_delta(Kind::Text, "one two three four five six");
        rows(&mut t, 8);
        let block = t.blocks_mut().next_back().unwrap();
        assert_eq!(block.rows(), 4);
        // Every row but the last stays; the sync resumes at the last.
        assert_eq!(block.layout.stable, 3);
        assert_eq!(block.layout.tail, block.layout.rows[3] as usize);
    }

    #[test]
    fn a_width_change_lays_the_block_out_again() {
        let mut t = plain();
        t.push_delta(Kind::Text, "one two three");
        assert_eq!(rows(&mut t, 80), vec!["one two three"]);
        assert_eq!(rows(&mut t, 8), vec!["one two ", "three"]);
        t.push_delta(Kind::Text, " four");
        assert_eq!(rows(&mut t, 8), vec!["one two ", "three ", "four"]);
    }

    #[test]
    fn sync_reports_the_rows_a_change_added_or_removed() {
        let mut t = plain();
        t.push_delta(Kind::Text, "one");
        let block = t.blocks_mut().next_back().unwrap();
        assert_eq!(block.sync(8, false), 1, "a first layout counts every row");
        assert!(block.clean(8));
        assert_eq!(block.sync(8, false), 0);
        // A new width lays out again and counts nothing.
        assert_eq!(block.sync(2, false), 0);
        assert_eq!(block.rows(), 2);
        assert_eq!(block.sync(8, false), 0);
        t.push_delta(Kind::Text, " two three\nfour");
        let block = t.blocks_mut().next_back().unwrap();
        assert!(!block.clean(8));
        assert_eq!(block.sync(8, false), 2);
        assert_eq!(block.sync(8, false), 0);
        // A rewrite to fewer rows reports the loss.
        block.set_text("ok".into());
        assert_eq!(block.sync(8, false), -2);
    }

    #[test]
    fn tool_blocks_update_in_place() {
        let mut t = plain();
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
        let mut t = plain();
        t.tool_start("call_1", "edit");
        t.tool_end("call_1", "stale tag", true);
        assert_eq!(blocks(&t), vec![(Kind::Tool, "✗ stale tag")]);
    }

    #[test]
    fn an_unknown_call_id_appends_instead() {
        let mut t = plain();
        t.tool_end("ghost", "late result", false);
        assert_eq!(blocks(&t), vec![(Kind::Tool, "· late result")]);
    }

    #[test]
    fn a_tool_update_lays_its_rows_out_again() {
        let mut t = plain();
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
        let mut t = plain();
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
        let mut t = plain();
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
        let mut t = plain();
        t.push_line(Kind::Prompt, &"é".repeat(MAX_BLOCK_BYTES));
        assert!(t.blocks().len() >= 2);
        let joined: String = t.blocks().map(|b| b.text()).collect();
        assert_eq!(joined, "é".repeat(MAX_BLOCK_BYTES));
        for block in t.blocks() {
            assert!(block.text().len() <= MAX_BLOCK_BYTES);
        }

        // One giant delta onto an open block.
        let mut t = plain();
        t.push_delta(Kind::Text, "start");
        t.push_delta(Kind::Text, &"y".repeat(2 * MAX_BLOCK_BYTES));
        for block in t.blocks() {
            assert!(block.text().len() <= MAX_BLOCK_BYTES);
        }
        let joined: String = t.blocks().map(|b| b.text()).collect();
        assert_eq!(joined.len(), "start".len() + 2 * MAX_BLOCK_BYTES);
    }

    #[test]
    fn a_large_paste_costs_its_length_once() {
        // Each block owns an allocation of its own size: no block
        // keeps the buffer of the whole paste.
        let paste = "a line of pasted text\n".repeat(50_000);
        let mut t = plain();
        t.push_line(Kind::Prompt, &paste);
        let mut t2 = plain();
        t2.push_delta(Kind::Text, "start");
        t2.push_delta(Kind::Text, &paste);
        for block in t.blocks().chain(t2.blocks()) {
            assert!(
                block.text.capacity() <= MAX_BLOCK_BYTES + "start".len(),
                "capacity {}",
                block.text.capacity()
            );
        }
        assert_eq!(t.blocks().len(), paste.len().div_ceil(MAX_BLOCK_BYTES));
    }

    #[test]
    fn close_marks_unanswered_calls_as_interrupted_and_forgets_them() {
        let mut t = plain();
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
    fn tool_lines_sanitize_a_hostile_tool_name_on_every_path() {
        let mut t = plain();
        t.tool_start("call_1", "ba\x1b[2Jsh\nSECOND");
        assert_eq!(rows(&mut t, 80), vec!["· ba[2Jsh SECOND …"]);
        t.close();
        let last = t.blocks().last().unwrap();
        assert_eq!(last.text(), "✗ ba[2Jsh SECOND: interrupted");
        // A name past the cap stays one block, so the update finds it.
        let mut t = plain();
        t.tool_start("call_2", &"n".repeat(2 * MAX_BLOCK_BYTES));
        t.tool_end("call_2", "ok", false);
        assert_eq!(blocks(&t), vec![(Kind::Tool, "· ok")]);
    }

    #[test]
    fn close_leaves_answered_calls_alone() {
        let mut t = plain();
        t.tool_start("call_1", "bash");
        t.tool_end("call_1", "bash: ok", false);
        t.close();
        assert_eq!(blocks(&t), vec![(Kind::Tool, "· bash: ok")]);
    }

    #[test]
    fn control_characters_never_reach_a_block() {
        let mut t = plain();
        t.push_delta(Kind::Text, "a\x1b[31mred\tdone\r");
        assert_eq!(blocks(&t), vec![(Kind::Text, "a[31mred    done")]);
        let mut t = plain();
        t.push_line(Kind::Notice, "a\x1b[31mred\tdone\r");
        assert_eq!(blocks(&t), vec![(Kind::Notice, "a[31mred    done")]);
    }

    #[test]
    fn a_browsing_view_stays_put_when_rows_arrive() {
        let mut t = plain();
        for i in 0..10 {
            t.push_line(Kind::Text, &format!("line {i}"));
        }
        frame(&mut t, 40, 4);
        t.scroll_up(4);
        // Rows that arrive below the viewport deepen the scroll by
        // their count, so the rows on screen do not move.
        t.push_line(Kind::Text, "one more");
        t.push_delta(Kind::Text, "and a stream\nof two rows");
        frame(&mut t, 40, 4);
        assert_eq!(t.scroll(), 7);
        // A delta that adds no row changes nothing.
        t.push_delta(Kind::Text, " still");
        frame(&mut t, 40, 4);
        assert_eq!(t.scroll(), 7);
        // A width change returns to follow mode.
        frame(&mut t, 20, 4);
        assert_eq!(t.scroll(), 0);
    }

    #[test]
    fn a_rewritten_block_below_the_viewport_top_moves_the_anchor() {
        // A tool block behind a clean notice grows: the walk reaches
        // it, and the rows it gained keep the view in place.
        let mut t = plain();
        for i in 0..20 {
            t.push_line(Kind::Text, &format!("line {i:02}"));
        }
        t.tool_start("c1", "bash");
        t.push_line(Kind::Notice, "note");
        frame(&mut t, 40, 10);
        t.scroll_up(2);
        t.tool_end(
            "c1",
            "a much longer summary that certainly wraps across three rows of a forty column pane",
            false,
        );
        frame(&mut t, 40, 10);
        assert_eq!(t.scroll(), 4);

        // A block that shrinks reports the loss.
        let mut t = plain();
        for i in 0..20 {
            t.push_line(Kind::Text, &format!("line {i:02}"));
        }
        t.tool_start(
            "c1",
            "a tool name so long that its pending line wraps to three rows in a narrow pane",
        );
        t.push_delta(Kind::Text, "streaming");
        frame(&mut t, 40, 10);
        t.scroll_up(3);
        t.tool_end("c1", "ok", false);
        t.push_delta(Kind::Text, " more");
        frame(&mut t, 40, 10);
        assert_eq!(t.scroll(), 1);
    }

    #[test]
    fn blocks_above_the_viewport_leave_the_anchor_alone() {
        // A tool call at the top, then a long history: the first
        // frame lays out only what it shows.
        let mut t = plain();
        t.tool_start("c1", "bash");
        for i in 0..30 {
            t.push_line(Kind::Text, &format!("line {i:02}"));
        }
        t.begin_frame(40, 4);
        for block in t.blocks_mut().rev().take(4) {
            block.sync(40, false);
        }
        // Scrolling into blocks the frame never laid out counts
        // nothing: they sat above the viewport all along.
        t.scroll_up(20);
        t.begin_frame(40, 4);
        assert_eq!(t.scroll(), 20);
        // A rewrite above the viewport changes nothing; an arrival
        // below it counts.
        t.tool_end(
            "c1",
            "a long summary that wraps to more rows than the pending line had, well above",
            false,
        );
        t.push_line(Kind::Text, "arrived");
        t.begin_frame(40, 4);
        assert_eq!(t.scroll(), 21);
    }

    /// Every row of every block as `(face, text)` spans.
    fn spans(t: &mut Transcript, width: u16) -> Vec<Vec<(Face, String)>> {
        let markdown = t.markdown();
        let mut out = Vec::new();
        for block in t.blocks_mut() {
            block.sync(width, markdown);
            for index in 0..block.rows() {
                let mut row = Vec::new();
                block.spans(index, |face, text| row.push((face, text.to_string())));
                out.push(row);
            }
        }
        out
    }

    #[test]
    fn markdown_renders_model_text_and_leaves_the_other_kinds_plain() {
        let mut t = Transcript::new(true);
        t.push_delta(Kind::Text, "# Title\n\n- one\n- two");
        t.push_line(Kind::Notice, "# not a heading");
        t.push_delta(Kind::Reasoning, "- not a list");
        assert_eq!(
            rows(&mut t, 80),
            vec![
                "# Title",
                "",
                "• one",
                "• two",
                "# not a heading",
                "- not a list"
            ]
        );
        let spans = spans(&mut t, 80);
        assert_eq!(
            spans[0],
            vec![
                (Face::Marker, "# ".to_string()),
                (Face::Heading, "Title".to_string())
            ]
        );
        assert_eq!(spans[4], vec![(Face::Plain, "# not a heading".to_string())]);
        assert_eq!(spans[5], vec![(Face::Plain, "- not a list".to_string())]);
        let blocks: Vec<_> = t.blocks().collect();
        assert!(!blocks[0].plain());
        assert!(blocks[1].plain());
        // Off, the reply shows its source.
        let mut t = plain();
        t.push_delta(Kind::Text, "# Title\n\n- one");
        assert_eq!(rows(&mut t, 80), vec!["# Title", "", "- one"]);
    }

    #[test]
    fn a_streamed_markdown_block_matches_a_fresh_render_after_every_delta() {
        // A delta can close a fence or an emphasis that opened rows
        // earlier, so the block renders again whole; the rows after
        // each delta must equal the rows of the whole text.
        let text = "Intro with *emphasis\nacross* lines.\n\n```rust\nfn a() {}\n```\n\n\
                    | a | b |\n|---|---|\n| 1 | 2 |\n\n> quote\n\n- item **bold**\n";
        for width in [7u16, 20, 80] {
            let mut t = Transcript::new(true);
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
                step = step % 5 + 1;
                let mut fresh = Transcript::new(true);
                fresh.push_delta(Kind::Text, &whole);
                assert_eq!(
                    spans(&mut t, width),
                    spans(&mut fresh, width),
                    "width {width} after {cut} bytes"
                );
            }
        }
    }

    #[test]
    fn a_render_past_its_cap_shows_the_source_as_plain_text() {
        // Rules render at the full width; enough of them outgrow the
        // rendered-row cap, and the block falls back to its source.
        let source = "---\n\n".repeat(MAX_BLOCK_BYTES / 5);
        let mut t = Transcript::new(true);
        t.push_delta(Kind::Text, &source);
        let rows = rows(&mut t, 200);
        assert_eq!(rows[0], "---");
        assert_eq!(rows[1], "");
        let block = t.blocks().next().unwrap();
        assert!(block.plain());
        assert!(block.layout.rendered.is_none());
    }

    #[test]
    fn a_markdown_block_reports_its_row_change_like_a_plain_one() {
        let mut t = Transcript::new(true);
        t.push_delta(Kind::Text, "one");
        let block = t.blocks_mut().next_back().unwrap();
        assert_eq!(block.sync(8, true), 1);
        assert_eq!(block.sync(8, true), 0);
        t.push_delta(Kind::Text, "\n\ntwo\n\nthree");
        let block = t.blocks_mut().next_back().unwrap();
        assert_eq!(block.sync(8, true), 4);
        assert_eq!(block.sync(2, true), 0);
        assert_eq!(block.rows(), 9);
    }

    #[test]
    fn follow_mode_ignores_arrivals_and_end_returns_to_it() {
        let mut t = plain();
        t.push_line(Kind::Text, "first");
        frame(&mut t, 40, 4);
        t.push_line(Kind::Text, "second");
        frame(&mut t, 40, 4);
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
