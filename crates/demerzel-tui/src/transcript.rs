//! The transcript model: what one session has shown so far, as blocks.
//!
//! A block is one unit of the session: a prompt echo, one streamed
//! model reply segment, one reasoning segment, a tool call, a notice,
//! or an error. A reasoning segment and a tool call hold a thought or
//! a card and write their text on demand, at the layout after a
//! change. Each block owns a layout cache: its styled rows at
//! the last width. A plain block keeps its rows as byte offsets into
//! its text. A model reply renders as markdown into a buffer of its
//! own, with the rows and the face changes over that buffer. The
//! renderer walks blocks from the bottom until the viewport is full,
//! so a long session pays for its window, not its history. A streamed
//! delta touches only the open block. A plain block's layout catches
//! up from its last row, not from its start: a wrap break depends
//! only on the text before it, so the rows already laid out stay
//! valid. A markdown block renders again whole: a delta can close a
//! fence or an emphasis that opened rows earlier. A block cut at the
//! byte cap inside a fence hands the fence opener to the block that
//! continues it, so the code stays code and the frame does not
//! repeat at the seam.
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
use std::ops::Range;
use std::time::Instant;

use crate::PROMPT;
use crate::cards::{Card, INDENT, Outcome, kept_output, push_sanitized, write_usage};
use crate::markdown::{self, Face, Seam, Target};
use crate::replay::{Replay, ReplayEntry, ReplayResult};
use crate::select::{self, Copied, Point, Selection};
use crate::thinking::{SUMMARY_RESERVE, Thinking, Thought};
use crate::wrap::{display_width, row_starts};

/// Blocks the transcript keeps. Old blocks beyond the cap drop.
const MAX_BLOCKS: usize = 2_000;

/// Byte cap for one block. A streamed block that reaches it closes
/// and the stream continues in a new block; a pushed block splits.
pub const MAX_BLOCK_BYTES: usize = 32 * 1024;

/// Byte cap for the rendered rows of one markdown block. Prefixes,
/// frames, and table padding grow the rows past the source; a block
/// whose render reaches the cap shows as plain text instead.
const MAX_RENDER_BYTES: usize = 2 * MAX_BLOCK_BYTES;

/// Bytes of summary a thinking block keeps: the block cap less the
/// room its header and cut mark take.
const SUMMARY_BYTES: usize = MAX_BLOCK_BYTES - SUMMARY_RESERVE;

/// What a block is. The frame maps a kind to an attribute and a
/// theme role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The user's submitted line, echoed with the prompt.
    Prompt,
    /// One model reply segment, streamed.
    Text,
    /// One reasoning summary segment, streamed, dim.
    Reasoning,
    /// One tool call: its card, updated in place.
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
    /// The markdown render reached its cap at this width; the block
    /// lays out as plain text until the width changes or the text
    /// shrinks below `fallback_len`. A text that grew since the
    /// failed render fails again, whether it streamed or a thought
    /// wrote it.
    fallback: bool,
    /// The text length the failed render ran on.
    fallback_len: usize,
}

/// One unit of the transcript with its layout cache.
#[derive(Debug)]
pub struct Block {
    pub kind: Kind,
    text: String,
    /// Lay out as markdown: a model reply in a transcript that
    /// renders. Fixed at birth, so a block is always laid out one
    /// way and its rows always index one buffer.
    markdown: bool,
    /// The fence opener the block before was cut inside, rendered in
    /// front of this block's text. Not part of the text: the plain
    /// view shows the source as it came.
    carry: Option<String>,
    /// The block was cut at the byte cap inside a fence that the next
    /// block continues.
    cut: bool,
    body: Body,
    layout: Layout,
}

/// What a block's text comes from. A card or a thought renders into
/// the text on demand: the block marks itself at a change and writes
/// the text at its next sync. The body follows the kind: a `Tool`
/// block holds a card, a `Reasoning` block a thought, every other
/// kind its plain text.
#[derive(Debug)]
enum Body {
    /// The text as it arrived.
    Plain,
    /// A tool call: the text is the card's render.
    Card(Box<Card>),
    /// A reasoning segment: the text is the thought's render, one
    /// line closed or the summary open.
    Thought(Box<Thought>),
}

impl Block {
    fn new(kind: Kind, text: String, markdown: bool, carry: Option<String>) -> Self {
        Self {
            kind,
            text,
            markdown,
            carry,
            cut: false,
            body: Body::Plain,
            layout: Layout::default(),
        }
    }

    /// A tool card. The text renders at the first sync.
    fn card(card: Card) -> Self {
        Self {
            body: Body::Card(Box::new(card)),
            ..Self::new(Kind::Tool, String::new(), false, None)
        }
    }

    fn card_mut(&mut self) -> Option<&mut Card> {
        match &mut self.body {
            Body::Card(card) => Some(card),
            _ => None,
        }
    }

    /// A thinking block. Its summary renders as markdown when the
    /// transcript does: a reasoning summary carries the same marks
    /// as a reply.
    fn thought(thought: Thought, markdown: bool) -> Self {
        Self {
            body: Body::Thought(Box::new(thought)),
            ..Self::new(Kind::Reasoning, String::new(), markdown, None)
        }
    }

    fn thought_mut(&mut self) -> Option<&mut Thought> {
        match &mut self.body {
            Body::Thought(thought) => Some(thought),
            _ => None,
        }
    }

    /// Close a block the byte cap cut: when the cut landed inside a
    /// fence, mark it and return the opener for the next block.
    fn cut_at_cap(&mut self) -> Option<String> {
        if !self.markdown {
            return None;
        }
        let carry = markdown::open_fence(self.carry.as_deref(), &self.text);
        self.cut = carry.is_some();
        carry
    }

    #[cfg(test)]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Bytes the text and the layout buffers hold, at their capacity.
    #[cfg(test)]
    pub(crate) fn retained(&self) -> usize {
        let layout = &self.layout;
        self.text.capacity()
            + layout.rendered.as_ref().map_or(0, String::capacity)
            + layout.rows.capacity() * std::mem::size_of::<u32>()
            + layout.faces.capacity() * std::mem::size_of::<(u32, Face)>()
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

    /// Bring the rows up to date at `width`. Returns the change in the
    /// row count since the last sync at this width: every row for a
    /// block laid out for the first time, zero for a layout at a new
    /// width, where a row count means nothing.
    pub(crate) fn sync(&mut self, width: u16) -> isize {
        debug_assert!(width > 0, "zero width is the never-laid-out mark");
        let rendered = self.render_body();
        let new_width = self.layout.width != width;
        let delta = self.sync_rows(width);
        // A body wrote new text, or every row was laid out again at
        // another width: the buffers may hold far more than they use.
        if rendered || new_width {
            self.trim_layout();
        }
        delta
    }

    /// Write a stale card or thought into the text. Returns whether
    /// it did: the text is new, so the rows and the render verdict
    /// from before no longer hold.
    fn render_body(&mut self) -> bool {
        match &mut self.body {
            Body::Card(card) if card.stale() => card.render(&mut self.text, MAX_BLOCK_BYTES),
            Body::Thought(thought) if thought.stale() => thought.render(&mut self.text),
            _ => return false,
        }
        self.text.truncate(cap_boundary(&self.text));
        // A collapse after an expand must give the memory back: the
        // block cap bounds the text, not what it once held.
        if self.text.capacity() > 2 * self.text.len().max(256) {
            self.text.shrink_to_fit();
        }
        // A text that shrank below the length the render failed on
        // gets a fresh verdict; one that grew fails again, and must
        // not run a render past the cap every frame.
        if self.text.len() < self.layout.fallback_len {
            self.layout.fallback = false;
        }
        self.relayout_whole();
        true
    }

    /// The layout buffers at their length, not at the peak they
    /// once held: a collapsed block keeps one line, not the rows,
    /// faces, and rendered text of its open form.
    fn trim_layout(&mut self) {
        let layout = &mut self.layout;
        if let Some(rendered) = &mut layout.rendered
            && rendered.capacity() > 2 * rendered.len().max(256)
        {
            rendered.shrink_to_fit();
        }
        if layout.rows.capacity() > 2 * layout.rows.len().max(64) {
            layout.rows.shrink_to_fit();
        }
        if layout.faces.capacity() > 2 * layout.faces.len().max(64) {
            layout.faces.shrink_to_fit();
        }
    }

    /// Bring the rows up to date at `width`; see [`Block::sync`].
    fn sync_rows(&mut self, width: u16) -> isize {
        if self.layout.width != width {
            let first = !self.laid();
            self.layout.width = width;
            self.layout.fallback = false;
            self.lay_out_whole();
            return if first { row_delta(0, self.rows()) } else { 0 };
        }
        if !self.layout.dirty {
            return 0;
        }
        let before = self.rows();
        if self.markdown && !self.layout.fallback {
            self.lay_out_whole();
        } else {
            self.layout.rows.truncate(self.layout.stable);
            let tail = self.layout.tail;
            self.lay_out_from(tail);
            self.card_faces();
            self.layout.dirty = false;
        }
        row_delta(before, self.rows())
    }

    /// The faces of a card over its plain rows. Any other plain
    /// block has no faces.
    fn card_faces(&mut self) {
        self.layout.faces.clear();
        if let Body::Card(card) = &self.body {
            card.faces(self.text.len(), &mut self.layout.faces);
        }
    }

    /// Lay every row out again: as markdown for a block that renders
    /// and stays inside the render cap, else as plain text.
    fn lay_out_whole(&mut self) {
        let layout = &mut self.layout;
        layout.rows.clear();
        layout.faces.clear();
        if self.markdown && !layout.fallback {
            let text = layout.rendered.get_or_insert_with(String::new);
            text.clear();
            let target = Target {
                text,
                rows: &mut layout.rows,
                faces: &mut layout.faces,
            };
            let seam = Seam {
                carried: self.carry.as_ref().map_or(0, String::len),
                cut: self.cut,
            };
            // A carried opener joins the source for the parse only.
            let joined;
            let source = match &self.carry {
                Some(carry) => {
                    joined = format!("{carry}{}", self.text);
                    joined.as_str()
                }
                None => self.text.as_str(),
            };
            if markdown::render(source, layout.width, MAX_RENDER_BYTES, seam, target) {
                layout.dirty = false;
                return;
            }
            layout.fallback = true;
            layout.fallback_len = self.text.len();
            layout.rows.clear();
            layout.faces.clear();
        }
        layout.rendered = None;
        self.lay_out_from(0);
        self.card_faces();
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

    /// Whether row `index` starts a line of the text, rather than
    /// continuing a wrapped one.
    fn line_start(&self, index: usize) -> bool {
        let start = self.layout.rows[index] as usize;
        start == 0 || self.buffer().as_bytes()[start - 1] == b'\n'
    }

    /// Whether row `index` continues a wrapped line of a plain
    /// layout. A markdown render keeps no line ends between its
    /// rows, so its rows never count as wrapped here.
    pub(crate) fn wrapped(&self, index: usize) -> bool {
        !self.rendered() && !self.line_start(index)
    }

    /// Whether the line of row `index` runs on into the next row: a
    /// row of frame alone then holds nothing of its own.
    pub(crate) fn continues(&self, index: usize) -> bool {
        index + 1 < self.rows() && self.wrapped(index + 1)
    }

    /// Whether the block changed since the last frame laid it out:
    /// text arrived, or a card or a thought waits for its render.
    /// The rows on the screen no longer describe it.
    pub(crate) fn pending(&self) -> bool {
        self.layout.dirty
            || match &self.body {
                Body::Plain => false,
                Body::Card(card) => card.stale(),
                Body::Thought(thought) => thought.stale(),
            }
    }

    /// The columns the frame takes at the start of row `index`,
    /// which a copy leaves out: a card's indent on each of its
    /// lines, the prompt on an echo, wherever its bytes fall when a
    /// narrow pane wraps it.
    pub(crate) fn frame_columns(&self, index: usize) -> usize {
        match self.kind {
            Kind::Tool if self.line_start(index) => display_width(INDENT),
            Kind::Prompt if self.buffer().starts_with(crate::PROMPT) => {
                let (start, row) = self.row_at(index);
                let end = (start + row.len()).min(crate::PROMPT.len());
                if start < end {
                    display_width(&self.buffer()[start..end])
                } else {
                    0
                }
            }
            _ => 0,
        }
    }

    /// Whether every row is one span in the block's own style.
    pub(crate) fn plain(&self) -> bool {
        self.layout.faces.is_empty()
    }

    /// Whether the rows come from a markdown render.
    pub(crate) fn rendered(&self) -> bool {
        self.layout.rendered.is_some()
    }

    /// One row at the synced width as its spans, in order.
    pub(crate) fn spans<'b>(&'b self, index: usize, mut span: impl FnMut(Face, &'b str)) {
        let (from, row) = self.row_at(index);
        let faces = &self.layout.faces;
        let mut next = faces.partition_point(|&(at, _)| at as usize <= from);
        let mut face = if next == 0 {
            Face::PLAIN
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
    /// Tool call id to the absolute index of its card, kept so an
    /// unanswered call can close as interrupted.
    tools: HashMap<String, usize>,
    /// The absolute index of the card between its dispatch and its
    /// result: the one an approval question is about.
    running: Option<usize>,
    /// Cards show their kept output whole. New cards follow.
    expanded: bool,
    /// How a thinking block starts.
    thinking: Thinking,
    /// The absolute index of the thinking block whose segment still
    /// streams: the one a delta extends and an end settles, whatever
    /// lands behind it.
    live: Option<usize>,
    /// Rows between the viewport bottom and the transcript end. The
    /// renderer clamps it against the rows that exist.
    scroll: usize,
    /// Viewport height at the last frame. A height change moves the
    /// scroll by the difference, so the top row holds.
    height: u16,
    /// The width of the last frame; zero before the first.
    width: u16,
    /// The absolute block count at the last frame: blocks past it
    /// arrived since.
    frame_end: usize,
    /// The drag selection: the press and the pointer. Cleared when
    /// a block under it changes, when the width changes, and when
    /// its first block drops off the cap.
    selection: Option<Selection>,
    /// The block and row on each viewport row of the last frame,
    /// top down, so a mouse position maps to a point.
    shown: Vec<(usize, usize)>,
    /// The viewport cells something covers — the completion popup,
    /// the browse indicator — as `(rows, columns)`; a position there
    /// maps to no row.
    covered: Vec<(Range<usize>, Range<usize>)>,
    /// The cells of the browse indicator on the last frame, as
    /// `(row, columns)`: a press there is the key it names.
    indicator: Option<(usize, Range<usize>)>,
}

impl Transcript {
    pub fn new(markdown: bool) -> Self {
        Self {
            markdown,
            blocks: VecDeque::new(),
            dropped: 0,
            open: None,
            tools: HashMap::new(),
            running: None,
            expanded: false,
            thinking: Thinking::default(),
            live: None,
            scroll: 0,
            height: 0,
            width: 0,
            frame_end: 0,
            selection: None,
            shown: Vec::new(),
            covered: Vec::new(),
            indicator: None,
        }
    }

    pub fn blocks(&self) -> impl DoubleEndedIterator<Item = &Block> + ExactSizeIterator {
        self.blocks.iter()
    }

    /// Blocks dropped from the front: the absolute index of the
    /// first block held.
    pub(crate) fn dropped(&self) -> usize {
        self.dropped
    }

    pub(crate) fn blocks_mut(
        &mut self,
    ) -> impl DoubleEndedIterator<Item = &mut Block> + ExactSizeIterator {
        self.blocks.iter_mut()
    }

    /// Append streamed text of `kind`. Consecutive deltas of one kind
    /// extend the open block; a kind switch or a pushed block closes
    /// it. A block that reaches the byte cap closes too, and the
    /// stream continues in a new one. Reasoning text has its own
    /// path, [`Transcript::reasoning_delta`]: a reasoning block is
    /// always a thought.
    pub fn push_delta(&mut self, kind: Kind, delta: &str) {
        debug_assert!(
            kind != Kind::Reasoning,
            "reasoning streams through reasoning_delta"
        );
        if delta.is_empty() {
            return;
        }
        if self.open == Some(kind)
            && let Some(last) = self.blocks.back_mut()
        {
            let overflow = last.append(delta);
            self.touch(self.dropped + self.blocks.len() - 1);
            if let Some(overflow) = overflow {
                let carry = self.blocks.back_mut().and_then(Block::cut_at_cap);
                self.push_blocks(kind, &overflow, carry);
            }
        } else {
            let mut text = String::with_capacity(delta.len());
            push_sanitized(&mut text, delta);
            self.push_blocks(kind, &text, None);
        }
        self.open = Some(kind);
    }

    /// Append one whole block. Newlines inside `text` stay: a preview
    /// or a pasted prompt is one block of several lines.
    pub fn push_line(&mut self, kind: Kind, text: &str) {
        self.open = None;
        let mut clean = String::with_capacity(text.len());
        push_sanitized(&mut clean, text);
        self.push_blocks(kind, &clean, None);
    }

    /// How a thinking block starts, from the config key.
    pub fn set_thinking(&mut self, thinking: Thinking) {
        self.thinking = thinking;
    }

    /// Append reasoning summary text. The deltas of one segment
    /// extend its block, whatever lands behind it; the first delta
    /// opens the block with its clock at `at`. Whitespace alone opens
    /// nothing: an adapter's paragraph break between segments is not
    /// content. A hidden setting drops the text.
    pub fn reasoning_delta(&mut self, delta: &str, at: Instant) {
        if self.thinking == Thinking::Hidden {
            return;
        }
        if let Some(index) = self.live
            && let Some(thought) = self.thought_at(index)
        {
            thought.push(delta, SUMMARY_BYTES);
            self.touch(index);
            return;
        }
        if delta.trim_start().is_empty() {
            return;
        }
        let mut thought = Thought::new(self.thinking == Thinking::Open, at);
        thought.push(delta, SUMMARY_BYTES);
        self.push_block(Block::thought(thought, self.markdown_for(Kind::Reasoning)));
        self.live = Some(self.dropped + self.blocks.len() - 1);
        // The pushed block closes the open stream, as any pushed
        // block does: a text delta must not join the thought's text.
        self.open = None;
    }

    /// The reasoning segment ended at `at`: its block shows the
    /// duration. A segment with no block — no summary, or a hidden
    /// setting — changes nothing, and a text stream still open
    /// stays open.
    pub fn reasoning_end(&mut self, at: Instant) {
        if let Some(index) = self.live.take()
            && let Some(thought) = self.thought_at(index)
        {
            thought.end(at);
            self.touch(index);
        }
    }

    /// Open the newest thinking block, or close it again. Nothing
    /// happens when the transcript holds none.
    pub fn toggle_thinking(&mut self) {
        let newest = self
            .blocks
            .iter_mut()
            .enumerate()
            .rev()
            .find_map(|(offset, block)| block.thought_mut().map(|thought| (offset, thought)));
        if let Some((offset, thought)) = newest {
            thought.set_open(!thought.open());
            self.touch(self.dropped + offset);
        }
    }

    /// One card for a started tool call, updatable by id.
    pub fn tool_start(&mut self, id: &str, name: &str) {
        self.open = None;
        self.push_block(Block::card(Card::new(name, self.expanded)));
        let index = self.dropped + self.blocks.len() - 1;
        self.tools.insert(id.to_string(), index);
    }

    /// The call dispatches at `at`: the card's duration counts from
    /// here, and an approval question that follows is about it.
    pub fn tool_run(&mut self, id: &str, at: Instant) {
        self.running = self.tools.get(id).copied();
        if let Some(card) = self.card_mut(id) {
            card.run(at);
        }
    }

    /// The driver's preview of a call goes onto its card. A call
    /// without a card — one that fell off the cap — shows it as a
    /// notice, so nothing runs unseen.
    pub fn tool_preview(&mut self, id: &str, preview: &str) {
        match self.tools.get(id).copied() {
            Some(index) if self.card_at(index).is_some() => {
                if let Some(card) = self.card_at(index) {
                    card.set_preview(preview);
                }
                self.touch(index);
            }
            _ => self.push_line(Kind::Notice, preview),
        }
    }

    /// The approval question for `tool` is up: the running call's
    /// card shows it. Returns false when no card can — no call runs,
    /// its card is gone, or the names differ.
    pub fn tool_pending(&mut self, tool: &str, preview: Option<&str>) -> bool {
        let Some(index) = self.running else {
            return false;
        };
        match self.card_at(index) {
            Some(card) if card.name() == tool => {
                card.pending(preview);
                self.touch(index);
                true
            }
            _ => false,
        }
    }

    /// The question about the running call is answered at `at`.
    pub fn tool_answered(&mut self, declined: bool, at: Instant) {
        if let Some(index) = self.running
            && let Some(card) = self.card_at(index)
        {
            card.answered(declined, at);
            self.touch(index);
        }
    }

    /// The call returned: its card ends in place. When the card
    /// already fell off the cap, a fresh one appends.
    pub fn tool_end(&mut self, id: &str, name: &str, outcome: Outcome<'_>) {
        let index = self.tools.remove(id);
        if index.is_some() && self.running == index {
            self.running = None;
        }
        match index.and_then(|index| self.card_at(index).map(|card| (index, card))) {
            Some((index, card)) => {
                card.end(outcome);
                self.touch(index);
            }
            None => {
                let mut card = Card::new(name, self.expanded);
                card.end(outcome);
                self.open = None;
                self.push_block(Block::card(card));
            }
        }
    }

    /// Show a resumed session's past turns as blocks: the replay on
    /// resume. Each entry lands finished, as the live turn left it:
    /// a prompt line, a text block, a thought, a card with its
    /// result, a usage line. A count of left-out messages shows
    /// first, as a dim line. Nothing streams and nothing lays out
    /// here: the blocks lay out at the next frame, for the rows it
    /// shows, so a long replay costs its window, not its length.
    pub fn replay(&mut self, replay: Replay) {
        // Nothing streams across a replay: a thought or a call still
        // open would keep an index the replay moves past. At the
        // quiet point the driver uses, this closes nothing.
        self.close();
        self.blocks.reserve(replay.entries.len().min(MAX_BLOCKS));
        let now = Instant::now();
        if replay.left_out > 0 {
            let noun = if replay.left_out == 1 {
                "message"
            } else {
                "messages"
            };
            self.push_line(
                Kind::Notice,
                &format!("… {} earlier {noun} left out", replay.left_out),
            );
        }
        for entry in replay.entries {
            match entry {
                ReplayEntry::Prompt(text) => {
                    self.push_line(Kind::Prompt, &format!("{PROMPT}{text}"));
                }
                // An empty text opens no block, as an empty delta
                // does.
                ReplayEntry::Text(text) if text.is_empty() => {}
                ReplayEntry::Text(text) => self.push_line(Kind::Text, &text),
                ReplayEntry::Reasoning(summary) => self.replay_thought(&summary, now),
                ReplayEntry::Tool {
                    name,
                    preview,
                    result,
                } => self.replay_card(&name, preview.as_deref(), result, now),
                ReplayEntry::Usage { usage, elapsed } => {
                    let mut line = String::new();
                    write_usage(&mut line, &usage, elapsed);
                    self.push_line(Kind::Notice, &line);
                }
            }
        }
    }

    /// A replayed reasoning segment: one thought, ended with no
    /// clock. Whitespace alone opens nothing, as a live delta of
    /// whitespace opens nothing; a hidden setting drops it.
    fn replay_thought(&mut self, summary: &str, now: Instant) {
        if self.thinking == Thinking::Hidden || summary.trim_start().is_empty() {
            return;
        }
        let mut thought = Thought::new(self.thinking == Thinking::Open, now);
        thought.push(summary, SUMMARY_BYTES);
        thought.end_without_clock();
        self.push_block(Block::thought(thought, self.markdown_for(Kind::Reasoning)));
    }

    /// A replayed tool call: one card, ended with the file's clock,
    /// or interrupted when the turn ended before its result. The
    /// card holds the kept tail of the result, as a live card does.
    fn replay_card(
        &mut self,
        name: &str,
        preview: Option<&str>,
        result: Option<ReplayResult>,
        now: Instant,
    ) {
        let mut card = Card::new(name, self.expanded);
        if let Some(preview) = preview {
            card.set_preview(preview);
        }
        match result {
            Some(result) => {
                let (output, output_cut) = kept_output(&result.output);
                card.replay(
                    Outcome {
                        summary: None,
                        is_error: result.is_error,
                        output,
                        output_cut,
                        at: now,
                    },
                    result.duration,
                );
            }
            None => card.interrupt(),
        }
        self.push_block(Block::card(card));
    }

    /// End-of-turn cleanup: close any block that still streams, and
    /// mark every unanswered call as interrupted — an aborted turn
    /// must not leave a running card that reads like an applied
    /// change. The state claims interruption, not absence: a dropped
    /// turn can cut an approved call mid-run, so whether the call
    /// changed anything is unknown here.
    pub fn close(&mut self) {
        self.open = None;
        self.running = None;
        // A thought still live when the turn ends never got its end
        // event: the block says so, instead of thinking forever.
        if let Some(index) = self.live.take()
            && let Some(thought) = self.thought_at(index)
        {
            thought.interrupt();
            self.touch(index);
        }
        let tools = std::mem::take(&mut self.tools);
        for index in tools.into_values() {
            if let Some(card) = self.card_at(index) {
                card.interrupt();
                self.touch(index);
            }
        }
    }

    /// Whether the cards show their kept output whole.
    pub fn expanded(&self) -> bool {
        self.expanded
    }

    /// Show every card's kept output whole, or the tail again. Each
    /// card renders again at its next sync, so the cost lands on the
    /// cards a frame shows.
    pub fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
        for offset in 0..self.blocks.len() {
            if let Some(card) = self.blocks[offset].card_mut() {
                card.set_expanded(expanded);
                self.touch(self.dropped + offset);
            }
        }
    }

    /// The card of an open call, when its block is still held.
    fn card_mut(&mut self, id: &str) -> Option<&mut Card> {
        let index = *self.tools.get(id)?;
        self.card_at(index)
    }

    /// The card at an absolute block index, when its block is still
    /// held.
    fn card_at(&mut self, index: usize) -> Option<&mut Card> {
        let offset = index.checked_sub(self.dropped)?;
        self.blocks.get_mut(offset)?.card_mut()
    }

    /// The thought at an absolute block index, when its block is
    /// still held.
    fn thought_at(&mut self, index: usize) -> Option<&mut Thought> {
        let offset = index.checked_sub(self.dropped)?;
        self.blocks.get_mut(offset)?.thought_mut()
    }

    /// Whether a block of `kind` renders as markdown: a model reply
    /// or a reasoning summary, in a transcript that renders.
    fn markdown_for(&self, kind: Kind) -> bool {
        self.markdown && matches!(kind, Kind::Text | Kind::Reasoning)
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
    /// change returns to follow mode. In browse mode, a height change
    /// moves the scroll by the rows the viewport lost or gained, so
    /// the top row holds when the input box grows or the terminal
    /// shrinks; a viewport that grows past the bottom returns to
    /// follow mode. Then the walk lays
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
            // The rows the points named no longer exist.
            self.selection = None;
        }
        if height != self.height {
            if self.scroll > 0 {
                let lost = i32::from(self.height) - i32::from(height);
                self.scroll = self.scroll.saturating_add_signed(lost as isize);
            }
            self.height = height;
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
                let change = block.sync(width);
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

    /// Append sanitized `text` as blocks of at most the byte cap, the
    /// first with `carry` in front of it. Each block gets an
    /// allocation of its own size, so a large paste costs its length
    /// once.
    fn push_blocks(&mut self, kind: Kind, text: &str, mut carry: Option<String>) {
        let mut rest = text;
        loop {
            let (head, tail) = rest.split_at(cap_boundary(rest));
            let markdown = self.markdown_for(kind);
            let mut block = Block::new(kind, head.to_string(), markdown, carry);
            carry = if tail.is_empty() {
                None
            } else {
                block.cut_at_cap()
            };
            self.push_block(block);
            if tail.is_empty() {
                break;
            }
            rest = tail;
        }
    }

    /// Append one block and drop the oldest past the block cap.
    fn push_block(&mut self, block: Block) {
        self.blocks.push_back(block);
        while self.blocks.len() > MAX_BLOCKS {
            self.blocks.pop_front();
            self.dropped += 1;
        }
        if self
            .selection
            .is_some_and(|selection| selection.span().0.block < self.dropped)
        {
            self.selection = None;
        }
    }

    /// Block `index` changed under the selection: the rows it named
    /// moved, so the selection clears. A change outside the span
    /// leaves it.
    fn touch(&mut self, index: usize) {
        if self
            .selection
            .is_some_and(|selection| selection.covers(index))
        {
            self.selection = None;
        }
    }

    /// The drag selection, for the frame.
    pub(crate) fn selection(&self) -> Option<Selection> {
        self.selection
    }

    /// The row map, taken for the frame to fill; see
    /// [`Transcript::set_shown`].
    pub(crate) fn take_shown(&mut self) -> Vec<(usize, usize)> {
        std::mem::take(&mut self.shown)
    }

    /// The block and row on each viewport row of the frame just
    /// drawn, top down. Nothing covers them until the frame says so.
    pub(crate) fn set_shown(&mut self, shown: Vec<(usize, usize)>) {
        self.shown = shown;
        self.covered.clear();
        self.indicator = None;
    }

    /// A frame drew no transcript rows: no position maps to one.
    pub(crate) fn clear_shown(&mut self) {
        self.shown.clear();
        self.covered.clear();
        self.indicator = None;
    }

    /// The frame drew the browse indicator on viewport row `row`
    /// over `columns`: the cells are covered, and a press there
    /// returns to follow mode.
    pub(crate) fn set_indicator(&mut self, row: usize, columns: Range<usize>) {
        self.cover(row..row + 1, columns.clone());
        self.indicator = Some((row, columns));
    }

    fn on_indicator(&self, x: u16, y: u16) -> bool {
        self.indicator.as_ref().is_some_and(|(row, columns)| {
            *row == usize::from(y) && columns.contains(&usize::from(x))
        })
    }

    /// The frame drew something over viewport `rows` and `columns`
    /// — the completion popup, the browse indicator: a position
    /// there maps to no row.
    pub(crate) fn cover(&mut self, rows: Range<usize>, columns: Range<usize>) {
        self.covered.push((rows, columns));
    }

    fn is_covered(&self, x: u16, y: u16) -> bool {
        self.covered.iter().any(|(rows, columns)| {
            rows.contains(&usize::from(y)) && columns.contains(&usize::from(x))
        })
    }

    /// Whether block `index` changed since the last frame, so the
    /// row map no longer describes its rows. A press or a drag must
    /// not name a row the user cannot see yet.
    fn pending(&self, index: usize) -> bool {
        index
            .checked_sub(self.dropped)
            .and_then(|offset| self.blocks.get(offset))
            .is_none_or(Block::pending)
    }

    /// The point under viewport column `x` and row `y`, when a row
    /// is shown there and its block still reads as the frame drew
    /// it.
    fn point_at(&self, x: u16, y: u16) -> Option<Point> {
        if self.is_covered(x, y) {
            return None;
        }
        let &(block, row) = self.shown.get(usize::from(y))?;
        if self.pending(block) {
            return None;
        }
        Some(Point {
            block,
            row,
            col: usize::from(x),
        })
    }

    /// A press at viewport column `x` and row `y` starts a selection
    /// on the row shown there, or clears the selection when no row
    /// is. A press on the browse indicator is the End key: the view
    /// returns to follow mode. Returns whether the frame changes.
    pub fn select_press(&mut self, x: u16, y: u16) -> bool {
        if self.on_indicator(x, y) {
            self.follow();
            self.selection = None;
            return true;
        }
        let before = self.selection;
        self.selection = self.point_at(x, y).map(|point| Selection {
            anchor: point,
            head: point,
        });
        before.is_some_and(|selection| selection.is_span())
    }

    /// The pointer moved to viewport column `x` and row `y` with the
    /// button down: the selection's head follows. On a blank cell
    /// under the last shown row, inside the pane, the head is the
    /// end of that row. Below the pane — the progress line, the
    /// input box, the bar — on a covered cell, and on a block that
    /// changed since the frame, the head stays. Returns whether the
    /// frame changes.
    pub fn select_drag(&mut self, x: u16, y: u16) -> bool {
        if self.is_covered(x, y) || y >= self.height {
            return false;
        }
        let head = match self.shown.get(usize::from(y)) {
            Some(&(block, row)) => Point {
                block,
                row,
                col: usize::from(x),
            },
            None => {
                let Some(&(block, row)) = self.shown.last() else {
                    return false;
                };
                Point {
                    block,
                    row,
                    col: usize::MAX,
                }
            }
        };
        if self.pending(head.block) {
            return false;
        }
        let Some(selection) = &mut self.selection else {
            return false;
        };
        let changed = selection.head != head;
        selection.head = head;
        changed
    }

    /// The button came up. A selection with a span yields its copy
    /// and stays on the screen; a click without a drag clears.
    pub fn select_release(&mut self) -> Option<Copied> {
        let selection = self.selection?;
        if !selection.is_span() || self.width == 0 {
            self.selection = None;
            return None;
        }
        Some(select::copy(
            selection,
            &mut self.blocks,
            self.dropped,
            self.width,
        ))
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wrap::wrap;

    /// Every block's kind and text, cards rendered.
    fn blocks(t: &mut Transcript) -> Vec<(Kind, &str)> {
        for block in t.blocks_mut() {
            block.sync(80);
        }
        t.blocks().map(|b| (b.kind, b.text())).collect()
    }

    /// End a call the way the loop reports it.
    fn end(t: &mut Transcript, id: &str, name: &str, summary: &str, is_error: bool, output: &str) {
        t.tool_end(
            id,
            name,
            Outcome {
                summary: Some(summary),
                is_error,
                output: output.to_string(),
                output_cut: false,
                at: Instant::now(),
            },
        );
    }

    /// A transcript that shows every kind as plain text: the layout
    /// mechanics under test hold for both modes, and plain rows are
    /// the ones that read as their text.
    fn plain() -> Transcript {
        Transcript::new(false)
    }

    /// Lay every block out at 80 columns.
    fn rows_of(t: &mut Transcript) {
        rows(t, 80);
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
        assert_eq!(blocks(&mut t), vec![(Kind::Text, "hello\nworld")]);
        assert_eq!(rows(&mut t, 80), vec!["hello", "world"]);
    }

    #[test]
    fn a_kind_switch_opens_a_new_block() {
        let mut t = plain();
        t.push_delta(Kind::Notice, "note");
        t.push_delta(Kind::Text, "answer");
        assert_eq!(
            blocks(&mut t),
            vec![(Kind::Notice, "note"), (Kind::Text, "answer")]
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
            blocks(&mut t),
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
        // A card that collapses reports the rows it lost.
        let mut t = plain();
        t.set_expanded(true);
        t.tool_start("c1", "bash");
        end(&mut t, "c1", "bash", "ok", false, "1\n2\n3\n4\n5\n6\n7\n8");
        let block = t.blocks_mut().next_back().unwrap();
        assert_eq!(block.sync(80), 10);
        t.set_expanded(false);
        let block = t.blocks_mut().next_back().unwrap();
        assert_eq!(block.sync(80), -3);
    }

    /// A fixed origin, so the durations under test are exact.
    fn at(secs: u64) -> Instant {
        static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *ORIGIN.get_or_init(Instant::now) + std::time::Duration::from_secs(secs)
    }

    #[test]
    fn reasoning_streams_into_one_closed_block_that_ends_with_its_duration() {
        let mut t = plain();
        t.reasoning_delta("**Plan**", at(0));
        t.reasoning_delta(" the fix\nthen more", at(0));
        assert_eq!(
            blocks(&mut t),
            vec![(Kind::Reasoning, "· thinking · **Plan** the fix…")]
        );
        t.reasoning_end(at(2));
        assert_eq!(
            blocks(&mut t),
            vec![(Kind::Reasoning, "▸ thought for 2.0s · **Plan** the fix…")]
        );
        // An end with no open segment changes nothing; the next
        // segment is a block of its own with its own clock.
        t.reasoning_end(at(3));
        t.reasoning_delta("again", at(3));
        t.reasoning_end(at(4));
        let shown = blocks(&mut t);
        assert_eq!(shown.len(), 2);
        assert_eq!(shown[1], (Kind::Reasoning, "▸ thought for 1.0s · again"));
    }

    #[test]
    fn a_paragraph_break_between_segments_opens_no_block() {
        let mut t = plain();
        t.reasoning_delta("\n\n", at(0));
        t.reasoning_delta("   ", at(0));
        assert!(blocks(&mut t).is_empty());
        t.toggle_thinking();
        assert!(
            blocks(&mut t).is_empty(),
            "a toggle with no block changes nothing"
        );
        t.reasoning_delta("\n\nreal", at(0));
        assert_eq!(blocks(&mut t), vec![(Kind::Reasoning, "· thinking · real")]);
    }

    #[test]
    fn the_toggle_opens_the_newest_block_and_closes_it_again() {
        let mut t = plain();
        t.reasoning_delta("first\nrest", at(0));
        t.reasoning_end(at(1));
        t.reasoning_delta(&"x".repeat(20_000), at(1));
        t.reasoning_end(at(2));
        t.toggle_thinking();
        let shown = blocks(&mut t);
        assert_eq!(
            shown[0].1, "▸ thought for 1.0s · first…",
            "the older block stays closed"
        );
        assert!(shown[1].1.starts_with("▾ thought for 1.0s\n\nxxx"));
        assert_eq!(shown[1].1.len(), "▾ thought for 1.0s\n\n".len() + 20_000);
        t.toggle_thinking();
        let shown = blocks(&mut t);
        assert!(shown[1].1.starts_with("▸ thought for 1.0s · xxx"));
        assert!(shown[1].1.ends_with('…') && shown[1].1.len() < 300);
    }

    #[test]
    fn the_thinking_setting_opens_blocks_or_hides_them() {
        let mut t = plain();
        t.set_thinking(Thinking::Open);
        t.reasoning_delta("a\nb", at(0));
        t.reasoning_end(at(1));
        assert_eq!(
            blocks(&mut t),
            vec![(Kind::Reasoning, "▾ thought for 1.0s\n\na\nb")]
        );

        let mut t = plain();
        t.set_thinking(Thinking::Hidden);
        t.reasoning_delta("a", at(0));
        t.reasoning_end(at(1));
        t.push_delta(Kind::Text, "reply");
        assert_eq!(blocks(&mut t), vec![(Kind::Text, "reply")]);
        t.toggle_thinking();
        assert_eq!(blocks(&mut t), vec![(Kind::Text, "reply")]);
    }

    #[test]
    fn close_marks_a_live_thought_interrupted() {
        let mut t = plain();
        t.reasoning_delta("half", at(0));
        t.close();
        let interrupted = vec![(Kind::Reasoning, "▸ thinking interrupted · half")];
        assert_eq!(blocks(&mut t), interrupted);
        // A late end changes nothing: the segment is over.
        t.reasoning_end(at(5));
        assert_eq!(blocks(&mut t), interrupted);
    }

    #[test]
    fn a_summary_past_the_cap_keeps_its_block_under_the_cap() {
        let mut t = plain();
        t.set_thinking(Thinking::Open);
        t.reasoning_delta(&"y".repeat(MAX_BLOCK_BYTES + 100), at(0));
        t.reasoning_delta("more", at(0));
        t.reasoning_end(at(1));
        let shown = blocks(&mut t);
        assert_eq!(shown.len(), 1);
        assert!(shown[0].1.len() <= MAX_BLOCK_BYTES, "{}", shown[0].1.len());
        assert!(shown[0].1.ends_with("\n…"));
    }

    #[test]
    fn a_thinking_block_renders_its_summary_as_markdown_when_the_transcript_does() {
        let mut t = Transcript::new(true);
        t.reasoning_delta("**Plan** the fix\n\n- step", at(0));
        t.reasoning_end(at(1));
        assert_eq!(rows(&mut t, 80), vec!["▸ thought for 1.0s · Plan the fix…"]);
        t.toggle_thinking();
        assert_eq!(
            rows(&mut t, 80),
            vec!["▾ thought for 1.0s", "", "Plan the fix", "", "• step"]
        );
        let strong: Vec<_> = spans(&mut t, 80)
            .into_iter()
            .flatten()
            .filter(|(face, text)| face.has(Face::STRONG) && !text.is_empty())
            .map(|(_, text)| text)
            .collect();
        assert_eq!(strong, vec!["Plan"]);
        // The plain transcript shows the marks as they came.
        let mut t = plain();
        t.reasoning_delta("**Plan** the fix", at(0));
        assert_eq!(rows(&mut t, 80), vec!["· thinking · **Plan** the fix"]);
    }

    #[test]
    fn a_segment_keeps_its_block_when_another_block_lands_behind_it() {
        let mut t = plain();
        t.reasoning_delta("a", at(0));
        t.push_line(Kind::Notice, "note");
        t.reasoning_delta("b", at(0));
        t.reasoning_end(at(1));
        assert_eq!(
            blocks(&mut t),
            vec![
                (Kind::Reasoning, "▸ thought for 1.0s · ab"),
                (Kind::Notice, "note")
            ]
        );
        // The end left the text stream alone: a reply continues in
        // its own block, not in the thought's.
        t.push_delta(Kind::Text, "reply");
        assert_eq!(blocks(&mut t).len(), 3);
    }

    #[test]
    fn a_hidden_segment_leaves_the_reply_in_one_block() {
        let mut t = plain();
        t.set_thinking(Thinking::Hidden);
        t.push_delta(Kind::Text, "one");
        t.reasoning_delta("hidden", at(0));
        t.reasoning_end(at(1));
        t.push_delta(Kind::Text, " two");
        assert_eq!(blocks(&mut t), vec![(Kind::Text, "one two")]);
    }

    #[test]
    fn a_collapsed_thought_gives_its_layout_back() {
        // Open, a long rendered summary holds its rendered text, its
        // rows, and its faces; closed, one line holds one line's worth.
        let mut t = Transcript::new(true);
        t.set_thinking(Thinking::Open);
        t.reasoning_delta("**Plan**\n", at(0));
        t.reasoning_delta(&"**bold** and plain text ".repeat(1_200), at(0));
        t.reasoning_end(at(1));
        frame(&mut t, 120, 40);
        let block = t.blocks().last().unwrap();
        assert!(block.rendered());
        let open = block.retained();
        assert!(open > 32 * 1024, "{open}");
        t.toggle_thinking();
        frame(&mut t, 120, 40);
        let block = t.blocks().last().unwrap();
        assert_eq!(block.row(0), "▸ thought for 1.0s · Plan…");
        assert!(block.retained() < 4 * 1024, "{}", block.retained());
    }

    #[test]
    fn a_collapsed_thought_renders_again_after_a_fallback() {
        // A summary the markdown render refuses: a wide table past
        // the render cap. Open, the block falls back to plain rows;
        // closed, its one line renders again.
        let mut t = Transcript::new(true);
        t.set_thinking(Thinking::Open);
        let table = format!(
            "|a|b|c|d|e|f|g|h|\n|-|-|-|-|-|-|-|-|\n{}",
            "|a|b|c|d|e|f|g|h|\n".repeat(2_000)
        );
        t.reasoning_delta("**Plan**\n", at(0));
        t.reasoning_delta(&table, at(0));
        t.reasoning_end(at(1));
        frame(&mut t, 120, 40);
        let block = t.blocks().last().unwrap();
        assert!(!block.rendered(), "the open form is past the render cap");
        t.toggle_thinking();
        frame(&mut t, 120, 40);
        let block = t.blocks().last().unwrap();
        assert!(block.rendered(), "the closed row renders as markdown");
        assert_eq!(block.row(0), "▸ thought for 1.0s · Plan…");
    }

    #[test]
    fn a_live_block_past_the_render_cap_keeps_its_verdict_while_it_grows() {
        let mut t = Transcript::new(true);
        t.set_thinking(Thinking::Open);
        let row = "|a|b|c|d|e|f|g|h|\n";
        t.reasoning_delta(
            &format!("{row}|-|-|-|-|-|-|-|-|\n{}", row.repeat(1_800)),
            at(0),
        );
        frame(&mut t, 120, 40);
        let block = t.blocks().last().unwrap();
        assert!(!block.rendered() && block.layout.fallback);
        // More text: the verdict stands, no render runs past the cap.
        t.reasoning_delta(row, at(0));
        frame(&mut t, 120, 40);
        let block = t.blocks().last().unwrap();
        assert!(block.layout.fallback, "the latch held while the text grew");
        // A width change gives a fresh verdict, as for any block.
        frame(&mut t, 60, 40);
        assert!(!t.blocks().last().unwrap().rendered());
    }

    #[test]
    fn a_card_laid_out_at_a_new_width_trims_its_rows() {
        let mut t = plain();
        t.set_expanded(true);
        t.tool_start("c1", "bash");
        end(
            &mut t,
            "c1",
            "bash",
            "ok",
            false,
            &"a long output line ".repeat(200),
        );
        frame(&mut t, 20, 40);
        let block = t.blocks().last().unwrap();
        let narrow = block.layout.rows.capacity();
        assert!(narrow > 128, "{narrow}");
        frame(&mut t, 200, 40);
        let block = t.blocks().last().unwrap();
        assert!(
            block.layout.rows.capacity() <= 2 * block.rows().max(64),
            "{} rows in {}",
            block.rows(),
            block.layout.rows.capacity()
        );
    }

    #[test]
    fn a_summary_that_starts_with_a_rule_leaves_the_header_a_header() {
        let mut t = Transcript::new(true);
        t.set_thinking(Thinking::Open);
        t.reasoning_delta("---\nbody", at(0));
        t.reasoning_end(at(1));
        let rows = rows(&mut t, 80);
        assert_eq!(rows[0], "▾ thought for 1.0s");
        assert!(rows.iter().any(|row| row == "body"), "{rows:?}");
    }

    #[test]
    fn a_toggled_block_below_the_viewport_top_moves_the_anchor() {
        let mut t = plain();
        for i in 0..10 {
            t.push_line(Kind::Text, &format!("line {i}"));
        }
        t.reasoning_delta("a\nb\nc", at(0));
        t.reasoning_end(at(1));
        frame(&mut t, 40, 4);
        t.scroll_up(3);
        frame(&mut t, 40, 4);
        // The block sits below the viewport: opening it adds four
        // rows under the view, and the view stays where it is.
        t.toggle_thinking();
        frame(&mut t, 40, 4);
        assert_eq!(t.scroll(), 7);
        t.toggle_thinking();
        frame(&mut t, 40, 4);
        assert_eq!(t.scroll(), 3);
    }

    #[test]
    fn a_card_updates_in_place_and_reads_as_a_card() {
        let mut t = plain();
        t.tool_start("call_1", "bash");
        t.push_delta(Kind::Text, "meanwhile");
        assert_eq!(
            blocks(&mut t),
            vec![(Kind::Tool, "· bash"), (Kind::Text, "meanwhile")]
        );
        let start = Instant::now();
        t.tool_run("call_1", start);
        t.tool_preview("call_1", "$ cargo test");
        t.tool_end(
            "call_1",
            "bash",
            Outcome {
                summary: Some("$ cargo test (exit 0)"),
                is_error: false,
                output: "ok\n".to_string(),
                output_cut: false,
                at: start + std::time::Duration::from_millis(250),
            },
        );
        assert_eq!(
            blocks(&mut t),
            vec![
                (
                    Kind::Tool,
                    "✓ bash $ cargo test\n  ok\n  250ms · $ cargo test (exit 0)"
                ),
                (Kind::Text, "meanwhile"),
            ]
        );
        // The header is strong, the output dim, the footer plain.
        let spans = spans(&mut t, 80);
        assert_eq!(
            spans[0],
            vec![
                (Face::TITLE | Face::SUCCESS, "✓".to_string()),
                (Face::TITLE, " bash $ cargo test".to_string()),
            ]
        );
        assert_eq!(spans[1], vec![(Face::MUTED, "  ok".to_string())]);
        assert_eq!(spans[2][0].0, Face::PLAIN);
    }

    #[test]
    fn a_failed_call_gets_the_error_mark_and_its_output() {
        let mut t = plain();
        t.tool_start("call_1", "edit");
        end(
            &mut t,
            "call_1",
            "edit",
            "stale tag",
            true,
            "the tag is stale",
        );
        assert_eq!(
            blocks(&mut t),
            vec![(Kind::Tool, "✗ edit\n  the tag is stale\n  stale tag")]
        );
    }

    #[test]
    fn a_replay_lands_every_entry_finished_with_the_left_out_count_first() {
        use std::time::Duration;
        let mut t = plain();
        t.replay(Replay {
            left_out: 2,
            entries: vec![
                ReplayEntry::Prompt("hello".into()),
                ReplayEntry::Reasoning("Plan the fix\nthen check".into()),
                ReplayEntry::Tool {
                    name: "bash".into(),
                    preview: Some("$ echo hi".into()),
                    result: Some(ReplayResult {
                        output: "hi\n".into(),
                        is_error: false,
                        duration: Some(Duration::from_millis(40)),
                    }),
                },
                ReplayEntry::Usage {
                    usage: demerzel_core::Usage {
                        input_tokens: 10,
                        output_tokens: 5,
                        cache_read_tokens: 2,
                        cache_creation_tokens: 0,
                    },
                    elapsed: Some(Duration::from_millis(1_200)),
                },
                ReplayEntry::Text("done".into()),
                ReplayEntry::Text(String::new()),
                ReplayEntry::Tool {
                    name: "read".into(),
                    preview: None,
                    result: Some(ReplayResult {
                        output: "no such file".into(),
                        is_error: true,
                        duration: None,
                    }),
                },
                ReplayEntry::Tool {
                    name: "edit".into(),
                    preview: None,
                    result: None,
                },
                ReplayEntry::Usage {
                    usage: demerzel_core::Usage::default(),
                    elapsed: None,
                },
            ],
        });
        // The turn's end touches no replayed card: none is open.
        t.close();
        assert_eq!(
            blocks(&mut t),
            vec![
                (Kind::Notice, "… 2 earlier messages left out"),
                (Kind::Prompt, "❯ hello"),
                (Kind::Reasoning, "▸ thought · Plan the fix…"),
                (Kind::Tool, "✓ bash $ echo hi\n  hi\n  40ms · done"),
                (Kind::Notice, "· 10 in · 5 out · 2 cached · 1.2s"),
                (Kind::Text, "done"),
                (Kind::Tool, "✗ read\n  no such file\n  error"),
                (Kind::Tool, "✗ edit\n  interrupted"),
                (Kind::Notice, "· 0 in · 0 out · 0 cached"),
            ]
        );
        // The thought opens after the fact, as a live one does.
        t.toggle_thinking();
        assert_eq!(
            blocks(&mut t)[2],
            (Kind::Reasoning, "▾ thought\n\nPlan the fix\nthen check")
        );
    }

    #[test]
    fn a_replay_follows_the_thinking_setting_and_a_blank_summary_opens_nothing() {
        let mut t = plain();
        t.set_thinking(Thinking::Hidden);
        t.replay(Replay {
            left_out: 0,
            entries: vec![
                ReplayEntry::Reasoning("hidden".into()),
                ReplayEntry::Text("shown".into()),
            ],
        });
        assert_eq!(blocks(&mut t), vec![(Kind::Text, "shown")]);

        let mut t = plain();
        t.set_thinking(Thinking::Open);
        t.replay(Replay {
            left_out: 1,
            entries: vec![
                ReplayEntry::Reasoning("  \n".into()),
                ReplayEntry::Reasoning("open".into()),
            ],
        });
        assert_eq!(
            blocks(&mut t),
            vec![
                (Kind::Notice, "… 1 earlier message left out"),
                (Kind::Reasoning, "▾ thought\n\nopen"),
            ]
        );
    }

    #[test]
    fn a_replayed_card_keeps_the_output_tail_and_expands_with_the_rest() {
        let mut t = plain();
        let long: String = (1..=8).map(|i| format!("row {i}\n")).collect();
        t.replay(Replay {
            left_out: 0,
            entries: vec![ReplayEntry::Tool {
                name: "read".into(),
                preview: None,
                result: Some(ReplayResult {
                    output: long,
                    is_error: false,
                    duration: None,
                }),
            }],
        });
        let text = blocks(&mut t)[0].1.to_string();
        assert!(text.starts_with("✓ read\n  row 4\n"), "{text}");
        assert!(text.ends_with("done · 3 more lines · Ctrl-O"), "{text}");
        t.set_expanded(true);
        let text = blocks(&mut t)[0].1.to_string();
        assert!(text.contains("row 1\n"), "{text}");
    }

    #[test]
    fn an_unknown_call_id_appends_a_card_instead() {
        let mut t = plain();
        end(&mut t, "ghost", "bash", "late result", false, "");
        assert_eq!(blocks(&mut t), vec![(Kind::Tool, "✓ bash\n  late result")]);
    }

    #[test]
    fn a_card_update_lays_its_rows_out_again() {
        let mut t = plain();
        t.tool_start("call_1", "bash");
        assert_eq!(rows(&mut t, 80), vec!["· bash"]);
        end(
            &mut t,
            "call_1",
            "bash",
            "exit 0 after a long summary",
            false,
            "",
        );
        assert_eq!(
            rows(&mut t, 12),
            vec!["✓ bash", "  exit 0 ", "after a ", "long summary"]
        );
    }

    #[test]
    fn the_cap_drops_old_blocks_and_tool_updates_survive() {
        let mut t = plain();
        t.tool_start("call_1", "bash");
        for i in 0..MAX_BLOCKS {
            t.push_line(Kind::Text, &format!("line {i}"));
        }
        // The card fell off the front: the preview shows as a notice
        // and the result appends a fresh card.
        t.tool_preview("call_1", "$ ls");
        end(&mut t, "call_1", "bash", "bash: ok", false, "");
        assert_eq!(t.blocks().len(), MAX_BLOCKS);
        let all = blocks(&mut t);
        assert_eq!(all[all.len() - 2], (Kind::Notice, "$ ls"));
        assert_eq!(all[all.len() - 1], (Kind::Tool, "✓ bash\n  bash: ok"));
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
        t.tool_run("call_1", Instant::now());
        t.close();
        // The card claims interruption, never absence: the call may
        // have run partway before the turn dropped.
        assert_eq!(blocks(&mut t), vec![(Kind::Tool, "✗ bash\n  interrupted")]);
        // A late end for a forgotten id appends; it rewrites nothing.
        end(&mut t, "call_1", "bash", "late", false, "");
        assert_eq!(t.blocks().len(), 2);
        // The question of a forgotten call finds no card.
        assert!(!t.tool_pending("bash", None));
    }

    #[test]
    fn cards_sanitize_a_hostile_tool_name_on_every_path() {
        let mut t = plain();
        t.tool_start("call_1", "ba\x1b[2Jsh\nSECOND");
        assert_eq!(rows(&mut t, 80), vec!["· ba[2Jsh SECOND"]);
        t.close();
        assert_eq!(rows(&mut t, 80), vec!["✗ ba[2Jsh SECOND", "  interrupted"]);
        // A name past the cap stays one block, so the update finds it.
        let mut t = plain();
        t.tool_start("call_2", &"n".repeat(2 * MAX_BLOCK_BYTES));
        end(&mut t, "call_2", "n", "ok", false, "");
        let all = blocks(&mut t);
        assert_eq!(all.len(), 1);
        assert!(all[0].1.ends_with("\n  ok"));
        assert!(all[0].1.len() < 1024);
    }

    #[test]
    fn close_leaves_answered_calls_alone() {
        let mut t = plain();
        t.tool_start("call_1", "bash");
        end(&mut t, "call_1", "bash", "bash: ok", false, "");
        t.close();
        assert_eq!(blocks(&mut t), vec![(Kind::Tool, "✓ bash\n  bash: ok")]);
    }

    #[test]
    fn the_question_lands_on_the_running_card_and_the_answer_settles_it() {
        let mut t = plain();
        t.tool_start("c1", "bash");
        t.tool_start("c2", "read");
        t.tool_run("c1", Instant::now());
        t.tool_preview("c1", "$ rm -r x");
        // The question names the running call's tool; another name
        // finds no card.
        assert!(!t.tool_pending("read", None));
        assert!(t.tool_pending("bash", None));
        assert_eq!(rows(&mut t, 80), vec!["? bash $ rm -r x", "· read"]);
        t.tool_answered(true, Instant::now());
        end(
            &mut t,
            "c1",
            "bash",
            "",
            true,
            "the user declined the bash call",
        );
        assert_eq!(
            rows(&mut t, 80),
            vec!["⊘ bash $ rm -r x", "  declined", "· read"]
        );
        // A call without a dispatch preview shows the question's.
        t.tool_run("c2", Instant::now());
        assert!(t.tool_pending("read", Some("{\n  \"path\": \"f\"\n}")));
        t.tool_answered(false, Instant::now());
        end(&mut t, "c2", "read", "read f", false, "1:x");
        assert_eq!(
            rows(&mut t, 80)[2..],
            [
                "✓ read",
                "  {",
                "    \"path\": \"f\"",
                "  }",
                "  1:x",
                "  0ms · read f"
            ]
        );
        // No running call: the question has no card.
        assert!(!t.tool_pending("bash", Some("x")));
    }

    #[test]
    fn a_card_past_the_block_cap_keeps_its_footer_inside_the_cap() {
        // An expanded output of short lines renders three bytes per
        // line, past the block cap: the output rows stop under the
        // cap, the footer lands, and the rows and faces index inside
        // the text.
        let mut t = plain();
        t.set_expanded(true);
        t.tool_start("c1", "bash");
        let output = "é\n".repeat(MAX_BLOCK_BYTES / 3);
        end(&mut t, "c1", "bash", "ok", false, &output);
        let rows = rows(&mut t, 80);
        let block = t.blocks().next().unwrap();
        assert!(block.text().len() <= MAX_BLOCK_BYTES);
        assert!(block.text().len() > MAX_BLOCK_BYTES - 1024);
        assert!(block.text().ends_with("\n  ok"));
        assert_eq!(rows.len(), block.rows());
        assert!(!block.plain());
        let last = block.rows() - 1;
        block.spans(last, |_, _| {});
        // The collapse gives the memory back: the text and the rows.
        t.set_expanded(false);
        rows_of(&mut t);
        let block = t.blocks().next().unwrap();
        assert!(
            block.text.capacity() < MAX_BLOCK_BYTES / 4,
            "{}",
            block.text.capacity()
        );
        assert!(
            block.layout.rows.capacity() <= 2 * block.rows().max(64),
            "{}",
            block.layout.rows.capacity()
        );
    }

    #[test]
    fn a_call_whose_card_fell_off_the_cap_leaves_no_entry_behind() {
        let mut t = plain();
        t.tool_start("c1", "bash");
        t.tool_run("c1", Instant::now());
        for i in 0..MAX_BLOCKS {
            t.push_line(Kind::Text, &format!("line {i}"));
        }
        end(&mut t, "c1", "bash", "ok", false, "");
        assert!(t.tools.is_empty());
        assert_eq!(t.running, None);
    }

    #[test]
    fn expanding_shows_the_kept_output_and_new_cards_follow() {
        let mut t = plain();
        let output: String = (1..=7).map(|i| format!("{i}\n")).collect();
        t.tool_start("c1", "bash");
        end(&mut t, "c1", "bash", "ok", false, &output);
        let collapsed = rows(&mut t, 80);
        assert_eq!(collapsed.len(), 7);
        assert_eq!(collapsed[1], "  3");
        assert!(collapsed[6].ends_with("2 more lines · Ctrl-O"));
        assert!(!t.expanded());
        t.set_expanded(true);
        let expanded = rows(&mut t, 80);
        assert_eq!(expanded.len(), 9);
        assert_eq!(expanded[1], "  1");
        assert_eq!(expanded[8], "  ok");
        // A card that starts while expanded renders expanded.
        t.tool_start("c2", "bash");
        end(&mut t, "c2", "bash", "ok", false, &output);
        assert_eq!(rows(&mut t, 80).len(), 18);
        t.set_expanded(false);
        assert_eq!(rows(&mut t, 80).len(), 14);
    }

    #[test]
    fn a_card_renders_at_its_sync_not_at_its_change() {
        // The toggle marks every card and renders none: the cost
        // lands on the cards a frame shows.
        let mut t = plain();
        t.tool_start("c1", "bash");
        end(&mut t, "c1", "bash", "ok", false, "out");
        let block = t.blocks().next().unwrap();
        assert!(block.text().is_empty());
        assert_eq!(rows(&mut t, 80), vec!["✓ bash", "  out", "  ok"]);
        t.set_expanded(true);
        let block = t.blocks().next().unwrap();
        assert!(matches!(&block.body, Body::Card(card) if card.stale()));
        assert_eq!(block.rows(), 3);
    }

    #[test]
    fn control_characters_never_reach_a_block() {
        let mut t = plain();
        t.push_delta(Kind::Text, "a\x1b[31mred\tdone\r");
        assert_eq!(blocks(&mut t), vec![(Kind::Text, "a[31mred    done")]);
        let mut t = plain();
        t.push_line(Kind::Notice, "a\x1b[31mred\tdone\r");
        assert_eq!(blocks(&mut t), vec![(Kind::Notice, "a[31mred    done")]);
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
        end(
            &mut t,
            "c1",
            "bash",
            "a much longer summary that certainly wraps across three rows of a forty column pane",
            false,
            "",
        );
        frame(&mut t, 40, 10);
        assert_eq!(t.scroll(), 5);

        // A block that shrinks reports the loss.
        let mut t = plain();
        for i in 0..20 {
            t.push_line(Kind::Text, &format!("line {i:02}"));
        }
        t.set_expanded(true);
        t.tool_start("c1", "bash");
        end(&mut t, "c1", "bash", "ok", false, "1\n2\n3\n4\n5\n6\n7\n8");
        t.push_delta(Kind::Text, "streaming");
        frame(&mut t, 40, 10);
        t.scroll_up(3);
        t.set_expanded(false);
        t.push_delta(Kind::Text, " more");
        frame(&mut t, 40, 10);
        assert_eq!(t.scroll(), 0);
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
            block.sync(40);
        }
        // Scrolling into blocks the frame never laid out counts
        // nothing: they sat above the viewport all along.
        t.scroll_up(20);
        t.begin_frame(40, 4);
        assert_eq!(t.scroll(), 20);
        // A rewrite above the viewport changes nothing; an arrival
        // below it counts.
        end(
            &mut t,
            "c1",
            "bash",
            "a long summary that wraps to more rows than the pending line had, well above",
            false,
            "",
        );
        t.push_line(Kind::Text, "arrived");
        t.begin_frame(40, 4);
        assert_eq!(t.scroll(), 21);
    }

    /// Every row of every block as `(face, text)` spans.
    fn spans(t: &mut Transcript, width: u16) -> Vec<Vec<(Face, String)>> {
        let mut out = Vec::new();
        for block in t.blocks_mut() {
            block.sync(width);
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
        t.push_line(Kind::Error, "- not a list");
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
                (Face::MARKER, "# ".to_string()),
                (Face::HEADING, "Title".to_string())
            ]
        );
        assert_eq!(spans[4], vec![(Face::PLAIN, "# not a heading".to_string())]);
        assert_eq!(spans[5], vec![(Face::PLAIN, "- not a list".to_string())]);
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
        // Rules render at the full width: 150 of them outgrow the
        // rendered-row cap at 200 columns and fit it at 10.
        let source = "---\n\n".repeat(150);
        let mut t = Transcript::new(true);
        t.push_delta(Kind::Text, &source);
        let rows = rows(&mut t, 200);
        assert_eq!(rows[0], "---");
        assert_eq!(rows[1], "");
        let block = t.blocks().next().unwrap();
        assert!(block.plain());
        assert!(!block.rendered());
        // The block stays plain and lays out from its tail while the
        // width holds; a new width tries the render again.
        assert!(block.layout.fallback);
        t.push_delta(Kind::Text, "tail");
        let block = t.blocks_mut().next().unwrap();
        assert_eq!(block.sync(200), 1);
        assert!(block.layout.fallback);
        block.sync(10);
        assert!(!block.layout.fallback);
        assert!(block.rendered());
        assert_eq!(block.row(0), "──────────");
    }

    #[test]
    fn a_fence_cut_at_the_byte_cap_stays_code_across_the_split() {
        // One fence longer than a block: the code stays code in every
        // block, one frame opens it and one closes it, and the prose
        // after the closer is prose.
        let mut source = String::from("```rust\n");
        while source.len() < MAX_BLOCK_BYTES + 1024 {
            source.push_str("let value = 1;\n");
        }
        source.push_str("```\n\nafter");
        for step in [source.len(), 7] {
            let mut t = Transcript::new(true);
            let mut cut = 0;
            while cut < source.len() {
                let end = (cut + step).min(source.len());
                t.push_delta(Kind::Text, &source[cut..end]);
                cut = end;
            }
            assert_eq!(t.blocks().len(), 2, "step {step}");
            let rows = rows(&mut t, 80);
            let opens = rows.iter().filter(|row| row.starts_with("╭─")).count();
            let closes = rows.iter().filter(|row| row.starts_with("╰─")).count();
            assert_eq!((opens, closes), (1, 1), "step {step}");
            assert_eq!(rows[0], "╭─ rust");
            assert_eq!(rows[rows.len() - 1], "after");
            let code = &rows[1..rows.len() - 3];
            assert!(code.iter().all(|row| row.starts_with("│ ")), "step {step}");
            // The plain view shows the source as it came: no opener
            // twice.
            let mut plain = Transcript::new(false);
            plain.push_delta(Kind::Text, &source);
            let joined: String = plain.blocks().map(|b| b.text()).collect();
            assert_eq!(joined, source);
        }
    }

    #[test]
    fn a_markdown_block_reports_its_row_change_like_a_plain_one() {
        let mut t = Transcript::new(true);
        t.push_delta(Kind::Text, "one");
        let block = t.blocks_mut().next_back().unwrap();
        assert_eq!(block.sync(8), 1);
        assert_eq!(block.sync(8), 0);
        t.push_delta(Kind::Text, "\n\ntwo\n\nthree");
        let block = t.blocks_mut().next_back().unwrap();
        assert_eq!(block.sync(8), 4);
        assert_eq!(block.sync(2), 0);
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

    #[test]
    fn a_height_change_holds_the_top_row_in_browse_mode() {
        let mut t = Transcript::new(false);
        for i in 0..10 {
            t.push_line(Kind::Text, &format!("line {i}"));
        }
        frame(&mut t, 20, 4);
        t.scroll_up(3);
        frame(&mut t, 20, 4);
        assert_eq!(t.scroll(), 3);
        // The viewport loses two rows: the scroll deepens by two, so
        // the top row stays where it was.
        frame(&mut t, 20, 2);
        assert_eq!(t.scroll(), 5);
        frame(&mut t, 20, 4);
        assert_eq!(t.scroll(), 3);
        // A viewport that grows past the bottom returns to follow
        // mode.
        frame(&mut t, 20, 9);
        assert_eq!(t.scroll(), 0);
        // Follow mode ignores the height.
        frame(&mut t, 20, 4);
        assert_eq!(t.scroll(), 0);
    }
}
