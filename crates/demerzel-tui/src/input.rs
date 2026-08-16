//! The input line: one editable buffer with a cursor.
//!
//! The editor is pure state; the event loop maps keys onto these
//! operations and the renderer draws the buffer. The buffer may hold
//! newlines from a paste; it renders and submits as one prompt.

/// One editable line. `cursor` is a byte index on a char boundary.
#[derive(Debug, Default)]
pub struct Input {
    buffer: String,
    cursor: usize,
}

impl Input {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(&self) -> &str {
        &self.buffer
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Take the buffer and reset the editor.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.buffer)
    }

    /// Replace the whole buffer, cursor at the end.
    pub fn set(&mut self, text: &str) {
        self.buffer.clear();
        self.buffer.push_str(text);
        self.cursor = self.buffer.len();
    }

    pub fn insert_char(&mut self, ch: char) {
        self.buffer.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    pub fn insert_str(&mut self, text: &str) {
        self.buffer.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    /// Delete the char before the cursor.
    pub fn backspace(&mut self) {
        if let Some(ch) = self.prev_char() {
            self.cursor -= ch.len_utf8();
            self.buffer.remove(self.cursor);
        }
    }

    /// Delete the char under the cursor.
    pub fn delete(&mut self) {
        if self.cursor < self.buffer.len() {
            self.buffer.remove(self.cursor);
        }
    }

    pub fn left(&mut self) {
        if let Some(ch) = self.prev_char() {
            self.cursor -= ch.len_utf8();
        }
    }

    pub fn right(&mut self) {
        if let Some(ch) = self.buffer[self.cursor..].chars().next() {
            self.cursor += ch.len_utf8();
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.buffer.len();
    }

    /// Delete from the line start to the cursor.
    pub fn kill_to_start(&mut self) {
        self.buffer.drain(..self.cursor);
        self.cursor = 0;
    }

    /// Delete from the cursor to the line end.
    pub fn kill_to_end(&mut self) {
        self.buffer.truncate(self.cursor);
    }

    /// Delete the word before the cursor: trailing spaces, then the
    /// non-space run.
    pub fn kill_word_back(&mut self) {
        let head = &self.buffer[..self.cursor];
        let trimmed = head.trim_end_matches(' ');
        let start = trimmed.rfind(' ').map(|space| space + 1).unwrap_or(0);
        self.buffer.drain(start..self.cursor);
        self.cursor = start;
    }

    fn prev_char(&self) -> Option<char> {
        self.buffer[..self.cursor].chars().next_back()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(text: &str) -> Input {
        let mut input = Input::new();
        input.set(text);
        input
    }

    #[test]
    fn insert_and_take_round_trip() {
        let mut i = Input::new();
        for ch in "héllo".chars() {
            i.insert_char(ch);
        }
        assert_eq!(i.text(), "héllo");
        assert_eq!(i.take(), "héllo");
        assert!(i.is_empty());
        assert_eq!(i.cursor(), 0);
    }

    #[test]
    fn backspace_and_delete_respect_char_boundaries() {
        let mut i = input("aé日");
        i.backspace();
        assert_eq!(i.text(), "aé");
        i.left();
        i.delete();
        assert_eq!(i.text(), "a");
    }

    #[test]
    fn movement_stops_at_the_edges() {
        let mut i = input("ab");
        i.right();
        assert_eq!(i.cursor(), 2);
        i.home();
        i.left();
        assert_eq!(i.cursor(), 0);
        i.right();
        assert_eq!(i.cursor(), 1);
    }

    #[test]
    fn mid_line_insert_lands_at_the_cursor() {
        let mut i = input("ad");
        i.left();
        i.insert_str("bc");
        assert_eq!(i.text(), "abcd");
        i.right();
        assert_eq!(i.cursor(), 4);
    }

    #[test]
    fn kill_operations() {
        let mut i = input("one two three");
        i.kill_word_back();
        assert_eq!(i.text(), "one two ");
        i.kill_word_back();
        assert_eq!(i.text(), "one ");
        i.home();
        i.kill_to_end();
        assert_eq!(i.text(), "");

        let mut i = input("keep drop");
        i.left();
        i.left();
        i.left();
        i.left();
        i.kill_to_start();
        assert_eq!(i.text(), "drop");
        assert_eq!(i.cursor(), 0);
    }
}
