//! Tool-facing domain types shared by the loop, the registry, and the
//! provider adapters.

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::approval::{ApprovalDecision, ApprovalPolicy};

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

/// The tool surface the loop drives: definitions for the request,
/// policy decisions before dispatch, and approved execution. The
/// registry in `demerzel-tools` implements it; MCP tool sets (M3) plug
/// in through the same trait.
pub trait ToolHost: Send + Sync {
    /// Tool definitions for the model request, in a stable order.
    fn definitions(&self) -> Vec<ToolDefinition>;

    /// The approval decision for one call, before dispatch.
    fn decide(&self, policy: &ApprovalPolicy, name: &str, input: &Value) -> ApprovalDecision;

    /// Run one approved call. Unknown names and tool failures come back
    /// as error results for the model; this method never fails the
    /// harness. Dropping the returned future cancels the call.
    fn run_approved(&self, name: &str, input: Value) -> BoxFuture<'_, ToolOutput>;
}
