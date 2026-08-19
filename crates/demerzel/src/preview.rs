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
    /// A line ran past the byte budget and was cut short.
    cut: bool,
}

impl PreviewLines {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            bytes: 0,
            skipped: 0,
            cut: false,
        }
    }

    /// Either cap reached? The callers stop building lines here, so
    /// past the caps a preview drops to a cheap counting scan — the
    /// input is model-sized and lands inside the frame loop.
    fn full(&self) -> bool {
        self.lines.len() >= PREVIEW_LINES || self.bytes >= PREVIEW_TOTAL_BYTES
    }

    fn push(&mut self, line: &str) {
        self.push_prefixed("", line);
    }

    /// One preview line, sanitized: tabs become spaces, other control
    /// characters drop. An escape byte must never reach the terminal,
    /// where it could redraw or hide the approval prompt. The line
    /// spends only the remaining byte budget, and a cut is always
    /// visible: an approval prompt that shows a prefix as the whole
    /// input invites abuse.
    fn push_prefixed(&mut self, prefix: &str, line: &str) {
        if self.full() {
            self.skipped += 1;
            return;
        }
        let budget = PREVIEW_TOTAL_BYTES - self.bytes;
        let mut clean = String::with_capacity((prefix.len() + line.len()).min(budget));
        clean.push_str(prefix);
        for ch in line.chars() {
            if clean.len() >= budget {
                clean.push('…');
                self.cut = true;
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

    /// The `+`-prefixed lines of one op's content. Past the caps only
    /// the remaining lines are counted — a cheap scan, no building.
    fn push_content(&mut self, content: &str) {
        let body = content.strip_suffix('\n').unwrap_or(content);
        if body.is_empty() {
            return;
        }
        let mut lines = body.split('\n');
        while let Some(line) = lines.next() {
            if self.full() {
                self.skipped += 1 + lines.count();
                break;
            }
            self.push_prefixed("+ ", line);
        }
    }

    fn finish(mut self) -> String {
        if self.skipped > 0 {
            let word = if self.skipped == 1 { "line" } else { "lines" };
            self.lines.push(format!("… {} more {word}", self.skipped));
        }
        if self.cut {
            self.lines
                .push("… a line is cut at the preview cap".to_string());
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
    if command.is_empty() {
        out.push("$");
        return Some(out.finish());
    }
    let mut lines = command.lines();
    let mut prefix = "$ ";
    while let Some(line) = lines.next() {
        if out.full() {
            out.skipped += 1 + lines.count();
            break;
        }
        out.push_prefixed(prefix, line);
        prefix = "  ";
    }
    Some(out.finish())
}

/// The line count one skipped op adds to the truncation trailer. A
/// cheap scan without validation: ops past the caps only count.
fn op_lines(op: &Value) -> usize {
    let content = op.get("content").and_then(Value::as_str).unwrap_or("");
    let body = content.strip_suffix('\n').unwrap_or(content);
    let content_lines = if body.is_empty() {
        0
    } else {
        body.split('\n').count()
    };
    1 + content_lines
}

fn edit_preview(input: &Value) -> Option<String> {
    let path = input.get("path")?.as_str()?;
    let edits = input.get("edits")?.as_array()?;
    let mut out = PreviewLines::new();
    out.push(&format!("edit {path}"));
    let mut ops = edits.iter();
    while let Some(op) = ops.next() {
        // Ops past the caps are counted, not validated: the tool
        // still rejects a malformed op, and the preview stays capped.
        if out.full() {
            out.skipped += op_lines(op) + ops.map(op_lines).sum::<usize>();
            break;
        }
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
    fn a_cut_line_is_always_visible() {
        // A long single line must not pass for the whole input: the
        // cut gets an in-line marker and a trailer, and the hidden
        // tail never renders.
        let long = format!("echo {}; echo hidden-tail", "x".repeat(8000));
        let input = json!({"command": long});
        let preview = run_preview("bash", &input).unwrap();
        assert!(
            preview.len() <= PREVIEW_TOTAL_BYTES + 128,
            "{}",
            preview.len()
        );
        let mut lines = preview.lines();
        let first = lines.next().unwrap();
        assert!(first.ends_with('…'), "a cut line must end with the marker");
        assert_eq!(lines.next().unwrap(), "… a line is cut at the preview cap");
        assert!(!preview.contains("hidden-tail"));
    }

    #[test]
    fn lines_share_one_byte_budget() {
        // Every line spends the remaining budget, so the whole
        // preview stays near the byte cap instead of line-count
        // times the cap.
        let content = format!("{}\n{}\n", "a".repeat(3000), "b".repeat(3000));
        let input = json!({"path": "big.txt", "content": content});
        let preview = run_preview("create", &input).unwrap();
        assert!(
            preview.len() <= PREVIEW_TOTAL_BYTES + 128,
            "{}",
            preview.len()
        );
        assert!(preview.contains("… a line is cut at the preview cap"));
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
    fn the_preview_reads_only_fields_the_tool_schemas_declare() {
        // The preview hardcodes tool and field names; this pins them
        // to the registry, so a schema rename cannot silently turn
        // every diff preview into the JSON fallback.
        let dir = tempfile::tempdir().unwrap();
        let registry = demerzel_tools::Registry::full(dir.path());
        let definitions = registry.definitions();
        let schema = |tool: &str| {
            definitions
                .iter()
                .find(|definition| definition.name == tool)
                .unwrap_or_else(|| panic!("the registry lost the {tool} tool"))
                .input_schema
                .clone()
        };

        let has =
            |schema: &serde_json::Value, field: &str| schema["properties"].get(field).is_some();
        let bash = schema("bash");
        assert!(has(&bash, "command"));

        let create = schema("create");
        assert!(has(&create, "path"));
        assert!(has(&create, "content"));

        let edit = schema("edit");
        assert!(has(&edit, "path"));
        assert!(has(&edit, "edits"));
        let variants = edit["properties"]["edits"]["items"]["oneOf"]
            .as_array()
            .expect("edit ops are a oneOf list");
        let ops: Vec<&str> = variants
            .iter()
            .flat_map(|variant| variant["properties"]["op"]["enum"].as_array())
            .flat_map(|names| names.iter().filter_map(serde_json::Value::as_str))
            .collect();
        for op in ["replace", "insert_before", "insert_after", "delete"] {
            assert!(ops.contains(&op), "the edit schema lost the {op} op");
        }
        for variant in variants {
            let fields = variant["properties"].as_object().unwrap();
            for field in fields.keys() {
                assert!(
                    ["op", "start", "end", "line", "content"].contains(&field.as_str()),
                    "the preview does not know the {field} field"
                );
            }
        }
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
