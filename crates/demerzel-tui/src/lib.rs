//! The full-screen terminal interface for demerzel.
//!
//! This crate owns the screen: the alternate-screen session, the
//! transcript pane, the input line, and the keys. It renders the
//! engine's display events; it never talks to a provider and the
//! engine does not change for it. The binary crate supplies a
//! [`Driver`] that turns a submitted line into work:
//!
//! - The driver receives an [`EventSender`] to use as the engine's
//!   event sink, and an [`ApprovalChannel`] to ask questions through.
//! - The interface loop draws what arrives and answers approval
//!   questions at the keyboard.
//! - Dropping the returned turn future aborts the turn; the loop does
//!   this on Ctrl-C, the contract the engine documents.

mod app;
#[doc(hidden)]
pub mod bench;
mod events;
mod history;
mod input;
mod render;
mod terminal;
mod transcript;
mod wrap;

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

pub use events::{ApprovalChannel, EventSender};

/// The input prompt, shared with the REPL. The bench probe waits for
/// these bytes on the output stream, so the spelling is load-bearing.
pub const PROMPT: &str = "dmz> ";

/// One user turn as the driver runs it. Not `Send`: the interface
/// drives it on its own single-threaded loop.
pub type TurnFuture<'a> = Pin<Box<dyn Future<Output = ()> + 'a>>;

/// What one submitted line becomes.
#[non_exhaustive]
pub enum Submission<'a> {
    /// End the session.
    Exit,
    /// A message instead of a turn: unknown commands and such.
    Note(String),
    /// A failure message instead of a turn: a command that could not
    /// apply.
    Error(String),
    /// One user turn. The future reports through the sender it was
    /// given and returns when the turn is over.
    Run(TurnFuture<'a>),
}

/// The binary crate's side of the interface: commands and turns. The
/// interface loop owns everything visual; the driver owns what a
/// question shows, through the preview it passes to
/// [`ApprovalChannel::ask`].
pub trait Driver {
    /// Handle one submitted line. The line is trimmed and non-empty.
    fn submit(
        &mut self,
        line: String,
        events: EventSender,
        approvals: ApprovalChannel,
    ) -> Submission<'_>;
}

/// Interface settings for one session.
#[derive(Debug, Default)]
pub struct Options {
    /// The input-history file, shared with the REPL. `None` keeps
    /// history in memory only.
    pub history_path: Option<PathBuf>,
    /// A dim line shown before the first prompt.
    pub banner: Option<String>,
    /// The session model, shown on the status bar. The driver updates
    /// it through [`EventSender::model`].
    pub model: String,
    /// The approval mode's display name, shown on the status bar.
    pub mode: String,
}

/// Run the interface until the user ends the session.
///
/// The caller prints the ready marker before this call; the terminal
/// switches to the alternate screen inside it and reverts on return,
/// on error, and on panic.
pub async fn run(driver: &mut dyn Driver, options: Options) -> std::io::Result<()> {
    app::run(driver, options).await
}
