//! Tool-facing domain types shared by the loop, the registry, and the
//! provider adapters.

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::approval::ToolAccess;

/// A tool the model may call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool input.
    pub input_schema: Value,
}

/// Boxed future, used to keep traits object-safe.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The structured result of one tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    /// Model-facing result text.
    pub content: String,
    pub is_error: bool,
    /// True when the output hit a size limit. The content also carries
    /// an explicit `[truncated …]` marker.
    pub truncated: bool,
    /// One short human-facing line for the status display and the
    /// session record, e.g. `edit src/lib.rs: 2 ops`. Never parsed
    /// from `content`.
    pub summary: Option<String>,
}

impl ToolOutput {
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            truncated: false,
            summary: None,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            content: message.into(),
            is_error: true,
            truncated: false,
            summary: None,
        }
    }

    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = Some(summary.into());
        self
    }
}

/// The tool surface the loop drives: definitions for the request, the
/// access class the policy keys on, and approved execution. The engine
/// applies the approval policy itself, so a composite host cannot
/// bypass it. The registry in `demerzel-tools` implements this trait;
/// MCP tool sets (M3) plug in through it.
pub trait ToolHost: Send + Sync {
    /// Tool definitions for the model request, in a stable order.
    fn definitions(&self) -> Vec<ToolDefinition>;

    /// The declared access class of the named tool. `None` means the
    /// host does not hold the tool; the engine then skips the policy
    /// and lets [`ToolHost::run_approved`] produce the unknown-tool
    /// error for the model.
    fn access(&self, name: &str) -> Option<ToolAccess>;

    /// Run one approved call. Unknown names and tool failures come back
    /// as error results for the model; this method never fails the
    /// harness. Dropping the returned future cancels the call.
    fn run_approved(&self, name: &str, input: Value) -> BoxFuture<'_, ToolOutput>;
}
