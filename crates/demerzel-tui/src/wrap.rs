//! Display-width line wrapping for the transcript pane.
//!
//! A block lays out its rows once per width and keeps the row starts,
//! so a frame walks cached rows and wraps nothing. The core here emits
//! row starts; the slice form serves the tests and small callers.

use unicode_width::UnicodeWidthChar;

/// Bytes one row can hold past its columns: room for wide chars and
/// a few combining marks per cell. A run of zero-width chars breaks
/// here, so a row never grows without bound.
const ROW_BYTES_PER_COLUMN: usize = 8;
const ROW_BYTES_SLACK: usize = 64;

/// Feed `row` the byte offset where each row of `line` starts when
/// wrapped at `width` columns, first row at zero. A break lands after
/// the last space inside the window when one exists, else before the
/// first character that does not fit. `line` must hold no newline; the
/// transcript splits on newlines before it lays out a line.
pub fn row_starts(line: &str, width: u16, mut row: impl FnMut(usize)) {
    let width = usize::from(width.max(1));
    let max_bytes = width * ROW_BYTES_PER_COLUMN + ROW_BYTES_SLACK;
    let mut start = 0usize;
    let mut cols = 0usize;
    let mut last_space: Option<usize> = None;

    row(0);
    for (index, ch) in line.char_indices() {
        let ch_cols = ch.width().unwrap_or(0);
        if (cols + ch_cols > width || index - start >= max_bytes) && index > start {
            // Break after the space when the window holds one; the
            // space stays on the finished row.
            let break_at = match last_space {
                Some(space) if space >= start => space + 1,
                _ => index,
            };
            row(break_at);
            start = break_at;
            last_space = None;
            cols = display_width(&line[start..index]) + ch_cols;
            // A space break can carry a tail that itself overflows
            // when the current char is wide; break once more.
            if cols > width && index > start {
                row(index);
                start = index;
                cols = ch_cols;
            }
        } else {
            cols += ch_cols;
        }
        if ch == ' ' {
            last_space = Some(index);
        }
    }
}

/// Split `line` into rows of at most `width` columns.
#[cfg(test)]
pub fn wrap(line: &str, width: u16) -> Vec<&str> {
    let mut starts = Vec::new();
    row_starts(line, width, |start| starts.push(start));
    let len = line.len();
    let ends = starts.iter().skip(1).chain(std::iter::once(&len));
    starts
        .iter()
        .zip(ends)
        .map(|(&start, &end)| &line[start..end])
        .collect()
}

/// Columns `text` occupies on one row.
pub fn display_width(text: &str) -> usize {
    text.chars().map(|c| c.width().unwrap_or(0)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_lines_stay_whole() {
        assert_eq!(wrap("hello", 10), vec!["hello"]);
        assert_eq!(wrap("", 10), vec![""]);
    }

    #[test]
    fn long_lines_break_at_the_last_space() {
        assert_eq!(wrap("one two three", 8), vec!["one two ", "three"]);
    }

    #[test]
    fn unbroken_words_hard_wrap() {
        assert_eq!(wrap("abcdefgh", 3), vec!["abc", "def", "gh"]);
    }

    #[test]
    fn wide_characters_count_two_columns() {
        // Four CJK characters are eight columns.
        assert_eq!(wrap("日本語だ", 4), vec!["日本", "語だ"]);
        // A wide character never splits: it moves whole to the next row.
        assert_eq!(wrap("a日", 2), vec!["a", "日"]);
    }

    #[test]
    fn a_width_of_zero_still_terminates() {
        assert_eq!(wrap("ab", 0), vec!["a", "b"]);
    }

    #[test]
    fn a_wide_char_after_a_space_break_cannot_overflow_the_row() {
        // The tail after the space break carries nine narrow chars;
        // the wide char must move to its own row, not overflow.
        let rows = wrap(" abcdefghi日xyz", 10);
        assert_eq!(rows, vec![" ", "abcdefghi", "日xyz"]);
        for row in &rows {
            assert!(display_width(row) <= 10, "{row:?} overflows");
        }
    }

    #[test]
    fn rows_rejoin_to_the_original_line() {
        let line = "fn main() { println!(\"日本語 output with spaces\"); }";
        for width in 1..20u16 {
            assert_eq!(wrap(line, width).concat(), line, "width {width}");
        }
    }

    #[test]
    fn no_row_exceeds_the_width_for_mixed_content() {
        let line = " a日 bcd日ef 日日日 ghi";
        for width in 2..12u16 {
            for row in wrap(line, width) {
                assert!(
                    display_width(row) <= usize::from(width),
                    "width {width}: {row:?}"
                );
            }
            assert_eq!(wrap(line, width).concat(), line);
        }
    }

    #[test]
    fn a_run_of_zero_width_chars_still_breaks_into_rows() {
        // Zero-width chars fill no column, so columns alone would
        // never break the row; the byte bound does.
        let line = "\u{200b}".repeat(10_000);
        let rows = wrap(&line, 10);
        assert!(rows.len() > 100, "{} rows", rows.len());
        // The bound is checked before a char, so a row can run over
        // it by the last char's length less one.
        for row in &rows {
            assert!(row.len() <= 10 * ROW_BYTES_PER_COLUMN + ROW_BYTES_SLACK + 3);
        }
        assert_eq!(rows.concat(), line);
    }

    #[test]
    fn row_starts_are_the_slice_boundaries() {
        let line = "one two three four";
        let mut starts = Vec::new();
        row_starts(line, 8, |start| starts.push(start));
        assert_eq!(starts, vec![0, 8, 14]);
        assert_eq!(wrap(line, 8), vec!["one two ", "three ", "four"]);
    }
}
