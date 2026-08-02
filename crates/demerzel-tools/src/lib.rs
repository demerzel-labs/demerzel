//! Tools the model calls through the harness.
//!
//! [`Tool`] is the contract: name, description, JSON-Schema input, and
//! one async `run`. [`Registry`] produces the [`ToolDefinition`] list
//! for the request and dispatches calls by name. Tool failures return
//! to the model as [`ToolOutput`] with `is_error: true`; they never
//! panic the harness.

mod glob;
mod grep;
mod hashline;
mod read;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use demerzel_providers::{BoxFuture, ToolDefinition};
use serde::de::DeserializeOwned;
use serde_json::Value;

pub use glob::GlobTool;
pub use grep::GrepTool;
pub use hashline::line_tag;
pub use read::ReadTool;

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

/// The structured result of one tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
    /// True when the output hit a size limit. The content also carries
    /// an explicit `[truncated …]` marker.
    pub truncated: bool,
}

impl ToolOutput {
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            truncated: false,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            content: message.into(),
            is_error: true,
            truncated: false,
        }
    }
}

/// The contract every tool implements. `run` returns a boxed future so
/// the trait stays object-safe; bash (step 4) needs real async.
pub trait Tool: Send + Sync {
    /// Stable tool name, e.g. "read". This is the dispatch key.
    fn name(&self) -> &'static str;

    fn description(&self) -> &'static str;

    /// JSON Schema for the tool input.
    fn input_schema(&self) -> Value;

    /// Run one call. Failures come back as `Err`; the registry turns
    /// them into error results for the model.
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

    /// The read-only tool set, with model-supplied relative paths
    /// resolved against `root`.
    pub fn read_only(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let mut registry = Self::new();
        registry.register(Box::new(ReadTool::new(root.clone())));
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

    /// Run the named tool. Unknown names and tool failures become
    /// error results for the model; this method never fails.
    pub fn dispatch(&self, name: &str, input: Value) -> BoxFuture<'_, ToolOutput> {
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

/// Parse a tool input value into its typed form.
pub(crate) fn parse_input<T: DeserializeOwned>(input: Value) -> Result<T, ToolError> {
    serde_json::from_value(input).map_err(|error| ToolError::InvalidInput(error.to_string()))
}

/// Resolve a model-supplied path against the tool root.
pub(crate) fn resolve(root: &Path, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

/// Walk the files under `base`, shared by grep and glob: hidden files
/// included, `.git` skipped, `.gitignore` respected even outside a git
/// repository (deterministic across machines; the user-global gitignore
/// is ignored for the same reason). Unreadable entries are skipped.
pub(crate) fn walk_files(base: &Path) -> impl Iterator<Item = PathBuf> {
    ignore::WalkBuilder::new(base)
        .hidden(false)
        .require_git(false)
        .git_global(false)
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

    /// Poll a ready future once. Read-only tools finish synchronously.
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
    fn definitions_keep_registration_order() {
        let dir = fixture();
        let registry = Registry::read_only(dir.path());
        let names: Vec<String> = registry
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect();
        assert_eq!(names, ["read", "grep", "glob"]);
        for definition in registry.definitions() {
            assert_eq!(definition.input_schema["type"], "object");
            assert!(!definition.description.is_empty());
        }
    }

    #[test]
    fn dispatch_runs_a_tool_end_to_end() {
        let dir = fixture();
        let registry = Registry::read_only(dir.path());
        let output = run_now(registry.dispatch("read", json!({"path": "hello.txt"})));
        assert!(!output.is_error);
        assert!(output.content.contains("|hello"));
    }

    #[test]
    fn dispatch_reports_unknown_tools_with_the_available_set() {
        let dir = fixture();
        let registry = Registry::read_only(dir.path());
        let output = run_now(registry.dispatch("write", json!({})));
        assert!(output.is_error);
        assert!(output.content.contains("unknown tool: write"));
        assert!(output.content.contains("read, grep, glob"));
    }

    #[test]
    fn dispatch_turns_tool_failures_into_error_results() {
        let dir = fixture();
        let registry = Registry::read_only(dir.path());
        let output = run_now(registry.dispatch("read", json!({"path": "missing.txt"})));
        assert!(output.is_error);
        assert!(output.content.contains("cannot read"));
        assert!(output.content.contains("missing.txt"));
    }

    #[test]
    fn dispatch_rejects_inputs_that_miss_the_schema() {
        let dir = fixture();
        let registry = Registry::read_only(dir.path());
        let output = run_now(registry.dispatch("read", json!({"file": "hello.txt"})));
        assert!(output.is_error);
        assert!(output.content.contains("invalid input"));
    }
}
