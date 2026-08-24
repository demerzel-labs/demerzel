//! Markdown rendering: the source of one model reply segment becomes
//! styled rows at one width.
//!
//! The parser turns the source into events; the renderer here maps
//! them to rows in one linear pass and keeps no tree. A block re-runs
//! the pass whole when its text changes, so the byte cap on a block
//! bounds the work of one frame. A fence the stream has not closed yet
//! renders as code from its opening line: the parser reads an open
//! fence to the end of the text, which is what a mid-fence stream is.
//!
//! Containers — block quotes, list items — stack a prefix that every
//! row inside them carries. Rows wrap at the width left after the
//! prefix, code rows included, so every character stays on screen.
//! Table rows are the exception: they cut at the width, because a
//! wrapped table reads as nothing. A newline inside a paragraph ends
//! the row, the way chat interfaces read it: a model writes short
//! lines as lines, not as one paragraph to reflow.
//!
//! The rendered rows live in a buffer of their own next to the source.
//! Prefixes, frames, and table padding make it larger than the source;
//! the caller passes a byte cap and no row starts past it, so a block
//! of rules, padded tables, or deep quotes cannot grow without bound.
//! The render reports the cap, and the caller then shows the source as
//! plain text.
//!
//! The source is sanitized before it gets here, but the parser decodes
//! character references, so control characters can come back; every
//! path to the rows drops them again.

use std::fmt::Write as _;
use std::ops::Range;

use pulldown_cmark::{
    Alignment, CodeBlockKind, CowStr, Event, HeadingLevel, Options, Parser, Tag, TagEnd,
};
use unicode_width::UnicodeWidthChar as _;

use crate::wrap::{display_width, row_starts};

/// The style class of one span. The frame maps faces to styles; the
/// theme owns that map once it lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Face {
    /// Body text in the block's own style.
    Plain,
    Heading,
    Strong,
    Emphasis,
    Strike,
    /// Inline code and the lines of a code block.
    Code,
    /// The text of a link.
    Link,
    /// The destination shown after a link's text.
    Url,
    /// Structure the renderer adds: heading marks, list markers,
    /// quote bars, code frames, rules, table lines.
    Marker,
}

/// Where the rows land: one text buffer, the byte offset where each
/// row starts, and the face changes as `(offset, face)`, sorted.
/// A row ends where the next starts; a face holds until the next
/// change.
pub(crate) struct Target<'a> {
    pub text: &'a mut String,
    pub rows: &'a mut Vec<u32>,
    pub faces: &'a mut Vec<(u32, Face)>,
}

/// Render `source` at `width` columns into `target`, which starts
/// empty. Returns `false` when the rows reached `cap` bytes; the
/// target then holds a partial render the caller must discard. The
/// buffer can run past the cap by one row: the check runs where a
/// row starts.
#[must_use]
pub(crate) fn render(source: &str, width: u16, cap: usize, target: Target<'_>) -> bool {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let mut renderer = Renderer {
        out: target,
        width: usize::from(width.max(1)),
        cap,
        full: false,
        face: Face::Plain,
        prefix: String::new(),
        markers: Vec::new(),
        marker_text: String::new(),
        lists: Vec::new(),
        inline: Vec::new(),
        line: String::new(),
        line_faces: Vec::new(),
        line_face: Face::Plain,
        starts: Vec::new(),
        need_gap: false,
        links: Vec::new(),
        table: None,
        cell_text: String::new(),
        cell_faces: Vec::new(),
    };
    for event in Parser::new_ext(source, options) {
        renderer.event(event);
        if renderer.full {
            return false;
        }
    }
    renderer.flush_pending();
    !renderer.full
}

/// One list container: its next ordinal when ordered, the indent its
/// open item pushed onto the prefix and the prefix length with it,
/// and whether its items hold paragraphs — a loose list, whose items
/// part with a blank row.
struct ListState {
    next: Option<u64>,
    item_cols: usize,
    item_prefix: usize,
    loose: bool,
}

/// A list marker waiting for its item's first row, where it takes
/// the place of the item's indent: where that indent starts in the
/// prefix, its length in bytes, and the marker's text in
/// `marker_text`.
struct Pending {
    at: usize,
    bytes: usize,
    text: Range<usize>,
}

/// An open link or image: its destination, where its text starts in
/// the line, and whether a row ended since, which makes that start
/// stale.
struct Link<'s> {
    dest: CowStr<'s>,
    from: usize,
    split: bool,
}

/// A table under construction: cells stay as text until the end
/// tag, when the column widths are known. Cells are ranges into the
/// renderer's cell scratch; `rows` holds the index of each row's
/// first cell.
struct TableState {
    aligns: Vec<Alignment>,
    cells: Vec<Cell>,
    rows: Vec<usize>,
    has_head: bool,
}

struct Cell {
    text: Range<usize>,
    faces: Range<usize>,
}

struct Renderer<'t, 's> {
    out: Target<'t>,
    /// Columns in the viewport.
    width: usize,
    cap: usize,
    /// The rows reached the cap; nothing more lands.
    full: bool,
    /// The face the output currently carries.
    face: Face,
    /// The continuation prefix of the open containers: quote bars and
    /// list indents. Every row starts with it.
    prefix: String,
    /// Markers waiting for a first row, outermost first.
    markers: Vec<Pending>,
    marker_text: String,
    lists: Vec<ListState>,
    /// The inline face stack: emphasis, strong, links, headings, code.
    inline: Vec<Face>,
    /// The logical line under construction, with its face changes.
    line: String,
    line_faces: Vec<(usize, Face)>,
    line_face: Face,
    /// Scratch for the row starts of one line.
    starts: Vec<usize>,
    /// A block ended; the next block opens with a blank row.
    need_gap: bool,
    /// Open links and images, outermost first.
    links: Vec<Link<'s>>,
    table: Option<TableState>,
    /// The text and faces of the open table's cells; face offsets are
    /// relative to their cell.
    cell_text: String,
    cell_faces: Vec<(usize, Face)>,
}

impl<'s> Renderer<'_, 's> {
    fn event(&mut self, event: Event<'s>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => {
                if self.inline.last() == Some(&Face::Code) {
                    // Inside a code block: every newline ends a row.
                    self.push_lines(&text);
                } else {
                    self.push_inline(&text);
                }
            }
            Event::Code(code) => self.push_text(&code, Face::Code),
            // Raw markup stays as text; a newline inside an HTML
            // block ends the row.
            Event::Html(html) | Event::InlineHtml(html) => self.push_lines(&html),
            Event::SoftBreak | Event::HardBreak => self.flush_line(),
            Event::Rule => {
                self.flush_pending();
                self.gap();
                let cols = self.avail();
                if self.begin_row() {
                    self.push_prefix();
                    self.set_face(Face::Marker);
                    for _ in 0..cols {
                        self.out.text.push('─');
                    }
                }
                self.need_gap = true;
            }
            // Extensions that are off never produce these.
            Event::InlineMath(_)
            | Event::DisplayMath(_)
            | Event::FootnoteReference(_)
            | Event::TaskListMarker(_) => {}
        }
    }

    fn start(&mut self, tag: Tag<'s>) {
        match tag {
            Tag::Paragraph => {
                self.flush_pending();
                self.gap();
                // The parser wraps item text in a paragraph only in a
                // loose list. A paragraph deeper inside the item — in
                // a quote — says nothing about the list.
                if let Some(list) = self.lists.last_mut()
                    && self.prefix.len() == list.item_prefix
                {
                    list.loose = true;
                }
            }
            Tag::Heading { level, .. } => {
                self.flush_pending();
                self.gap();
                let marks = match level {
                    HeadingLevel::H1 => "# ",
                    HeadingLevel::H2 => "## ",
                    HeadingLevel::H3 => "### ",
                    HeadingLevel::H4 => "#### ",
                    HeadingLevel::H5 => "##### ",
                    HeadingLevel::H6 => "###### ",
                };
                self.push_text(marks, Face::Marker);
                self.inline.push(Face::Heading);
            }
            Tag::BlockQuote(_) => {
                self.flush_pending();
                self.gap();
                self.prefix.push_str("│ ");
            }
            Tag::CodeBlock(kind) => {
                self.flush_pending();
                self.gap();
                if self.begin_row() {
                    self.push_prefix();
                    self.push_out("╭─", Face::Marker);
                    if let CodeBlockKind::Fenced(info) = kind
                        && let Some(language) = info.split_whitespace().next()
                    {
                        // The label cuts at the width, like a table row.
                        let avail = self.avail();
                        let mut used = 2usize;
                        self.push_pad(1, &mut used, avail);
                        for ch in language.chars().filter(|ch| !ch.is_control()) {
                            let cols = ch.width().unwrap_or(0);
                            if used + cols > avail {
                                break;
                            }
                            self.out.text.push(ch);
                            used += cols;
                        }
                    }
                }
                self.prefix.push_str("│ ");
                self.inline.push(Face::Code);
            }
            Tag::HtmlBlock => {
                self.flush_pending();
                self.gap();
            }
            Tag::List(first) => {
                self.flush_pending();
                // A list nested in a tight item follows the item's
                // text with no gap: only a block that ended sets the
                // flag, and the item's text is no block.
                self.gap();
                self.lists.push(ListState {
                    next: first,
                    item_cols: 0,
                    item_prefix: 0,
                    loose: false,
                });
            }
            Tag::Item => {
                self.flush_pending();
                let start = self.marker_text.len();
                match self.lists.last_mut().and_then(|list| list.next.as_mut()) {
                    Some(next) => {
                        let _ = write!(self.marker_text, "{next}. ");
                        *next += 1;
                    }
                    None => self.marker_text.push_str("• "),
                }
                let cols = display_width(&self.marker_text[start..]);
                let at = self.prefix.len();
                self.prefix.extend(std::iter::repeat_n(' ', cols));
                self.markers.push(Pending {
                    at,
                    bytes: self.prefix.len() - at,
                    text: start..self.marker_text.len(),
                });
                if let Some(list) = self.lists.last_mut() {
                    list.item_cols = cols;
                    list.item_prefix = self.prefix.len();
                }
            }
            Tag::Table(aligns) => {
                self.flush_pending();
                self.gap();
                self.table = Some(TableState {
                    aligns,
                    cells: Vec::new(),
                    rows: Vec::new(),
                    has_head: false,
                });
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(table) = &mut self.table {
                    table.has_head |= matches!(tag, Tag::TableHead);
                    table.rows.push(table.cells.len());
                }
            }
            Tag::TableCell => {}
            Tag::Emphasis => self.inline.push(Face::Emphasis),
            Tag::Strong => self.inline.push(Face::Strong),
            Tag::Strikethrough => self.inline.push(Face::Strike),
            Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                self.links.push(Link {
                    dest: dest_url,
                    from: self.line.len(),
                    split: false,
                });
                self.inline.push(Face::Link);
            }
            // Extensions that are off never produce these.
            Tag::FootnoteDefinition(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::Superscript
            | Tag::Subscript
            | Tag::MetadataBlock(_) => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::HtmlBlock => {
                self.flush_pending();
                self.need_gap = true;
            }
            TagEnd::Heading(_) => {
                self.inline.pop();
                self.flush_line();
                self.need_gap = true;
            }
            TagEnd::BlockQuote(_) => {
                self.flush_pending();
                self.pop_prefix("│ ".len());
                self.need_gap = true;
            }
            TagEnd::CodeBlock => {
                // The parser ends every code line with a newline, so
                // the line is empty here unless the stream is
                // mid-line; a mid-line tail shows.
                self.flush_pending();
                self.inline.pop();
                self.pop_prefix("│ ".len());
                if self.begin_row() {
                    self.push_prefix();
                    self.push_out("╰─", Face::Marker);
                }
                self.need_gap = true;
            }
            TagEnd::List(_) => {
                self.lists.pop();
                // A nested list's end leaves the gap to the item that
                // holds it; the outer list's end sets one.
                if self.lists.is_empty() {
                    self.need_gap = true;
                }
            }
            TagEnd::Item => {
                if !self.line.is_empty() {
                    self.flush_line();
                } else if !self.markers.is_empty() && self.begin_row() {
                    // An empty item still shows its marker, without
                    // the space that would follow its text.
                    self.push_prefix();
                    while self.out.text.ends_with(' ') {
                        self.out.text.pop();
                    }
                }
                // An item's end is the boundary: a loose list parts
                // its items with a blank row, a tight one does not,
                // whatever block ended inside the item.
                let list = self.lists.last();
                self.need_gap = list.is_some_and(|list| list.loose);
                let cols = list.map_or(0, |list| list.item_cols);
                self.pop_prefix(cols);
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    self.emit_table(table);
                }
                self.cell_text.clear();
                self.cell_faces.clear();
                self.need_gap = true;
            }
            TagEnd::TableHead | TagEnd::TableRow => {}
            TagEnd::TableCell => {
                let text = self.cell_text.len()..self.cell_text.len() + self.line.len();
                self.cell_text.push_str(&self.line);
                let faces = self.cell_faces.len()..self.cell_faces.len() + self.line_faces.len();
                self.cell_faces.extend_from_slice(&self.line_faces);
                if let Some(table) = &mut self.table {
                    table.cells.push(Cell { text, faces });
                }
                self.line.clear();
                self.line_faces.clear();
                self.line_face = Face::Plain;
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.inline.pop();
            }
            TagEnd::Link | TagEnd::Image => {
                self.inline.pop();
                let Some(link) = self.links.pop() else {
                    return;
                };
                if link.dest.is_empty() {
                    return;
                }
                // The destination shows once: not after text that is
                // the destination already, and alone when the link
                // has no text.
                let from = link.from.min(self.line.len());
                let has_text = if link.split {
                    !self.line.is_empty()
                } else {
                    self.line.len() > from
                };
                if !link.split && self.line[from..] == *link.dest {
                    return;
                }
                if has_text {
                    self.push_text(" (", Face::Url);
                    self.push_text(&link.dest, Face::Url);
                    self.push_text(")", Face::Url);
                } else {
                    self.push_text(&link.dest, Face::Url);
                }
            }
            TagEnd::FootnoteDefinition
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::MetadataBlock(_) => {}
        }
    }

    /// Columns a row holds after the prefix.
    fn avail(&self) -> usize {
        (self.width - display_width(&self.prefix).min(self.width)).max(1)
    }

    fn pop_prefix(&mut self, bytes: usize) {
        let keep = self.prefix.len().saturating_sub(bytes);
        self.prefix.truncate(keep);
    }

    /// Append inline text in the current inline face.
    fn push_inline(&mut self, text: &str) {
        let face = self.inline.last().copied().unwrap_or(Face::Plain);
        self.push_text(text, face);
    }

    /// Append text that may hold newlines; each one ends the row.
    fn push_lines(&mut self, text: &str) {
        let mut rest = text;
        while let Some(at) = rest.find('\n') {
            self.push_inline(&rest[..at]);
            self.flush_line();
            rest = &rest[at + 1..];
        }
        self.push_inline(rest);
    }

    /// Append text to the line under construction. Control characters
    /// drop and a tab widens, the transcript's rule; the parser can
    /// decode a character reference into either.
    fn push_text(&mut self, text: &str, face: Face) {
        if text.is_empty() {
            return;
        }
        if face != self.line_face {
            self.line_faces.push((self.line.len(), face));
            self.line_face = face;
        }
        // The fast path: no ASCII control, and no 0xC2 lead byte,
        // which is where the C1 controls live.
        if text
            .bytes()
            .all(|byte| byte >= 0x20 && byte != 0x7f && byte != 0xC2)
        {
            self.line.push_str(text);
            return;
        }
        for ch in text.chars() {
            if ch == '\t' {
                self.line.push_str("    ");
            } else if !ch.is_control() {
                self.line.push(ch);
            }
        }
    }

    /// Text that goes to the output row directly, not through the
    /// line: frames and markers, from this module's own literals.
    fn push_out(&mut self, text: &str, face: Face) {
        self.set_face(face);
        self.out.text.push_str(text);
    }

    /// Change the face from here on. A change before any text lands
    /// replaces the last one, or removes it when the face in force
    /// before it is the one asked for, so no entry covers nothing and
    /// a block of prose keeps no faces at all.
    fn set_face(&mut self, face: Face) {
        if face == self.face {
            return;
        }
        let at = self.out.text.len() as u32;
        let faces = &mut *self.out.faces;
        match faces.last() {
            Some(&(last, _)) if last == at => {
                let before = faces
                    .len()
                    .checked_sub(2)
                    .map_or(Face::Plain, |index| faces[index].1);
                if before == face {
                    faces.pop();
                } else if let Some(last) = faces.last_mut() {
                    last.1 = face;
                }
            }
            _ => faces.push((at, face)),
        }
        self.face = face;
    }

    /// Start a row, unless the rows reached the cap.
    #[must_use]
    fn begin_row(&mut self) -> bool {
        if self.full || self.out.text.len() >= self.cap {
            self.full = true;
            return false;
        }
        self.out.rows.push(self.out.text.len() as u32);
        true
    }

    /// The container prefix at the start of a row. Each pending list
    /// marker takes the place of its item's indent, once.
    fn push_prefix(&mut self) {
        self.set_face(Face::Marker);
        let Self {
            out,
            prefix,
            markers,
            marker_text,
            ..
        } = self;
        let mut at = 0usize;
        for pending in markers.drain(..) {
            out.text.push_str(&prefix[at..pending.at]);
            out.text.push_str(&marker_text[pending.text]);
            at = pending.at + pending.bytes;
        }
        out.text.push_str(&prefix[at..]);
        marker_text.clear();
    }

    /// The blank row between blocks: the prefix without its trailing
    /// spaces, so a quote's bar continues.
    fn gap(&mut self) {
        if self.need_gap {
            self.need_gap = false;
            if !self.out.rows.is_empty() && self.begin_row() {
                self.set_face(Face::Marker);
                self.out.text.push_str(self.prefix.trim_end());
            }
        }
    }

    /// Flush the line when it holds text.
    fn flush_pending(&mut self) {
        if !self.line.is_empty() {
            self.flush_line();
        }
    }

    /// Wrap the line at the width left after the prefix and emit its
    /// rows, the prefix on each. An empty line is one row.
    fn flush_line(&mut self) {
        if self.full {
            // The render ends after this event; the wrap pass would
            // be waste.
            self.line.clear();
            self.line_faces.clear();
            self.line_face = Face::Plain;
            return;
        }
        let line = std::mem::take(&mut self.line);
        let faces = std::mem::take(&mut self.line_faces);
        let avail = u16::try_from(self.avail()).unwrap_or(u16::MAX);
        let mut starts = std::mem::take(&mut self.starts);
        starts.clear();
        row_starts(&line, avail, |start| starts.push(start));
        for (index, &from) in starts.iter().enumerate() {
            let to = starts.get(index + 1).copied().unwrap_or(line.len());
            if !self.begin_row() {
                break;
            }
            self.push_prefix();
            self.push_span(&line, &faces, from, to);
        }
        for link in &mut self.links {
            link.split = true;
        }
        self.starts = starts;
        self.line = line;
        self.line.clear();
        self.line_faces = faces;
        self.line_faces.clear();
        self.line_face = Face::Plain;
    }

    /// Copy `line[from..to]` to the output with its faces.
    fn push_span(&mut self, line: &str, faces: &[(usize, Face)], from: usize, to: usize) {
        let mut next = faces.partition_point(|&(at, _)| at <= from);
        let face = if next == 0 {
            Face::Plain
        } else {
            faces[next - 1].1
        };
        self.set_face(face);
        let mut at = from;
        while next < faces.len() && faces[next].0 < to {
            let (change, face) = faces[next];
            self.out.text.push_str(&line[at..change]);
            self.set_face(face);
            at = change;
            next += 1;
        }
        self.out.text.push_str(&line[at..to]);
    }

    /// Lay the table out on the column widths of its content and cut
    /// every row at the viewport width.
    fn emit_table(&mut self, table: TableState) {
        let cell_text = std::mem::take(&mut self.cell_text);
        let cell_faces = std::mem::take(&mut self.cell_faces);
        let row_cells = |index: usize| {
            let from = table.rows[index];
            let to = table
                .rows
                .get(index + 1)
                .copied()
                .unwrap_or(table.cells.len());
            &table.cells[from..to]
        };
        let columns = (0..table.rows.len())
            .map(|index| row_cells(index).len())
            .max()
            .unwrap_or(0);
        if columns > 0 {
            let mut widths = vec![1usize; columns];
            for index in 0..table.rows.len() {
                for (column, cell) in row_cells(index).iter().enumerate() {
                    widths[column] =
                        widths[column].max(display_width(&cell_text[cell.text.clone()]));
                }
            }
            let avail = self.avail();
            for index in 0..table.rows.len() {
                let head = table.has_head && index == 0;
                if !self.begin_row() {
                    break;
                }
                self.push_prefix();
                let cells = row_cells(index);
                let mut used = 0usize;
                for (column, width) in widths.iter().enumerate() {
                    if column > 0 && !self.push_cut(" │ ", &mut used, avail) {
                        break;
                    }
                    let (text, faces) = match cells.get(column) {
                        Some(cell) => (
                            &cell_text[cell.text.clone()],
                            &cell_faces[cell.faces.clone()],
                        ),
                        None => ("", &[][..]),
                    };
                    let pad = width - display_width(text);
                    let (left, right) = match table.aligns.get(column) {
                        Some(Alignment::Right) => (pad, 0),
                        Some(Alignment::Center) => (pad / 2, pad - pad / 2),
                        _ => (0, pad),
                    };
                    if !self.push_pad(left, &mut used, avail)
                        || !self.push_cell(text, faces, head, &mut used, avail)
                        || !self.push_pad(right, &mut used, avail)
                    {
                        break;
                    }
                }
                if head && self.begin_row() {
                    self.push_prefix();
                    self.set_face(Face::Marker);
                    let mut used = 0usize;
                    for (column, width) in widths.iter().enumerate() {
                        if column > 0 && !self.push_cut("─┼─", &mut used, avail) {
                            break;
                        }
                        for _ in 0..*width {
                            if used >= avail {
                                break;
                            }
                            self.out.text.push('─');
                            used += 1;
                        }
                    }
                }
            }
        }
        self.cell_text = cell_text;
        self.cell_faces = cell_faces;
    }

    /// Append `count` spaces inside the cut.
    fn push_pad(&mut self, count: usize, used: &mut usize, avail: usize) -> bool {
        for _ in 0..count {
            if *used >= avail {
                return false;
            }
            self.out.text.push(' ');
            *used += 1;
        }
        true
    }

    /// Append a cell inside the cut, its faces kept; a header cell
    /// reads bold where it has no face of its own.
    fn push_cell(
        &mut self,
        text: &str,
        faces: &[(usize, Face)],
        head: bool,
        used: &mut usize,
        avail: usize,
    ) -> bool {
        let mut next = 0usize;
        let mut face = Face::Plain;
        for (at, ch) in text.char_indices() {
            while next < faces.len() && faces[next].0 <= at {
                face = faces[next].1;
                next += 1;
            }
            let shown = if head && face == Face::Plain {
                Face::Strong
            } else {
                face
            };
            let cols = ch.width().unwrap_or(0);
            if *used + cols > avail {
                return false;
            }
            self.set_face(shown);
            self.out.text.push(ch);
            *used += cols;
        }
        true
    }

    /// Append a table line inside the cut, whole or not at all.
    fn push_cut(&mut self, text: &str, used: &mut usize, avail: usize) -> bool {
        let cols = display_width(text);
        if *used + cols > avail {
            return false;
        }
        self.push_out(text, Face::Marker);
        *used += cols;
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// The rows of `source` at `width`, each as `(text, faces)` where
    /// faces are `(column offset, face)` changes inside the row.
    fn rows(source: &str, width: u16) -> Vec<(String, Vec<(usize, Face)>)> {
        let mut text = String::new();
        let mut starts = Vec::new();
        let mut faces = Vec::new();
        let ok = render(
            source,
            width,
            usize::MAX,
            Target {
                text: &mut text,
                rows: &mut starts,
                faces: &mut faces,
            },
        );
        assert!(ok);
        let mut out = Vec::new();
        for (index, &from) in starts.iter().enumerate() {
            let from = from as usize;
            let to = starts
                .get(index + 1)
                .map_or(text.len(), |&next| next as usize);
            let row = &text[from..to];
            let mut row_faces = Vec::new();
            let first = faces.partition_point(|&(at, _)| (at as usize) <= from);
            let face = if first == 0 {
                Face::Plain
            } else {
                faces[first - 1].1
            };
            row_faces.push((0, face));
            for &(at, face) in &faces[first..] {
                let at = at as usize;
                if at >= to {
                    break;
                }
                row_faces.push((at - from, face));
            }
            out.push((row.to_string(), row_faces));
        }
        out
    }

    fn texts(source: &str, width: u16) -> Vec<String> {
        rows(source, width)
            .into_iter()
            .map(|(text, _)| text)
            .collect()
    }

    #[test]
    fn a_paragraph_wraps_and_a_newline_inside_it_ends_the_row() {
        assert_eq!(texts("one two three", 8), vec!["one two ", "three"]);
        assert_eq!(texts("one two\nthree", 80), vec!["one two", "three"]);
        assert_eq!(texts("", 80), Vec::<String>::new());
    }

    #[test]
    fn blocks_separate_with_one_blank_row() {
        assert_eq!(
            texts("first\n\nsecond\n\n\n\nthird", 80),
            vec!["first", "", "second", "", "third"]
        );
    }

    #[test]
    fn headings_keep_their_level_marks() {
        let rows = rows("# Title\n\n### Sub *em*", 80);
        assert_eq!(rows[0].0, "# Title");
        assert_eq!(rows[0].1, vec![(0, Face::Marker), (2, Face::Heading)]);
        assert_eq!(rows[2].0, "### Sub em");
        assert_eq!(rows[2].1[2], (8, Face::Emphasis));
    }

    #[test]
    fn inline_faces_change_inside_a_row() {
        let rows = rows("a **b** `c` ~~d~~ *e*", 80);
        assert_eq!(rows[0].0, "a b c d e");
        assert_eq!(
            rows[0].1,
            vec![
                (0, Face::Plain),
                (2, Face::Strong),
                (3, Face::Plain),
                (4, Face::Code),
                (5, Face::Plain),
                (6, Face::Strike),
                (7, Face::Plain),
                (8, Face::Emphasis),
            ]
        );
    }

    #[test]
    fn a_face_survives_a_wrap() {
        let rows = rows("aa **bb cc** dd", 6);
        assert_eq!(rows[0].0, "aa bb ");
        assert_eq!(rows[1].0, "cc dd");
        assert_eq!(rows[1].1, vec![(0, Face::Strong), (2, Face::Plain)]);
    }

    #[test]
    fn lists_mark_items_and_indent_continuations() {
        assert_eq!(
            texts("- one\n- two words here\n  - nested\n- three", 12),
            vec!["• one", "• two words ", "  here", "  • nested", "• three"]
        );
        assert_eq!(texts("3. c\n4. d", 80), vec!["3. c", "4. d"]);
        // A loose list keeps its blank rows; an empty item shows its
        // marker.
        assert_eq!(texts("- a\n\n- b\n-\n", 80), vec!["• a", "", "• b", "•"]);
        // An item that opens with a list joins the markers, and the
        // outer item's indent is the one that pops.
        assert_eq!(
            texts("10. - a\n    - b\n11. c", 80),
            vec!["10. • a", "    • b", "11. c"]
        );
    }

    #[test]
    fn a_list_after_a_paragraph_gets_a_gap_and_a_nested_one_does_not() {
        assert_eq!(
            texts("intro\n\n- a\n  - b\n- c\n\nafter", 80),
            vec!["intro", "", "• a", "  • b", "• c", "", "after"]
        );
    }

    #[test]
    fn block_quotes_carry_a_bar_on_every_row() {
        assert_eq!(
            texts("> quoted text\n>\n> more\n\nplain", 9),
            vec!["│ quoted ", "│ text", "│", "│ more", "", "plain"]
        );
    }

    #[test]
    fn a_fence_gets_a_frame_and_a_label_and_wraps_its_rows() {
        let rows = rows("```rust\nfn main() {}\nlet x = 1234567;\n```\n", 12);
        let shown: Vec<_> = rows.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(
            shown,
            vec![
                "╭─ rust",
                "│ fn main() ",
                "│ {}",
                "│ let x = ",
                "│ 1234567;",
                "╰─"
            ]
        );
        assert_eq!(rows[1].1, vec![(0, Face::Marker), ("│ ".len(), Face::Code)]);
        // No language: the frame alone. An indented block is one too.
        assert_eq!(texts("```\nx\n```", 80), vec!["╭─", "│ x", "╰─"]);
        assert_eq!(texts("    x\n", 80), vec!["╭─", "│ x", "╰─"]);
    }

    #[test]
    fn an_open_fence_renders_as_code_to_the_end() {
        // Mid-stream: the fence has no closing line yet.
        assert_eq!(
            texts("text\n\n```py\nprint(1)\nprint(", 80),
            vec!["text", "", "╭─ py", "│ print(1)", "│ print(", "╰─"]
        );
    }

    #[test]
    fn a_blank_code_line_keeps_the_bar() {
        assert_eq!(
            texts("```\na\n\nb\n```", 80),
            vec!["╭─", "│ a", "│ ", "│ b", "╰─"]
        );
    }

    #[test]
    fn links_show_their_destination_dim_once() {
        let rows = rows("see [docs](https://x.y/d) and <https://x.y/>", 80);
        assert_eq!(rows[0].0, "see docs (https://x.y/d) and https://x.y/");
        assert_eq!(
            rows[0].1,
            vec![
                (0, Face::Plain),
                (4, Face::Link),
                (8, Face::Url),
                (24, Face::Plain),
                (29, Face::Link),
            ]
        );
        // An image with no alt text shows the source alone.
        assert_eq!(texts("![](i.png)", 80), vec!["i.png"]);
    }

    #[test]
    fn tables_align_columns_and_cut_at_the_width() {
        let source = "| name | n |\n|:--|--:|\n| ab | 1 |\n| c | 22 |\n";
        assert_eq!(
            texts(source, 80),
            vec!["name │  n", "─────┼───", "ab   │  1", "c    │ 22"]
        );
        let rows = rows(source, 80);
        assert_eq!(rows[0].1[0], (0, Face::Strong));
        // A separator that would cross the width stays out whole; a
        // cell cuts.
        assert_eq!(texts(source, 6), vec!["name", "────", "ab  ", "c   "]);
        assert_eq!(texts(source, 3), vec!["nam", "───", "ab ", "c  "]);
    }

    #[test]
    fn a_rule_spans_the_row() {
        assert_eq!(texts("a\n\n---\n\nb", 5), vec!["a", "", "─────", "", "b"]);
    }

    #[test]
    fn a_hard_break_ends_the_row_inside_the_item() {
        assert_eq!(texts("- a  \n  b", 80), vec!["• a", "  b"]);
    }

    #[test]
    fn html_passes_through_as_text() {
        assert_eq!(texts("<b>x</b> y", 80), vec!["<b>x</b> y"]);
        assert_eq!(
            texts("<div>\nx\n</div>\n", 80),
            vec!["<div>", "x", "</div>"]
        );
    }

    #[test]
    fn a_quote_that_opens_an_item_keeps_the_marker_and_the_bar() {
        assert_eq!(texts("- > careful", 80), vec!["• │ careful"]);
        assert_eq!(
            texts("1. > x\n   > y\n2. z", 80),
            vec!["1. │ x", "   │ y", "2. z"]
        );
        assert_eq!(texts("- > > deep 日本", 80), vec!["• │ │ deep 日本"]);
    }

    #[test]
    fn a_link_whose_text_crosses_a_line_break_keeps_its_destination() {
        assert_eq!(
            texts(
                "Read the [documentation for the\nAPI](http://example.test/api).",
                80
            ),
            vec![
                "Read the documentation for the",
                "API (http://example.test/api)."
            ]
        );
        // The break lands at the end of the link text.
        assert_eq!(
            texts("[text\n](http://x) tail", 80),
            vec!["text", "http://x tail"]
        );
        // An image inside a link shows both destinations.
        assert_eq!(
            texts("[![alt](i.png)](http://y)", 80),
            vec!["alt (i.png) (http://y)"]
        );
    }

    #[test]
    fn a_decoded_character_reference_cannot_put_a_control_character_back() {
        assert_eq!(texts("esc &#27;[31mRED&#13;", 80), vec!["esc [31mRED"]);
        assert_eq!(texts("csi &#155;[31m é", 80), vec!["csi [31m é"]);
        assert_eq!(texts("&#9;x", 80), vec!["    x"]);
        assert_eq!(
            texts("[a&#27;b](http://x/&#27;)", 80),
            vec!["ab (http://x/)"]
        );
    }

    #[test]
    fn a_loose_list_keeps_its_gap_after_a_nested_list() {
        assert_eq!(
            texts("- a\n\n  - b\n\n- c", 80),
            vec!["• a", "", "  • b", "", "• c"]
        );
        // A loose inner list parts its own items; the tight outer
        // list stays tight, and the gap after it goes to the block
        // that follows the list.
        assert_eq!(
            texts("- a\n  - b\n\n  - c\n- d\n", 80),
            vec!["• a", "  • b", "", "  • c", "• d"]
        );
        assert_eq!(
            texts("- a\n  - b\n\n  - c\n\ntail", 80),
            vec!["• a", "  • b", "", "  • c", "", "tail"]
        );
        // A paragraph inside a quote inside an item leaves the list
        // tight.
        assert_eq!(
            texts("- > q\n- a\n  - b", 80),
            vec!["• │ q", "• a", "  • b"]
        );
    }

    #[test]
    fn prose_keeps_no_faces() {
        let mut text = String::new();
        let mut starts = Vec::new();
        let mut faces = Vec::new();
        let ok = render(
            "plain prose\n\nand a second paragraph that wraps",
            12,
            usize::MAX,
            Target {
                text: &mut text,
                rows: &mut starts,
                faces: &mut faces,
            },
        );
        assert!(ok);
        assert!(faces.is_empty(), "{faces:?}");
        assert_eq!(starts.len(), 6);
    }

    #[test]
    fn the_cap_holds_inside_one_event() {
        // Sixty quote levels leave one column, so a paragraph of three
        // thousand characters is three thousand rows of a long
        // prefix: the cap must stop it inside the paragraph.
        let mut text = String::new();
        let mut starts = Vec::new();
        let mut faces = Vec::new();
        let source = format!("{} {}", ">".repeat(60), "x".repeat(3000));
        let ok = render(
            &source,
            120,
            4096,
            Target {
                text: &mut text,
                rows: &mut starts,
                faces: &mut faces,
            },
        );
        assert!(!ok);
        assert!(text.len() < 4096 + 512, "{}", text.len());
    }

    #[test]
    fn the_cap_stops_a_render_that_outgrows_it() {
        let mut text = String::new();
        let mut starts = Vec::new();
        let mut faces = Vec::new();
        let source = "---\n\n".repeat(100);
        let ok = render(
            &source,
            120,
            1024,
            Target {
                text: &mut text,
                rows: &mut starts,
                faces: &mut faces,
            },
        );
        assert!(!ok);
        // The overshoot is one event's worth, not the whole render.
        assert!(text.len() < 4096, "{}", text.len());
    }

    #[test]
    fn every_row_fits_the_width() {
        let source = "# Heading with several words\n\n\
                      > a quote that runs on and on past any narrow pane\n\n\
                      - item one is long enough to wrap\n  - nested item also long\n\n\
                      ```rust\nfn main() { println!(\"a long line of code that wraps\"); }\n```\n\n\
                      | col | other column |\n|---|---|\n| a | b |\n\n\
                      text with a [link](https://example.test/path) and 日本語 text\n";
        // From five columns up, every prefix fits; a narrower pane
        // shows the prefix and cuts.
        for width in [5u16, 8, 13, 21, 34, 80] {
            for (text, _) in rows(source, width) {
                assert!(
                    display_width(&text) <= usize::from(width),
                    "width {width}: {text:?}"
                );
            }
        }
    }
}
