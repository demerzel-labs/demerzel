//! Display-width line wrapping for the transcript pane.
//!
//! The transcript stores logical lines and the renderer wraps only the
//! lines the viewport shows, so a long session never pays a full
//! re-layout per frame.

use unicode_width::UnicodeWidthChar;

/// Split `line` into rows of at most `width` columns. A break lands on
/// the last space inside the window when one exists, else on the last
/// character that fits. `line` must hold no newline; the transcript
/// splits on newlines before it stores a line.
pub fn wrap(line: &str, width: u16) -> Vec<&str> {
    let width = usize::from(width.max(1));
    let mut rows = Vec::new();
    let mut start = 0usize;
    let mut cols = 0usize;
    let mut last_space: Option<usize> = None;

    for (index, ch) in line.char_indices() {
        let ch_cols = ch.width().unwrap_or(0);
        if cols + ch_cols > width && index > start {
            // Break after the space when the window holds one; the
            // space stays on the finished row.
            let break_at = match last_space {
                Some(space) if space >= start => space + 1,
                _ => index,
            };
            rows.push(&line[start..break_at]);
            start = break_at;
            last_space = None;
            cols = display_width(&line[start..index]) + ch_cols;
        } else {
            cols += ch_cols;
        }
        if ch == ' ' {
            last_space = Some(index);
        }
    }
    rows.push(&line[start..]);
    rows
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
    fn rows_rejoin_to_the_original_line() {
        let line = "fn main() { println!(\"日本語 output with spaces\"); }";
        for width in 1..20u16 {
            assert_eq!(wrap(line, width).concat(), line, "width {width}");
        }
    }
}
