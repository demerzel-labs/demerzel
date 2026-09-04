//! Tools the model calls through the harness.
//!
//! [`Tool`] is the contract: name, description, access class, JSON-
//! Schema input, and one async `run`. [`Registry`] produces the
//! [`ToolDefinition`] list for the request, reports each tool's access
//! class for the engine's policy check, and runs approved calls. Tool
//! failures return to the model as [`ToolOutput`] with
//! `is_error: true`; they never panic the harness.
//!
//! Cancellation contract: dropping the future returned by `run`
//! cancels the call. Every tool must leave the filesystem and the
//! process table clean on drop.

mod bash;
mod create;
mod edit;
mod glob;
mod grep;
mod read;
mod snapshot;

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use demerzel_core::{BoxFuture, ToolAccess, ToolDefinition, ToolHost};
use serde::de::DeserializeOwned;
use serde_json::Value;

pub use demerzel_core::ToolOutput;

pub use bash::BashTool;
pub use create::CreateTool;
pub use edit::EditTool;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use read::ReadTool;
pub use snapshot::file_tag;

/// Input cap for the read and edit tools, and the walker's file-size
/// cap for grep and glob. One bound, so the tools agree on what a
/// too-large file is.
pub(crate) const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// Tool failure classes. Both reach the model as an error result with
/// a message it can act on.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// The input JSON does not match the tool schema.
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// The tool ran and failed against the current filesystem state.
    #[error("{0}")]
    Failed(String),
}

/// The contract every tool implements. `run` returns a boxed future so
/// the trait stays object-safe. Names and descriptions are borrowed,
/// not `'static`, so runtime-defined tools fit the trait.
pub trait Tool: Send + Sync {
    /// Stable tool name, e.g. "read". This is the dispatch key.
    fn name(&self) -> &str;

    fn description(&self) -> &str;

    /// The declared access class. The approval policy keys on it
    /// before dispatch; it never reaches the model.
    fn access(&self) -> ToolAccess;

    /// JSON Schema for the tool input.
    fn input_schema(&self) -> Value;

    /// Run one call. Failures come back as `Err`; the registry turns
    /// them into error results for the model. Dropping the returned
    /// future cancels the call.
    fn run(&self, input: Value) -> BoxFuture<'_, Result<ToolOutput, ToolError>>;
}

/// Holds the tool set for one session. Registration order is the
/// definition order, so the prompt prefix stays stable.
#[derive(Default)]
pub struct Registry {
    tools: Vec<Box<dyn Tool>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The built-in tool set: read, edit, create, bash, grep, glob.
    /// Model-supplied relative paths resolve against `root`. Shape the
    /// set with [`Registry::retain`] or extend it with `register`.
    pub fn full(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let mut registry = Self::new();
        registry.register(Box::new(ReadTool::new(root.clone())));
        registry.register(Box::new(EditTool::new(root.clone())));
        registry.register(Box::new(CreateTool::new(root.clone())));
        registry.register(Box::new(BashTool::new(root.clone())));
        registry.register(Box::new(GrepTool::new(root.clone())));
        registry.register(Box::new(GlobTool::new(root)));
        registry
    }

    /// Add a tool. Tool sets are wired at startup, so a duplicate name
    /// is a programming error and panics.
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        assert!(
            self.find(tool.name()).is_none(),
            "duplicate tool name: {}",
            tool.name()
        );
        self.tools.push(tool);
    }

    /// Keep only the tools the predicate accepts. A read-only session
    /// drops the write tools instead of advertising and refusing them:
    /// `registry.retain(|tool| tool.access() == ToolAccess::ReadOnly)`.
    pub fn retain(&mut self, mut keep: impl FnMut(&dyn Tool) -> bool) {
        self.tools.retain(|tool| keep(tool.as_ref()));
    }

    /// Tool definitions for the model request, in registration order.
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .iter()
            .map(|tool| ToolDefinition {
                name: tool.name().to_string(),
                description: tool.description().to_string(),
                input_schema: tool.input_schema(),
            })
            .collect()
    }

    /// The declared access class of the named tool. `None` for a name
    /// the registry does not hold; the engine then skips the policy
    /// and `run_approved` produces the unknown-tool error.
    pub fn access(&self, name: &str) -> Option<ToolAccess> {
        self.find(name).map(Tool::access)
    }

    /// Run one approved call. The caller owns the approval check;
    /// unknown names and tool failures become error results for the
    /// model, and this method never fails the harness.
    pub fn run_approved(&self, name: &str, input: Value) -> BoxFuture<'_, ToolOutput> {
        let Some(tool) = self.find(name) else {
            let known: Vec<&str> = self.tools.iter().map(|tool| tool.name()).collect();
            let message = format!(
                "unknown tool: {name}; available tools: {}",
                known.join(", ")
            );
            return Box::pin(std::future::ready(ToolOutput::error(message)));
        };
        Box::pin(async move {
            tool.run(input)
                .await
                .unwrap_or_else(|error| ToolOutput::error(error.to_string()))
        })
    }

    fn find(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|tool| tool.name() == name)
            .map(Box::as_ref)
    }
}

/// The registry is the loop's tool surface. The engine applies the
/// approval policy itself from the access class reported here.
impl ToolHost for Registry {
    fn definitions(&self) -> Vec<ToolDefinition> {
        Registry::definitions(self)
    }

    fn access(&self, name: &str) -> Option<ToolAccess> {
        Registry::access(self, name)
    }

    fn run_approved(&self, name: &str, input: Value) -> BoxFuture<'_, ToolOutput> {
        Registry::run_approved(self, name, input)
    }
}

/// Parse a tool input value into its typed form.
pub(crate) fn parse_input<T: DeserializeOwned>(input: Value) -> Result<T, ToolError> {
    serde_json::from_value(input).map_err(|error| ToolError::InvalidInput(error.to_string()))
}

/// Read a file as UTF-8 text with actionable errors, shared by the
/// read and edit tools. Refuses directories, files over
/// [`MAX_FILE_BYTES`], and binary files — before loading, where the
/// metadata allows it.
pub(crate) fn read_text(path: &Path) -> Result<String, ToolError> {
    let fail = |error: std::io::Error| {
        ToolError::Failed(format!("cannot read {}: {error}", path.display()))
    };
    let metadata = std::fs::metadata(path).map_err(fail)?;
    if metadata.is_dir() {
        return Err(ToolError::Failed(format!(
            "{} is a directory, not a file",
            path.display()
        )));
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(ToolError::Failed(format!(
            "{} is {} bytes and the tool cap is {MAX_FILE_BYTES} bytes; use grep or bash for large files",
            path.display(),
            metadata.len(),
        )));
    }
    let bytes = std::fs::read(path).map_err(fail)?;
    if bytes.iter().take(8192).any(|byte| *byte == 0) {
        return Err(ToolError::Failed(format!(
            "{} is a binary file; only text files are supported",
            path.display()
        )));
    }
    String::from_utf8(bytes).map_err(|_| {
        ToolError::Failed(format!(
            "{} is not valid UTF-8; only text files are supported",
            path.display()
        ))
    })
}

/// Resolve a model-supplied path against the tool root. Read tools may
/// leave the root; write tools use [`resolve_write`].
pub(crate) fn resolve(root: &Path, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

/// Drop `.` components and resolve `..` lexically, so a later
/// existing-ancestor check cannot be bypassed by a path that climbs
/// out through a directory that does not exist yet.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Resolve a path for a write tool and confine it to the root: the
/// deepest existing ancestor must canonicalize to a path inside the
/// canonical root, so neither `..` nor a symlink can move a write
/// outside the project.
pub(crate) fn resolve_write(root: &Path, input: &str) -> Result<PathBuf, ToolError> {
    if input.trim().is_empty() {
        return Err(ToolError::InvalidInput("path must not be empty".into()));
    }
    let canonical_root = root
        .canonicalize()
        .map_err(|error| ToolError::Failed(format!("cannot resolve the project root: {error}")))?;
    // Resolve against the canonical root, so the ancestor walk below
    // always terminates at the root even when `root` is relative.
    let resolved = normalize(&resolve(&canonical_root, input));
    let mut ancestor = resolved.as_path();
    let canonical_ancestor = loop {
        match ancestor.canonicalize() {
            Ok(canonical) => break canonical,
            Err(_) => match ancestor.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => ancestor = parent,
                _ => {
                    return Err(ToolError::Failed(format!(
                        "cannot resolve {input}: no existing ancestor directory"
                    )));
                }
            },
        }
    };
    if !canonical_ancestor.starts_with(&canonical_root) {
        return Err(ToolError::Failed(format!(
            "{input} is outside the project root; write tools stay inside the project"
        )));
    }
    Ok(resolved)
}

/// Format a resolved path for model-facing output: relative to the
/// root when inside it, absolute otherwise. Canonical, so one file has
/// one display form. The canonical root also strips, so write paths
/// resolved through a symlinked root still display relative.
pub(crate) fn display_path(root: &Path, resolved: &Path) -> String {
    if let Ok(stripped) = resolved.strip_prefix(root) {
        return stripped.display().to_string();
    }
    if let Ok(canonical_root) = root.canonicalize()
        && let Ok(stripped) = resolved.strip_prefix(&canonical_root)
    {
        return stripped.display().to_string();
    }
    resolved.display().to_string()
}

/// Walk the files under `base`, shared by grep and glob: hidden files
/// included, `.git` skipped, `.gitignore` respected even outside a git
/// repository (deterministic across machines; the user-global gitignore
/// is ignored for the same reason). Files over [`MAX_FILE_BYTES`] and
/// unreadable entries are skipped.
pub(crate) fn walk_files(base: &Path) -> impl Iterator<Item = PathBuf> {
    ignore::WalkBuilder::new(base)
        .hidden(false)
        .require_git(false)
        .git_global(false)
        .max_filesize(Some(MAX_FILE_BYTES))
        .filter_entry(|entry| entry.file_name() != OsStr::new(".git"))
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(ignore::DirEntry::into_path)
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    /// Poll a ready future once. The filesystem tools finish
    /// synchronously.
    pub(crate) fn run_now<T>(future: impl Future<Output = T>) -> T {
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        match std::pin::pin!(future).poll(&mut cx) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("tool future must resolve immediately"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::run_now;
    use super::*;
    use serde_json::json;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "hello\nworld\n").unwrap();
        dir
    }

    #[test]
    fn definitions_keep_registration_order_with_access_classes() {
        let dir = fixture();
        let registry = Registry::full(dir.path());
        let names: Vec<String> = registry
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect();
        assert_eq!(names, ["read", "edit", "create", "bash", "grep", "glob"]);
        for definition in registry.definitions() {
            assert_eq!(definition.input_schema["type"], "object");
            assert!(!definition.description.is_empty());
        }
        assert_eq!(registry.access("read"), Some(ToolAccess::ReadOnly));
        assert_eq!(registry.access("grep"), Some(ToolAccess::ReadOnly));
        assert_eq!(registry.access("glob"), Some(ToolAccess::ReadOnly));
        assert_eq!(registry.access("edit"), Some(ToolAccess::Write));
        assert_eq!(registry.access("create"), Some(ToolAccess::Write));
        assert_eq!(registry.access("bash"), Some(ToolAccess::Execute));
        assert_eq!(registry.access("write"), None);
    }

    #[test]
    fn retain_shapes_a_read_only_set() {
        let dir = fixture();
        let mut registry = Registry::full(dir.path());
        registry.retain(|tool| tool.access() == ToolAccess::ReadOnly);
        let names: Vec<String> = registry
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect();
        assert_eq!(names, ["read", "grep", "glob"]);
    }

    #[test]
    fn run_approved_runs_a_tool_end_to_end() {
        let dir = fixture();
        let registry = Registry::full(dir.path());
        let output = run_now(registry.run_approved("read", json!({"path": "hello.txt"})));
        assert!(!output.is_error);
        assert!(output.content.contains("1:hello"));
        assert!(output.summary.is_some());
    }

    #[test]
    fn run_approved_reports_unknown_tools_with_the_available_set() {
        let dir = fixture();
        let registry = Registry::full(dir.path());
        let output = run_now(registry.run_approved("write", json!({})));
        assert!(output.is_error);
        assert!(output.content.contains("unknown tool: write"));
        assert!(
            output
                .content
                .contains("read, edit, create, bash, grep, glob")
        );
    }

    #[test]
    fn run_approved_turns_tool_failures_into_error_results() {
        let dir = fixture();
        let registry = Registry::full(dir.path());
        let output = run_now(registry.run_approved("read", json!({"path": "missing.txt"})));
        assert!(output.is_error);
        assert!(output.content.contains("cannot read"));
        assert!(output.content.contains("missing.txt"));
    }

    #[test]
    fn run_approved_rejects_inputs_that_miss_the_schema() {
        let dir = fixture();
        let registry = Registry::full(dir.path());
        let output = run_now(registry.run_approved("read", json!({"file": "hello.txt"})));
        assert!(output.is_error);
        assert!(output.content.contains("invalid input"));
    }

    #[test]
    fn read_text_refuses_oversized_and_binary_files() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("data.bin");
        std::fs::write(&binary, b"text\x00binary").unwrap();
        let error = read_text(&binary).unwrap_err();
        assert!(error.to_string().contains("binary file"));

        let dir_error = read_text(dir.path()).unwrap_err();
        assert!(dir_error.to_string().contains("is a directory"));
    }

    #[test]
    fn resolve_write_confines_writes_to_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(dir.path().join("outside.txt"), "x\n").unwrap();

        assert!(resolve_write(&root, "src/lib.rs").is_ok());
        let escape = resolve_write(&root, "../outside.txt").unwrap_err();
        assert!(escape.to_string().contains("outside the project root"));
        let absolute =
            resolve_write(&root, dir.path().join("outside.txt").to_str().unwrap()).unwrap_err();
        assert!(absolute.to_string().contains("outside the project root"));
        let deep = resolve_write(&root, "new/dir/../../../outside.txt").unwrap_err();
        assert!(deep.to_string().contains("outside the project root"));

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path().join("outside.txt"), root.join("link.txt"))
                .unwrap();
            let through_link = resolve_write(&root, "link.txt").unwrap_err();
            assert!(
                through_link
                    .to_string()
                    .contains("outside the project root")
            );
        }
    }

    #[test]
    fn resolve_write_accepts_new_files_under_a_relative_root() {
        // The cwd is an existing directory, so "." exercises the
        // relative-root ancestor walk without touching the filesystem.
        assert!(resolve_write(Path::new("."), "dmz-test-brand-new-file.txt").is_ok());
        let escape = resolve_write(Path::new("."), "../dmz-escape.txt").unwrap_err();
        assert!(escape.to_string().contains("outside the project root"));
        let empty = resolve_write(Path::new("."), "  ").unwrap_err();
        assert!(matches!(empty, ToolError::InvalidInput(_)));
    }
}
