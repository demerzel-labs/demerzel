//! The `edit` tool: line edits validated against a snapshot tag.
//!
//! The model takes the tag and the line numbers from its latest read.
//! The tool applies the edits only while the live file still hashes to
//! that tag; one mismatch voids every line number from that read. The
//! response carries the new tag and the renumbered changed lines, so
//! chained edits need no fresh read.

use std::path::{Path, PathBuf};

use demerzel_core::ToolAccess;
use demerzel_providers::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::hashline::file_tag;
use crate::{Tool, ToolError, ToolOutput, parse_input, read_text, resolve};

/// Lines of stale-file context shown around the first edit target.
const STALE_CONTEXT_LINES: usize = 3;

/// Changed lines echoed back before the echo truncates.
const MAX_ECHO_LINES: usize = 100;

pub struct EditTool {
    root: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditInput {
    path: String,
    /// Snapshot tag from the latest read. Required for every op except
    /// `create`.
    tag: Option<String>,
    edits: Vec<EditOp>,
}

// `deny_unknown_fields` does not compose with internally tagged enums,
// so unknown fields inside one op pass silently; an unknown `op` still
// fails with the variant list.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum EditOp {
    /// Replace lines `start..=end` with the content lines.
    Replace {
        start: usize,
        end: usize,
        content: String,
    },
    /// Insert the content lines before line `line`. `line` may be
    /// one past the last line to append at the end of the file.
    InsertBefore { line: usize, content: String },
    /// Insert the content lines after line `line`.
    InsertAfter { line: usize, content: String },
    /// Delete lines `start..=end`.
    Delete { start: usize, end: usize },
    /// Create a new file with the content. The file must not exist,
    /// and `create` must be the only op in the call.
    Create { content: String },
}

/// One validated op, positioned on the original file.
enum Planned<'a> {
    /// Insert at gap `gap`: between line `gap` and line `gap + 1`;
    /// gap 0 is the file head, gap `total` is the file tail.
    Insert { gap: usize, lines: Vec<&'a str> },
    /// Replace lines `start..=end`; `lines` empty means delete.
    Span {
        start: usize,
        end: usize,
        lines: Vec<&'a str>,
    },
}

impl Planned<'_> {
    /// Sort key: the gap before the first affected line. Inserts at a
    /// gap come before a span that starts right after it.
    fn position(&self) -> (usize, u8) {
        match self {
            Planned::Insert { gap, .. } => (*gap, 0),
            Planned::Span { start, .. } => (start - 1, 1),
        }
    }
}

/// One line of the original file with its own terminator, so an edit
/// rewrites only the lines it touches and keeps every other byte.
struct Line<'a> {
    content: &'a str,
    terminator: &'a str,
}

fn split_lines(text: &str) -> Vec<Line<'_>> {
    let mut lines = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            let content_end = if index > start && bytes[index - 1] == b'\r' {
                index - 1
            } else {
                index
            };
            lines.push(Line {
                content: &text[start..content_end],
                terminator: &text[content_end..=index],
            });
            start = index + 1;
        }
    }
    if start < text.len() {
        lines.push(Line {
            content: &text[start..],
            terminator: "",
        });
    }
    lines
}

/// Split op content into lines. One trailing newline is optional.
fn content_lines(content: &str) -> Vec<&str> {
    content
        .strip_suffix('\n')
        .unwrap_or(content)
        .split('\n')
        .collect()
}

impl EditTool {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn edit(&self, input: Value) -> Result<ToolOutput, ToolError> {
        let input: EditInput = parse_input(input)?;
        if input.edits.is_empty() {
            return Err(ToolError::InvalidInput("edits must not be empty".into()));
        }
        let path = resolve(&self.root, &input.path);
        if let Some(EditOp::Create { content }) = input.edits.first() {
            if input.edits.len() > 1 {
                return Err(ToolError::InvalidInput(
                    "create must be the only op in the call".into(),
                ));
            }
            return self.create(&input.path, &path, content);
        }
        if input
            .edits
            .iter()
            .any(|op| matches!(op, EditOp::Create { .. }))
        {
            return Err(ToolError::InvalidInput(
                "create must be the only op in the call".into(),
            ));
        }

        let text = read_text(&path)?;
        let current = file_tag(&text);
        let Some(tag) = &input.tag else {
            return Err(ToolError::InvalidInput(
                "tag is required; take it from the latest read of the file".into(),
            ));
        };
        let lines = split_lines(&text);
        let total = lines.len();
        if *tag != current {
            return Err(stale_error(&input, &current, &lines));
        }

        let planned = plan(&input.edits, total)?;
        let (new_text, echo, echo_truncated) = apply(&text, &lines, &planned);
        write_atomic(&path, &new_text)?;

        let new_total = split_lines(&new_text).len();
        let mut content = format!(
            "[{}#{}]\napplied {} edit{}; the file now has {} line{}",
            input.path,
            file_tag(&new_text),
            input.edits.len(),
            if input.edits.len() == 1 { "" } else { "s" },
            new_total,
            if new_total == 1 { "" } else { "s" },
        );
        if !echo.is_empty() {
            content.push('\n');
            content.push_str(&echo);
        }
        Ok(ToolOutput {
            content,
            is_error: false,
            truncated: echo_truncated,
        })
    }

    fn create(&self, display: &str, path: &Path, content: &str) -> Result<ToolOutput, ToolError> {
        if path.exists() {
            return Err(ToolError::Failed(format!(
                "{display} exists; read it and edit with replace, insert, or delete"
            )));
        }
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|error| {
                ToolError::Failed(format!("cannot create {}: {error}", parent.display()))
            })?;
        }
        std::fs::write(path, content)
            .map_err(|error| ToolError::Failed(format!("cannot create {display}: {error}")))?;
        let total = split_lines(content).len();
        Ok(ToolOutput::text(format!(
            "[{display}#{}]\ncreated with {total} line{}",
            file_tag(content),
            if total == 1 { "" } else { "s" },
        )))
    }
}

/// Validate every op against the original file and position it.
fn plan<'a>(edits: &'a [EditOp], total: usize) -> Result<Vec<Planned<'a>>, ToolError> {
    let mut planned = Vec::with_capacity(edits.len());
    for op in edits {
        planned.push(match op {
            EditOp::Replace {
                start,
                end,
                content,
            } => {
                check_span("replace", *start, *end, total)?;
                Planned::Span {
                    start: *start,
                    end: *end,
                    lines: content_lines(content),
                }
            }
            EditOp::Delete { start, end } => {
                check_span("delete", *start, *end, total)?;
                Planned::Span {
                    start: *start,
                    end: *end,
                    lines: Vec::new(),
                }
            }
            EditOp::InsertBefore { line, content } => {
                if *line == 0 || *line > total + 1 {
                    return Err(ToolError::InvalidInput(format!(
                        "insert_before line {line} is out of range: valid lines are 1-{} (use {} to append)",
                        total + 1,
                        total + 1,
                    )));
                }
                Planned::Insert {
                    gap: line - 1,
                    lines: content_lines(content),
                }
            }
            EditOp::InsertAfter { line, content } => {
                if *line == 0 || *line > total {
                    return Err(ToolError::InvalidInput(format!(
                        "insert_after line {line} is out of range: the file has {total} line{}",
                        if total == 1 { "" } else { "s" },
                    )));
                }
                Planned::Insert {
                    gap: *line,
                    lines: content_lines(content),
                }
            }
            EditOp::Create { .. } => unreachable!("create is handled before planning"),
        });
    }
    planned.sort_by_key(Planned::position);
    check_conflicts(&planned)?;
    Ok(planned)
}

fn check_span(op: &str, start: usize, end: usize, total: usize) -> Result<(), ToolError> {
    if start == 0 || start > end {
        return Err(ToolError::InvalidInput(format!(
            "{op} range {start}-{end} is invalid: start counts from 1 and must not pass end"
        )));
    }
    if end > total {
        return Err(ToolError::InvalidInput(format!(
            "{op} range {start}-{end} is out of range: the file has {total} line{}",
            if total == 1 { "" } else { "s" },
        )));
    }
    Ok(())
}

/// Reject overlapping spans, two inserts at one gap, and inserts
/// inside a span. `planned` is sorted by position.
fn check_conflicts(planned: &[Planned<'_>]) -> Result<(), ToolError> {
    for pair in planned.windows(2) {
        let conflict = match (&pair[0], &pair[1]) {
            (Planned::Span { start, end, .. }, Planned::Span { start: next, .. }) => {
                if next <= end {
                    Some(format!("lines {start}-{end} and lines {next}-…"))
                } else {
                    None
                }
            }
            // Sorted order puts the insert first when its gap sits
            // before or inside the span; inside means gap >= start.
            (Planned::Insert { gap, .. }, Planned::Span { start, end, .. }) => {
                if *gap >= *start {
                    Some(format!("an insert at line {gap} and lines {start}-{end}"))
                } else {
                    None
                }
            }
            (Planned::Span { start, end, .. }, Planned::Insert { gap, .. }) => {
                if *gap < *end {
                    Some(format!("lines {start}-{end} and an insert at line {gap}"))
                } else {
                    None
                }
            }
            (Planned::Insert { gap, .. }, Planned::Insert { gap: next, .. }) => {
                if gap == next {
                    Some(format!("two inserts at line {gap}"))
                } else {
                    None
                }
            }
        };
        if let Some(what) = conflict {
            return Err(ToolError::InvalidInput(format!(
                "edits conflict: {what}; merge them or send separate calls"
            )));
        }
    }
    Ok(())
}

/// Build the new file text and the renumbered echo of changed lines.
/// Original lines keep their own terminators; new lines take the file
/// newline style.
fn apply(text: &str, lines: &[Line<'_>], planned: &[Planned<'_>]) -> (String, String, bool) {
    let style = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let total = lines.len();
    let mut out = String::with_capacity(text.len());
    let mut echo = String::new();
    let mut echoed = 0usize;
    let mut suppressed = 0usize;
    let mut new_number = 0usize;
    let mut next_op = 0usize;
    let echo_section = |echo: &mut String,
                        echoed: &mut usize,
                        suppressed: &mut usize,
                        number: usize,
                        section: &[&str]| {
        if !echo.is_empty() && *suppressed == 0 {
            echo.push_str("…\n");
        }
        for (offset, line) in section.iter().enumerate() {
            if *echoed >= MAX_ECHO_LINES {
                *suppressed += 1;
                continue;
            }
            echo.push_str(&format!("{}:{line}\n", number + offset));
            *echoed += 1;
        }
    };

    let mut index = 0usize; // 0-based original line index
    loop {
        let gap = index;
        while let Some(Planned::Insert {
            gap: g,
            lines: section,
        }) = planned.get(next_op)
            && *g == gap
        {
            echo_section(
                &mut echo,
                &mut echoed,
                &mut suppressed,
                new_number + 1,
                section,
            );
            for line in section {
                out.push_str(line);
                out.push_str(style);
                new_number += 1;
            }
            next_op += 1;
        }
        if index >= total {
            break;
        }
        if let Some(Planned::Span {
            start,
            end,
            lines: section,
        }) = planned.get(next_op)
            && *start == index + 1
        {
            if !section.is_empty() {
                echo_section(
                    &mut echo,
                    &mut echoed,
                    &mut suppressed,
                    new_number + 1,
                    section,
                );
                for line in section {
                    out.push_str(line);
                    out.push_str(style);
                    new_number += 1;
                }
            }
            index = *end;
            next_op += 1;
            continue;
        }
        let line = &lines[index];
        out.push_str(line.content);
        if line.terminator.is_empty() {
            out.push_str(style);
        } else {
            out.push_str(line.terminator);
        }
        new_number += 1;
        index += 1;
    }

    // The trailing newline follows the original file: strip the one
    // the loop added when the original had none.
    let had_trailing = text.is_empty() || text.ends_with('\n');
    if !had_trailing && out.ends_with(style) {
        out.truncate(out.len() - style.len());
    }
    if suppressed > 0 {
        echo.push_str(&format!(
            "[echo truncated: {suppressed} more changed lines]\n"
        ));
    }
    if echo.ends_with('\n') {
        echo.pop();
    }
    (out, echo, suppressed > 0)
}

/// The stale-tag error: name both tags and show the current lines near
/// the first edit target, so the model can re-anchor without a read.
fn stale_error(input: &EditInput, current: &str, lines: &[Line<'_>]) -> ToolError {
    let first_line = input
        .edits
        .iter()
        .map(|op| match op {
            EditOp::Replace { start, .. } | EditOp::Delete { start, .. } => *start,
            EditOp::InsertBefore { line, .. } | EditOp::InsertAfter { line, .. } => *line,
            EditOp::Create { .. } => 1,
        })
        .min()
        .unwrap_or(1)
        .max(1);
    let total = lines.len();
    let mut message = format!(
        "stale tag for {}: the file changed since that read (tag is now {current}). Current lines near the edit target:",
        input.path,
    );
    let start = first_line.saturating_sub(STALE_CONTEXT_LINES).max(1);
    let end = (first_line + STALE_CONTEXT_LINES).min(total);
    for number in start..=end {
        message.push_str(&format!("\n{number}:{}", lines[number - 1].content));
    }
    if total == 0 {
        message.push_str("\n[file is empty]");
    }
    message.push_str(&format!(
        "\nSend the edit again with tag {current} and corrected line numbers, or read {} for more context.",
        input.path,
    ));
    ToolError::Failed(message)
}

/// Write through a same-directory temp file and rename, so a crash
/// never leaves a half-written file. Keeps the original permissions.
fn write_atomic(path: &Path, text: &str) -> Result<(), ToolError> {
    let fail = |error: std::io::Error| {
        ToolError::Failed(format!("cannot write {}: {error}", path.display()))
    };
    // Resolve symlinks so the rename replaces the target file, not the
    // link itself.
    let target = path.canonicalize().map_err(fail)?;
    let mut temp_name = target.file_name().unwrap_or_default().to_os_string();
    temp_name.push(".dmz-tmp");
    let temp = target.with_file_name(temp_name);
    std::fs::write(&temp, text).map_err(fail)?;
    let permissions = std::fs::metadata(&target).map_err(fail)?.permissions();
    std::fs::set_permissions(&temp, permissions).map_err(fail)?;
    std::fs::rename(&temp, &target).map_err(fail)
}

impl Tool for EditTool {
    fn name(&self) -> &'static str {
        "edit"
    }

    fn description(&self) -> &'static str {
        "Edit a file with line ops validated against the snapshot tag from your latest read. The response returns the new tag and the renumbered changed lines. Use op `create` for a new file."
    }

    fn access(&self) -> ToolAccess {
        ToolAccess::Write
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The file path, absolute or relative to the project root."
                },
                "tag": {
                    "type": "string",
                    "description": "The snapshot tag from your latest read of this file. Not used with `create`."
                },
                "edits": {
                    "type": "array",
                    "minItems": 1,
                    "description": "The ops to apply as one atomic change. Line numbers refer to the read that produced the tag.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "op": {
                                "type": "string",
                                "enum": ["replace", "insert_before", "insert_after", "delete", "create"],
                                "description": "replace lines start-end; insert content before or after a line; delete lines start-end; create a new file."
                            },
                            "start": {"type": "integer", "minimum": 1, "description": "First line of a replace or delete range, inclusive."},
                            "end": {"type": "integer", "minimum": 1, "description": "Last line of a replace or delete range, inclusive."},
                            "line": {"type": "integer", "minimum": 1, "description": "Target line of an insert."},
                            "content": {"type": "string", "description": "The new lines. A trailing newline is optional."}
                        },
                        "required": ["op"]
                    }
                }
            },
            "required": ["path", "edits"],
            "additionalProperties": false
        })
    }

    fn run(&self, input: Value) -> BoxFuture<'_, Result<ToolOutput, ToolError>> {
        Box::pin(async move { self.edit(input) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::run_now;

    fn edit(dir: &tempfile::TempDir, input: Value) -> Result<ToolOutput, ToolError> {
        run_now(EditTool::new(dir.path()).run(input))
    }

    fn write(dir: &tempfile::TempDir, name: &str, content: &str) {
        std::fs::write(dir.path().join(name), content).unwrap();
    }

    fn content(dir: &tempfile::TempDir, name: &str) -> String {
        std::fs::read_to_string(dir.path().join(name)).unwrap()
    }

    fn tag_of(text: &str) -> String {
        file_tag(text)
    }

    #[test]
    fn replaces_a_line_range_and_returns_the_new_tag() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "f.txt", "one\ntwo\nthree\n");
        let output = edit(
            &dir,
            json!({
                "path": "f.txt",
                "tag": tag_of("one\ntwo\nthree\n"),
                "edits": [{"op": "replace", "start": 2, "end": 3, "content": "TWO\nTHREE"}],
            }),
        )
        .unwrap();
        assert_eq!(content(&dir, "f.txt"), "one\nTWO\nTHREE\n");
        let expected_tag = tag_of("one\nTWO\nTHREE\n");
        assert!(
            output
                .content
                .starts_with(&format!("[f.txt#{expected_tag}]"))
        );
        assert!(
            output
                .content
                .contains("applied 1 edit; the file now has 3 lines")
        );
        assert!(output.content.contains("2:TWO\n3:THREE"));
    }

    #[test]
    fn a_stale_tag_is_rejected_with_current_context() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "f.txt", "one\ntwo\nthree\nfour\nfive\n");
        let error = edit(
            &dir,
            json!({
                "path": "f.txt",
                "tag": "000000",
                "edits": [{"op": "replace", "start": 4, "end": 4, "content": "FOUR"}],
            }),
        )
        .unwrap_err();
        let message = error.to_string();
        let current = tag_of("one\ntwo\nthree\nfour\nfive\n");
        assert!(message.contains("stale tag for f.txt"));
        assert!(message.contains(&format!("tag is now {current}")));
        assert!(message.contains("1:one"));
        assert!(message.contains("5:five"));
        assert_eq!(content(&dir, "f.txt"), "one\ntwo\nthree\nfour\nfive\n");
    }

    #[test]
    fn anchors_stay_valid_while_the_file_is_unchanged() {
        // Two windows of one read share the tag; a later edit with that
        // tag still applies.
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "f.txt", "a\nb\nc\n");
        let tag = tag_of("a\nb\nc\n");
        let first = edit(
            &dir,
            json!({"path": "f.txt", "tag": tag, "edits": [{"op": "delete", "start": 2, "end": 2}]}),
        )
        .unwrap();
        assert_eq!(content(&dir, "f.txt"), "a\nc\n");
        // The old tag is now stale; the response tag re-grounds.
        let stale = edit(
            &dir,
            json!({"path": "f.txt", "tag": tag, "edits": [{"op": "delete", "start": 1, "end": 1}]}),
        )
        .unwrap_err();
        assert!(stale.to_string().contains("stale tag"));
        let new_tag = tag_of("a\nc\n");
        assert!(first.content.contains(&new_tag));
    }

    #[test]
    fn inserts_before_after_and_at_the_file_ends() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "f.txt", "b\nd\n");
        let output = edit(
            &dir,
            json!({
                "path": "f.txt",
                "tag": tag_of("b\nd\n"),
                "edits": [
                    {"op": "insert_before", "line": 1, "content": "a"},
                    {"op": "insert_after", "line": 1, "content": "c"},
                    {"op": "insert_before", "line": 3, "content": "e"},
                ],
            }),
        )
        .unwrap();
        assert_eq!(content(&dir, "f.txt"), "a\nb\nc\nd\ne\n");
        assert!(output.content.contains("1:a"));
        assert!(output.content.contains("3:c"));
        assert!(output.content.contains("5:e"));
    }

    #[test]
    fn multiple_edits_apply_atomically_with_original_numbering() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "f.txt", "one\ntwo\nthree\nfour\n");
        let output = edit(
            &dir,
            json!({
                "path": "f.txt",
                "tag": tag_of("one\ntwo\nthree\nfour\n"),
                "edits": [
                    {"op": "delete", "start": 1, "end": 1},
                    {"op": "replace", "start": 3, "end": 3, "content": "THREE\nAND A HALF"},
                    {"op": "insert_after", "line": 4, "content": "five"},
                ],
            }),
        )
        .unwrap();
        assert_eq!(
            content(&dir, "f.txt"),
            "two\nTHREE\nAND A HALF\nfour\nfive\n"
        );
        assert!(output.content.contains("the file now has 5 lines"));
        assert!(output.content.contains("2:THREE\n3:AND A HALF"));
        assert!(output.content.contains("5:five"));
    }

    #[test]
    fn overlapping_and_conflicting_edits_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "f.txt", "one\ntwo\nthree\nfour\n");
        let tag = tag_of("one\ntwo\nthree\nfour\n");
        let overlap = edit(
            &dir,
            json!({"path": "f.txt", "tag": tag, "edits": [
                {"op": "replace", "start": 1, "end": 2, "content": "x"},
                {"op": "delete", "start": 2, "end": 3},
            ]}),
        )
        .unwrap_err();
        assert!(overlap.to_string().contains("edits conflict"));
        let inside = edit(
            &dir,
            json!({"path": "f.txt", "tag": tag, "edits": [
                {"op": "replace", "start": 2, "end": 3, "content": "x"},
                {"op": "insert_after", "line": 2, "content": "y"},
            ]}),
        )
        .unwrap_err();
        assert!(inside.to_string().contains("edits conflict"));
        let same_gap = edit(
            &dir,
            json!({"path": "f.txt", "tag": tag, "edits": [
                {"op": "insert_after", "line": 2, "content": "y"},
                {"op": "insert_before", "line": 3, "content": "z"},
            ]}),
        )
        .unwrap_err();
        assert!(same_gap.to_string().contains("two inserts"));
        assert_eq!(content(&dir, "f.txt"), "one\ntwo\nthree\nfour\n");
    }

    #[test]
    fn out_of_range_edits_name_the_line_count() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "f.txt", "one\ntwo\n");
        let tag = tag_of("one\ntwo\n");
        let error = edit(
            &dir,
            json!({"path": "f.txt", "tag": tag, "edits": [
                {"op": "replace", "start": 2, "end": 9, "content": "x"},
            ]}),
        )
        .unwrap_err();
        assert!(error.to_string().contains("the file has 2 lines"));
        let inverted = edit(
            &dir,
            json!({"path": "f.txt", "tag": tag, "edits": [
                {"op": "delete", "start": 2, "end": 1},
            ]}),
        )
        .unwrap_err();
        assert!(matches!(inverted, ToolError::InvalidInput(_)));
    }

    #[test]
    fn missing_tag_is_invalid_input() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "f.txt", "one\n");
        let error = edit(
            &dir,
            json!({"path": "f.txt", "edits": [{"op": "delete", "start": 1, "end": 1}]}),
        )
        .unwrap_err();
        assert!(matches!(error, ToolError::InvalidInput(_)));
        assert!(error.to_string().contains("tag is required"));
    }

    #[test]
    fn crlf_endings_survive_on_untouched_lines() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "f.txt", "one\r\ntwo\r\nthree\r\n");
        edit(
            &dir,
            json!({
                "path": "f.txt",
                "tag": tag_of("one\r\ntwo\r\nthree\r\n"),
                "edits": [{"op": "replace", "start": 2, "end": 2, "content": "TWO"}],
            }),
        )
        .unwrap();
        assert_eq!(content(&dir, "f.txt"), "one\r\nTWO\r\nthree\r\n");
    }

    #[test]
    fn a_missing_trailing_newline_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "f.txt", "one\ntwo");
        edit(
            &dir,
            json!({
                "path": "f.txt",
                "tag": tag_of("one\ntwo"),
                "edits": [{"op": "replace", "start": 1, "end": 1, "content": "ONE"}],
            }),
        )
        .unwrap();
        assert_eq!(content(&dir, "f.txt"), "ONE\ntwo");
    }

    #[test]
    fn deleting_every_line_leaves_an_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "f.txt", "one\ntwo\n");
        let output = edit(
            &dir,
            json!({
                "path": "f.txt",
                "tag": tag_of("one\ntwo\n"),
                "edits": [{"op": "delete", "start": 1, "end": 2}],
            }),
        )
        .unwrap();
        assert_eq!(content(&dir, "f.txt"), "");
        assert!(output.content.contains("the file now has 0 lines"));
    }

    #[test]
    fn create_writes_a_new_file_and_refuses_an_existing_one() {
        let dir = tempfile::tempdir().unwrap();
        let output = edit(
            &dir,
            json!({"path": "sub/new.txt", "edits": [{"op": "create", "content": "alpha\nbeta\n"}]}),
        )
        .unwrap();
        assert_eq!(content(&dir, "sub/new.txt"), "alpha\nbeta\n");
        assert!(output.content.contains("created with 2 lines"));
        assert!(output.content.contains(&tag_of("alpha\nbeta\n")));

        let error = edit(
            &dir,
            json!({"path": "sub/new.txt", "edits": [{"op": "create", "content": "x"}]}),
        )
        .unwrap_err();
        assert!(error.to_string().contains("exists"));

        let mixed = edit(
            &dir,
            json!({"path": "other.txt", "edits": [
                {"op": "create", "content": "x"},
                {"op": "insert_after", "line": 1, "content": "y"},
            ]}),
        )
        .unwrap_err();
        assert!(mixed.to_string().contains("only op"));
    }

    #[test]
    fn editing_an_empty_file_inserts_at_the_head() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "f.txt", "");
        let output = edit(
            &dir,
            json!({
                "path": "f.txt",
                "tag": tag_of(""),
                "edits": [{"op": "insert_before", "line": 1, "content": "first"}],
            }),
        )
        .unwrap();
        assert_eq!(content(&dir, "f.txt"), "first\n");
        assert!(output.content.contains("1:first"));
    }

    #[cfg(unix)]
    #[test]
    fn file_permissions_survive_the_atomic_write() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.sh");
        std::fs::write(&path, "#!/bin/sh\necho hi\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        edit(
            &dir,
            json!({
                "path": "run.sh",
                "tag": tag_of("#!/bin/sh\necho hi\n"),
                "edits": [{"op": "replace", "start": 2, "end": 2, "content": "echo bye"}],
            }),
        )
        .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
    }

    #[test]
    fn a_long_echo_truncates_with_a_marker() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "f.txt", "seed\n");
        let big: String = (0..150).map(|n| format!("line {n}\n")).collect();
        let output = edit(
            &dir,
            json!({
                "path": "f.txt",
                "tag": tag_of("seed\n"),
                "edits": [{"op": "insert_after", "line": 1, "content": big}],
            }),
        )
        .unwrap();
        assert!(output.truncated);
        assert!(
            output
                .content
                .contains("[echo truncated: 50 more changed lines]")
        );
    }
}
