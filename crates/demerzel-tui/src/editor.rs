//! The input editor: a buffer of several lines, a cursor, the row
//! layout of the box, and command completion.
//!
//! The editor is pure state; the loop maps keys onto these operations
//! and the renderer draws the rows. The buffer holds newlines: a
//! newline key and a paste insert them, and the box shows each line
//! on rows of its own. Rows wrap hard at the box width, so every
//! character stays on screen. The layout is a cache: an edit or a
//! width change rebuilds it, and a frame walks only the rows it
//! shows.
//!
//! A line that starts with `/` opens the completion popup: the
//! commands whose name starts with the line. The loop routes the
//! selection keys here while the popup is open. What a command name
//! looks like is the driver's business: the popup matches the names
//! it was given and states no rule of its own.
//!
//! The row readers — `row`, `cursor_row`, `cursor_col`,
//! `scroll_to_cursor` — read the layout and run after `sync` in the
//! same frame; a debug build checks that.

use unicode_width::UnicodeWidthChar as _;

use crate::wrap::{ROW_BYTES_PER_COLUMN, ROW_BYTES_SLACK};

/// Row-cache capacity kept after a submit or a shrink. A large paste
/// grows the cache once; the next prompt must not keep that memory.
const KEPT_ROWS: usize = 64;

/// One command the popup can complete: the usage line and the help
/// line, the same two the `/help` reply shows. The popup lists the
/// commands in the order the driver gave them.
#[derive(Debug, Clone)]
pub struct CommandSpec {
    usage: String,
    help: String,
    /// Bytes of `usage` that are the command name.
    name_len: usize,
    /// The usage line names arguments after the command.
    takes_args: bool,
}

impl CommandSpec {
    /// `usage` starts with the command name, then the arguments the
    /// command takes, if any: `/model [name]`. The name starts with
    /// `/`, as the popup opens on that char; leading whitespace is
    /// dropped. `help` is one line.
    pub fn new(usage: impl Into<String>, help: impl Into<String>) -> Self {
        let mut usage = usage.into();
        let lead = usage.len() - usage.trim_start().len();
        usage.drain(..lead);
        let mut words = usage.split_whitespace();
        let name_len = words.next().map_or(0, str::len);
        let takes_args = words.next().is_some();
        Self {
            usage,
            help: help.into(),
            name_len,
            takes_args,
        }
    }

    /// The usage line.
    pub fn usage(&self) -> &str {
        &self.usage
    }

    /// The help line.
    pub fn help(&self) -> &str {
        &self.help
    }

    /// The command name: the usage line's first word.
    pub fn name(&self) -> &str {
        &self.usage[..self.name_len]
    }
}

/// What Enter did with the popup open.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Completion {
    /// The selected command went onto the line.
    Applied,
    /// The line held the selected command already: submit it.
    OnTheLine,
    /// The popup is closed; nothing changed.
    NoMatch,
}

/// The editor: the buffer, the cursor, the row layout, the popup.
/// `cursor` is a byte index on a char boundary.
#[derive(Debug, Default)]
pub(crate) struct Editor {
    buffer: String,
    cursor: usize,
    /// The rows at `width` text columns, as byte ranges of the
    /// buffer. A row ends before the newline that ends its line. The
    /// cursor at the end of a full row sits one column past it; the
    /// box keeps that column free.
    rows: Vec<(usize, usize)>,
    /// Text columns the layout used. Zero before the first frame: one
    /// row per line.
    width: u16,
    /// The buffer changed since the layout.
    dirty: bool,
    /// The first row the box shows.
    top: usize,
    /// The commands the popup lists.
    commands: Vec<CommandSpec>,
    /// The selected match while the popup is open.
    selected: usize,
    /// Esc closed the popup; the next edit opens it again.
    dismissed: bool,
}

impl Editor {
    pub(crate) fn new(commands: Vec<CommandSpec>) -> Self {
        Self {
            commands,
            ..Self::default()
        }
    }

    pub(crate) fn text(&self) -> &str {
        &self.buffer
    }

    #[cfg(test)]
    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Take the buffer and reset the editor. The rows described the
    /// taken buffer; they go with it.
    pub(crate) fn take(&mut self) -> String {
        self.cursor = 0;
        self.top = 0;
        self.rows.clear();
        self.rows.shrink_to(KEPT_ROWS);
        self.edited();
        std::mem::take(&mut self.buffer)
    }

    /// Replace the whole buffer, cursor at the end.
    pub(crate) fn set(&mut self, text: &str) {
        self.buffer.clear();
        self.buffer.push_str(text);
        self.cursor = self.buffer.len();
        self.edited();
    }

    /// Show a history entry: [`Editor::set`] with the popup closed,
    /// so a recalled command does not take the arrows from the
    /// history. The next edit opens the popup again.
    pub(crate) fn recall(&mut self, text: &str) {
        self.set(text);
        self.dismissed = true;
    }

    pub(crate) fn insert_char(&mut self, ch: char) {
        self.buffer.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
        self.edited();
    }

    pub(crate) fn insert_str(&mut self, text: &str) {
        self.buffer.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.edited();
    }

    /// Delete the char before the cursor: a newline joins the lines.
    pub(crate) fn backspace(&mut self) {
        if let Some(ch) = self.prev_char() {
            self.cursor -= ch.len_utf8();
            self.buffer.remove(self.cursor);
            self.edited();
        }
    }

    /// Delete the char under the cursor.
    pub(crate) fn delete(&mut self) {
        if self.cursor < self.buffer.len() {
            self.buffer.remove(self.cursor);
            self.edited();
        }
    }

    pub(crate) fn left(&mut self) {
        if let Some(ch) = self.prev_char() {
            self.cursor -= ch.len_utf8();
        }
    }

    pub(crate) fn right(&mut self) {
        if let Some(ch) = self.next_char() {
            self.cursor += ch.len_utf8();
        }
    }

    /// The start of the cursor's line.
    pub(crate) fn home(&mut self) {
        self.cursor = self.line_start();
    }

    /// The end of the cursor's line.
    pub(crate) fn end(&mut self) {
        self.cursor = self.line_end();
    }

    /// Move the cursor one row up, at the same column. Returns false
    /// on the first row, where the key means history.
    pub(crate) fn up(&mut self) -> bool {
        self.ensure_layout();
        let row = self.cursor_row();
        if row == 0 {
            return false;
        }
        self.cursor = self.offset_at(row - 1, self.cursor_col());
        true
    }

    /// Move the cursor one row down, at the same column. Returns
    /// false on the last row, where the key means history.
    pub(crate) fn down(&mut self) -> bool {
        self.ensure_layout();
        let row = self.cursor_row();
        if row + 1 >= self.rows.len() {
            return false;
        }
        self.cursor = self.offset_at(row + 1, self.cursor_col());
        true
    }

    /// Delete from the line start to the cursor.
    pub(crate) fn kill_to_start(&mut self) {
        let start = self.line_start();
        if start < self.cursor {
            self.buffer.drain(start..self.cursor);
            self.cursor = start;
            self.edited();
        }
    }

    /// Delete from the cursor to the line end. At the line end,
    /// delete the newline: the next line joins this one.
    pub(crate) fn kill_to_end(&mut self) {
        let end = self.line_end();
        if end > self.cursor {
            self.buffer.drain(self.cursor..end);
            self.edited();
        } else if self.next_char() == Some('\n') {
            self.buffer.remove(self.cursor);
            self.edited();
        }
    }

    /// Delete the word before the cursor: trailing spaces, then the
    /// non-space run, inside the line. At the line start, delete the
    /// newline.
    pub(crate) fn kill_word_back(&mut self) {
        let line_start = self.line_start();
        if line_start == self.cursor {
            self.backspace();
            return;
        }
        let head = &self.buffer[line_start..self.cursor];
        let trimmed = head.trim_end_matches(' ');
        let start = line_start + trimmed.rfind(' ').map(|space| space + 1).unwrap_or(0);
        self.buffer.drain(start..self.cursor);
        self.cursor = start;
        self.edited();
    }

    /// Lay the rows out at `width` text columns when the buffer or
    /// the width changed. Returns the row count.
    pub(crate) fn sync(&mut self, width: u16) -> usize {
        if self.dirty || self.width != width || self.rows.is_empty() {
            self.layout(width);
        }
        self.rows.len()
    }

    /// Move the box so the cursor row is inside its `height` rows.
    /// Returns the first row the box shows. Call after [`Editor::sync`].
    pub(crate) fn scroll_to_cursor(&mut self, height: usize) -> usize {
        self.assert_laid_out();
        let height = height.max(1);
        let row = self.cursor_row();
        if row < self.top {
            self.top = row;
        } else if row >= self.top + height {
            self.top = row + 1 - height;
        }
        self.top = self.top.min(self.rows.len().saturating_sub(height));
        self.top
    }

    /// The text of row `index`, without its newline. Call after
    /// [`Editor::sync`].
    pub(crate) fn row(&self, index: usize) -> &str {
        self.assert_laid_out();
        let (start, end) = self.rows[index];
        &self.buffer[start..end]
    }

    /// The row the cursor is on. Call after [`Editor::sync`].
    pub(crate) fn cursor_row(&self) -> usize {
        self.assert_laid_out();
        // The first row starts at zero, so the cursor is at or past
        // one row start.
        self.rows
            .partition_point(|&(start, _)| start <= self.cursor)
            .saturating_sub(1)
    }

    /// The cursor's column inside its row. Call after
    /// [`Editor::sync`].
    pub(crate) fn cursor_col(&self) -> usize {
        let (start, _) = self.rows[self.cursor_row()];
        self.buffer[start..self.cursor]
            .chars()
            .map(displayed_width)
            .sum()
    }

    /// The commands the popup lists, when it is open: the ones whose
    /// name starts with the line. A line with a space, a newline, or
    /// any char outside a name matches no name, so the popup closes
    /// by itself. Each test is bounded by the name, not the line.
    pub(crate) fn matches(&self) -> impl Iterator<Item = &CommandSpec> {
        let open = !self.dismissed && self.buffer.starts_with('/');
        self.commands
            .iter()
            .filter(move |command| open && command.name().starts_with(&self.buffer))
    }

    /// The selected match's index among [`Editor::matches`].
    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    pub(crate) fn popup_open(&self) -> bool {
        self.matches().next().is_some()
    }

    pub(crate) fn select_next(&mut self) {
        let count = self.matches().count();
        if count > 0 {
            self.selected = (self.selected + 1) % count;
        }
    }

    pub(crate) fn select_prev(&mut self) {
        let count = self.matches().count();
        if count > 0 {
            self.selected = (self.selected + count - 1) % count;
        }
    }

    /// Close the popup until the next edit.
    pub(crate) fn dismiss(&mut self) {
        self.dismissed = true;
    }

    /// Put the selected command on the line, with a space after a
    /// command that takes arguments. When the line holds the command
    /// already, the key that completes submits instead.
    pub(crate) fn complete(&mut self) -> Completion {
        let Some(command) = self.matches().nth(self.selected) else {
            return Completion::NoMatch;
        };
        if self.buffer == command.name() {
            return Completion::OnTheLine;
        }
        let mut text = command.name().to_string();
        if command.takes_args {
            text.push(' ');
        }
        self.set(&text);
        Completion::Applied
    }

    /// Every edit: the layout is stale, the popup starts over. An
    /// emptied buffer gives its capacity back, so a large paste that
    /// was killed does not stay allocated.
    fn edited(&mut self) {
        self.dirty = true;
        self.selected = 0;
        self.dismissed = false;
        if self.buffer.is_empty() {
            self.buffer = String::new();
        }
    }

    /// The row readers' precondition: the rows describe this buffer.
    fn assert_laid_out(&self) {
        debug_assert!(
            !self.dirty && !self.rows.is_empty(),
            "the row readers run after sync"
        );
    }

    fn ensure_layout(&mut self) {
        if self.dirty || self.rows.is_empty() {
            self.layout(self.width);
        }
    }

    /// Rebuild the rows at `width` text columns. Linear in the
    /// buffer.
    fn layout(&mut self, width: u16) {
        // Before the first frame the width is unknown: one row per
        // line, so the history keys work from the first key.
        let cols_per_row = if width == 0 {
            usize::MAX / ROW_BYTES_PER_COLUMN
        } else {
            usize::from(width)
        };
        let max_bytes = cols_per_row
            .saturating_mul(ROW_BYTES_PER_COLUMN)
            .saturating_add(ROW_BYTES_SLACK);
        self.rows.clear();
        let mut line_start = 0;
        loop {
            let line_end = self.buffer[line_start..]
                .find('\n')
                .map_or(self.buffer.len(), |at| line_start + at);
            let mut start = line_start;
            let mut cols = 0;
            for (offset, ch) in self.buffer[line_start..line_end].char_indices() {
                let index = line_start + offset;
                let ch_cols = displayed_width(ch);
                if (cols + ch_cols > cols_per_row || index - start >= max_bytes) && index > start {
                    self.rows.push((start, index));
                    start = index;
                    cols = 0;
                }
                cols += ch_cols;
            }
            self.rows.push((start, line_end));
            if line_end == self.buffer.len() {
                break;
            }
            line_start = line_end + 1;
        }
        // A paste that grew the cache and left does not keep it.
        let kept = self.rows.len().max(KEPT_ROWS);
        if self.rows.capacity() > 2 * kept {
            self.rows.shrink_to(kept);
        }
        self.width = width;
        self.dirty = false;
    }

    /// The byte offset in row `row` at column `col`: the start of the
    /// char under that column, else the row end. The end of a row
    /// that wraps is the next row's start, so there the last char's
    /// start is the answer; the row's own end stays on a row that
    /// ends at a newline or at the buffer end.
    fn offset_at(&self, row: usize, col: usize) -> usize {
        let (start, end) = self.rows[row];
        let mut cols = 0;
        let mut last = start;
        for (offset, ch) in self.buffer[start..end].char_indices() {
            let ch_cols = displayed_width(ch);
            if cols >= col || cols + ch_cols > col {
                return start + offset;
            }
            cols += ch_cols;
            last = start + offset;
        }
        let wraps = end < self.buffer.len() && self.buffer.as_bytes()[end] != b'\n';
        if wraps { last } else { end }
    }

    fn line_start(&self) -> usize {
        self.buffer[..self.cursor]
            .rfind('\n')
            .map_or(0, |at| at + 1)
    }

    fn line_end(&self) -> usize {
        self.buffer[self.cursor..]
            .find('\n')
            .map_or(self.buffer.len(), |at| self.cursor + at)
    }

    fn prev_char(&self) -> Option<char> {
        self.buffer[..self.cursor].chars().next_back()
    }

    fn next_char(&self) -> Option<char> {
        self.buffer[self.cursor..].chars().next()
    }
}

/// The printable stand-in for one buffer char: a tab shows as a
/// mark, a control char shows as nothing.
pub(crate) fn displayed(ch: char) -> Option<char> {
    match ch {
        '\t' => Some('⇥'),
        ch if ch.is_control() => None,
        ch => Some(ch),
    }
}

/// Columns one buffer char takes on a row.
fn displayed_width(ch: char) -> usize {
    displayed(ch).map_or(0, |shown| shown.width().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(text: &str) -> Editor {
        let mut editor = Editor::new(Vec::new());
        editor.set(text);
        editor
    }

    fn rows(editor: &mut Editor, width: u16) -> Vec<&str> {
        let count = editor.sync(width);
        (0..count).map(|index| editor.row(index)).collect()
    }

    fn commands() -> Vec<CommandSpec> {
        vec![
            CommandSpec::new("/help", "list the commands"),
            CommandSpec::new("/model [name]", "show or switch the session model"),
            CommandSpec::new("/exit", "end the session"),
        ]
    }

    #[test]
    fn insert_and_take_round_trip() {
        let mut e = Editor::new(Vec::new());
        for ch in "héllo".chars() {
            e.insert_char(ch);
        }
        assert_eq!(e.text(), "héllo");
        assert_eq!(e.take(), "héllo");
        assert!(e.is_empty());
        assert_eq!(e.cursor(), 0);
    }

    #[test]
    fn backspace_and_delete_respect_char_boundaries() {
        let mut e = editor("aé日");
        e.backspace();
        assert_eq!(e.text(), "aé");
        e.left();
        e.delete();
        assert_eq!(e.text(), "a");
    }

    #[test]
    fn movement_stops_at_the_edges_and_crosses_lines() {
        let mut e = editor("ab");
        e.right();
        assert_eq!(e.cursor(), 2);
        e.home();
        e.left();
        assert_eq!(e.cursor(), 0);
        e.right();
        assert_eq!(e.cursor(), 1);

        let mut e = editor("ab\ncd");
        e.home();
        assert_eq!(e.cursor(), 3);
        e.left();
        assert_eq!(e.cursor(), 2);
        e.end();
        assert_eq!(e.cursor(), 2);
        e.right();
        assert_eq!(e.cursor(), 3);
    }

    #[test]
    fn mid_line_insert_lands_at_the_cursor() {
        let mut e = editor("ad");
        e.left();
        e.insert_str("bc");
        assert_eq!(e.text(), "abcd");
        e.right();
        assert_eq!(e.cursor(), 4);
    }

    #[test]
    fn kill_operations_stay_inside_the_line() {
        let mut e = editor("one two three");
        e.kill_word_back();
        assert_eq!(e.text(), "one two ");
        e.kill_word_back();
        assert_eq!(e.text(), "one ");
        e.home();
        e.kill_to_end();
        assert_eq!(e.text(), "");

        let mut e = editor("keep drop");
        for _ in 0..4 {
            e.left();
        }
        e.kill_to_start();
        assert_eq!(e.text(), "drop");
        assert_eq!(e.cursor(), 0);

        let mut e = editor("first\nsecond");
        e.kill_to_start();
        assert_eq!(e.text(), "first\n");
        e.kill_word_back();
        assert_eq!(e.text(), "first");
        e.home();
        e.kill_to_end();
        assert_eq!(e.text(), "");
    }

    #[test]
    fn kill_to_end_at_a_line_end_joins_the_next_line() {
        let mut e = editor("first\nsecond");
        e.up();
        e.end();
        e.kill_to_end();
        assert_eq!(e.text(), "firstsecond");
        assert_eq!(e.cursor(), 5);
    }

    #[test]
    fn a_newline_makes_a_row_and_the_arrows_cross_rows() {
        let mut e = editor("ab");
        e.insert_char('\n');
        e.insert_str("cde");
        assert_eq!(rows(&mut e, 20), vec!["ab", "cde"]);
        assert_eq!(e.cursor_row(), 1);
        assert_eq!(e.cursor_col(), 3);
        // Up keeps the column when the row is long enough, else the
        // row end.
        assert!(e.up());
        assert_eq!(e.cursor(), 2);
        assert_eq!(e.cursor_row(), 0);
        assert!(!e.up());
        assert!(e.down());
        assert_eq!(e.cursor(), 5);
        assert!(!e.down());
        e.home();
        e.right();
        assert!(e.up());
        assert_eq!(e.cursor(), 1);
    }

    #[test]
    fn rows_wrap_hard_at_the_width() {
        let mut e = editor("abcdefgh");
        assert_eq!(rows(&mut e, 3), vec!["abc", "def", "gh"]);
        assert_eq!(e.cursor_row(), 2);
        assert_eq!(e.cursor_col(), 2);
        // The cursor after a full row sits one column past it; at a
        // row break it sits on the next row.
        let mut e = editor("abcdef");
        assert_eq!(rows(&mut e, 3), vec!["abc", "def"]);
        assert_eq!(e.cursor_row(), 1);
        assert_eq!(e.cursor_col(), 3);
        for _ in 0..3 {
            e.left();
        }
        assert_eq!(e.cursor_row(), 1);
        assert_eq!(e.cursor_col(), 0);
        // Wide chars move whole to the next row; a tab shows as one
        // column.
        let mut e = editor("a日本\tb");
        assert_eq!(rows(&mut e, 3), vec!["a日", "本\t", "b"]);
        e.left();
        assert_eq!((e.cursor_row(), e.cursor_col()), (2, 0));
        e.left();
        assert_eq!((e.cursor_row(), e.cursor_col()), (1, 2));
        // An empty line is one empty row.
        let mut e = editor("a\n\nb");
        assert_eq!(rows(&mut e, 10), vec!["a", "", "b"]);
        assert!(e.up());
        assert_eq!(e.cursor(), 2);
    }

    #[test]
    fn a_width_change_relays_and_the_arrows_use_the_layout() {
        let mut e = editor("abcdef");
        e.sync(3);
        e.home();
        assert!(!e.up());
        e.end();
        // The end of a wrapped row is the next row's start, so Up
        // from the end of the second row lands on the first row's
        // last char.
        assert!(e.up());
        assert_eq!(e.cursor(), 2);
        assert_eq!(e.cursor_row(), 0);
        assert_eq!(e.sync(10), 1);
        assert!(!e.up());
    }

    #[test]
    fn the_arrows_cross_a_wrapped_row_one_row_at_a_time() {
        // Line 1 is exactly one row wide: Down from its end must land
        // on the next row, not skip it.
        let mut e = editor("abc\ndefg");
        e.sync(3);
        e.up();
        e.up();
        e.end();
        assert_eq!((e.cursor_row(), e.cursor_col()), (0, 3));
        assert!(e.down());
        assert_eq!((e.cursor_row(), e.cursor_col()), (1, 2));
        assert!(e.down());
        assert_eq!((e.cursor_row(), e.cursor_col()), (2, 1));
        // A row of zero-width chars: Up moves, and at the top it
        // yields to the history.
        let mut e = editor(&"\u{200b}".repeat(200));
        e.sync(10);
        assert!(e.cursor_row() > 0);
        while e.up() {}
        assert_eq!(e.cursor_row(), 0);
    }

    #[test]
    fn take_and_a_kill_give_the_capacity_back() {
        let mut e = editor(&"x\n".repeat(4_000));
        e.sync(20);
        assert!(e.rows.capacity() >= 4_000);
        e.take();
        assert!(e.rows.capacity() <= KEPT_ROWS);
        assert_eq!(e.buffer.capacity(), 0);
        e.set(&"x".repeat(8_000));
        e.sync(20);
        assert!(e.rows.capacity() >= 400);
        e.kill_to_start();
        assert_eq!(e.text(), "");
        assert_eq!(e.buffer.capacity(), 0);
        e.sync(20);
        assert!(e.rows.capacity() <= 2 * KEPT_ROWS);
    }

    #[test]
    fn a_run_of_zero_width_chars_still_breaks_into_rows() {
        let mut e = editor(&"\u{200b}".repeat(2_000));
        let rows = rows(&mut e, 10);
        assert!(rows.len() > 5, "{} rows", rows.len());
    }

    #[test]
    fn the_box_scrolls_to_the_cursor() {
        let mut e = editor("a\nb\nc\nd\ne");
        e.sync(10);
        assert_eq!(e.scroll_to_cursor(2), 3);
        e.up();
        assert_eq!(e.scroll_to_cursor(2), 3);
        e.up();
        assert_eq!(e.scroll_to_cursor(2), 2);
        e.up();
        e.up();
        assert_eq!(e.scroll_to_cursor(2), 0);
        // A taller box shows from the top.
        assert_eq!(e.scroll_to_cursor(10), 0);
        e.end();
        e.down();
        e.down();
        e.down();
        e.down();
        assert_eq!(e.scroll_to_cursor(10), 0);
        assert_eq!(e.scroll_to_cursor(3), 2);
    }

    #[test]
    fn the_popup_lists_the_commands_the_line_starts() {
        let mut e = Editor::new(commands());
        assert!(!e.popup_open());
        e.insert_char('/');
        let names: Vec<&str> = e.matches().map(CommandSpec::name).collect();
        assert_eq!(names, vec!["/help", "/model", "/exit"]);
        e.insert_char('m');
        let names: Vec<&str> = e.matches().map(CommandSpec::name).collect();
        assert_eq!(names, vec!["/model"]);
        // A space, a newline, or a capital ends the command shape.
        e.insert_char(' ');
        assert!(!e.popup_open());
        e.backspace();
        e.insert_char('\n');
        assert!(!e.popup_open());
        let mut e = editor("/Weird");
        assert!(!e.popup_open());
        e.set("x/m");
        assert!(!e.popup_open());
        e.set("/zzz");
        assert!(!e.popup_open());
        e.set("/help me");
        assert!(!e.popup_open());
    }

    #[test]
    fn a_spec_name_is_the_first_word_and_leading_space_drops() {
        let spec = CommandSpec::new("  /model [name]", "help");
        assert_eq!(spec.name(), "/model");
        assert_eq!(spec.usage(), "/model [name]");
        assert!(spec.takes_args);
        let spec = CommandSpec::new(" /é", "help");
        assert_eq!(spec.name(), "/é");
        assert!(!spec.takes_args);
    }

    #[test]
    fn the_selection_wraps_and_an_edit_resets_it() {
        let mut e = Editor::new(commands());
        e.insert_char('/');
        assert_eq!(e.selected(), 0);
        e.select_prev();
        assert_eq!(e.selected(), 2);
        e.select_next();
        assert_eq!(e.selected(), 0);
        e.select_next();
        assert_eq!(e.selected(), 1);
        e.insert_char('e');
        assert_eq!(e.selected(), 0);
        let names: Vec<&str> = e.matches().map(CommandSpec::name).collect();
        assert_eq!(names, vec!["/exit"]);
    }

    #[test]
    fn complete_puts_the_command_on_the_line_and_a_whole_line_submits() {
        let mut e = Editor::new(commands());
        e.insert_str("/mo");
        assert_eq!(e.complete(), Completion::Applied);
        // A command with arguments gets the space; the popup closes.
        assert_eq!(e.text(), "/model ");
        assert_eq!(e.cursor(), 7);
        assert!(!e.popup_open());

        let mut e = Editor::new(commands());
        e.insert_str("/he");
        assert_eq!(e.complete(), Completion::Applied);
        assert_eq!(e.text(), "/help");
        // The line holds the command: the next Enter submits.
        assert!(e.popup_open());
        assert_eq!(e.complete(), Completion::OnTheLine);
        assert_eq!(e.text(), "/help");

        // No popup, nothing to complete.
        let mut e = Editor::new(commands());
        e.insert_str("plain");
        assert_eq!(e.complete(), Completion::NoMatch);
        assert_eq!(e.text(), "plain");
    }

    #[test]
    fn a_recalled_entry_opens_no_popup_until_the_next_edit() {
        let mut e = Editor::new(commands());
        e.recall("/help");
        assert_eq!(e.text(), "/help");
        assert!(!e.popup_open());
        e.insert_char(' ');
        e.backspace();
        assert!(e.popup_open());
    }

    #[test]
    fn dismiss_closes_the_popup_until_the_next_edit() {
        let mut e = Editor::new(commands());
        e.insert_char('/');
        e.dismiss();
        assert!(!e.popup_open());
        assert_eq!(e.complete(), Completion::NoMatch);
        e.insert_char('h');
        assert!(e.popup_open());
        e.take();
        assert!(!e.popup_open());
    }
}
