//! The full-screen terminal interface for demerzel.
//!
//! This crate owns the screen: the alternate-screen session, the
//! transcript pane, the input editor, and the keys. It renders the
//! engine's display events; it never talks to a provider and the
//! engine does not change for it. The binary crate supplies a
//! [`Driver`] that turns a submitted line into work:
//!
//! - The driver receives an [`EventSender`] to use as the engine's
//!   event sink, and an [`ApprovalChannel`] to ask questions through.
//!   The driver renders each dispatching call's preview and sends it
//!   with [`EventSender::tool_preview`]; the interface shows it on
//!   the call's card. A resumed session's past turns arrive the
//!   same way, as a [`Replay`] through [`EventSender::replay`].
//! - The interface loop draws what arrives and answers approval
//!   questions at the keyboard.
//! - Dropping the returned turn future aborts the turn; the loop does
//!   this on Ctrl-C, the contract the engine documents.

mod app;
#[doc(hidden)]
pub mod bench;
mod cards;
mod editor;
mod events;
mod first_run;
mod highlight;
mod history;
mod markdown;
mod render;
mod replay;
mod select;
mod terminal;
pub mod theme;
mod thinking;
mod transcript;
mod wrap;

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

use demerzel_core::ApprovalMode;

pub use cards::{OUTPUT_TAIL_BYTES, write_duration};
pub use editor::CommandSpec;
pub use events::{ApprovalChannel, EventSender, StatusPatch};
pub use first_run::FirstRun;
pub use replay::{Replay, ReplayEntry, ReplayResult};
pub use theme::Theme;
pub use thinking::Thinking;

/// The input prompt, shared with the REPL: a heavy angle bracket and
/// a space. The bench probe waits for these bytes on the output
/// stream, so the spelling is load-bearing.
pub const PROMPT: &str = "❯ ";

/// The hint the status shows after one Ctrl-C on an empty input box.
/// The REPL prints the same words on stderr.
pub const QUIT_HINT: &str = "press Ctrl-C again to quit";

/// How long the quit hint stays. A second Ctrl-C inside this span
/// ends the session; a later one asks again. Shared with the REPL.
pub const QUIT_SPAN: Duration = Duration::from_secs(3);

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
    /// Display events the driver sends through `events` before this
    /// call returns — a status patch for a model or effort switch,
    /// the replay of a resumed session — land above the line's echo,
    /// and show on the same frame as the note the call returns. The
    /// call runs on the interface loop, so nothing draws and no key
    /// lands until it returns: a file read is fine, a network round
    /// trip is not.
    fn submit(
        &mut self,
        line: String,
        events: EventSender,
        approvals: ApprovalChannel,
    ) -> Submission<'_>;
}

/// Interface settings for one session. Start from [`Options::default`]
/// and set fields; the struct grows with the interface.
#[derive(Debug)]
#[non_exhaustive]
pub struct Options {
    /// The input-history file, shared with the REPL. `None` keeps
    /// history in memory only.
    pub history_path: Option<PathBuf>,
    /// A dim line shown before the first prompt.
    pub banner: Option<String>,
    /// The session model, shown on the status. The driver updates
    /// it through [`EventSender::status`].
    pub model: String,
    /// The approval mode, shown on the status. The driver updates it
    /// through [`EventSender::status`]; Shift-Tab submits
    /// `/approval <next>` to the driver, the next mode in
    /// [`ApprovalMode::ALL`].
    pub mode: ApprovalMode,
    /// The reasoning effort's name, shown on the status. The driver
    /// updates it through [`EventSender::status`].
    pub effort: String,
    /// The project directory's name, shown on the status. Empty
    /// shows no segment. The interface cuts it at 64 bytes and
    /// drops control characters.
    pub project: String,
    /// The git branch, shown on the status. Empty shows no segment.
    /// The driver reads it and updates it through
    /// [`EventSender::status`] at each turn's start. The interface
    /// cuts it at 64 bytes and drops control characters.
    pub branch: String,
    /// The model's context window, in tokens, for the share on the
    /// context segment. `None` shows the count alone. The driver
    /// updates it through [`EventSender::status`] on a model switch.
    pub context_window: Option<u64>,
    /// Capture the mouse: the wheel scrolls the transcript, and a
    /// drag selects text that the release copies to the clipboard
    /// through the terminal. On by default; off gives the terminal
    /// its own text selection back. The binary sets it from the
    /// `tui.mouse` config key.
    pub mouse: bool,
    /// Render model replies as markdown. On by default; off shows the
    /// raw text, for copying.
    pub markdown: bool,
    /// How a thinking block starts: closed to one line, open, or not
    /// shown. Closed by default; Ctrl-T opens and closes the newest.
    pub thinking: Thinking,
    /// The colors. The terminal's own palette by default; the binary
    /// sets it from the `tui.theme` config key. The interface fits it
    /// to the color depth the terminal declares, and to `NO_COLOR`.
    pub theme: Theme,
    /// The commands the completion popup lists when a line starts
    /// with `/`. The driver still dispatches the submitted line; the
    /// list only completes it.
    pub commands: Vec<CommandSpec>,
    /// The first-run screen, shown before the prompt when set: the
    /// driver fills it when no credential resolves for the session's
    /// provider. Enter continues to the session.
    pub first_run: Option<FirstRun>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            history_path: None,
            banner: None,
            model: String::new(),
            mode: ApprovalMode::Ask,
            effort: String::new(),
            project: String::new(),
            branch: String::new(),
            context_window: None,
            mouse: true,
            markdown: true,
            thinking: Thinking::default(),
            theme: Theme::default(),
            commands: Vec::new(),
            first_run: None,
        }
    }
}

/// Run the interface until the user ends the session.
///
/// The caller prints the ready marker before this call; the terminal
/// switches to the alternate screen inside it and reverts on return,
/// on error, and on panic.
pub async fn run(driver: &mut dyn Driver, options: Options) -> std::io::Result<()> {
    app::run(driver, options).await
}
