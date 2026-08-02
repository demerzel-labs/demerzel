//! The `glob` tool: in-process file matching. Respects `.gitignore`.

use std::path::PathBuf;

use demerzel_core::{BoxFuture, ToolAccess};
use globset::GlobBuilder;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{Tool, ToolError, ToolOutput, parse_input, resolve, walk_files};

/// Paths reported before the output truncates.
const MAX_RESULTS: usize = 1000;

pub struct GlobTool {
    root: PathBuf,
}

impl GlobTool {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn glob(&self, input: Value) -> Result<ToolOutput, ToolError> {
        let input: GlobInput = parse_input(input)?;
        // With a literal separator, `*` stays inside one path segment
        // and `**` crosses segments — shell semantics, not substring.
        let glob = GlobBuilder::new(&input.pattern)
            .literal_separator(true)
            .build()
            .map_err(|error| ToolError::InvalidInput(format!("invalid glob pattern: {error}")))?;
        let matcher = glob.compile_matcher();
        let base = resolve(&self.root, input.path.as_deref().unwrap_or("."));
        if !base.is_dir() {
            return Err(ToolError::Failed(format!(
                "cannot glob in {}: not a directory",
                base.display()
            )));
        }

        let mut paths: Vec<String> = walk_files(&base)
            .filter(|path| matcher.is_match(path.strip_prefix(&base).unwrap_or(path)))
            .map(|path| {
                path.strip_prefix(&self.root)
                    .unwrap_or(&path)
                    .display()
                    .to_string()
            })
            .collect();
        paths.sort();

        if paths.is_empty() {
            return Ok(ToolOutput::text("no files match").with_summary("glob: no files"));
        }
        let truncated = paths.len() > MAX_RESULTS;
        if truncated {
            paths.truncate(MAX_RESULTS);
            paths.push(format!("[truncated after {MAX_RESULTS} files]"));
        }
        let found = paths.len() - usize::from(truncated);
        Ok(ToolOutput {
            content: paths.join("\n"),
            is_error: false,
            truncated,
            summary: Some(format!("glob: {found} files")),
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GlobInput {
    pattern: String,
    path: Option<String>,
}

impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Find files that match a glob pattern. The search skips files that .gitignore excludes."
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
                    "description": "The glob pattern, for example `**/*.rs`."
                },
                "path": {
                    "type": "string",
                    "description": "The directory to search. Default: the project root."
                }
            },
            "required": ["pattern"],
            "additionalProperties": false
        })
    }

    fn run(&self, input: Value) -> BoxFuture<'_, Result<ToolOutput, ToolError>> {
        Box::pin(async move { self.glob(input) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::run_now;

    fn glob(dir: &tempfile::TempDir, input: Value) -> Result<ToolOutput, ToolError> {
        run_now(GlobTool::new(dir.path()).run(input))
    }

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src/nested")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join(".github")).unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(root.join("root.rs"), "").unwrap();
        std::fs::write(root.join("src/main.rs"), "").unwrap();
        std::fs::write(root.join("src/nested/lib.rs"), "").unwrap();
        std::fs::write(root.join("target/gen.rs"), "").unwrap();
        std::fs::write(root.join(".git/hook.rs"), "").unwrap();
        std::fs::write(root.join(".github/ci.yml"), "").unwrap();
        dir
    }

    #[test]
    fn recursive_patterns_find_nested_files_sorted() {
        let dir = fixture();
        let output = glob(&dir, json!({"pattern": "**/*.rs"})).unwrap();
        let lines: Vec<&str> = output.content.lines().collect();
        assert_eq!(lines, ["root.rs", "src/main.rs", "src/nested/lib.rs"]);
        assert!(!output.truncated);
    }

    #[test]
    fn a_single_star_stays_in_one_path_segment() {
        let dir = fixture();
        let output = glob(&dir, json!({"pattern": "*.rs"})).unwrap();
        assert_eq!(output.content, "root.rs");
    }

    #[test]
    fn skips_gitignored_files_and_the_git_dir() {
        let dir = fixture();
        let output = glob(&dir, json!({"pattern": "**/*.rs"})).unwrap();
        assert!(!output.content.contains("target/gen.rs"));
        assert!(!output.content.contains(".git/hook.rs"));
    }

    #[test]
    fn hidden_files_outside_git_are_found() {
        let dir = fixture();
        let output = glob(&dir, json!({"pattern": "**/*.yml"})).unwrap();
        assert_eq!(output.content, ".github/ci.yml");
    }

    #[test]
    fn a_path_input_scopes_the_search() {
        let dir = fixture();
        let output = glob(&dir, json!({"pattern": "**/*.rs", "path": "src"})).unwrap();
        let lines: Vec<&str> = output.content.lines().collect();
        assert_eq!(lines, ["src/main.rs", "src/nested/lib.rs"]);
    }

    #[test]
    fn reports_no_files_match_without_an_error() {
        let dir = fixture();
        let output = glob(&dir, json!({"pattern": "**/*.py"})).unwrap();
        assert_eq!(output.content, "no files match");
        assert!(!output.is_error);
    }

    #[test]
    fn rejects_an_invalid_pattern_with_the_parse_error() {
        let dir = fixture();
        let error = glob(&dir, json!({"pattern": "["})).unwrap_err();
        assert!(matches!(error, ToolError::InvalidInput(_)));
        assert!(error.to_string().contains("invalid glob pattern"));
    }

    #[test]
    fn errors_when_the_base_is_not_a_directory() {
        let dir = fixture();
        let error = glob(&dir, json!({"pattern": "*", "path": "root.rs"})).unwrap_err();
        assert!(error.to_string().contains("not a directory"));
    }

    #[test]
    fn truncates_after_the_result_limit_with_a_marker() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..MAX_RESULTS + 10 {
            std::fs::write(dir.path().join(format!("f{n:04}.rs")), "").unwrap();
        }
        let output = glob(&dir, json!({"pattern": "*.rs"})).unwrap();
        assert!(output.truncated);
        assert_eq!(output.content.lines().count(), MAX_RESULTS + 1);
        assert!(
            output
                .content
                .ends_with(&format!("[truncated after {MAX_RESULTS} files]"))
        );
    }
}
