//! The interface loop: one task multiplexes terminal input, display
//! events from the running turn, and approval questions.
//!
//! Ctrl-C rules carry over from the REPL: at the prompt it clears the
//! line, during a turn it aborts the turn by dropping its future, and
//! at an approval question it stops the turn. Ctrl-D on an empty line
//! ends the session.
//!
//! The transcript follows the bottom until the user scrolls up: the
//! wheel, PageUp, and PageDown browse; End and a submitted line return
//! to the bottom. Ctrl-O expands every tool card to its kept output
//! and collapses them again. Ctrl-T opens the newest thinking block
//! and closes it again.
//!
//! The editor holds several lines: Enter submits, Alt-Enter and
//! Shift-Enter insert a newline, and a paste keeps its newlines. Up
//! and Down move across rows, and at the first or last row they
//! browse the history. A line that starts with `/` opens the
//! completion popup: Tab and the arrows select, Enter completes, Esc
//! closes it.
//!
//! While a turn runs, a progress line above the input box shows a
//! spinner, the elapsed time, and the stage. The status bar is the
//! bottom row, under the input box. A tick timer wakes the
//! loop for it during the turn only; at idle nothing wakes.

use std::future::poll_fn;
use std::io;
use std::time::Duration;

use demerzel_core::ApprovalAnswer;
use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
};
use tokio::signal::unix::{Signal, SignalKind};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use crate::cards::{self, Outcome};
use crate::editor::{Completion, Editor};
use crate::events::{ApprovalChannel, ApprovalRequest, DisplayEvent, EventSender};
use crate::history::History;
use crate::render::{self, Status, View};
use crate::terminal::Guard;
use crate::transcript::{Kind, Transcript};
use crate::{Options, PROMPT, Submission, TurnFuture};

/// Shortest span between two frames. Wakes inside the span mark the
/// interface dirty; the frame timer draws at the deadline. This caps
/// draw cost during fast streams without adding idle wakeups.
const FRAME: Duration = Duration::from_millis(16);

/// Rows one wheel notch scrolls.
const WHEEL_ROWS: usize = 3;

/// Span between two progress-line ticks during a turn: the spinner's
/// frame rate.
const TICK: Duration = Duration::from_millis(100);

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

/// What the running turn is doing, for the progress line.
enum Stage {
    Thinking,
    Writing,
    /// A tool runs, by name.
    Tool(String),
}

/// The progress line's state: the turn's start and its stage.
struct Progress {
    started: Instant,
    stage: Stage,
}

impl Progress {
    fn write(&self, out: &mut String) {
        let stage = match &self.stage {
            Stage::Thinking => "thinking",
            Stage::Writing => "writing",
            Stage::Tool(name) => name.as_str(),
        };
        cards::write_progress(out, self.started.elapsed(), stage);
    }
}

/// The input box is either the editor or an approval question.
enum Mode {
    Edit,
    Approval {
        tool: String,
        /// The question row, built once: the tool name is
        /// model-controlled and the row draws every frame.
        question: String,
        /// The question landed on the running call's card, so the
        /// answer settles that card. Without one — a question asked
        /// outside a dispatch — the answer touches no card.
        card: bool,
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
        app.transcript
            .push_line(Kind::Prompt, &format!("{PROMPT}{line}"));
        app.dirty = true;
        match driver.submit(line, app.sender(), app.approver()) {
            Submission::Exit => return Ok(()),
            Submission::Note(text) => {
                app.transcript.push_line(Kind::Notice, &text);
                app.dirty = true;
                app.draw()?;
            }
            Submission::Error(text) => {
                app.transcript.push_line(Kind::Error, &text);
                app.dirty = true;
                app.draw()?;
            }
            Submission::Run(turn) => app.run_turn(turn).await?,
        }
    }
}

struct App {
    guard: Guard,
    transcript: Transcript,
    editor: Editor,
    history: History,
    status: Status,
    mode: Mode,
    /// Set while a turn runs.
    progress: Option<Progress>,
    /// The progress line's text, rebuilt each frame in place.
    progress_line: String,
    /// When the last event other than reasoning text landed; see
    /// [`thinking_clock`]. A thinking block's clock starts here, not
    /// at its first delta: a provider can stream the summary after
    /// the reasoning, so the first delta can come at the end of the
    /// thought.
    reasoning_since: std::time::Instant,
    keys: mpsc::UnboundedReceiver<Event>,
    displays: mpsc::UnboundedReceiver<DisplayEvent>,
    display_tx: mpsc::UnboundedSender<DisplayEvent>,
    approvals: mpsc::UnboundedReceiver<ApprovalRequest>,
    approval_tx: mpsc::UnboundedSender<ApprovalRequest>,
    sigint: Signal,
    /// Transcript viewport height from the last frame: the page unit.
    page: u16,
    /// State changed since the last frame.
    dirty: bool,
    last_draw: Instant,
}

impl App {
    fn new(options: Options) -> io::Result<Self> {
        // The signal listener installs before the terminal goes raw,
        // so an external SIGINT can never kill a raw terminal.
        let sigint = tokio::signal::unix::signal(SignalKind::interrupt())?;
        let guard = Guard::enter(options.mouse)?;
        let keys = spawn_input_thread();
        let (display_tx, displays) = mpsc::unbounded_channel();
        let (approval_tx, approvals) = mpsc::unbounded_channel();
        let mut transcript = Transcript::new(options.markdown);
        transcript.set_thinking(options.thinking);
        if let Some(banner) = &options.banner {
            transcript.push_line(Kind::Notice, banner);
        }
        Ok(Self {
            guard,
            transcript,
            editor: Editor::new(options.commands),
            history: History::load(options.history_path),
            status: Status {
                model: options.model,
                mode: options.mode,
                session: None,
                input_tokens: 0,
                output_tokens: 0,
                cached_tokens: 0,
            },
            mode: Mode::Edit,
            progress: None,
            progress_line: String::new(),
            reasoning_since: std::time::Instant::now(),
            keys,
            displays,
            display_tx,
            approvals,
            approval_tx,
            sigint,
            page: 0,
            dirty: true,
            // Seeded in the past, so the first draw runs at once.
            last_draw: Instant::now() - FRAME,
        })
    }

    fn sender(&self) -> EventSender {
        EventSender::new(self.display_tx.clone())
    }

    fn approver(&self) -> ApprovalChannel {
        ApprovalChannel::new(self.approval_tx.clone())
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
                _ = self.sigint.recv() => {
                    self.dirty = true;
                    self.ctrl_c(Phase::Idle)
                }
                event = self.displays.recv() => match event {
                    Some(event) => {
                        self.display(event);
                        self.dirty = true;
                        Action::None
                    }
                    // The loop holds a sender, so this cannot happen;
                    // exit instead of spinning if a refactor breaks
                    // that.
                    None => Action::Exit,
                },
                _ = tokio::time::sleep_until(self.last_draw + FRAME), if self.dirty => {
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
        // The signal listener is long-lived, so a signal from between
        // turns may still be queued; drop it instead of aborting this
        // turn with it.
        drain_signal(&mut self.sigint).await;
        self.progress = Some(Progress {
            started: Instant::now(),
            stage: Stage::Thinking,
        });
        self.reasoning_since = std::time::Instant::now();
        self.dirty = true;
        let mut tick = Instant::now() + TICK;
        let mut aborted = false;
        loop {
            enum Wake {
                Done,
                Key(Option<Event>),
                Signal,
                Display(Option<DisplayEvent>),
                Approval(Option<ApprovalRequest>),
                Frame,
                Tick,
            }
            let wake = tokio::select! {
                _ = &mut turn => Wake::Done,
                event = self.keys.recv() => Wake::Key(event),
                _ = self.sigint.recv() => Wake::Signal,
                event = self.displays.recv() => Wake::Display(event),
                request = self.approvals.recv() => Wake::Approval(request),
                _ = tokio::time::sleep_until(self.last_draw + FRAME), if self.dirty => Wake::Frame,
                // The spinner rests while a question waits: nothing
                // runs, and the frame would only redraw the question.
                _ = tokio::time::sleep_until(tick), if matches!(self.mode, Mode::Edit) => Wake::Tick,
            };
            let action = match wake {
                Wake::Done => break,
                Wake::Frame => Action::None,
                Wake::Tick => {
                    // From now, not from the last tick: a loop that
                    // fell behind must not fire a burst to catch up.
                    tick = Instant::now() + TICK;
                    self.dirty = true;
                    Action::None
                }
                Wake::Key(Some(event)) => self.terminal_event(event, Phase::Turn),
                Wake::Key(None) | Wake::Signal => self.ctrl_c(Phase::Turn),
                // The loop holds senders for both channels, so `None`
                // cannot happen; abort instead of spinning if a
                // refactor breaks that.
                Wake::Display(None) | Wake::Approval(None) => Action::AbortTurn,
                Wake::Display(Some(event)) => {
                    self.display(event);
                    self.dirty = true;
                    Action::None
                }
                Wake::Approval(Some(request)) => {
                    self.show_approval(request);
                    self.dirty = true;
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
        // Questions the dropped turn never collected die with it: an
        // abort can leave one queued, and it must not haunt the next
        // turn as a dead prompt.
        while self.approvals.try_recv().is_ok() {}
        self.mode = Mode::Edit;
        self.progress = None;
        // Apply what the turn streamed before the notice, so an
        // aborted line does not split around it.
        self.drain_displays();
        self.transcript.close();
        if aborted {
            self.transcript.push_line(Kind::Notice, "[turn aborted]");
        }
        self.dirty = true;
        self.draw()
    }

    /// Apply every display event that is already queued, so one frame
    /// covers a burst of deltas.
    fn drain_displays(&mut self) {
        while let Ok(event) = self.displays.try_recv() {
            self.display(event);
            self.dirty = true;
        }
    }

    fn display(&mut self, event: DisplayEvent) {
        if let Some(since) = thinking_clock(&event, std::time::Instant::now()) {
            self.reasoning_since = since;
        }
        match event {
            DisplayEvent::TextDelta(delta) => {
                self.transcript.push_delta(Kind::Text, &delta);
                self.stage(Stage::Writing);
            }
            DisplayEvent::ReasoningDelta(text) => {
                self.transcript.reasoning_delta(&text, self.reasoning_since);
                self.stage(Stage::Thinking);
            }
            DisplayEvent::ReasoningEnd { at } => self.transcript.reasoning_end(at),
            DisplayEvent::ToolStart { id, name } => {
                self.transcript.tool_start(&id, &name);
                self.stage(Stage::Writing);
            }
            DisplayEvent::ToolRun { id, name, at } => {
                self.transcript.tool_run(&id, at);
                // The name is model-controlled: one clean line for
                // the progress row.
                self.stage(Stage::Tool(cards::one_line(
                    &name,
                    cards::PROGRESS_NAME_BYTES,
                )));
            }
            DisplayEvent::ToolPreview { id, text } => self.transcript.tool_preview(&id, &text),
            DisplayEvent::ToolEnd {
                id,
                name,
                summary,
                is_error,
                output,
                output_cut,
                at,
            } => {
                self.transcript.tool_end(
                    &id,
                    &name,
                    Outcome {
                        summary: summary.as_deref(),
                        is_error,
                        output,
                        output_cut,
                        at,
                    },
                );
                self.stage(Stage::Thinking);
            }
            DisplayEvent::Usage(usage) => {
                self.status.input_tokens += usage.input_tokens;
                self.status.output_tokens += usage.output_tokens;
                self.status.cached_tokens += usage.cache_read_tokens;
                self.transcript.push_line(
                    Kind::Notice,
                    &format!(
                        "· {} in · {} out · {} cached",
                        usage.input_tokens, usage.output_tokens, usage.cache_read_tokens
                    ),
                );
            }
            DisplayEvent::Notice(text) => self.transcript.push_line(Kind::Notice, &text),
            DisplayEvent::Error(text) => self.transcript.push_line(Kind::Error, &text),
            DisplayEvent::Model(name) => self.status.model = name,
            DisplayEvent::Session(id) => {
                // The totals belong to the open session; a session
                // change restarts them.
                self.status.session = id;
                self.status.input_tokens = 0;
                self.status.output_tokens = 0;
                self.status.cached_tokens = 0;
            }
        }
    }

    /// The progress line's stage, while a turn runs.
    fn stage(&mut self, stage: Stage) {
        if let Some(progress) = &mut self.progress {
            progress.stage = stage;
        }
    }

    fn show_approval(&mut self, request: ApprovalRequest) {
        // The question follows the call's dispatch event, which can
        // still sit in the display queue: apply it first, so the
        // running card is the one the question is about.
        self.drain_displays();
        // The running call's card shows that it waits, with the
        // question's preview when the card has none. Without a card
        // — a driver that asks outside a dispatch — the preview shows
        // as a notice, so nothing is approved blind.
        let preview = request.preview.as_deref();
        let card = self.transcript.tool_pending(&request.tool, preview);
        if !card && let Some(preview) = preview {
            self.transcript
                .push_line(Kind::Notice, &format!("{} wants to run:", request.tool));
            self.transcript.push_line(Kind::Notice, preview);
        }
        // A question needs its call visible: jump to the bottom, where
        // the preview and the question row are.
        self.transcript.follow();
        let question = format!(
            "approve {}? [y]es / [a]lways / [N]o",
            cards::one_line(&request.tool, cards::PROGRESS_NAME_BYTES)
        );
        self.mode = Mode::Approval {
            tool: request.tool,
            question,
            card,
            reply: request.reply,
        };
    }

    /// Apply one terminal event. Only an event that changes what the
    /// screen shows marks a frame: a mouse event other than the wheel
    /// changes nothing and must not wake one.
    fn terminal_event(&mut self, event: Event, phase: Phase) -> Action {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                self.dirty = true;
                self.key(key, phase)
            }
            Event::Paste(text) => {
                if matches!(self.mode, Mode::Edit) {
                    self.editor.insert_str(&normalize_paste(&text));
                    self.dirty = true;
                }
                Action::None
            }
            Event::Mouse(mouse) => {
                match mouse.kind {
                    MouseEventKind::ScrollUp => self.transcript.scroll_up(WHEEL_ROWS),
                    MouseEventKind::ScrollDown => self.transcript.scroll_down(WHEEL_ROWS),
                    _ => return Action::None,
                }
                self.dirty = true;
                Action::None
            }
            Event::Resize(..) => {
                self.dirty = true;
                Action::None
            }
            _ => Action::None,
        }
    }

    fn key(&mut self, key: KeyEvent, phase: Phase) -> Action {
        // The browse keys work at every prompt, the approval question
        // included: the call under question may sit above the window.
        match key.code {
            KeyCode::PageUp => {
                self.transcript.scroll_up(usize::from(self.page.max(1)));
                return Action::None;
            }
            KeyCode::PageDown => {
                self.transcript.scroll_down(usize::from(self.page.max(1)));
                return Action::None;
            }
            // End returns a browsed transcript to follow mode; at the
            // bottom it is the cursor key, with Ctrl-E as the twin.
            KeyCode::End if self.transcript.browsing() => {
                self.transcript.follow();
                return Action::None;
            }
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let expanded = !self.transcript.expanded();
                self.transcript.set_expanded(expanded);
                return Action::None;
            }
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.transcript.toggle_thinking();
                return Action::None;
            }
            _ => {}
        }
        if matches!(self.mode, Mode::Approval { .. }) {
            return self.approval_key(key);
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let plain = !ctrl && !key.modifiers.contains(KeyModifiers::ALT);
        let newline = key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT);
        // The popup takes the selection keys while it is open. Enter
        // completes the line, or submits it when it holds the command
        // already; a newline key stays a newline key.
        if plain && self.editor.popup_open() {
            match key.code {
                KeyCode::Tab | KeyCode::Down => {
                    self.editor.select_next();
                    return Action::None;
                }
                KeyCode::BackTab | KeyCode::Up => {
                    self.editor.select_prev();
                    return Action::None;
                }
                KeyCode::Esc => {
                    self.editor.dismiss();
                    return Action::None;
                }
                KeyCode::Enter if !newline => match self.editor.complete() {
                    Completion::Applied => return Action::None,
                    Completion::OnTheLine | Completion::NoMatch => {}
                },
                _ => {}
            }
        }
        match key.code {
            KeyCode::Char('c') if ctrl => return self.ctrl_c(phase),
            KeyCode::Char('d') if ctrl => {
                if !self.editor.is_empty() {
                    self.editor.delete();
                } else if phase == Phase::Idle {
                    return Action::Exit;
                }
            }
            KeyCode::Char('a') if ctrl => self.editor.home(),
            KeyCode::Char('e') if ctrl => self.editor.end(),
            KeyCode::Char('u') if ctrl => self.editor.kill_to_start(),
            KeyCode::Char('k') if ctrl => self.editor.kill_to_end(),
            KeyCode::Char('w') if ctrl => self.editor.kill_word_back(),
            KeyCode::Char(ch) if plain => self.editor.insert_char(ch),
            // Alt-Enter is the portable newline; Shift-Enter reaches
            // the loop as its own key where the terminal takes the
            // keyboard-enhancement flags.
            KeyCode::Enter if newline => self.editor.insert_char('\n'),
            KeyCode::Enter => {
                if phase == Phase::Idle {
                    let line = self.editor.take();
                    let line = line.trim();
                    if !line.is_empty() {
                        // The reply lands at the bottom; show it.
                        self.transcript.follow();
                        return Action::Submit(line.to_string());
                    }
                }
            }
            KeyCode::Backspace => self.editor.backspace(),
            KeyCode::Delete => self.editor.delete(),
            KeyCode::Left => self.editor.left(),
            KeyCode::Right => self.editor.right(),
            KeyCode::Home => self.editor.home(),
            KeyCode::End => self.editor.end(),
            // The arrows cross the editor's rows; past its first or
            // last row they browse the history.
            KeyCode::Up => {
                if !self.editor.up()
                    && let Some(text) = self.history.up(self.editor.text())
                {
                    let text = text.to_string();
                    self.editor.set(&text);
                }
            }
            KeyCode::Down => {
                if !self.editor.down()
                    && let Some(text) = self.history.down()
                {
                    let text = text.to_string();
                    self.editor.set(&text);
                }
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
        let Mode::Approval {
            tool, card, reply, ..
        } = std::mem::replace(&mut self.mode, Mode::Edit)
        else {
            unreachable!("approval_key runs only in approval mode");
        };
        if card {
            self.transcript.tool_answered(
                matches!(answer, ApprovalAnswer::Deny | ApprovalAnswer::Abort),
                std::time::Instant::now(),
            );
        }
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
                self.editor.take();
                self.history.reset();
                Action::None
            }
            Phase::Turn => Action::AbortTurn,
        }
    }

    /// Draw the next frame when one is due. Inside the frame span the
    /// state stays dirty and the loops' frame timer finishes the job.
    fn draw(&mut self) -> io::Result<()> {
        if !self.dirty || Instant::now() < self.last_draw + FRAME {
            return Ok(());
        }
        let Self {
            guard,
            transcript,
            editor,
            status,
            page,
            progress,
            progress_line,
            mode,
            ..
        } = self;
        let question = match mode {
            Mode::Edit => None,
            Mode::Approval { question, .. } => Some(question.as_str()),
        };
        let progress = progress.as_ref().map(|progress| {
            progress.write(progress_line);
            progress_line.as_str()
        });
        guard.terminal.draw(|frame| {
            *page = render::draw(
                frame,
                View {
                    transcript,
                    editor,
                    status,
                    approval: question,
                    progress,
                },
            );
        })?;
        self.dirty = false;
        self.last_draw = Instant::now();
        Ok(())
    }
}

/// Where the thinking clock moves to after `event`, or `None` when
/// it stays. Every event but reasoning text moves it: the thought
/// began when the last other thing ended. An event that ends
/// something — a segment, a tool call — carries that time; the rest
/// count from `now`.
fn thinking_clock(event: &DisplayEvent, now: std::time::Instant) -> Option<std::time::Instant> {
    match event {
        DisplayEvent::ReasoningDelta(_) => None,
        DisplayEvent::ReasoningEnd { at } | DisplayEvent::ToolEnd { at, .. } => Some(*at),
        _ => Some(now),
    }
}

/// Discard queued signal notifications.
async fn drain_signal(sigint: &mut Signal) {
    poll_fn(|cx| {
        while let std::task::Poll::Ready(Some(())) = sigint.poll_recv(cx) {}
        std::task::Poll::Ready(())
    })
    .await;
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

    #[test]
    fn the_thinking_clock_moves_at_every_event_but_reasoning_text() {
        use std::time::{Duration, Instant};
        let now = Instant::now();
        let then = now - Duration::from_secs(3);
        assert_eq!(
            thinking_clock(&DisplayEvent::ReasoningDelta("x".into()), now),
            None
        );
        // Back-to-back segments: the second counts from the first's end.
        assert_eq!(
            thinking_clock(&DisplayEvent::ReasoningEnd { at: then }, now),
            Some(then)
        );
        assert_eq!(
            thinking_clock(
                &DisplayEvent::ToolEnd {
                    id: "c".into(),
                    name: "bash".into(),
                    summary: None,
                    is_error: false,
                    output: String::new(),
                    output_cut: false,
                    at: then,
                },
                now
            ),
            Some(then)
        );
        for event in [
            DisplayEvent::TextDelta("t".into()),
            DisplayEvent::ToolStart {
                id: "c".into(),
                name: "bash".into(),
            },
            DisplayEvent::Notice("n".into()),
        ] {
            assert_eq!(thinking_clock(&event, now), Some(now));
        }
    }
}
