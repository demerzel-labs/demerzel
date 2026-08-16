//! Input history: in-memory ring with an optional file behind it.
//!
//! The file format matches the REPL's line-editor history, so the two
//! interfaces share one file. The current format starts with a `#V2`
//! line; entries follow one per line with newlines written as `\n` and
//! backslashes as `\\`. A file without the header is the old format:
//! plain lines, no escaping. All file work is best effort — history
//! must never stop a session.

use std::io::Write as _;
use std::path::PathBuf;

/// Entry cap, in memory and on load.
const MAX_ENTRIES: usize = 500;

/// Byte cap for one entry. A pasted document must not grow the file
/// and slow every later boot.
const MAX_ENTRY_BYTES: usize = 8 * 1024;

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
}

impl History {
    /// Load the file when one is given. A missing or unreadable file
    /// is an empty history.
    pub fn load(path: Option<PathBuf>) -> Self {
        let mut entries = Vec::new();
        if let Some(path) = &path
            && let Ok(raw) = std::fs::read_to_string(path)
        {
            entries = parse(&raw);
            if entries.len() > MAX_ENTRIES {
                entries.drain(..entries.len() - MAX_ENTRIES);
            }
        }
        Self {
            entries,
            path,
            nav: None,
            draft: String::new(),
        }
    }

    /// Record a submitted line and append it to the file. Blank lines,
    /// oversized lines, and repeats of the last entry are skipped.
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
        if let Some(path) = &self.path {
            let _ = append(path, line);
        }
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

fn parse(raw: &str) -> Vec<String> {
    let mut lines = raw.lines();
    let mut entries = Vec::new();
    let Some(first) = lines.next() else {
        return entries;
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
    entries
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
/// permissions when it does not exist.
fn append(path: &std::path::Path, line: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    if file.metadata()?.len() == 0 {
        writeln!(file, "{FILE_HEADER}")?;
    }
    writeln!(file, "{}", escape(line))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history");
        std::fs::write(&path, "first\nsecond\n").unwrap();
        let mut h = History::load(Some(path));
        assert_eq!(h.up(""), Some("second"));
        assert_eq!(h.up(""), Some("first"));
    }

    #[test]
    fn the_load_cap_keeps_the_tail() {
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
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("history");
        let mut h = History::load(Some(path.clone()));
        h.record("entry");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
