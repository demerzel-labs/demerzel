//! Input history: in-memory ring with an optional file behind it.
//!
//! The file format is the `rustyline` V2 file history the REPL's line
//! editor writes, so the two interfaces share one file. The format
//! starts with a `#V2` line; entries follow one per line with
//! newlines written as `\n` and backslashes as `\\`. A file without
//! the header is the old format: plain lines, no escaping; the first
//! write upgrades it. All file work is best effort — history must
//! never stop a session.

use std::io::Write as _;
use std::path::PathBuf;

/// Entry cap, in memory and on load.
const MAX_ENTRIES: usize = 500;

/// Byte cap for one entry. A pasted document must not grow the file
/// and slow every later boot.
const MAX_ENTRY_BYTES: usize = 8 * 1024;

/// Entry lines the file may hold before a record compacts it back to
/// the entry cap. Without the budget the file grows forever and every
/// boot parses all of it.
const FILE_LINE_BUDGET: usize = 2 * MAX_ENTRIES;

const FILE_HEADER: &str = "#V2";

/// Session input history with arrow-key navigation.
pub struct History {
    entries: Vec<String>,
    path: Option<PathBuf>,
    /// Index into `entries` while the user browses; `None` at the
    /// live line.
    nav: Option<usize>,
    /// The live line saved when browsing starts.
    draft: String,
    /// The next write must rewrite the whole file: it is the old
    /// headerless format, or it does not end in a newline.
    rewrite: bool,
    /// Entry lines in the file, the compaction trigger.
    file_lines: usize,
}

impl History {
    /// Load the file when one is given. A missing or unreadable file
    /// is an empty history.
    pub fn load(path: Option<PathBuf>) -> Self {
        let mut entries = Vec::new();
        let mut rewrite = false;
        let mut file_lines = 0;
        if let Some(path) = &path
            && let Ok(raw) = read_locked(path)
        {
            let v2;
            (entries, v2) = parse(&raw);
            file_lines = entries.len();
            // A headerless file must not take escaped entries, and a
            // cut final line must not merge with the next entry.
            rewrite = !raw.is_empty() && (!v2 || !raw.ends_with('\n'));
            if entries.len() > MAX_ENTRIES {
                entries.drain(..entries.len() - MAX_ENTRIES);
            }
        }
        Self {
            entries,
            path,
            nav: None,
            draft: String::new(),
            rewrite,
            file_lines,
        }
    }

    /// Record a submitted line and write it through to the file.
    /// Blank lines, oversized lines, and repeats of the last entry
    /// are skipped.
    pub fn record(&mut self, line: &str) {
        self.nav = None;
        if line.trim().is_empty()
            || line.len() > MAX_ENTRY_BYTES
            || self.entries.last().is_some_and(|last| last == line)
        {
            return;
        }
        self.entries.push(line.to_string());
        if self.entries.len() > MAX_ENTRIES {
            self.entries.remove(0);
        }
        let Some(path) = &self.path else {
            return;
        };
        if self.rewrite || self.file_lines >= FILE_LINE_BUDGET {
            if rewrite_file(path, &self.entries).is_ok() {
                self.rewrite = false;
                self.file_lines = self.entries.len();
            }
        } else if append(path, line).is_ok() {
            self.file_lines += 1;
        }
    }

    /// Leave history browsing: a discarded line starts over at the
    /// newest entry.
    pub fn reset(&mut self) {
        self.nav = None;
    }

    /// Step to the previous entry. The first step saves `current` as
    /// the draft. Returns the text to show, or `None` at the top.
    pub fn up(&mut self, current: &str) -> Option<&str> {
        let next = match self.nav {
            None if self.entries.is_empty() => return None,
            None => {
                self.draft = current.to_string();
                self.entries.len() - 1
            }
            Some(0) => return None,
            Some(index) => index - 1,
        };
        self.nav = Some(next);
        Some(&self.entries[next])
    }

    /// Step back toward the live line. Returns the text to show, or
    /// `None` when not browsing.
    pub fn down(&mut self) -> Option<&str> {
        let index = self.nav?;
        if index + 1 < self.entries.len() {
            self.nav = Some(index + 1);
            Some(&self.entries[index + 1])
        } else {
            self.nav = None;
            Some(&self.draft)
        }
    }
}

/// Entries plus whether the file carries the current-format header.
fn parse(raw: &str) -> (Vec<String>, bool) {
    let mut lines = raw.lines();
    let mut entries = Vec::new();
    let Some(first) = lines.next() else {
        return (entries, false);
    };
    let v2 = first == FILE_HEADER;
    if !v2 && !first.is_empty() {
        entries.push(first.to_string());
    }
    for line in lines {
        if line.is_empty() {
            continue;
        }
        entries.push(if v2 { unescape(line) } else { line.to_string() });
    }
    (entries, v2)
}

fn unescape(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn escape(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    for ch in line.chars() {
        match ch {
            '\n' => out.push_str("\\n"),
            '\\' => out.push_str("\\\\"),
            other => out.push(other),
        }
    }
    out
}

/// Append one entry, creating the file with the header and owner-only
/// permissions when it does not exist. One locked `write_all` carries
/// the whole payload, so two sessions cannot interleave mid-entry.
fn append(path: &std::path::Path, line: &str) -> std::io::Result<()> {
    let file = open_history_file(path, false)?;
    file.lock()?;
    let mut payload = String::with_capacity(line.len() + FILE_HEADER.len() + 2);
    if file.metadata()?.len() == 0 {
        payload.push_str(FILE_HEADER);
        payload.push('\n');
    }
    payload.push_str(&escape(line));
    payload.push('\n');
    (&file).write_all(payload.as_bytes())
}

/// Read the whole file under a shared lock, so a rewrite in another
/// session cannot hand back a cut file.
fn read_locked(path: &std::path::Path) -> std::io::Result<String> {
    use std::io::Read as _;
    let mut file = std::fs::File::open(path)?;
    file.lock_shared()?;
    let mut raw = String::new();
    file.read_to_string(&mut raw)?;
    Ok(raw)
}

/// Replace the file with the in-memory entries: format upgrade and
/// compaction share this path. In place under the lock, so another
/// session's open handle stays valid. Known trade: entries another
/// session appended after this session loaded do not survive the
/// rewrite. History is best effort; the append path keeps them.
fn rewrite_file(path: &std::path::Path, entries: &[String]) -> std::io::Result<()> {
    let file = open_history_file(path, true)?;
    file.lock()?;
    file.set_len(0)?;
    let mut payload = String::from(FILE_HEADER);
    payload.push('\n');
    for entry in entries {
        payload.push_str(&escape(entry));
        payload.push('\n');
    }
    (&file).write_all(payload.as_bytes())
}

fn open_history_file(path: &std::path::Path, write: bool) -> std::io::Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(!write).write(write);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The interop test drives the REPL's line editor, whose save
    /// path flips the process umask for a moment. A temp directory
    /// another test creates inside that window loses its search bit
    /// and every file call in it fails. One lock serializes the
    /// file-backed tests, so the window overlaps nothing.
    static FILE_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn file_test_guard() -> std::sync::MutexGuard<'static, ()> {
        FILE_TESTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn navigation_walks_entries_and_restores_the_draft() {
        let mut h = History::load(None);
        h.record("one");
        h.record("two");
        assert_eq!(h.up("dra"), Some("two"));
        assert_eq!(h.up(""), Some("one"));
        assert_eq!(h.up(""), None);
        assert_eq!(h.down(), Some("two"));
        assert_eq!(h.down(), Some("dra"));
        assert_eq!(h.down(), None);
    }

    #[test]
    fn blanks_repeats_and_oversized_lines_are_skipped() {
        let mut h = History::load(None);
        h.record("  ");
        h.record("same");
        h.record("same");
        h.record(&"x".repeat(MAX_ENTRY_BYTES + 1));
        assert_eq!(h.up(""), Some("same"));
        assert_eq!(h.up(""), None);
    }

    #[test]
    fn the_file_round_trips_with_escaping() {
        let _guard = file_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history");
        let mut h = History::load(Some(path.clone()));
        h.record("plain");
        h.record("two\nlines \\ backslash");

        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(raw, "#V2\nplain\ntwo\\nlines \\\\ backslash\n");

        let mut again = History::load(Some(path));
        assert_eq!(again.up(""), Some("two\nlines \\ backslash"));
        assert_eq!(again.up(""), Some("plain"));
    }

    #[test]
    fn a_headerless_file_reads_as_plain_lines() {
        let _guard = file_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history");
        std::fs::write(&path, "first\nsecond\n").unwrap();
        let mut h = History::load(Some(path));
        assert_eq!(h.up(""), Some("second"));
        assert_eq!(h.up(""), Some("first"));
    }

    #[test]
    fn the_first_write_upgrades_a_headerless_file() {
        let _guard = file_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history");
        std::fs::write(&path, "old entry\n").unwrap();
        let mut h = History::load(Some(path.clone()));
        h.record("two\nlines and a \\ backslash");

        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(raw, "#V2\nold entry\ntwo\\nlines and a \\\\ backslash\n");
        let mut again = History::load(Some(path));
        assert_eq!(again.up(""), Some("two\nlines and a \\ backslash"));
        assert_eq!(again.up(""), Some("old entry"));
    }

    #[test]
    fn a_missing_final_newline_cannot_merge_entries() {
        let _guard = file_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history");
        std::fs::write(&path, "#V2\npartial").unwrap();
        let mut h = History::load(Some(path.clone()));
        h.record("next");

        let mut again = History::load(Some(path));
        assert_eq!(again.up(""), Some("next"));
        assert_eq!(again.up(""), Some("partial"));
    }

    #[test]
    fn the_line_budget_compacts_the_file() {
        let _guard = file_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history");
        let mut h = History::load(Some(path.clone()));
        for i in 0..(FILE_LINE_BUDGET + 1) {
            h.record(&format!("entry {i}"));
        }
        let raw = std::fs::read_to_string(&path).unwrap();
        let lines = raw.lines().count();
        assert!(
            lines <= MAX_ENTRIES + 1,
            "{lines} lines survived compaction"
        );
        let mut again = History::load(Some(path));
        assert_eq!(
            again.up(""),
            Some(format!("entry {FILE_LINE_BUDGET}").as_str())
        );
    }

    #[test]
    fn reset_leaves_browsing() {
        let mut h = History::load(None);
        h.record("one");
        h.record("two");
        assert_eq!(h.up(""), Some("two"));
        h.reset();
        assert_eq!(h.up(""), Some("two"));
    }

    /// The REPL's line editor and this module must read each other's
    /// files: they share one history path.
    #[test]
    fn interop_with_the_repl_line_editor() {
        let _guard = file_test_guard();
        use rustyline::history::History as _;

        let dir = tempfile::tempdir().unwrap();

        // This module writes; the line editor reads.
        let ours = dir.path().join("ours");
        let mut h = History::load(Some(ours.clone()));
        h.record("plain");
        h.record("two\nlines and a \\ backslash");
        let mut editor = rustyline::history::FileHistory::new();
        editor.load(&ours).unwrap();
        assert_eq!(editor.len(), 2);
        let entry = editor
            .get(1, rustyline::history::SearchDirection::Forward)
            .unwrap()
            .unwrap();
        assert_eq!(entry.entry, "two\nlines and a \\ backslash");

        // The line editor writes; this module reads.
        let theirs = dir.path().join("theirs");
        let mut editor = rustyline::history::FileHistory::new();
        editor.add_owned("from the editor".to_string()).unwrap();
        editor
            .add_owned("multi\nline \\ entry".to_string())
            .unwrap();
        editor.save(&theirs).unwrap();
        let mut h = History::load(Some(theirs));
        assert_eq!(h.up(""), Some("multi\nline \\ entry"));
        assert_eq!(h.up(""), Some("from the editor"));
    }

    #[test]
    fn the_load_cap_keeps_the_tail() {
        let _guard = file_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history");
        let mut body = String::from("#V2\n");
        for i in 0..(MAX_ENTRIES + 10) {
            body.push_str(&format!("entry {i}\n"));
        }
        std::fs::write(&path, body).unwrap();
        let mut h = History::load(Some(path));
        assert_eq!(
            h.up(""),
            Some(format!("entry {}", MAX_ENTRIES + 9).as_str())
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_created_file_is_owner_only() {
        let _guard = file_test_guard();
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("history");
        let mut h = History::load(Some(path.clone()));
        h.record("entry");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
