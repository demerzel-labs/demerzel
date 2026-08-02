//! The `grep` tool: in-process content search over ripgrep's engine
//! crates. No subprocess.

use std::path::PathBuf;

use demerzel_core::ToolAccess;
use demerzel_providers::BoxFuture;
use grep_regex::RegexMatcher;
use grep_searcher::{BinaryDetection, SearcherBuilder, sinks::Lossy};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{Tool, ToolError, ToolOutput, parse_input, resolve, walk_files};

/// Matches reported before the output truncates.
const MAX_MATCHES: usize = 200;

/// Bytes of one matched line reported before the line truncates.
const MAX_LINE_BYTES: usize = 500;

pub struct GrepTool {
    root: PathBuf,
}

impl GrepTool {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn grep(&self, input: Value) -> Result<ToolOutput, ToolError> {
        let input: GrepInput = parse_input(input)?;
        let matcher = RegexMatcher::new(&input.pattern)
            .map_err(|error| ToolError::InvalidInput(format!("invalid regex pattern: {error}")))?;
        let base = resolve(&self.root, input.path.as_deref().unwrap_or("."));
        if !base.exists() {
            return Err(ToolError::Failed(format!(
                "cannot search {}: path not found",
                base.display()
            )));
        }
        let files = if base.is_file() {
            vec![base.clone()]
        } else {
            let mut files: Vec<PathBuf> = walk_files(&base).collect();
            files.sort();
            files
        };

        let mut searcher = SearcherBuilder::new()
            .line_number(true)
            .binary_detection(BinaryDetection::quit(b'\x00'))
            .build();
        let mut matches: Vec<String> = Vec::new();
        let mut clipped = false;
        for file in &files {
            let display = file
                .strip_prefix(&self.root)
                .unwrap_or(file)
                .display()
                .to_string();
            // Unreadable files are skipped; the search covers the rest.
            let _ = searcher.search_path(
                &matcher,
                file,
                Lossy(|number, line| {
                    let line = line.trim_end_matches(['\n', '\r']);
                    let (line, was_clipped) = clip(line);
                    clipped |= was_clipped;
                    matches.push(format!("{display}:{number}:{line}"));
                    Ok(matches.len() <= MAX_MATCHES)
                }),
            );
            if matches.len() > MAX_MATCHES {
                break;
            }
        }

        if matches.is_empty() {
            return Ok(ToolOutput::text("no matches"));
        }
        let over_limit = matches.len() > MAX_MATCHES;
        if over_limit {
            matches.truncate(MAX_MATCHES);
            matches.push(format!("[truncated after {MAX_MATCHES} matches]"));
        }
        Ok(ToolOutput {
            content: matches.join("\n"),
            is_error: false,
            truncated: over_limit || clipped,
        })
    }
}

/// Cut one matched line to the byte cap on a character boundary.
fn clip(line: &str) -> (String, bool) {
    if line.len() <= MAX_LINE_BYTES {
        return (line.to_string(), false);
    }
    let mut end = MAX_LINE_BYTES;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    (format!("{} [line truncated]", &line[..end]), true)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GrepInput {
    pattern: String,
    path: Option<String>,
}

impl Tool for GrepTool {
    fn name(&self) -> &'static str {
        "grep"
    }

    fn description(&self) -> &'static str {
        "Search file contents with a regular expression. The search skips files that .gitignore excludes."
    }

    fn access(&self) -> ToolAccess {
        ToolAccess::ReadOnly
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "The regular expression to search for."
                },
                "path": {
                    "type": "string",
                    "description": "The file or directory to search. Default: the project root."
                }
            },
            "required": ["pattern"],
            "additionalProperties": false
        })
    }

    fn run(&self, input: Value) -> BoxFuture<'_, Result<ToolOutput, ToolError>> {
        Box::pin(async move { self.grep(input) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::run_now;

    fn grep(dir: &tempfile::TempDir, input: Value) -> Result<ToolOutput, ToolError> {
        run_now(GrepTool::new(dir.path()).run(input))
    }

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src/nested")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join(".github")).unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {\n    needle();\n}\n").unwrap();
        std::fs::write(root.join("src/nested/lib.rs"), "// needle here\n").unwrap();
        std::fs::write(root.join("target/out.rs"), "needle in ignored\n").unwrap();
        std::fs::write(root.join(".git/config"), "needle in git dir\n").unwrap();
        std::fs::write(root.join(".github/ci.yml"), "run: needle\n").unwrap();
        dir
    }

    #[test]
    fn finds_matches_with_path_line_and_text() {
        let dir = fixture();
        let output = grep(&dir, json!({"pattern": "needle"})).unwrap();
        let lines: Vec<&str> = output.content.lines().collect();
        assert_eq!(
            lines,
            [
                ".github/ci.yml:1:run: needle",
                "src/main.rs:2:    needle();",
                "src/nested/lib.rs:1:// needle here",
            ]
        );
        assert!(!output.truncated);
    }

    #[test]
    fn skips_gitignored_files_and_the_git_dir() {
        let dir = fixture();
        let output = grep(&dir, json!({"pattern": "needle"})).unwrap();
        assert!(!output.content.contains("target/out.rs"));
        assert!(!output.content.contains(".git/config"));
    }

    #[test]
    fn searches_a_single_file_when_path_is_a_file() {
        let dir = fixture();
        let output = grep(&dir, json!({"pattern": "needle", "path": "src/main.rs"})).unwrap();
        assert_eq!(output.content, "src/main.rs:2:    needle();");
    }

    #[test]
    fn scopes_the_search_to_a_directory() {
        let dir = fixture();
        let output = grep(&dir, json!({"pattern": "needle", "path": "src"})).unwrap();
        assert_eq!(output.content.lines().count(), 2);
        assert!(!output.content.contains(".github"));
    }

    #[test]
    fn reports_no_matches_without_an_error() {
        let dir = fixture();
        let output = grep(&dir, json!({"pattern": "absent_zz"})).unwrap();
        assert_eq!(output.content, "no matches");
        assert!(!output.is_error);
    }

    #[test]
    fn rejects_an_invalid_regex_with_the_parse_error() {
        let dir = fixture();
        let error = grep(&dir, json!({"pattern": "(unclosed"})).unwrap_err();
        assert!(matches!(error, ToolError::InvalidInput(_)));
        assert!(error.to_string().contains("invalid regex pattern"));
    }

    #[test]
    fn errors_on_a_missing_search_path() {
        let dir = fixture();
        let error = grep(&dir, json!({"pattern": "x", "path": "no/such/dir"})).unwrap_err();
        assert!(error.to_string().contains("path not found"));
    }

    #[test]
    fn truncates_after_the_match_limit_with_a_marker() {
        let dir = tempfile::tempdir().unwrap();
        let content: String = (1..=MAX_MATCHES + 50)
            .map(|n| format!("needle {n}\n"))
            .collect();
        std::fs::write(dir.path().join("big.txt"), content).unwrap();
        let output = grep(&dir, json!({"pattern": "needle"})).unwrap();
        assert!(output.truncated);
        assert_eq!(output.content.lines().count(), MAX_MATCHES + 1);
        assert!(
            output
                .content
                .ends_with(&format!("[truncated after {MAX_MATCHES} matches]"))
        );
    }

    #[test]
    fn exactly_the_match_limit_does_not_truncate() {
        let dir = tempfile::tempdir().unwrap();
        let content: String = (1..=MAX_MATCHES).map(|n| format!("needle {n}\n")).collect();
        std::fs::write(dir.path().join("big.txt"), content).unwrap();
        let output = grep(&dir, json!({"pattern": "needle"})).unwrap();
        assert!(!output.truncated);
        assert_eq!(output.content.lines().count(), MAX_MATCHES);
    }

    #[test]
    fn clips_oversized_matched_lines_on_a_char_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let content = format!("start {} needle\n", "é".repeat(MAX_LINE_BYTES));
        std::fs::write(dir.path().join("long.txt"), content).unwrap();
        let output = grep(&dir, json!({"pattern": "start"})).unwrap();
        assert!(output.truncated);
        assert!(output.content.ends_with("[line truncated]"));
    }

    #[test]
    fn skips_binary_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("data.bin"), b"needle\x00binary").unwrap();
        std::fs::write(dir.path().join("text.txt"), "needle text\n").unwrap();
        let output = grep(&dir, json!({"pattern": "needle"})).unwrap();
        assert_eq!(output.content, "text.txt:1:needle text");
    }
}
