//! Channel plumbing between a running turn and the interface loop.
//!
//! One task owns the terminal; the turn future runs inside the same
//! task but cannot touch interface state directly. Display events and
//! approval questions travel over channels instead, the same
//! one-owner rule the plain REPL applies to stdin.

use std::time::Instant;

use demerzel_core::{ApprovalAnswer, ApprovalMode, EngineEvent, EventSink, Usage};
use tokio::sync::{mpsc, oneshot};

use crate::cards::kept_output;

/// One owned display event, decoupled from the engine's borrowed
/// [`EngineEvent`].
#[derive(Debug)]
pub(crate) enum DisplayEvent {
    TextDelta(String),
    ReasoningDelta(String),
    /// The reasoning segment ended at `at`.
    ReasoningEnd {
        at: Instant,
    },
    ToolStart {
        id: String,
        name: String,
    },
    /// The call dispatches; its card's duration counts from `at`.
    ToolRun {
        id: String,
        name: String,
        at: Instant,
    },
    /// The driver's preview of a dispatching call, for its card.
    ToolPreview {
        id: String,
        text: String,
    },
    /// The call returned. `output` is the kept tail of the tool's
    /// output, sanitized; `output_cut` says the head fell over the
    /// cap.
    ToolEnd {
        id: String,
        name: String,
        summary: Option<String>,
        is_error: bool,
        output: String,
        output_cut: bool,
        at: Instant,
    },
    Usage(Usage),
    Notice(String),
    Error(String),
    /// A status patch: the segments it names change.
    Status(StatusPatch),
    /// A session opened or closed; the status totals restart.
    SessionChanged,
}

/// A change to the status. Each field that is `Some` replaces its
/// segment; `None` leaves it. Start from [`StatusPatch::default`]
/// and set the fields to change; the struct grows with the status.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct StatusPatch {
    /// The session model's name.
    pub model: Option<String>,
    /// The approval mode.
    pub mode: Option<ApprovalMode>,
    /// The reasoning effort's name; empty shows no segment.
    pub effort: Option<String>,
    /// The git branch, or the short hash of a detached head; empty
    /// shows no segment.
    pub branch: Option<String>,
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

    /// Change the status: the fields of `patch` that are `Some`
    /// replace their segments.
    pub fn status(&self, patch: StatusPatch) {
        let _ = self.tx.send(DisplayEvent::Status(patch));
    }

    /// A session opened or closed: the status totals restart.
    pub fn session_changed(&self) {
        let _ = self.tx.send(DisplayEvent::SessionChanged);
    }

    /// The preview of a dispatching call — the diff, the command —
    /// for the card of call `id`. Send it at the call's `ToolRun`
    /// event; the interface attaches it to the card by id. The first
    /// line joins the card's header, without a leading tool name;
    /// the rest is the body, kept up to 8 KiB.
    pub fn tool_preview(&self, id: &str, text: impl Into<String>) {
        let _ = self.tx.send(DisplayEvent::ToolPreview {
            id: id.to_string(),
            text: text.into(),
        });
    }
}

impl EventSink for EventSender {
    fn event(&self, event: EngineEvent<'_>) {
        let owned = match event {
            EngineEvent::TextDelta(delta) => DisplayEvent::TextDelta(delta.to_string()),
            EngineEvent::ReasoningDelta(delta) => DisplayEvent::ReasoningDelta(delta.to_string()),
            EngineEvent::ReasoningEnd { .. } => DisplayEvent::ReasoningEnd { at: Instant::now() },
            EngineEvent::ToolStart { id, name, .. } => DisplayEvent::ToolStart {
                id: id.to_string(),
                name: name.to_string(),
            },
            // The input stays with the driver: it renders the dispatch
            // preview, so this crate needs no knowledge of tools. The
            // event itself marks the card's start time.
            EngineEvent::ToolRun { id, name, .. } => DisplayEvent::ToolRun {
                id: id.to_string(),
                name: name.to_string(),
                at: Instant::now(),
            },
            EngineEvent::ToolEnd {
                id,
                name,
                summary,
                is_error,
                output,
                ..
            } => {
                // The interface keeps a sanitized tail: the copy is
                // bounded here, before the channel.
                let (output, output_cut) = kept_output(output);
                DisplayEvent::ToolEnd {
                    id: id.to_string(),
                    name: name.to_string(),
                    summary: summary.map(str::to_string),
                    is_error,
                    output,
                    output_cut,
                    at: Instant::now(),
                }
            }
            EngineEvent::ModelUsage(usage) => DisplayEvent::Usage(usage),
            _ => return,
        };
        let _ = self.tx.send(owned);
    }
}

/// One approval question on its way to the screen.
pub(crate) struct ApprovalRequest {
    pub tool: String,
    /// What the question shows above itself. `None` when the driver
    /// already rendered the call into the transcript.
    pub preview: Option<String>,
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

    /// Show the question and wait for the answer. `preview` is what
    /// the question shows above itself; pass `None` only when the
    /// call is already on the screen. A dead interface answers abort.
    pub async fn ask(&mut self, tool: &str, preview: Option<String>) -> ApprovalAnswer {
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
