//! The `edit` tool: line edits validated against a snapshot tag.
//!
//! The model takes the tag and the line numbers from its latest read.
//! The tool applies the edits only while the live file still hashes to
//! that tag; one mismatch voids every line number from that read. The
//! response carries the new tag and the new position of every op — not
//! the content, which the model already holds — so chained edits need
//! no fresh read.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use demerzel_core::{BoxFuture, ToolAccess};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::hashline::file_tag;
use crate::{Tool, ToolError, ToolOutput, display_path, parse_input, read_text, resolve_write};

/// Lines of stale-file context shown around the first edit target.
const STALE_CONTEXT_LINES: usize = 3;

pub struct EditTool {
    root: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditInput {
    path: String,
    /// Snapshot tag from the latest read of the file.
    tag: String,
    /// Raw ops; each is routed to its strict per-variant shape.
    edits: Vec<Value>,
}

enum EditOp {
    Replace {
        start: usize,
        end: usize,
        content: String,
    },
    InsertBefore {
        line: usize,
        content: String,
    },
    InsertAfter {
        line: usize,
        content: String,
    },
    Delete {
        start: usize,
        end: usize,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplaceOp {
    start: usize,
    end: usize,
    content: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InsertOp {
    line: usize,
    content: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteOp {
    start: usize,
    end: usize,
}

/// Parse one op strictly: the variant decides the accepted fields, so
/// a stray field is an error instead of a silently dropped intent.
fn parse_op(index: usize, value: &Value) -> Result<EditOp, ToolError> {
    let invalid = |message: String| ToolError::InvalidInput(format!("edits[{index}]: {message}"));
    let Some(object) = value.as_object() else {
        return Err(invalid("each edit is an object with an `op` field".into()));
    };
    let Some(op) = object.get("op").and_then(Value::as_str) else {
        return Err(invalid(
            "missing `op`; use replace, insert_before, insert_after, or delete".into(),
        ));
    };
    let mut fields = object.clone();
    fields.remove("op");
    let fields = Value::Object(fields);
    match op {
        "replace" => serde_json::from_value::<ReplaceOp>(fields)
            .map(|op| EditOp::Replace {
                start: op.start,
                end: op.end,
                content: op.content,
            })
            .map_err(|error| invalid(format!("replace takes start, end, content: {error}"))),
        "insert_before" => serde_json::from_value::<InsertOp>(fields)
            .map(|op| EditOp::InsertBefore {
                line: op.line,
                content: op.content,
            })
            .map_err(|error| invalid(format!("insert_before takes line, content: {error}"))),
        "insert_after" => serde_json::from_value::<InsertOp>(fields)
            .map(|op| EditOp::InsertAfter {
                line: op.line,
                content: op.content,
            })
            .map_err(|error| invalid(format!("insert_after takes line, content: {error}"))),
        "delete" => serde_json::from_value::<DeleteOp>(fields)
            .map(|op| EditOp::Delete {
                start: op.start,
                end: op.end,
            })
            .map_err(|error| invalid(format!("delete takes start, end: {error}"))),
        other => Err(invalid(format!(
            "unknown op `{other}`; use replace, insert_before, insert_after, or delete (the create tool makes new files)"
        ))),
    }
}

/// The caller's own words for one op, used in conflict errors and the
/// response, so the model can map a message back to its input.
fn describe(op: &EditOp) -> String {
    match op {
        EditOp::Replace { start, end, .. } => format!("replace {start}-{end}"),
        EditOp::InsertBefore { line, .. } => format!("insert_before {line}"),
        EditOp::InsertAfter { line, .. } => format!("insert_after {line}"),
        EditOp::Delete { start, end } => format!("delete {start}-{end}"),
    }
}

/// One validated op, positioned on the original file.
enum Planned<'a> {
    /// Insert at gap `gap`: between line `gap` and line `gap + 1`;
    /// gap 0 is the file head, gap `total` is the file tail.
    Insert {
        gap: usize,
        lines: Vec<&'a str>,
        index: usize,
    },
    /// Replace lines `start..=end`; `lines` empty means delete.
    Span {
        start: usize,
        end: usize,
        lines: Vec<&'a str>,
        index: usize,
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

    fn index(&self) -> usize {
        match self {
            Planned::Insert { index, .. } | Planned::Span { index, .. } => *index,
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

/// Split op content into bare lines. One trailing newline is optional,
/// and CRLF endings are normalized away — `apply` alone owns the
/// terminators, so op content can never write a stray `\r`.
fn content_lines(content: &str) -> Vec<&str> {
    content
        .strip_suffix('\n')
        .unwrap_or(content)
        .split('\n')
        .map(|piece| piece.strip_suffix('\r').unwrap_or(piece))
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
        let ops = input
            .edits
            .iter()
            .enumerate()
            .map(|(index, value)| parse_op(index, value))
            .collect::<Result<Vec<_>, _>>()?;
        let path = resolve_write(&self.root, &input.path)?;
        let display = display_path(&self.root, &path);

        let text = read_text(&path)?;
        let current = file_tag(&text);
        if input.tag != current {
            return Err(stale_error(&display, &ops, &current, &text));
        }
        let lines = split_lines(&text);
        let total = lines.len();

        let planned = plan(&ops, total)?;
        let (new_text, new_total, placements) = apply(&text, &lines, &planned);
        write_atomic(&path, &new_text)?;

        let mut content = format!(
            "[{display}#{}]\napplied {} edit{}; the file now has {new_total} line{}",
            file_tag(&new_text),
            ops.len(),
            if ops.len() == 1 { "" } else { "s" },
            if new_total == 1 { "" } else { "s" },
        );
        for (index, placement) in &placements {
            let _ = write!(
                content,
                "\nedits[{index}] {} -> {placement}",
                describe(&ops[*index])
            );
        }
        Ok(ToolOutput::text(content).with_summary(format!("edit {display}: {} ops", ops.len())))
    }
}

/// Validate every op against the original file and position it.
fn plan<'a>(ops: &'a [EditOp], total: usize) -> Result<Vec<Planned<'a>>, ToolError> {
    let mut planned = Vec::with_capacity(ops.len());
    for (index, op) in ops.iter().enumerate() {
        planned.push(match op {
            EditOp::Replace {
                start,
                end,
                content,
            } => {
                check_span(index, "replace", *start, *end, total)?;
                Planned::Span {
                    start: *start,
                    end: *end,
                    lines: content_lines(content),
                    index,
                }
            }
            EditOp::Delete { start, end } => {
                check_span(index, "delete", *start, *end, total)?;
                Planned::Span {
                    start: *start,
                    end: *end,
                    lines: Vec::new(),
                    index,
                }
            }
            EditOp::InsertBefore { line, content } => {
                if *line == 0 || *line > total + 1 {
                    return Err(ToolError::InvalidInput(format!(
                        "edits[{index}]: insert_before line {line} is out of range: valid lines are 1-{} (use {} to append)",
                        total + 1,
                        total + 1,
                    )));
                }
                Planned::Insert {
                    gap: line - 1,
                    lines: content_lines(content),
                    index,
                }
            }
            EditOp::InsertAfter { line, content } => {
                if *line == 0 || *line > total {
                    return Err(ToolError::InvalidInput(format!(
                        "edits[{index}]: insert_after line {line} is out of range: the file has {total} line{}",
                        if total == 1 { "" } else { "s" },
                    )));
                }
                Planned::Insert {
                    gap: *line,
                    lines: content_lines(content),
                    index,
                }
            }
        });
    }
    planned.sort_by_key(Planned::position);
    check_conflicts(&planned, ops)?;
    Ok(planned)
}

fn check_span(
    index: usize,
    op: &str,
    start: usize,
    end: usize,
    total: usize,
) -> Result<(), ToolError> {
    if start == 0 || start > end {
        return Err(ToolError::InvalidInput(format!(
            "edits[{index}]: {op} range {start}-{end} is invalid: start counts from 1 and must not pass end"
        )));
    }
    if end > total {
        return Err(ToolError::InvalidInput(format!(
            "edits[{index}]: {op} range {start}-{end} is out of range: the file has {total} line{}",
            if total == 1 { "" } else { "s" },
        )));
    }
    Ok(())
}

/// Reject overlapping spans, two inserts at one gap, and inserts
/// inside a span. `planned` is sorted by position, which makes every
/// conflicting pair adjacent.
fn check_conflicts(planned: &[Planned<'_>], ops: &[EditOp]) -> Result<(), ToolError> {
    for pair in planned.windows(2) {
        let conflicts = match (&pair[0], &pair[1]) {
            (Planned::Span { end, .. }, Planned::Span { start: next, .. }) => next <= end,
            // Sorted order puts an insert first only when its gap sits
            // before the span, so the reverse pair is the inside case.
            (Planned::Insert { .. }, Planned::Span { .. }) => false,
            (Planned::Span { end, .. }, Planned::Insert { gap, .. }) => gap < end,
            (Planned::Insert { gap, .. }, Planned::Insert { gap: next, .. }) => gap == next,
        };
        if conflicts {
            let (first, second) = (pair[0].index(), pair[1].index());
            return Err(ToolError::InvalidInput(format!(
                "edits conflict: edits[{first}] ({}) and edits[{second}] ({}) touch the same position; merge them or send separate calls",
                describe(&ops[first]),
                describe(&ops[second]),
            )));
        }
    }
    Ok(())
}

/// Build the new file text and the new position of every op. Original
/// lines keep their own terminators; new lines take the file newline
/// style.
fn apply(
    text: &str,
    lines: &[Line<'_>],
    planned: &[Planned<'_>],
) -> (String, usize, Vec<(usize, String)>) {
    let style = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let total = lines.len();
    let mut out = String::with_capacity(text.len());
    let mut placements: Vec<(usize, String)> = Vec::with_capacity(planned.len());
    let mut new_number = 0usize;
    let mut next_op = 0usize;
    let emit = |out: &mut String, new_number: &mut usize, section: &[&str]| {
        let first = *new_number + 1;
        for line in section {
            out.push_str(line);
            out.push_str(style);
            *new_number += 1;
        }
        if section.len() == 1 {
            format!("now line {first}")
        } else {
            format!("now lines {first}-{}", *new_number)
        }
    };

    let mut index = 0usize; // 0-based original line index
    loop {
        let gap = index;
        while let Some(Planned::Insert {
            gap: g,
            lines: section,
            index: op_index,
        }) = planned.get(next_op)
            && *g == gap
        {
            let placement = emit(&mut out, &mut new_number, section);
            placements.push((*op_index, placement));
            next_op += 1;
        }
        if index >= total {
            break;
        }
        if let Some(Planned::Span {
            start,
            end,
            lines: section,
            index: op_index,
        }) = planned.get(next_op)
            && *start == index + 1
        {
            let placement = if section.is_empty() {
                format!("removed; the next line is now line {}", new_number + 1)
            } else {
                emit(&mut out, &mut new_number, section)
            };
            placements.push((*op_index, placement));
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
    placements.sort_by_key(|(index, _)| *index);
    (out, new_number, placements)
}

/// The stale-tag error: name the current tag and show the current
/// lines near the first edit target, so the model can re-anchor
/// without a read.
fn stale_error(display: &str, ops: &[EditOp], current: &str, text: &str) -> ToolError {
    let target = ops
        .iter()
        .map(|op| match op {
            EditOp::Replace { start, .. } | EditOp::Delete { start, .. } => *start,
            EditOp::InsertBefore { line, .. } | EditOp::InsertAfter { line, .. } => *line,
        })
        .min()
        .unwrap_or(1)
        .max(1);
    let total = text.lines().count();
    let mut message = format!(
        "stale tag for {display}: the file changed since that read (tag is now {current})."
    );
    if total == 0 {
        message.push_str("\n[file is empty]");
    } else {
        // When the file shrank below the target, land the window on
        // the file end instead of promising context and showing none.
        let center = target.min(total);
        let start = center.saturating_sub(STALE_CONTEXT_LINES).max(1);
        let end = (center + STALE_CONTEXT_LINES).min(total);
        if target > total {
            let _ = write!(
                message,
                "\nThe file now has {total} line{}.",
                if total == 1 { "" } else { "s" }
            );
        }
        message.push_str("\nCurrent lines near the edit target:");
        for (index, line) in text.lines().enumerate().skip(start - 1) {
            let number = index + 1;
            if number > end {
                break;
            }
            let _ = write!(message, "\n{number}:{line}");
        }
    }
    let _ = write!(
        message,
        "\nSend the edit again with tag {current} and corrected line numbers, or read {display} for more context."
    );
    ToolError::Failed(message)
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write through a same-directory temp file and rename, so a crash
/// never leaves a half-written file. Keeps the original permissions,
/// uses a unique temp name, and removes the temp file on failure.
fn write_atomic(path: &Path, text: &str) -> Result<(), ToolError> {
    let fail = |error: std::io::Error| {
        ToolError::Failed(format!("cannot write {}: {error}", path.display()))
    };
    // Resolve symlinks so the rename replaces the target file, not the
    // link itself.
    let target = path.canonicalize().map_err(fail)?;
    let mut temp_name = target.file_name().unwrap_or_default().to_os_string();
    temp_name.push(format!(
        ".dmz-tmp.{}.{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let temp = target.with_file_name(temp_name);
    let write = || -> std::io::Result<()> {
        std::fs::write(&temp, text)?;
        let permissions = std::fs::metadata(&target)?.permissions();
        std::fs::set_permissions(&temp, permissions)?;
        std::fs::rename(&temp, &target)
    };
    write().map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        fail(error)
    })
}

impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Edit a file with line ops validated against the snapshot tag from your latest read. The response returns the new tag and the new position of every op. The create tool makes new files."
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
                    "description": "The file path, relative to the project root."
                },
                "tag": {
                    "type": "string",
                    "description": "The snapshot tag from your latest read of this file."
                },
                "edits": {
                    "type": "array",
                    "minItems": 1,
                    "description": "The ops to apply as one atomic change. Line numbers refer to the read that produced the tag.",
                    "items": {
                        "oneOf": [
                            {
                                "type": "object",
                                "properties": {
                                    "op": {"enum": ["replace"], "description": "Replace lines start-end with the content."},
                                    "start": {"type": "integer", "minimum": 1},
                                    "end": {"type": "integer", "minimum": 1},
                                    "content": {"type": "string", "description": "The new lines. A trailing newline is optional."}
                                },
                                "required": ["op", "start", "end", "content"],
                                "additionalProperties": false
                            },
                            {
                                "type": "object",
                                "properties": {
                                    "op": {"enum": ["insert_before"], "description": "Insert the content before this line. line may be one past the last line to append."},
                                    "line": {"type": "integer", "minimum": 1},
                                    "content": {"type": "string"}
                                },
                                "required": ["op", "line", "content"],
                                "additionalProperties": false
                            },
                            {
                                "type": "object",
                                "properties": {
                                    "op": {"enum": ["insert_after"], "description": "Insert the content after this line."},
                                    "line": {"type": "integer", "minimum": 1},
                                    "content": {"type": "string"}
                                },
                                "required": ["op", "line", "content"],
                                "additionalProperties": false
                            },
                            {
                                "type": "object",
                                "properties": {
                                    "op": {"enum": ["delete"], "description": "Delete lines start-end."},
                                    "start": {"type": "integer", "minimum": 1},
                                    "end": {"type": "integer", "minimum": 1}
                                },
                                "required": ["op", "start", "end"],
                                "additionalProperties": false
                            }
                        ]
                    }
                }
            },
            "required": ["path", "tag", "edits"],
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
    fn replaces_a_line_range_and_returns_the_new_tag_and_placement() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "f.txt", "one\ntwo\nthree\n");
        let output = edit(
            &dir,
            json!({
                "path": "f.txt",
                "tag": tag_of("one\ntwo\nthree\n"),
                "edits": [{"op": "replace", "start": 2, "end": 3, "content": "TWO\nTHREE\nEXTRA"}],
            }),
        )
        .unwrap();
        assert_eq!(content(&dir, "f.txt"), "one\nTWO\nTHREE\nEXTRA\n");
        let expected_tag = tag_of("one\nTWO\nTHREE\nEXTRA\n");
        assert!(
            output
                .content
                .starts_with(&format!("[f.txt#{expected_tag}]"))
        );
        assert!(
            output
                .content
                .contains("applied 1 edit; the file now has 4 lines")
        );
        assert!(
            output
                .content
                .contains("edits[0] replace 2-3 -> now lines 2-4")
        );
        assert_eq!(output.summary.as_deref(), Some("edit f.txt: 1 ops"));
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
    fn a_stale_tag_on_a_shrunken_file_still_shows_context() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "f.txt", "one\ntwo\n");
        let error = edit(
            &dir,
            json!({
                "path": "f.txt",
                "tag": "000000",
                "edits": [{"op": "replace", "start": 40, "end": 40, "content": "X"}],
            }),
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("The file now has 2 lines"));
        assert!(message.contains("1:one"));
        assert!(message.contains("2:two"));
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
        assert!(
            output
                .content
                .contains("edits[0] insert_before 1 -> now line 1")
        );
        assert!(
            output
                .content
                .contains("edits[1] insert_after 1 -> now line 3")
        );
        assert!(
            output
                .content
                .contains("edits[2] insert_before 3 -> now line 5")
        );
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
        assert!(
            output
                .content
                .contains("edits[0] delete 1-1 -> removed; the next line is now line 1")
        );
        assert!(
            output
                .content
                .contains("edits[1] replace 3-3 -> now lines 2-3")
        );
        assert!(
            output
                .content
                .contains("edits[2] insert_after 4 -> now line 5")
        );
    }

    #[test]
    fn conflicting_edits_are_rejected_with_their_indices() {
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
        let message = overlap.to_string();
        assert!(message.contains("edits conflict"));
        assert!(message.contains("edits[0] (replace 1-2)"));
        assert!(message.contains("edits[1] (delete 2-3)"));

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
        let message = same_gap.to_string();
        assert!(message.contains("edits[0] (insert_after 2)"));
        assert!(message.contains("edits[1] (insert_before 3)"));
        assert_eq!(content(&dir, "f.txt"), "one\ntwo\nthree\nfour\n");
    }

    #[test]
    fn stray_fields_on_an_op_are_rejected_not_ignored() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "f.txt", "one\ntwo\n");
        let error = edit(
            &dir,
            json!({"path": "f.txt", "tag": tag_of("one\ntwo\n"), "edits": [
                {"op": "delete", "start": 2, "end": 2, "content": "REPLACEMENT"},
            ]}),
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("edits[0]"));
        assert!(message.contains("delete takes start, end"));
        assert_eq!(content(&dir, "f.txt"), "one\ntwo\n");
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
    fn crlf_content_is_normalized_and_crlf_files_stay_crlf() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "f.txt", "one\r\ntwo\r\nthree\r\n");
        edit(
            &dir,
            json!({
                "path": "f.txt",
                "tag": tag_of("one\r\ntwo\r\nthree\r\n"),
                "edits": [{"op": "replace", "start": 2, "end": 2, "content": "TWO\r\nMORE\r\n"}],
            }),
        )
        .unwrap();
        assert_eq!(content(&dir, "f.txt"), "one\r\nTWO\r\nMORE\r\nthree\r\n");

        write(&dir, "lf.txt", "one\ntwo\n");
        edit(
            &dir,
            json!({
                "path": "lf.txt",
                "tag": tag_of("one\ntwo\n"),
                "edits": [{"op": "replace", "start": 2, "end": 2, "content": "TWO\r\n"}],
            }),
        )
        .unwrap();
        assert_eq!(content(&dir, "lf.txt"), "one\nTWO\n");
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
        assert!(
            output
                .content
                .contains("edits[0] insert_before 1 -> now line 1")
        );
    }

    #[test]
    fn edits_outside_the_root_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(dir.path().join("outside.txt"), "x\n").unwrap();
        let error = run_now(EditTool::new(&root).run(json!({
            "path": "../outside.txt",
            "tag": tag_of("x\n"),
            "edits": [{"op": "delete", "start": 1, "end": 1}],
        })))
        .unwrap_err();
        assert!(error.to_string().contains("outside the project root"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("outside.txt")).unwrap(),
            "x\n"
        );
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
}
