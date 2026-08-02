//! The `read` tool: file content with hash-tagged lines.

use std::path::PathBuf;

use demerzel_providers::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::hashline::line_tag;
use crate::{Tool, ToolError, ToolOutput, parse_input, resolve};

/// Lines returned when the model sends no `limit`.
const DEFAULT_LIMIT: usize = 2000;

/// Output budget in bytes. The tool emits whole lines only — a partial
/// line would break the tag contract — so one oversized line can pass
/// the budget alone.
const MAX_OUTPUT_BYTES: usize = 256 * 1024;

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
        let bytes = std::fs::read(&path).map_err(|error| {
            ToolError::Failed(format!("cannot read {}: {error}", path.display()))
        })?;
        let text = String::from_utf8(bytes).map_err(|_| {
            ToolError::Failed(format!(
                "{} is not valid UTF-8; the read tool supports text files only",
                path.display()
            ))
        })?;
        let lines: Vec<&str> = text.lines().collect();
        let total = lines.len();
        if total == 0 {
            return Ok(ToolOutput::text("[file is empty]"));
        }
        if offset > total {
            return Err(ToolError::Failed(format!(
                "offset {offset} is past the end of {}: the file has {total} lines",
                path.display()
            )));
        }

        let mut content = String::new();
        let mut last = offset - 1;
        for (index, line) in lines.iter().enumerate().skip(offset - 1) {
            let over_lines = index + 1 - offset >= limit;
            // 8 = tag (6) + '|' + '\n'.
            let over_bytes =
                !content.is_empty() && content.len() + line.len() + 8 > MAX_OUTPUT_BYTES;
            if over_lines || over_bytes {
                break;
            }
            content.push_str(&line_tag(line));
            content.push('|');
            content.push_str(line);
            content.push('\n');
            last = index + 1;
        }
        let truncated = last < total;
        if truncated {
            content.push_str(&format!(
                "[truncated: lines {offset}-{last} of {total}; continue with offset={}]",
                last + 1
            ));
        } else {
            content.pop();
        }
        Ok(ToolOutput {
            content,
            is_error: false,
            truncated,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

impl Tool for ReadTool {
    fn name(&self) -> &'static str {
        "read"
    }

    fn description(&self) -> &'static str {
        "Read a text file. Each output line starts with a six-character content-hash tag: `a3f2c1|line text`."
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
    fn tags_every_line_with_its_content_hash() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "f.txt", "fn main() {\nline text\n");
        let output = read(&dir, json!({"path": "f.txt"})).unwrap();
        assert_eq!(output.content, "74ebed|fn main() {\ne7edf3|line text");
        assert!(!output.truncated);
    }

    #[test]
    fn identical_lines_share_one_tag() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "f.txt", "alpha\nalpha\n");
        let output = read(&dir, json!({"path": "f.txt"})).unwrap();
        let lines: Vec<&str> = output.content.lines().collect();
        assert_eq!(lines[0], lines[1]);
    }

    #[test]
    fn crlf_endings_hash_like_lf_endings() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "lf.txt", "alpha\nbeta\n");
        write(&dir, "crlf.txt", "alpha\r\nbeta\r\n");
        let lf = read(&dir, json!({"path": "lf.txt"})).unwrap();
        let crlf = read(&dir, json!({"path": "crlf.txt"})).unwrap();
        assert_eq!(lf.content, crlf.content);
    }

    #[test]
    fn offset_and_limit_select_a_window() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "f.txt", "one\ntwo\nthree\nfour\n");
        let output = read(&dir, json!({"path": "f.txt", "offset": 2, "limit": 2})).unwrap();
        assert!(output.truncated);
        let lines: Vec<&str> = output.content.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].ends_with("|two"));
        assert!(lines[1].ends_with("|three"));
        assert_eq!(
            lines[2],
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
        assert_eq!(output.content.lines().count(), 2001);
        assert!(
            output
                .content
                .ends_with("[truncated: lines 1-2000 of 2005; continue with offset=2001]")
        );
    }

    #[test]
    fn byte_budget_truncates_before_the_line_limit() {
        let dir = tempfile::tempdir().unwrap();
        let content = format!("{}\n{}\nshort\n", "a".repeat(200_000), "b".repeat(100_000));
        write(&dir, "f.txt", &content);
        let output = read(&dir, json!({"path": "f.txt"})).unwrap();
        assert!(output.truncated);
        assert_eq!(output.content.lines().count(), 2);
        assert!(
            output
                .content
                .ends_with("[truncated: lines 1-1 of 3; continue with offset=2]")
        );
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

        std::fs::write(dir.path().join("bin"), [0xff, 0xfe, 0x00, 0x01]).unwrap();
        let binary = read(&dir, json!({"path": "bin"})).unwrap_err();
        assert!(binary.to_string().contains("not valid UTF-8"));
    }

    #[test]
    fn empty_files_read_as_an_explicit_marker() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "empty.txt", "");
        let output = read(&dir, json!({"path": "empty.txt"})).unwrap();
        assert_eq!(output.content, "[file is empty]");
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
