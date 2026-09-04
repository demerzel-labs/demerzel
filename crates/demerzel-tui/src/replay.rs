//! The transcript replay on resume: a session's past turns as blocks,
//! before the `continuing session` notice.
//!
//! The driver reads the session file at the first prompt — the boot
//! reads none — caps what it shows, and hands the interface the
//! entries through [`crate::EventSender::replay`]. The interface
//! knows no messages and no tools: each entry is one block as the
//! live turn showed it — a prompt line, model text, a closed
//! thinking block, a finished card with its preview and its output
//! tail, a usage line — and the driver counts what the cap left out.

use std::time::Duration;

use demerzel_core::Usage;

/// A resumed session's past turns, for the transcript to show as
/// blocks. The driver builds it from the session file and sends it
/// through [`crate::EventSender::replay`] before the notice that
/// names the session.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Replay {
    /// Earlier messages the replay leaves out. A count above zero
    /// shows as a dim line above the first entry.
    pub left_out: usize,
    /// The entries, oldest first.
    pub entries: Vec<ReplayEntry>,
}

/// One block of a replayed session.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ReplayEntry {
    /// A user prompt, shown with the prompt mark.
    Prompt(String),
    /// Model text: one reply segment.
    Text(String),
    /// A reasoning summary: a thinking block, closed unless the
    /// `thinking` setting opens every block.
    Reasoning(String),
    /// A tool call: a finished card. `result` is `None` when the
    /// turn ended before the call returned; the card shows it as
    /// interrupted.
    Tool {
        name: String,
        /// The driver's preview of the call, the text
        /// [`crate::EventSender::tool_preview`] takes for a live
        /// call.
        preview: Option<String>,
        result: Option<ReplayResult>,
    },
    /// One model call's usage line. `elapsed` is the time since the
    /// turn started, as the live line shows it; `None` when the file
    /// kept no clock.
    Usage {
        usage: Usage,
        elapsed: Option<Duration>,
    },
}

/// A replayed tool call's result.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayResult {
    /// The result text the model received. The card keeps its tail.
    pub output: String,
    pub is_error: bool,
    /// The call's wall time, when the file kept it.
    pub duration: Option<Duration>,
}
