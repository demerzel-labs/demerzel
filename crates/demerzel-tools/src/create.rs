//! The `create` tool: write a new file. Refuses existing files, so an
//! overwrite is always an explicit read + edit.

use std::io::Write as _;
use std::path::PathBuf;

use demerzel_core::{BoxFuture, ToolAccess};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::hashline::file_tag;
use crate::{Tool, ToolError, ToolOutput, display_path, parse_input, resolve_write};

pub struct CreateTool {
    root: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateInput {
    path: String,
    content: String,
}

impl CreateTool {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn create(&self, input: Value) -> Result<ToolOutput, ToolError> {
        let input: CreateInput = parse_input(input)?;
        let path = resolve_write(&self.root, &input.path)?;
        let display = display_path(&self.root, &path);
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|error| {
                ToolError::Failed(format!("cannot create {}: {error}", parent.display()))
            })?;
        }
        // create_new is O_CREAT|O_EXCL: it fails on any existing path,
        // including a dangling symlink, with no check-then-write gap.
        let mut file = std::fs::File::options()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    ToolError::Failed(format!(
                        "{display} exists; read it and change it with the edit tool"
                    ))
                } else {
                    ToolError::Failed(format!("cannot create {display}: {error}"))
                }
            })?;
        file.write_all(input.content.as_bytes())
            .map_err(|error| ToolError::Failed(format!("cannot create {display}: {error}")))?;
        let total = input.content.lines().count();
        Ok(ToolOutput::text(format!(
            "[{display}#{}]\ncreated with {total} line{}",
            file_tag(&input.content),
            if total == 1 { "" } else { "s" },
        ))
        .with_summary(format!("create {display} ({total} lines)")))
    }
}

impl Tool for CreateTool {
    fn name(&self) -> &str {
        "create"
    }

    fn description(&self) -> &str {
        "Create a new file with the given content. Fails if the file exists; change an existing file with the edit tool."
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
                    "description": "The new file path, relative to the project root. Parent directories are created."
                },
                "content": {
                    "type": "string",
                    "description": "The full file content."
                }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        })
    }

    fn run(&self, input: Value) -> BoxFuture<'_, Result<ToolOutput, ToolError>> {
        Box::pin(async move { self.create(input) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::run_now;

    fn create(dir: &tempfile::TempDir, input: Value) -> Result<ToolOutput, ToolError> {
        run_now(CreateTool::new(dir.path()).run(input))
    }

    #[test]
    fn creates_a_new_file_with_parents_and_returns_the_tag() {
        let dir = tempfile::tempdir().unwrap();
        let output = create(
            &dir,
            json!({"path": "sub/new.txt", "content": "alpha\nbeta\n"}),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("sub/new.txt")).unwrap(),
            "alpha\nbeta\n"
        );
        assert!(output.content.contains("created with 2 lines"));
        assert!(output.content.contains(&file_tag("alpha\nbeta\n")));
        assert_eq!(
            output.summary.as_deref(),
            Some("create sub/new.txt (2 lines)")
        );
    }

    #[test]
    fn refuses_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "old\n").unwrap();
        let error = create(&dir, json!({"path": "f.txt", "content": "new\n"})).unwrap_err();
        assert!(error.to_string().contains("exists"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "old\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_dangling_symlink() {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(dir.path().join("gone.txt"), dir.path().join("link.txt"))
            .unwrap();
        // O_EXCL fails with EEXIST on a symlink, even a dangling one,
        // so nothing is written through the link.
        let error = create(&dir, json!({"path": "link.txt", "content": "x\n"})).unwrap_err();
        assert!(error.to_string().contains("exists"));
        assert!(!dir.path().join("gone.txt").exists());
    }

    #[test]
    fn refuses_paths_outside_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let error =
            run_now(CreateTool::new(&root).run(json!({"path": "../escape.txt", "content": "x\n"})))
                .unwrap_err();
        assert!(error.to_string().contains("outside the project root"));
        assert!(!dir.path().join("escape.txt").exists());
    }
}
