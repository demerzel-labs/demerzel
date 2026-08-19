//! Capped tool-input previews. Both interfaces show the same preview
//! for the same call.
//!
//! Two kinds. [`run_preview`] renders a dispatching call for the
//! transcript: a diff for the edit and create tools, the command line
//! for bash. [`ask_preview`] covers the approval question for every
//! other tool with the capped JSON input, so the user never approves
//! a call blind.

use serde_json::Value;

/// Byte cap for the tool input shown in an approval prompt.
pub(crate) const PREVIEW_BYTES: usize = 1200;

/// Line cap for one rendered call preview.
const PREVIEW_LINES: usize = 24;

/// Byte cap for one rendered call preview.
const PREVIEW_TOTAL_BYTES: usize = 4096;

/// Serializes into a capped buffer, so a preview of a large input
/// never materializes the whole document.
struct CappedWriter {
    buf: Vec<u8>,
    cap: usize,
}

impl std::io::Write for CappedWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.buf.len() >= self.cap {
            return Err(std::io::Error::other("preview cap reached"));
        }
        let take = data.len().min(self.cap - self.buf.len());
        self.buf.extend_from_slice(&data[..take]);
        if take < data.len() {
            return Err(std::io::Error::other("preview cap reached"));
        }
        Ok(take)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The preview a dispatching tool call renders into the transcript.
/// `None` for read-only tools and for inputs whose shape the preview
/// does not know; the approval path then falls back to the JSON
/// preview.
pub(crate) fn run_preview(name: &str, input: &Value) -> Option<String> {
    match name {
        "bash" => bash_preview(input),
        "edit" => edit_preview(input),
        "create" => create_preview(input),
        _ => None,
    }
}

/// The preview an approval question still needs to show. `None` when
/// the dispatch preview already rendered the call.
pub(crate) fn ask_preview(name: &str, input: &Value) -> Option<String> {
    if run_preview(name, input).is_some() {
        None
    } else {
        Some(input_preview(input))
    }
}

/// Collects preview lines under the line and byte caps, and counts
/// what fell over them.
struct PreviewLines {
    lines: Vec<String>,
    bytes: usize,
    skipped: usize,
}

impl PreviewLines {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            bytes: 0,
            skipped: 0,
        }
    }

    /// One preview line, sanitized: tabs become spaces, other control
    /// characters drop. An escape byte must never reach the terminal,
    /// where it could redraw or hide the approval prompt.
    fn push(&mut self, line: &str) {
        if self.lines.len() >= PREVIEW_LINES || self.bytes >= PREVIEW_TOTAL_BYTES {
            self.skipped += 1;
            return;
        }
        let mut clean = String::with_capacity(line.len().min(PREVIEW_TOTAL_BYTES));
        for ch in line.chars() {
            if clean.len() >= PREVIEW_TOTAL_BYTES {
                break;
            }
            if ch == '\t' {
                clean.push_str("    ");
            } else if !ch.is_control() {
                clean.push(ch);
            }
        }
        self.bytes += clean.len();
        self.lines.push(clean);
    }

    /// The `+`-prefixed lines of one op's content.
    fn push_content(&mut self, content: &str) {
        let body = content.strip_suffix('\n').unwrap_or(content);
        if body.is_empty() {
            return;
        }
        for line in body.split('\n') {
            self.push(&format!("+ {line}"));
        }
    }

    fn finish(mut self) -> String {
        if self.skipped > 0 {
            let word = if self.skipped == 1 { "line" } else { "lines" };
            self.lines.push(format!("… {} more {word}", self.skipped));
        }
        self.lines.join("\n")
    }
}

/// A line range in the caller's own words.
fn range(start: u64, end: u64) -> String {
    if start == end {
        format!("line {start}")
    } else {
        format!("lines {start}-{end}")
    }
}

fn bash_preview(input: &Value) -> Option<String> {
    let command = input.get("command")?.as_str()?;
    let mut out = PreviewLines::new();
    for (index, line) in command.lines().enumerate() {
        if index == 0 {
            out.push(&format!("$ {line}"));
        } else {
            out.push(&format!("  {line}"));
        }
    }
    if command.is_empty() {
        out.push("$");
    }
    Some(out.finish())
}

fn edit_preview(input: &Value) -> Option<String> {
    let path = input.get("path")?.as_str()?;
    let edits = input.get("edits")?.as_array()?;
    let mut out = PreviewLines::new();
    out.push(&format!("edit {path}"));
    for op in edits {
        let object = op.as_object()?;
        let field = |name: &str| object.get(name).and_then(Value::as_u64);
        let content = || object.get("content").and_then(Value::as_str);
        match object.get("op")?.as_str()? {
            "replace" => {
                out.push(&format!(
                    "@ replace {}",
                    range(field("start")?, field("end")?)
                ));
                out.push_content(content()?);
            }
            "insert_before" => {
                out.push(&format!("@ insert before line {}", field("line")?));
                out.push_content(content()?);
            }
            "insert_after" => {
                out.push(&format!("@ insert after line {}", field("line")?));
                out.push_content(content()?);
            }
            "delete" => {
                out.push(&format!(
                    "@ delete {}",
                    range(field("start")?, field("end")?)
                ));
            }
            _ => return None,
        }
    }
    Some(out.finish())
}

fn create_preview(input: &Value) -> Option<String> {
    let path = input.get("path")?.as_str()?;
    let content = input.get("content")?.as_str()?;
    let mut out = PreviewLines::new();
    out.push(&format!("create {path}"));
    out.push_content(content);
    Some(out.finish())
}

/// Pretty tool input, capped for the screen.
pub(crate) fn input_preview(input: &Value) -> String {
    let mut writer = CappedWriter {
        buf: Vec::with_capacity(PREVIEW_BYTES.min(256)),
        cap: PREVIEW_BYTES,
    };
    let complete = serde_json::to_writer_pretty(&mut writer, input).is_ok();
    let text = String::from_utf8_lossy(&writer.buf).into_owned();
    if complete {
        text
    } else {
        format!("{text}\n[input truncated]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_input_preview_caps_long_inputs_without_serializing_them() {
        let short = json!({"path": "f.txt"});
        assert_eq!(
            input_preview(&short),
            serde_json::to_string_pretty(&short).unwrap()
        );

        let long = json!({"content": "é".repeat(200_000)});
        let preview = input_preview(&long);
        assert!(preview.len() < PREVIEW_BYTES + 100);
        assert!(preview.ends_with("[input truncated]"));
    }

    #[test]
    fn a_bash_call_previews_as_its_command_line() {
        let input = json!({"command": "cargo test", "timeout_seconds": 60});
        assert_eq!(run_preview("bash", &input).unwrap(), "$ cargo test");

        let multi = json!({"command": "cargo fmt\ncargo test"});
        assert_eq!(
            run_preview("bash", &multi).unwrap(),
            "$ cargo fmt\n  cargo test"
        );
    }

    #[test]
    fn an_edit_call_previews_as_a_diff_of_its_ops() {
        let input = json!({
            "path": "src/main.rs",
            "tag": "abc",
            "edits": [
                {"op": "replace", "start": 3, "end": 4, "content": "one\ntwo\n"},
                {"op": "insert_after", "line": 9, "content": "three"},
                {"op": "delete", "start": 12, "end": 12},
            ],
        });
        assert_eq!(
            run_preview("edit", &input).unwrap(),
            "edit src/main.rs\n\
             @ replace lines 3-4\n\
             + one\n\
             + two\n\
             @ insert after line 9\n\
             + three\n\
             @ delete line 12"
        );
    }

    #[test]
    fn a_create_call_previews_its_content_as_added_lines() {
        let input = json!({"path": "notes.txt", "content": "alpha\nbeta\n"});
        assert_eq!(
            run_preview("create", &input).unwrap(),
            "create notes.txt\n+ alpha\n+ beta"
        );
    }

    #[test]
    fn previews_cap_their_lines_and_report_the_rest() {
        let content: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let input = json!({"path": "big.txt", "content": content});
        let preview = run_preview("create", &input).unwrap();
        assert!(preview.lines().count() <= 26);
        assert!(preview.ends_with("more lines"), "{preview}");
    }

    #[test]
    fn previews_neutralize_control_bytes() {
        let input = json!({"command": "echo \u{1b}[2Jhi\tthere"});
        assert_eq!(
            run_preview("bash", &input).unwrap(),
            "$ echo [2Jhi    there"
        );
    }

    #[test]
    fn a_malformed_input_falls_back_to_the_json_preview() {
        let input = json!({"path": "f.rs", "edits": [{"op": "explode"}]});
        assert_eq!(run_preview("edit", &input), None);
        let ask = ask_preview("edit", &input).unwrap();
        assert!(ask.contains("explode"));

        // A covered call needs nothing more at the question.
        let bash = json!({"command": "ls"});
        assert_eq!(ask_preview("bash", &bash), None);
    }
}
