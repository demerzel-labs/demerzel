//! The `read` tool: numbered file content under a snapshot tag.

use std::fmt::Write as _;
use std::path::PathBuf;

use demerzel_core::BoxFuture;
use demerzel_core::ToolAccess;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::snapshot::file_tag;
use crate::{Tool, ToolError, ToolOutput, display_path, parse_input, read_text, resolve};

/// Lines returned when the model sends no `limit`.
const DEFAULT_LIMIT: usize = 2000;

/// Total output budget in bytes.
const MAX_OUTPUT_BYTES: usize = 256 * 1024;

/// Bytes of one line shown before the line clips. Keeps a minified
/// one-line file from blowing the output budget in a single line.
const MAX_LINE_BYTES: usize = 2000;

pub struct ReadTool {
    root: PathBuf,
}

impl ReadTool {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn read(&self, input: Value) -> Result<ToolOutput, ToolError> {
        let input: ReadInput = parse_input(input)?;
        let offset = input.offset.unwrap_or(1);
        if offset == 0 {
            return Err(ToolError::InvalidInput(
                "offset counts lines from 1".to_string(),
            ));
        }
        let limit = input.limit.unwrap_or(DEFAULT_LIMIT);
        if limit == 0 {
            return Err(ToolError::InvalidInput(
                "limit must be at least 1".to_string(),
            ));
        }
        let path = resolve(&self.root, &input.path);
        let text = read_text(&path)?;
        let display = display_path(&self.root, &path);
        // The tag covers the whole file even for a window read: edit
        // validates the whole snapshot, not the window.
        let mut content = format!("[{display}#{}]\n", file_tag(&text));
        let total = text.lines().count();
        if total == 0 {
            content.push_str("[file is empty]");
            return Ok(ToolOutput::text(content).with_summary(format!("read {display} (empty)")));
        }
        if offset > total {
            return Err(ToolError::Failed(format!(
                "offset {offset} is past the end of {display}: the file has {total} lines"
            )));
        }

        let mut last = offset - 1;
        let mut clipped = false;
        for (index, line) in text.lines().enumerate().skip(offset - 1) {
            let number = index + 1;
            if number - offset >= limit {
                break;
            }
            let (shown, marker) = clip(line);
            let rendered_len = shown.len() + marker.map_or(0, str::len) + 12;
            if last >= offset && content.len() + rendered_len > MAX_OUTPUT_BYTES {
                break;
            }
            let _ = write!(content, "{number}:{shown}");
            if let Some(marker) = marker {
                clipped = true;
                content.push_str(marker);
            }
            content.push('\n');
            last = number;
        }
        let truncated = last < total || clipped;
        if last < total {
            let _ = write!(
                content,
                "[truncated: lines {offset}-{last} of {total}; continue with offset={}]",
                last + 1
            );
        } else {
            content.pop();
        }
        Ok(ToolOutput {
            content,
            is_error: false,
            truncated,
            summary: Some(format!("read {display} lines {offset}-{last} of {total}")),
        })
    }
}

/// Cut one line to the byte cap on a character boundary. The marker
/// names the full length, so the model does not copy a clipped line
/// back through the edit tool as if it were complete.
fn clip(line: &str) -> (&str, Option<&'static str>) {
    if line.len() <= MAX_LINE_BYTES {
        return (line, None);
    }
    let mut end = MAX_LINE_BYTES;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    (
        &line[..end],
        Some(" [line truncated: do not copy this line into an edit]"),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Read a text file. The output starts with a `[path#tag]` snapshot header; each line follows as `number:content`. The edit tool needs the tag and the line numbers."
    }

    fn access(&self) -> ToolAccess {
        ToolAccess::ReadOnly
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The file path, absolute or relative to the project root."
                },
                "offset": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "The line number of the first line to return. The count starts at 1."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "The maximum number of lines to return. Default: 2000."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn run(&self, input: Value) -> BoxFuture<'_, Result<ToolOutput, ToolError>> {
        Box::pin(async move { self.read(input) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::run_now;

    fn read(dir: &tempfile::TempDir, input: Value) -> Result<ToolOutput, ToolError> {
        run_now(ReadTool::new(dir.path()).run(input))
    }

    fn write(dir: &tempfile::TempDir, name: &str, content: &str) {
        std::fs::write(dir.path().join(name), content).unwrap();
    }

    #[test]
    fn numbers_every_line_under_the_snapshot_header() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "f.txt", "hello\nworld\n");
        let output = read(&dir, json!({"path": "f.txt"})).unwrap();
        assert_eq!(output.content, "[f.txt#1d0a3d]\n1:hello\n2:world");
        assert!(!output.truncated);
        assert_eq!(output.summary.as_deref(), Some("read f.txt lines 1-2 of 2"));
    }

    #[test]
    fn the_header_path_is_canonical_for_dotted_inputs() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "f.txt", "hello\n");
        let dotted = read(&dir, json!({"path": "./f.txt"})).unwrap();
        let plain = read(&dir, json!({"path": "f.txt"})).unwrap();
        assert_eq!(dotted.content, plain.content);
    }

    #[test]
    fn the_tag_covers_the_whole_file_for_a_window_read() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "f.txt", "one\ntwo\nthree\n");
        let full = read(&dir, json!({"path": "f.txt"})).unwrap();
        let window = read(&dir, json!({"path": "f.txt", "offset": 2, "limit": 1})).unwrap();
        assert!(full.content.starts_with("[f.txt#7b258d]\n"));
        assert!(window.content.starts_with("[f.txt#7b258d]\n"));
        assert!(window.content.contains("2:two"));
    }

    #[test]
    fn offset_and_limit_select_a_window_with_absolute_numbers() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "f.txt", "one\ntwo\nthree\nfour\n");
        let output = read(&dir, json!({"path": "f.txt", "offset": 2, "limit": 2})).unwrap();
        assert!(output.truncated);
        let lines: Vec<&str> = output.content.lines().collect();
        assert_eq!(lines[1], "2:two");
        assert_eq!(lines[2], "3:three");
        assert_eq!(
            lines[3],
            "[truncated: lines 2-3 of 4; continue with offset=4]"
        );
    }

    #[test]
    fn default_limit_truncates_large_files_with_a_marker() {
        let dir = tempfile::tempdir().unwrap();
        let content: String = (1..=2005).map(|n| format!("line {n}\n")).collect();
        write(&dir, "f.txt", &content);
        let output = read(&dir, json!({"path": "f.txt"})).unwrap();
        assert!(output.truncated);
        assert_eq!(output.content.lines().count(), 2002);
        assert!(
            output
                .content
                .ends_with("[truncated: lines 1-2000 of 2005; continue with offset=2001]")
        );
    }

    #[test]
    fn a_single_oversized_line_clips_instead_of_blowing_the_budget() {
        let dir = tempfile::tempdir().unwrap();
        let content = format!("{}\n", "x".repeat(500_000));
        write(&dir, "big.txt", &content);
        let output = read(&dir, json!({"path": "big.txt"})).unwrap();
        assert!(output.truncated);
        assert!(output.content.len() < 5_000);
        assert!(output.content.contains("[line truncated"));
    }

    #[test]
    fn byte_budget_truncates_before_the_line_limit() {
        let dir = tempfile::tempdir().unwrap();
        let long = "a".repeat(1500);
        let content: String = (0..200).map(|_| format!("{long}\n")).collect();
        write(&dir, "f.txt", &content);
        let output = read(&dir, json!({"path": "f.txt"})).unwrap();
        assert!(output.truncated);
        assert!(output.content.len() <= MAX_OUTPUT_BYTES + 100);
        assert!(output.content.contains("; continue with offset="));
    }

    #[test]
    fn stale_offset_past_the_end_is_a_precise_error() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "f.txt", "one\ntwo\n");
        let error = read(&dir, json!({"path": "f.txt", "offset": 9})).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("offset 9 is past the end"));
        assert!(message.contains("the file has 2 lines"));
    }

    #[test]
    fn missing_files_and_binary_files_are_actionable_errors() {
        let dir = tempfile::tempdir().unwrap();
        let missing = read(&dir, json!({"path": "missing.txt"})).unwrap_err();
        assert!(missing.to_string().contains("cannot read"));

        std::fs::write(dir.path().join("bin"), [0xff, 0x00, 0xfe, 0x01]).unwrap();
        let binary = read(&dir, json!({"path": "bin"})).unwrap_err();
        assert!(binary.to_string().contains("binary file"));
    }

    #[test]
    fn empty_files_read_as_a_tagged_marker() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "empty.txt", "");
        let output = read(&dir, json!({"path": "empty.txt"})).unwrap();
        assert_eq!(output.content, "[empty.txt#d0bfc1]\n[file is empty]");
        assert!(!output.is_error);
    }

    #[test]
    fn zero_offset_and_zero_limit_are_invalid_input() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "f.txt", "one\n");
        let offset = read(&dir, json!({"path": "f.txt", "offset": 0})).unwrap_err();
        assert!(matches!(offset, ToolError::InvalidInput(_)));
        let limit = read(&dir, json!({"path": "f.txt", "limit": 0})).unwrap_err();
        assert!(matches!(limit, ToolError::InvalidInput(_)));
    }
}
