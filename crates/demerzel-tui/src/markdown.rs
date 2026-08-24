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
//! the caller passes a byte cap and the render reports when it would
//! cross it, so a block of rules or padded tables cannot grow past a
//! bound. The caller then shows the source as plain text.

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

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
/// empty. Returns `false` when the rows would grow past `cap` bytes;
/// the target then holds a partial render the caller must discard.
pub(crate) fn render(source: &str, width: u16, cap: usize, target: Target<'_>) -> bool {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let mut renderer = Renderer {
        out: target,
        width: usize::from(width.max(1)),
        face: Face::Plain,
        prefix: String::new(),
        marker: None,
        marker_cols: 0,
        lists: Vec::new(),
        inline: Vec::new(),
        line: String::new(),
        line_faces: Vec::new(),
        line_face: Face::Plain,
        starts: Vec::new(),
        need_gap: false,
        link: None,
        table: None,
    };
    for event in Parser::new_ext(source, options) {
        renderer.event(event);
        if renderer.out.text.len() > cap {
            return false;
        }
    }
    renderer.flush_pending();
    renderer.out.text.len() <= cap
}

/// One list container: its next ordinal when ordered, and the
/// indent its open item pushed onto the prefix.
struct ListState {
    next: Option<u64>,
    item_cols: usize,
}

/// A table under construction: cells stay as text until the end
/// tag, when the column widths are known.
struct TableState {
    aligns: Vec<Alignment>,
    /// Every row, the header first when there is one.
    rows: Vec<Vec<Cell>>,
    has_head: bool,
}

struct Cell {
    text: String,
    faces: Vec<(usize, Face)>,
}

struct Renderer<'a> {
    out: Target<'a>,
    /// Columns in the viewport.
    width: usize,
    /// The face the output currently carries.
    face: Face,
    /// The continuation prefix of the open containers: quote bars and
    /// list indents. Every row starts with it.
    prefix: String,
    /// A list marker waiting for the item's first row, where it takes
    /// the place of the item's indent.
    marker: Option<String>,
    /// Columns the pending marker spans; the indent pushed for it.
    marker_cols: usize,
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
    /// The open link's destination and where its text starts in
    /// `line`.
    link: Option<(String, usize)>,
    table: Option<TableState>,
}

impl Renderer<'_> {
    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => {
                if self.inline.last() == Some(&Face::Code) {
                    // Inside a code block: every newline ends a row.
                    let mut rest = text.as_ref();
                    while let Some(at) = rest.find('\n') {
                        self.push_inline(&rest[..at]);
                        self.flush_line();
                        rest = &rest[at + 1..];
                    }
                    self.push_inline(rest);
                } else {
                    self.push_inline(&text);
                }
            }
            Event::Code(code) => self.push_text(&code, Face::Code),
            Event::Html(html) | Event::InlineHtml(html) => {
                // Raw markup stays as text; a newline inside an HTML
                // block ends the row.
                let mut rest = html.as_ref();
                while let Some(at) = rest.find('\n') {
                    self.push_inline(&rest[..at]);
                    self.flush_line();
                    rest = &rest[at + 1..];
                }
                self.push_inline(rest);
            }
            Event::SoftBreak | Event::HardBreak => self.flush_line(),
            Event::Rule => {
                self.flush_pending();
                self.gap();
                let cols = self.avail();
                self.begin_row();
                self.push_prefix();
                self.set_face(Face::Marker);
                for _ in 0..cols {
                    self.out.text.push('─');
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

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                self.flush_pending();
                self.gap();
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
                self.begin_row();
                self.push_prefix();
                self.push_out("╭─", Face::Marker);
                if let CodeBlockKind::Fenced(info) = kind
                    && let Some(language) = info.split_whitespace().next()
                {
                    // The label cuts at the width, like a table row.
                    let avail = self.avail();
                    let mut used = 2usize;
                    self.push_pad(1, &mut used, avail);
                    for ch in language.chars() {
                        let cols = ch.width().unwrap_or(0);
                        if used + cols > avail {
                            break;
                        }
                        self.out.text.push(ch);
                        used += cols;
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
                });
            }
            Tag::Item => {
                self.flush_pending();
                let marker = match self.lists.last_mut().and_then(|list| list.next.as_mut()) {
                    Some(next) => {
                        let marker = format!("{next}. ");
                        *next += 1;
                        marker
                    }
                    None => "• ".to_string(),
                };
                let cols = display_width(&marker);
                self.prefix.extend(std::iter::repeat_n(' ', cols));
                if let Some(list) = self.lists.last_mut() {
                    list.item_cols = cols;
                }
                // An item whose first content is a list: the markers
                // join on one row.
                match &mut self.marker {
                    Some(pending) => {
                        pending.push_str(&marker);
                        self.marker_cols += cols;
                    }
                    None => {
                        self.marker = Some(marker);
                        self.marker_cols = cols;
                    }
                }
            }
            Tag::Table(aligns) => {
                self.flush_pending();
                self.gap();
                self.table = Some(TableState {
                    aligns,
                    rows: Vec::new(),
                    has_head: false,
                });
            }
            Tag::TableHead => {
                if let Some(table) = &mut self.table {
                    table.has_head = true;
                    table.rows.push(Vec::new());
                }
            }
            Tag::TableRow => {
                if let Some(table) = &mut self.table {
                    table.rows.push(Vec::new());
                }
            }
            Tag::TableCell => {}
            Tag::Emphasis => self.inline.push(Face::Emphasis),
            Tag::Strong => self.inline.push(Face::Strong),
            Tag::Strikethrough => self.inline.push(Face::Strike),
            Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                self.link = Some((dest_url.into_string(), self.line.len()));
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
                self.begin_row();
                self.push_prefix();
                self.push_out("╰─", Face::Marker);
                self.need_gap = true;
            }
            TagEnd::List(_) => {
                self.lists.pop();
                // A list that ends another list's item leaves the
                // gap to the outer list's end.
                self.need_gap = self.lists.is_empty();
            }
            TagEnd::Item => {
                if !self.line.is_empty() {
                    self.flush_line();
                } else if let Some(marker) = self.marker.take() {
                    // An empty item still shows its marker, without
                    // the space that would follow its text.
                    self.begin_row();
                    self.set_face(Face::Marker);
                    let keep = self.prefix.len() - self.marker_cols;
                    self.out.text.push_str(&self.prefix[..keep]);
                    self.out.text.push_str(marker.trim_end());
                    self.marker_cols = 0;
                }
                let cols = self.lists.last().map_or(0, |list| list.item_cols);
                self.pop_prefix(cols);
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    self.emit_table(table);
                }
                self.need_gap = true;
            }
            TagEnd::TableHead | TagEnd::TableRow => {}
            TagEnd::TableCell => {
                let text = std::mem::take(&mut self.line);
                let faces = std::mem::take(&mut self.line_faces);
                self.line_face = Face::Plain;
                if let Some(row) = self.table.as_mut().and_then(|table| table.rows.last_mut()) {
                    row.push(Cell { text, faces });
                }
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.inline.pop();
            }
            TagEnd::Link | TagEnd::Image => {
                self.inline.pop();
                if let Some((dest, from)) = self.link.take()
                    && !dest.is_empty()
                    && self.line[from..] != dest
                {
                    let shown = if self.line.len() > from {
                        format!(" ({dest})")
                    } else {
                        dest
                    };
                    self.push_text(&shown, Face::Url);
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

    /// Append text to the line under construction.
    fn push_text(&mut self, text: &str, face: Face) {
        if text.is_empty() {
            return;
        }
        if face != self.line_face {
            self.line_faces.push((self.line.len(), face));
            self.line_face = face;
        }
        self.line.push_str(text);
    }

    /// Text that goes to the output row directly, not through the
    /// line: frames and markers.
    fn push_out(&mut self, text: &str, face: Face) {
        self.set_face(face);
        self.out.text.push_str(text);
    }

    /// Change the face from here on. A change before any text lands
    /// under the last one replaces it, so no entry covers nothing.
    fn set_face(&mut self, face: Face) {
        if face == self.face {
            return;
        }
        let at = self.out.text.len() as u32;
        match self.out.faces.last_mut() {
            Some(last) if last.0 == at => last.1 = face,
            _ => self.out.faces.push((at, face)),
        }
        self.face = face;
    }

    fn begin_row(&mut self) {
        self.out.rows.push(self.out.text.len() as u32);
    }

    /// The container prefix at the start of a row. A pending list
    /// marker takes the place of its item's indent, once.
    fn push_prefix(&mut self) {
        self.set_face(Face::Marker);
        match self.marker.take() {
            Some(marker) => {
                let keep = self.prefix.len() - self.marker_cols;
                self.out.text.push_str(&self.prefix[..keep]);
                self.out.text.push_str(&marker);
            }
            None => self.out.text.push_str(&self.prefix),
        }
        self.marker_cols = 0;
    }

    /// The blank row between blocks: the prefix without its trailing
    /// spaces, so a quote's bar continues.
    fn gap(&mut self) {
        if self.need_gap {
            self.need_gap = false;
            if !self.out.rows.is_empty() {
                self.begin_row();
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
        let line = std::mem::take(&mut self.line);
        let faces = std::mem::take(&mut self.line_faces);
        let avail = u16::try_from(self.avail()).unwrap_or(u16::MAX);
        let mut starts = std::mem::take(&mut self.starts);
        starts.clear();
        row_starts(&line, avail, |start| starts.push(start));
        for (index, &from) in starts.iter().enumerate() {
            let to = starts.get(index + 1).copied().unwrap_or(line.len());
            self.begin_row();
            self.push_prefix();
            self.push_span(&line, &faces, from, to);
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
        let columns = table.rows.iter().map(Vec::len).max().unwrap_or(0);
        if columns == 0 {
            return;
        }
        let mut widths = vec![1usize; columns];
        for row in &table.rows {
            for (index, cell) in row.iter().enumerate() {
                widths[index] = widths[index].max(display_width(&cell.text));
            }
        }
        let avail = self.avail();
        for (index, row) in table.rows.iter().enumerate() {
            let head = table.has_head && index == 0;
            self.begin_row();
            self.push_prefix();
            let mut used = 0usize;
            for (column, width) in widths.iter().enumerate() {
                if column > 0 && !self.push_cut(" │ ", Face::Marker, &mut used, avail) {
                    break;
                }
                let empty = Cell {
                    text: String::new(),
                    faces: Vec::new(),
                };
                let cell = row.get(column).unwrap_or(&empty);
                let pad = width - display_width(&cell.text);
                let (left, right) = match table.aligns.get(column) {
                    Some(Alignment::Right) => (pad, 0),
                    Some(Alignment::Center) => (pad / 2, pad - pad / 2),
                    _ => (0, pad),
                };
                if !self.push_pad(left, &mut used, avail) {
                    break;
                }
                if !self.push_cell(cell, head, &mut used, avail) {
                    break;
                }
                if !self.push_pad(right, &mut used, avail) {
                    break;
                }
            }
            if head {
                self.begin_row();
                self.push_prefix();
                self.set_face(Face::Marker);
                let mut used = 0usize;
                for (column, width) in widths.iter().enumerate() {
                    if column > 0 && !self.push_cut("─┼─", Face::Marker, &mut used, avail) {
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
    fn push_cell(&mut self, cell: &Cell, head: bool, used: &mut usize, avail: usize) -> bool {
        let mut next = 0usize;
        let mut face = Face::Plain;
        for (at, ch) in cell.text.char_indices() {
            while next < cell.faces.len() && cell.faces[next].0 <= at {
                face = cell.faces[next].1;
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

    /// Append `text` inside the cut, whole or not at all.
    fn push_cut(&mut self, text: &str, face: Face, used: &mut usize, avail: usize) -> bool {
        let cols = display_width(text);
        if *used + cols > avail {
            return false;
        }
        self.push_out(text, face);
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
