//! The interface loop: one task multiplexes terminal input, display
//! events from the running turn, and approval questions.
//!
//! Ctrl-C rules carry over from the REPL: at the prompt it clears the
//! line, during a turn it aborts the turn by dropping its future, and
//! at an approval question it stops the turn. Ctrl-D on an empty line
//! ends the session.

use std::io;

use demerzel_core::ApprovalAnswer;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::signal::unix::{Signal, SignalKind};
use tokio::sync::{mpsc, oneshot};

use crate::events::{ApprovalRequest, ChannelApprover, DisplayEvent, EventSender};
use crate::history::History;
use crate::input::Input;
use crate::render::{self, View};
use crate::terminal::Guard;
use crate::transcript::{Kind, Transcript};
use crate::{Options, Submission, TurnFuture};

/// What a handled key asks the loop to do.
enum Action {
    None,
    Submit(String),
    Exit,
    AbortTurn,
}

/// Whether a turn is running when a key arrives.
#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Idle,
    Turn,
}

/// The input row is either the editor or an approval question.
enum Mode {
    Edit,
    Approval {
        tool: String,
        reply: oneshot::Sender<ApprovalAnswer>,
    },
}

/// Run the interface until the user ends the session. The caller
/// prints the ready marker first; this function owns the terminal
/// from entry to return.
pub async fn run(driver: &mut dyn crate::Driver, options: Options) -> io::Result<()> {
    let mut app = App::new(options)?;
    app.draw()?;
    loop {
        let Some(line) = app.next_submission().await? else {
            return Ok(());
        };
        app.history.record(&line);
        match driver.submit(line, app.sender(), app.approver()) {
            Submission::Exit => return Ok(()),
            Submission::Note(text) => {
                app.transcript.push_line(Kind::Notice, &text);
                app.draw()?;
            }
            Submission::Run(turn) => app.run_turn(turn).await?,
        }
    }
}

struct App {
    guard: Guard,
    transcript: Transcript,
    input: Input,
    history: History,
    mode: Mode,
    keys: mpsc::UnboundedReceiver<Event>,
    displays: mpsc::UnboundedReceiver<DisplayEvent>,
    display_tx: mpsc::UnboundedSender<DisplayEvent>,
    approvals: mpsc::UnboundedReceiver<ApprovalRequest>,
    approval_tx: mpsc::UnboundedSender<ApprovalRequest>,
    sigint: Signal,
    /// Transcript viewport height from the last frame: the page unit.
    page: u16,
}

impl App {
    fn new(options: Options) -> io::Result<Self> {
        // The signal listener installs before the terminal goes raw,
        // so an external SIGINT can never kill a raw terminal.
        let sigint = tokio::signal::unix::signal(SignalKind::interrupt())?;
        let guard = Guard::enter()?;
        let keys = spawn_input_thread();
        let (display_tx, displays) = mpsc::unbounded_channel();
        let (approval_tx, approvals) = mpsc::unbounded_channel();
        let mut transcript = Transcript::new();
        if let Some(banner) = &options.banner {
            transcript.push_line(Kind::Notice, banner);
        }
        Ok(Self {
            guard,
            transcript,
            input: Input::new(),
            history: History::load(options.history_path),
            mode: Mode::Edit,
            keys,
            displays,
            display_tx,
            approvals,
            approval_tx,
            sigint,
            page: 0,
        })
    }

    fn sender(&self) -> EventSender {
        EventSender::new(self.display_tx.clone())
    }

    fn approver(&self) -> ChannelApprover {
        ChannelApprover::new(self.approval_tx.clone())
    }

    /// Wait at the prompt. `None` means the session ends.
    async fn next_submission(&mut self) -> io::Result<Option<String>> {
        loop {
            let action = tokio::select! {
                event = self.keys.recv() => match event {
                    Some(event) => self.terminal_event(event, Phase::Idle),
                    // The input thread died; without keys the session
                    // cannot continue.
                    None => Action::Exit,
                },
                _ = self.sigint.recv() => self.ctrl_c(Phase::Idle),
                event = self.displays.recv() => {
                    if let Some(event) = event {
                        self.display(event);
                    }
                    Action::None
                }
            };
            self.drain_displays();
            self.draw()?;
            match action {
                Action::None | Action::AbortTurn => {}
                Action::Exit => return Ok(None),
                Action::Submit(line) => return Ok(Some(line)),
            }
        }
    }

    /// Drive one turn to its end while the interface stays live.
    /// Dropping the future is the abort path, the same contract the
    /// REPL relies on.
    async fn run_turn(&mut self, mut turn: TurnFuture<'_>) -> io::Result<()> {
        let mut aborted = false;
        loop {
            enum Wake {
                Done,
                Key(Option<Event>),
                Signal,
                Display(Option<DisplayEvent>),
                Approval(Option<ApprovalRequest>),
            }
            let wake = tokio::select! {
                _ = &mut turn => Wake::Done,
                event = self.keys.recv() => Wake::Key(event),
                _ = self.sigint.recv() => Wake::Signal,
                event = self.displays.recv() => Wake::Display(event),
                request = self.approvals.recv() => Wake::Approval(request),
            };
            let action = match wake {
                Wake::Done => break,
                Wake::Key(Some(event)) => self.terminal_event(event, Phase::Turn),
                Wake::Key(None) | Wake::Signal => self.ctrl_c(Phase::Turn),
                Wake::Display(event) => {
                    if let Some(event) = event {
                        self.display(event);
                    }
                    Action::None
                }
                Wake::Approval(request) => {
                    if let Some(request) = request {
                        self.show_approval(request);
                    }
                    Action::None
                }
            };
            if matches!(action, Action::AbortTurn) {
                aborted = true;
                break;
            }
            self.drain_displays();
            self.draw()?;
        }
        drop(turn);
        // A question the dropped turn never collected dies with it.
        self.mode = Mode::Edit;
        self.transcript.close();
        if aborted {
            self.transcript.push_line(Kind::Notice, "[turn aborted]");
        }
        self.drain_displays();
        self.draw()
    }

    /// Apply every display event that is already queued, so one frame
    /// covers a burst of deltas.
    fn drain_displays(&mut self) {
        while let Ok(event) = self.displays.try_recv() {
            self.display(event);
        }
    }

    fn display(&mut self, event: DisplayEvent) {
        match event {
            DisplayEvent::TextDelta(delta) => self.transcript.push_delta(Kind::Text, &delta),
            DisplayEvent::ReasoningDelta(delta) => {
                self.transcript.push_delta(Kind::Reasoning, &delta)
            }
            DisplayEvent::ToolStart { id, name } => self.transcript.tool_start(&id, &name),
            DisplayEvent::ToolEnd {
                id,
                name,
                summary,
                is_error,
            } => {
                let summary = summary.unwrap_or(name);
                self.transcript.tool_end(&id, &summary, is_error);
            }
            DisplayEvent::Usage(usage) => self.transcript.push_line(
                Kind::Notice,
                &format!(
                    "· {} in · {} out · {} cached",
                    usage.input_tokens, usage.output_tokens, usage.cache_read_tokens
                ),
            ),
            DisplayEvent::Notice(text) => self.transcript.push_line(Kind::Notice, &text),
            DisplayEvent::Error(text) => self.transcript.push_line(Kind::Error, &text),
        }
    }

    fn show_approval(&mut self, request: ApprovalRequest) {
        self.transcript
            .push_line(Kind::Status, &format!("{} wants to run:", request.tool));
        self.transcript.push_line(Kind::Notice, &request.preview);
        self.mode = Mode::Approval {
            tool: request.tool,
            reply: request.reply,
        };
    }

    fn terminal_event(&mut self, event: Event, phase: Phase) -> Action {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.key(key, phase),
            Event::Paste(text) => {
                if matches!(self.mode, Mode::Edit) {
                    self.input.insert_str(&normalize_paste(&text));
                }
                Action::None
            }
            _ => Action::None,
        }
    }

    fn key(&mut self, key: KeyEvent, phase: Phase) -> Action {
        if matches!(self.mode, Mode::Approval { .. }) {
            return self.approval_key(key);
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => return self.ctrl_c(phase),
            KeyCode::Char('d') if ctrl => {
                if !self.input.is_empty() {
                    self.input.delete();
                } else if phase == Phase::Idle {
                    return Action::Exit;
                }
            }
            KeyCode::Char('a') if ctrl => self.input.home(),
            KeyCode::Char('e') if ctrl => self.input.end(),
            KeyCode::Char('u') if ctrl => self.input.kill_to_start(),
            KeyCode::Char('k') if ctrl => self.input.kill_to_end(),
            KeyCode::Char('w') if ctrl => self.input.kill_word_back(),
            KeyCode::Char(ch) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.input.insert_char(ch)
            }
            KeyCode::Enter => {
                if phase == Phase::Idle {
                    let line = self.input.take();
                    let line = line.trim();
                    if !line.is_empty() {
                        return Action::Submit(line.to_string());
                    }
                }
            }
            KeyCode::Backspace => self.input.backspace(),
            KeyCode::Delete => self.input.delete(),
            KeyCode::Left => self.input.left(),
            KeyCode::Right => self.input.right(),
            KeyCode::Home => self.input.home(),
            KeyCode::End => self.input.end(),
            KeyCode::Up => {
                if let Some(text) = self.history.up(self.input.text()) {
                    let text = text.to_string();
                    self.input.set(&text);
                }
            }
            KeyCode::Down => {
                if let Some(text) = self.history.down() {
                    let text = text.to_string();
                    self.input.set(&text);
                }
            }
            KeyCode::PageUp => {
                self.transcript.scroll += usize::from(self.page.max(1));
            }
            KeyCode::PageDown => {
                self.transcript.scroll = self
                    .transcript
                    .scroll
                    .saturating_sub(usize::from(self.page.max(1)));
            }
            _ => {}
        }
        Action::None
    }

    /// Answer keys while an approval question is up.
    fn approval_key(&mut self, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let answer = match key.code {
            KeyCode::Char('y' | 'Y') if !ctrl => ApprovalAnswer::Allow,
            KeyCode::Char('a' | 'A') if !ctrl => ApprovalAnswer::AllowForSession,
            KeyCode::Char('n' | 'N') if !ctrl => ApprovalAnswer::Deny,
            KeyCode::Enter | KeyCode::Esc => ApprovalAnswer::Deny,
            KeyCode::Char('c' | 'd') if ctrl => ApprovalAnswer::Abort,
            _ => return Action::None,
        };
        let Mode::Approval { tool, reply } = std::mem::replace(&mut self.mode, Mode::Edit) else {
            unreachable!("approval_key runs only in approval mode");
        };
        let word = match answer {
            ApprovalAnswer::Allow => "yes",
            ApprovalAnswer::AllowForSession => "always",
            ApprovalAnswer::Deny => "no",
            ApprovalAnswer::Abort => "abort",
            _ => "no",
        };
        self.transcript
            .push_line(Kind::Notice, &format!("approve {tool}? {word}"));
        let _ = reply.send(answer);
        Action::None
    }

    /// The Ctrl-C rules. An external SIGINT lands here too.
    fn ctrl_c(&mut self, phase: Phase) -> Action {
        match phase {
            Phase::Idle => {
                self.input.take();
                Action::None
            }
            Phase::Turn => Action::AbortTurn,
        }
    }

    fn draw(&mut self) -> io::Result<()> {
        let question = match &self.mode {
            Mode::Edit => None,
            Mode::Approval { tool, .. } => Some(format!("approve {tool}? [y]es / [a]lways / [N]o")),
        };
        let Self {
            guard,
            transcript,
            input,
            page,
            ..
        } = self;
        guard.terminal.draw(|frame| {
            *page = render::draw(
                frame,
                View {
                    transcript,
                    input,
                    approval: question.as_deref(),
                },
            );
        })?;
        Ok(())
    }
}

/// One thread owns terminal input and forwards every event. It parks
/// in the blocking read; it ends with the process.
fn spawn_input_thread() -> mpsc::UnboundedReceiver<Event> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        while let Ok(event) = ratatui::crossterm::event::read() {
            if tx.send(event).is_err() {
                return;
            }
        }
    });
    rx
}

/// Pasted line endings become plain newlines; other control bytes
/// drop before they reach the buffer.
fn normalize_paste(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\n' => out.push('\n'),
            '\t' => out.push('\t'),
            ch if ch.is_control() => {}
            ch => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_normalizes_line_endings_and_drops_controls() {
        assert_eq!(normalize_paste("a\r\nb\rc\nd"), "a\nb\nc\nd");
        assert_eq!(normalize_paste("x\x1b[31my\tz"), "x[31my\tz");
    }
}
