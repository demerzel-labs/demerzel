//! Channel plumbing between a running turn and the interface loop.
//!
//! One task owns the terminal; the turn future runs inside the same
//! task but cannot touch interface state directly. Display events and
//! approval questions travel over channels instead, the same
//! one-owner rule the plain REPL applies to stdin.

use demerzel_core::{ApprovalAnswer, EngineEvent, EventSink, Usage};
use tokio::sync::{mpsc, oneshot};

/// One owned display event, decoupled from the engine's borrowed
/// [`EngineEvent`].
#[derive(Debug)]
pub(crate) enum DisplayEvent {
    TextDelta(String),
    ReasoningDelta(String),
    ToolStart {
        id: String,
        name: String,
    },
    ToolEnd {
        id: String,
        name: String,
        summary: Option<String>,
        is_error: bool,
    },
    Usage(Usage),
    Notice(String),
    Error(String),
}

/// Sends display events into the interface loop. The driver hands it
/// to the engine as its [`EventSink`] and uses the helpers for its own
/// messages.
#[derive(Clone)]
pub struct EventSender {
    tx: mpsc::UnboundedSender<DisplayEvent>,
}

impl EventSender {
    pub(crate) fn new(tx: mpsc::UnboundedSender<DisplayEvent>) -> Self {
        Self { tx }
    }

    /// A dim interface message: session notes, turn outcomes.
    pub fn notice(&self, text: impl Into<String>) {
        let _ = self.tx.send(DisplayEvent::Notice(text.into()));
    }

    /// A failure message.
    pub fn error(&self, text: impl Into<String>) {
        let _ = self.tx.send(DisplayEvent::Error(text.into()));
    }
}

impl EventSink for EventSender {
    fn event(&self, event: EngineEvent<'_>) {
        let owned = match event {
            EngineEvent::TextDelta(delta) => DisplayEvent::TextDelta(delta.to_string()),
            EngineEvent::ReasoningDelta(delta) => DisplayEvent::ReasoningDelta(delta.to_string()),
            EngineEvent::ToolStart { id, name } => DisplayEvent::ToolStart {
                id: id.to_string(),
                name: name.to_string(),
            },
            EngineEvent::ToolEnd {
                id,
                name,
                summary,
                is_error,
            } => DisplayEvent::ToolEnd {
                id: id.to_string(),
                name: name.to_string(),
                summary: summary.map(str::to_string),
                is_error,
            },
            EngineEvent::ModelUsage(usage) => DisplayEvent::Usage(usage),
            // Edit previews render from this event in the next step.
            _ => return,
        };
        let _ = self.tx.send(owned);
    }
}

/// One approval question on its way to the screen.
pub(crate) struct ApprovalRequest {
    pub tool: String,
    pub preview: String,
    pub reply: oneshot::Sender<ApprovalAnswer>,
}

/// Asks the user one question through the interface loop. The driver
/// builds the preview — what the question shows is its call — and
/// implements the engine's approver over this channel.
pub struct ApprovalChannel {
    tx: mpsc::UnboundedSender<ApprovalRequest>,
}

impl ApprovalChannel {
    pub(crate) fn new(tx: mpsc::UnboundedSender<ApprovalRequest>) -> Self {
        Self { tx }
    }

    /// Show the question and wait for the answer. A dead interface
    /// answers abort.
    pub async fn ask(&mut self, tool: &str, preview: String) -> ApprovalAnswer {
        let (reply, answer) = oneshot::channel();
        let request = ApprovalRequest {
            tool: tool.to_string(),
            preview,
            reply,
        };
        if self.tx.send(request).is_err() {
            return ApprovalAnswer::Abort;
        }
        answer.await.unwrap_or(ApprovalAnswer::Abort)
    }
}
