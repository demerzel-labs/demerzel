//! Drag selection over the transcript, the copy it yields, and the
//! clipboard write.
//!
//! A selection is two points in transcript rows and columns: the
//! press and the pointer. A point names a block by its absolute
//! index, a row of that block's layout, and a screen column, so it
//! holds still while content arrives below it and moves with nothing
//! else. The transcript owns the points and clears them when a block
//! under them changes; this module holds the geometry, the copy
//! rules, and the OSC 52 sequence.
//!
//! The copy holds the shown rows between the points, without the
//! marks the frame adds: a fence gives its code without the frame, a
//! card gives its output without its header and indent, a prompt
//! echo gives the line without the prompt. A newline joins each row,
//! trailing spaces drop, and the text is capped at [`COPY_CAP`]
//! bytes. The copy goes to the terminal as one OSC 52 write, which
//! reaches the clipboard over SSH and through a multiplexer that
//! forwards the sequence. The base64 encoder lives here, so the
//! write adds no dependency.

use std::io::Write as _;

use unicode_width::UnicodeWidthChar as _;

use crate::PROMPT;
use crate::markdown::Face;
use crate::transcript::{Block, Kind};
use crate::wrap::display_width;

/// Bytes of text one copy holds at most. The write blocks the loop,
/// and terminals cap the sequence near this size.
pub(crate) const COPY_CAP: usize = 64 * 1024;

/// One end of a selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Point {
    /// The absolute block index.
    pub block: usize,
    /// The row of the block's layout.
    pub row: usize,
    /// The screen column.
    pub col: usize,
}

/// The press and the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Selection {
    pub anchor: Point,
    pub head: Point,
}

impl Selection {
    /// The two points in order.
    pub fn span(&self) -> (Point, Point) {
        if self.head < self.anchor {
            (self.head, self.anchor)
        } else {
            (self.anchor, self.head)
        }
    }

    /// Whether the points differ: a click without a drag selects
    /// nothing.
    pub fn is_span(&self) -> bool {
        self.anchor != self.head
    }

    /// Whether `block` lies between the two ends, inclusive.
    pub fn covers(&self, block: usize) -> bool {
        let (start, end) = self.span();
        (start.block..=end.block).contains(&block)
    }

    /// The columns of row `row` of block `block` inside the
    /// selection, as `from..to`, or `None` when the row is outside.
    /// A row strictly between the ends is whole; the last row runs
    /// through the column under the pointer.
    pub fn columns(&self, block: usize, row: usize) -> Option<(usize, usize)> {
        if !self.is_span() {
            return None;
        }
        let (start, end) = self.span();
        let at = (block, row);
        if at < (start.block, start.row) || at > (end.block, end.row) {
            return None;
        }
        let from = if at == (start.block, start.row) {
            start.col
        } else {
            0
        };
        let to = if at == (end.block, end.row) {
            end.col.saturating_add(1)
        } else {
            usize::MAX
        };
        Some((from, to))
    }
}

/// The text one release yields.
pub(crate) struct Copied {
    pub text: String,
    /// The text reached the cap; the rest of the selection dropped.
    pub cut: bool,
}

/// Whether a char at column `col` with width `width` lies inside
/// `from..to`. A zero-width char joins the char before it.
fn inside(col: usize, width: usize, from: usize, to: usize) -> bool {
    if width == 0 {
        col > from && col <= to
    } else {
        col < to && col + width > from
    }
}

/// The byte range of `text` inside columns `from..to`, with `col`
/// the column `text` starts at; `col` advances past the text. The
/// range is contiguous: the chars inside a column window are.
pub(crate) fn window(text: &str, from: usize, to: usize, col: &mut usize) -> (usize, usize) {
    let mut start = None;
    let mut end = 0usize;
    for (at, ch) in text.char_indices() {
        let width = ch.width().unwrap_or(0);
        if inside(*col, width, from, to) {
            start.get_or_insert(at);
            end = at + ch.len_utf8();
        }
        *col += width;
    }
    match start {
        Some(start) => (start, end),
        None => (text.len(), text.len()),
    }
}

/// Append row `index` of `block`, cut to columns `from..to`, as one
/// line of the copy: the chars inside the window that are no mark,
/// trailing spaces dropped, a newline after. A card's header row and
/// a row of marks alone — a fence frame, a rule — add nothing.
pub(crate) fn copy_row(block: &Block, index: usize, from: usize, to: usize, out: &mut String) {
    let start = out.len();
    // The columns the block's own frame takes at the row's start: a
    // card's indent on each of its lines, the prompt on an echo.
    let skip = match block.kind {
        Kind::Tool if block.line_start(index) => 2,
        Kind::Prompt if index == 0 && block.row(0).starts_with(PROMPT) => display_width(PROMPT),
        _ => 0,
    };
    let mut col = 0usize;
    let mut header = false;
    let mut first = true;
    let mut content = false;
    let mut push = |face: Face, text: &str| {
        if first {
            first = false;
            header = face.has(Face::TITLE);
        }
        let mark = face.has(Face::MARKER);
        for ch in text.chars() {
            let width = ch.width().unwrap_or(0);
            let at = col;
            col += width;
            if mark || at < skip {
                continue;
            }
            content = true;
            if inside(at, width, from, to) {
                out.push(ch);
            }
        }
    };
    if block.plain() {
        push(Face::PLAIN, block.row(index));
    } else {
        block.spans(index, push);
    }
    if header || (!content && col > 0) {
        out.truncate(start);
        return;
    }
    while out.len() > start && out.ends_with(' ') {
        out.pop();
    }
    out.push('\n');
}

/// The OSC 52 sequence that puts `text` on the clipboard.
pub(crate) fn osc52(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len().div_ceil(3) * 4 + 16);
    out.extend_from_slice(b"\x1b]52;c;");
    base64(text.as_bytes(), &mut out);
    out.push(0x07);
    out
}

/// Write `text` to the clipboard through the terminal, as one OSC 52
/// sequence on stdout.
pub(crate) fn write_clipboard(text: &str) -> std::io::Result<()> {
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&osc52(text))?;
    stdout.flush()
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Append the standard base64 form of `input`, with padding.
fn base64(input: &[u8], out: &mut Vec<u8>) {
    for chunk in input.chunks(3) {
        let bytes = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let bits = u32::from(bytes[0]) << 16 | u32::from(bytes[1]) << 8 | u32::from(bytes[2]);
        out.push(ALPHABET[(bits >> 18 & 63) as usize]);
        out.push(ALPHABET[(bits >> 12 & 63) as usize]);
        out.push(if chunk.len() > 1 {
            ALPHABET[(bits >> 6 & 63) as usize]
        } else {
            b'='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(bits & 63) as usize]
        } else {
            b'='
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(block: usize, row: usize, col: usize) -> Point {
        Point { block, row, col }
    }

    #[test]
    fn the_span_orders_the_points_and_names_the_columns() {
        let selection = Selection {
            anchor: point(3, 1, 7),
            head: point(1, 2, 4),
        };
        assert_eq!(selection.span(), (point(1, 2, 4), point(3, 1, 7)));
        assert!(selection.is_span());
        assert!(selection.covers(2));
        assert!(!selection.covers(4));
        // The first row from its column, the rows between whole, the
        // last row through the pointer's column.
        assert_eq!(selection.columns(1, 2), Some((4, usize::MAX)));
        assert_eq!(selection.columns(1, 3), Some((0, usize::MAX)));
        assert_eq!(selection.columns(2, 0), Some((0, usize::MAX)));
        assert_eq!(selection.columns(3, 1), Some((0, 8)));
        assert_eq!(selection.columns(1, 1), None);
        assert_eq!(selection.columns(3, 2), None);
        // One row: from the press through the pointer.
        let row = Selection {
            anchor: point(2, 0, 9),
            head: point(2, 0, 3),
        };
        assert_eq!(row.columns(2, 0), Some((3, 10)));
        // A click selects nothing.
        let click = Selection {
            anchor: point(2, 0, 3),
            head: point(2, 0, 3),
        };
        assert!(!click.is_span());
        assert_eq!(click.columns(2, 0), None);
    }

    #[test]
    fn the_window_cuts_at_columns_and_keeps_wide_chars_whole() {
        let mut col = 0;
        assert_eq!(window("abcdef", 2, 4, &mut col), (2, 4));
        assert_eq!(col, 6);
        // The column count runs across spans.
        let mut col = 4;
        assert_eq!(window("xyz", 2, 6, &mut col), (0, 2));
        // A wide char under either edge stays in.
        let mut col = 0;
        assert_eq!(window("日本語", 1, 3, &mut col), (0, 6));
        // Nothing inside: an empty range at the end.
        let mut col = 0;
        assert_eq!(window("ab", 5, 9, &mut col), (2, 2));
        // A combining mark follows its char.
        let mut col = 0;
        assert_eq!(window("e\u{301}x", 0, 1, &mut col), (0, 3));
        let mut col = 0;
        assert_eq!(window("e\u{301}x", 1, 2, &mut col), (3, 4));
    }

    #[test]
    fn base64_matches_the_standard_form() {
        let encode = |text: &str| {
            let mut out = Vec::new();
            base64(text.as_bytes(), &mut out);
            String::from_utf8(out).unwrap()
        };
        assert_eq!(encode(""), "");
        assert_eq!(encode("f"), "Zg==");
        assert_eq!(encode("fo"), "Zm8=");
        assert_eq!(encode("foo"), "Zm9v");
        assert_eq!(encode("foobar"), "Zm9vYmFy");
        assert_eq!(encode("日"), "5pel");
        assert_eq!(
            String::from_utf8(osc52("hi")).unwrap(),
            "\x1b]52;c;aGk=\x07"
        );
    }
}
