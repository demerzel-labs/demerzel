//! The plain-terminal REPL: line editor, streamed tokens, one status
//! line per tool call, and approval prompts.
//!
//! One dedicated thread owns the line editor and stdin; the async loop
//! asks it for lines over a channel. Approval prompts use the same
//! thread, so no two readers ever hold stdin. During a prompt the
//! terminal is in raw mode and Ctrl-C answers the prompt: at the
//! main prompt it discards the line and asks once, and a second
//! press inside the span ends the session; at an approval prompt it
//! stops the turn. During a running turn Ctrl-C is a signal that
//! aborts the turn by dropping its future. The session file stays
//! valid across an abort, and the next turn repairs any cut tool
//! call.

use std::io::Write as _;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc as std_mpsc;
use std::time::Instant;

use demerzel_core::{
    ApprovalAnswer, Approver, BoxFuture, Engine, EngineEvent, EventSink, TurnOutcome,
};
use demerzel_tui::{QUIT_HINT, QUIT_SPAN};
use serde_json::Value;
use tokio::signal::unix::{Signal, SignalKind};
use tokio::sync::oneshot;

use crate::commands::{
    Effect, NEW_SESSION_NOTE, approval_note, dispatch, effort_note, exit_line, model_note,
    session_note,
};
use crate::preview::{ask_preview, run_preview};
use crate::{READY_MARKER, Resume, RunConfig, RunError, Runtime, SessionSettings};

/// Byte cap for one history entry. A pasted document must not grow
/// the history file and slow every later boot.
const HISTORY_ENTRY_BYTES: usize = 8 * 1024;

/// Entry cap for the history, in memory and on disk.
const HISTORY_ENTRIES: usize = 500;

/// One request to the reader thread.
struct ReadRequest {
    prompt: String,
    /// Add the answered line to the editor history.
    remember: bool,
    reply: oneshot::Sender<ReadOutcome>,
}

enum ReadOutcome {
    Line(String),
    /// Ctrl-C at the prompt. The line editor discards the line; it
    /// cannot say whether one was there, so the loop cannot tell a
    /// clear from an empty press the way the full screen does.
    Interrupted,
    /// Ctrl-D, or stdin closed.
    Closed,
}

/// Handle to the thread that owns the line editor and stdin.
#[derive(Clone)]
struct Reader {
    requests: std_mpsc::Sender<ReadRequest>,
}

impl Reader {
    /// Start the reader thread. Fails when the line editor cannot
    /// start, instead of exiting silently later. `history` is the
    /// input-history file; `None` keeps history in memory only.
    fn spawn(history: Option<std::path::PathBuf>) -> std::io::Result<Self> {
        let (requests, receiver) = std_mpsc::channel::<ReadRequest>();
        let (init_tx, init_rx) = std_mpsc::channel::<Result<(), String>>();
        std::thread::spawn(move || {
            // With a terminal on stdin, prompt on /dev/tty so stdout
            // stays model text even when it is a pipe. With piped
            // stdin, read the pipe.
            use std::io::IsTerminal as _;
            let behavior = if std::io::stdin().is_terminal() {
                rustyline::config::Behavior::PreferTerm
            } else {
                rustyline::config::Behavior::Stdio
            };
            let config = rustyline::Config::builder()
                .behavior(behavior)
                .max_history_size(HISTORY_ENTRIES)
                .expect("the entry cap is a small constant")
                .build();
            // `with_history` does not hand the config to the history,
            // so the entry cap must go in here too.
            let file_history = rustyline::history::FileHistory::with_config(&config);
            let mut editor =
                match rustyline::Editor::<(), rustyline::history::FileHistory>::with_history(
                    config,
                    file_history,
                ) {
                    Ok(editor) => editor,
                    Err(error) => {
                        let _ = init_tx.send(Err(error.to_string()));
                        return;
                    }
                };
            // History is best effort: a missing file is the first
            // run, and a write error must not stop the session. The
            // load runs before the init reply, so the ready marker —
            // and the boot bench behind it — covers it.
            if let Some(path) = &history {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = editor.load_history(path);
            }
            let _ = init_tx.send(Ok(()));
            while let Ok(request) = receiver.recv() {
                if request.reply.is_closed() {
                    continue;
                }
                let outcome = match editor.readline(&request.prompt) {
                    Ok(line) => ReadOutcome::Line(line),
                    Err(rustyline::error::ReadlineError::Interrupted) => ReadOutcome::Interrupted,
                    Err(_) => ReadOutcome::Closed,
                };
                if let ReadOutcome::Line(line) = &outcome
                    && request.remember
                    && !line.trim().is_empty()
                    && line.len() <= HISTORY_ENTRY_BYTES
                {
                    let _ = editor.add_history_entry(line.as_str());
                    if let Some(path) = &history {
                        let _ = editor.append_history(path);
                    }
                }
                // An answer whose asker went away is discarded: a line
                // meant for a dead approval prompt must never become
                // the next model prompt.
                if request.reply.send(outcome).is_err() {
                    let _ = std::io::Write::write_all(
                        &mut std::io::stderr().lock(),
                        b"[input discarded]\n",
                    );
                }
            }
        });
        match init_rx.recv() {
            Ok(Ok(())) => Ok(Self { requests }),
            Ok(Err(error)) => Err(std::io::Error::other(format!(
                "cannot start the line editor: {error}"
            ))),
            Err(_) => Err(std::io::Error::other("the line editor thread died")),
        }
    }

    async fn read(&self, prompt: &str, remember: bool) -> ReadOutcome {
        let (reply, receiver) = oneshot::channel();
        let request = ReadRequest {
            prompt: prompt.to_string(),
            remember,
            reply,
        };
        if self.requests.send(request).is_err() {
            return ReadOutcome::Closed;
        }
        receiver.await.unwrap_or(ReadOutcome::Closed)
    }
}

/// Which stream holds an unterminated line.
const LINE_NONE: u8 = 0;
const LINE_STDOUT: u8 = 1;
const LINE_STDERR: u8 = 2;

/// Streams engine events to the terminal: model text to stdout, tool
/// status and reasoning to stderr. Reasoning shows dim when stderr is
/// a terminal; one-shot mode hides it. Writes never panic: a closed
/// pipe ends the output silently.
pub struct Renderer {
    show_reasoning: bool,
    dim: bool,
    line_open: AtomicU8,
    /// Started tool calls awaiting their end event, as id and name.
    /// An aborted turn leaves entries; `finish` marks them
    /// interrupted, so a rendered preview cannot read like an
    /// applied change. Interrupted, not absent: a dropped turn can
    /// cut an approved call mid-run.
    pending_calls: std::sync::Mutex<Vec<(String, String)>>,
    /// When the turn started, for the time on the usage line.
    started: std::time::Instant,
}

impl Renderer {
    pub fn new(show_reasoning: bool) -> Self {
        use std::io::IsTerminal as _;
        // A non-empty `NO_COLOR` turns the dim off, the rule the
        // full-screen interface follows too.
        let dim = std::io::stderr().is_terminal()
            && std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty());
        Self {
            show_reasoning,
            dim,
            line_open: AtomicU8::new(LINE_NONE),
            pending_calls: std::sync::Mutex::new(Vec::new()),
            started: std::time::Instant::now(),
        }
    }

    /// Close any line the stream left open, and mark every call the
    /// turn never answered as interrupted.
    pub fn finish(&self) {
        let pending = std::mem::take(&mut *self.pending_calls.lock().unwrap());
        for (_, name) in pending {
            self.status_line(&format!("✗ {name}: interrupted"));
        }
        self.close_line();
    }

    fn write_out(&self, text: &str) {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
    }

    fn write_err(&self, text: &str) {
        let _ = std::io::stderr().lock().write_all(text.as_bytes());
    }

    fn close_line(&self) {
        match self.line_open.swap(LINE_NONE, Ordering::Relaxed) {
            LINE_STDOUT => self.write_out("\n"),
            LINE_STDERR => self.write_err("\n"),
            _ => {}
        }
    }

    /// One full status line on stderr, closing any open line first.
    fn status_line(&self, text: &str) {
        self.close_line();
        self.write_err(&format!("{text}\n"));
    }

    fn dimmed(&self, text: &str) -> String {
        if self.dim {
            format!("\x1b[2m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }
}

impl EventSink for Renderer {
    fn event(&self, event: EngineEvent<'_>) {
        match event {
            EngineEvent::TextDelta(delta) => {
                if delta.is_empty() {
                    return;
                }
                if self.line_open.load(Ordering::Relaxed) == LINE_STDERR {
                    self.close_line();
                }
                self.write_out(delta);
                let open = if delta.ends_with('\n') {
                    LINE_NONE
                } else {
                    LINE_STDOUT
                };
                self.line_open.store(open, Ordering::Relaxed);
            }
            EngineEvent::ReasoningDelta(delta) => {
                if !self.show_reasoning || delta.is_empty() {
                    return;
                }
                if self.line_open.load(Ordering::Relaxed) == LINE_STDOUT {
                    self.close_line();
                }
                self.write_err(&self.dimmed(delta));
                let open = if delta.ends_with('\n') {
                    LINE_NONE
                } else {
                    LINE_STDERR
                };
                self.line_open.store(open, Ordering::Relaxed);
            }
            EngineEvent::ToolStart { id, name, .. } => {
                self.pending_calls
                    .lock()
                    .unwrap()
                    .push((id.to_string(), name.to_string()));
                self.status_line(&format!("· {name} …"));
            }
            EngineEvent::ToolRun { name, input, .. } => {
                // One-shot mode hides previews with the reasoning; an
                // interactive session shows them.
                if self.show_reasoning
                    && let Some(preview) = run_preview(name, input)
                {
                    self.close_line();
                    self.write_err(&self.dimmed(&preview));
                    self.write_err("\n");
                }
            }
            EngineEvent::ToolEnd {
                id,
                name,
                summary,
                is_error,
                ..
            } => {
                self.pending_calls
                    .lock()
                    .unwrap()
                    .retain(|(pending, _)| pending != id);
                let mark = if is_error { "✗" } else { "·" };
                self.status_line(&format!("{mark} {}", summary.unwrap_or(name)));
            }
            EngineEvent::ModelUsage(usage) if self.show_reasoning => {
                // The time since the turn started, as the full-screen
                // interface shows it.
                let mut text = format!(
                    "· {} in · {} out · {} cached · ",
                    usage.input_tokens, usage.output_tokens, usage.cache_read_tokens
                );
                demerzel_tui::write_duration(&mut text, self.started.elapsed());
                let line = self.dimmed(&text);
                self.status_line(&line);
            }
            _ => {}
        }
    }
}

/// Asks on the terminal for every gated tool call. Ctrl-C or Ctrl-D at
/// the prompt stops the whole turn.
struct TerminalApprover {
    reader: Reader,
}

impl Approver for TerminalApprover {
    fn approve<'a>(&'a mut self, tool: &'a str, input: &'a Value) -> BoxFuture<'a, ApprovalAnswer> {
        Box::pin(async move {
            // A call the dispatch preview already rendered needs only
            // the question; anything else shows its input here.
            if let Some(preview) = ask_preview(tool, input) {
                eprintln!("\n{tool} wants to run:");
                eprintln!("{preview}");
            }
            match self
                .reader
                .read(&format!("approve {tool}? [y]es / [a]lways / [N]o "), false)
                .await
            {
                ReadOutcome::Line(line) => parse_answer(&line),
                ReadOutcome::Interrupted | ReadOutcome::Closed => ApprovalAnswer::Abort,
            }
        })
    }
}

/// Only explicit words approve; everything else declines.
fn parse_answer(line: &str) -> ApprovalAnswer {
    match line.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => ApprovalAnswer::Allow,
        "a" | "always" => ApprovalAnswer::AllowForSession,
        _ => ApprovalAnswer::Deny,
    }
}

/// Run the interactive REPL to completion. The caller owns the tokio
/// runtime.
pub async fn run(config: &RunConfig) -> Result<(), RunError> {
    let runtime = Runtime::new(config.clone())?;
    // The credentials resolve at boot: a missing login stops the
    // plain interface before its prompt.
    runtime.provider()?;
    repl(&runtime).await
}

async fn repl(runtime: &Runtime) -> Result<(), RunError> {
    // Install the SIGINT listener before the first prompt, so an
    // external signal cannot kill the process while the terminal is
    // raw.
    let mut sigint = tokio::signal::unix::signal(SignalKind::interrupt())?;
    let reader = Reader::spawn(runtime.config().history_path.clone())?;

    let mode = runtime.config().approval;
    // The first line on stderr. The PTY boot probe waits for it.
    eprintln!("{READY_MARKER} — {} ({mode})", runtime.config().model);
    crate::trace::announce();
    eprintln!("Type a prompt. /exit, Ctrl-D, or Ctrl-C twice ends the session.");

    // Every session opens on the first submitted prompt — `-c` too —
    // so an idle boot reads and writes no session file.
    let mut engine: Option<Engine<'_>> = None;
    // The interface owns the session settings: `/model`, `/effort`,
    // and `/approval` switch them.
    let mut settings = SessionSettings::from_config(runtime.config());
    // `/new` turns the next open into a fresh session, `-c` or not.
    let mut resume = runtime.config().resume.clone();
    // Set after one Ctrl-C at the prompt: the deadline for the second
    // press. Any line takes it back. The press discards a typed line
    // too, so a stray press costs the line, never the session.
    let mut quit_asked: Option<Instant> = None;
    // The id of the last session this run opened, for the exit line.
    // `/new` drops the engine, not this.
    let mut opened: Option<String> = None;
    let mut result = Ok(());
    loop {
        match reader.read(demerzel_tui::PROMPT, true).await {
            ReadOutcome::Line(line) => {
                quit_asked = None;
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match dispatch(line, &settings) {
                    Some(Effect::Exit) => break,
                    Some(Effect::Note(note)) => {
                        eprintln!("{note}");
                        continue;
                    }
                    Some(Effect::NewSession) => {
                        engine = None;
                        resume = Resume::Fresh;
                        eprintln!("{NEW_SESSION_NOTE}");
                        continue;
                    }
                    Some(Effect::Session) => {
                        let session = engine
                            .as_ref()
                            .map(|engine| (engine.session().id(), engine.session().path()));
                        eprintln!("{}", session_note(session));
                        continue;
                    }
                    Some(Effect::SetModel(name)) => {
                        if let Some(engine) = engine.as_mut()
                            && let Err(error) = engine.set_model(&name)
                        {
                            eprintln!("demerzel: {error}");
                            continue;
                        }
                        settings.model = name;
                        eprintln!("{}", model_note(&settings.model));
                        continue;
                    }
                    Some(Effect::SetEffort(level)) => {
                        if let Some(engine) = engine.as_mut()
                            && let Err(error) = engine.set_effort(level)
                        {
                            eprintln!("demerzel: {error}");
                            continue;
                        }
                        settings.effort = level;
                        settings.effort_set = true;
                        eprintln!("{}", effort_note(&settings));
                        continue;
                    }
                    Some(Effect::SetApproval(mode)) => {
                        if let Some(engine) = engine.as_mut()
                            && let Err(error) = engine.set_approval(mode, runtime.tools(mode))
                        {
                            eprintln!("demerzel: {error}");
                            continue;
                        }
                        settings.approval = mode;
                        eprintln!("{}", approval_note(settings.approval));
                        continue;
                    }
                    None => {}
                }
                if engine.is_none() {
                    let (session, replay) = match runtime.open_session(&resume, &settings) {
                        Ok(opened) => opened,
                        Err(error) => {
                            result = Err(error);
                            break;
                        }
                    };
                    let id = session.id().to_string();
                    let messages = replay.messages.len();
                    // The state changes after the engine builds, as
                    // the full-screen driver does it.
                    engine = match runtime.engine(session, replay.messages, &settings) {
                        Ok(engine) => Some(engine),
                        Err(error) => {
                            result = Err(error);
                            break;
                        }
                    };
                    opened = Some(id.clone());
                    // The notice alone: the plain interface shows no
                    // history.
                    if messages > 0 {
                        eprintln!("continuing session {id} ({messages} messages)");
                    }
                }
                let engine = engine.as_mut().expect("engine opened above");
                run_turn(
                    engine,
                    line.to_string(),
                    &reader,
                    &mut sigint,
                    runtime.config().max_model_calls,
                )
                .await;
            }
            ReadOutcome::Interrupted => {
                if quit_asked.is_some_and(|until| Instant::now() < until) {
                    break;
                }
                quit_asked = Some(Instant::now() + QUIT_SPAN);
                eprintln!("{QUIT_HINT}");
            }
            ReadOutcome::Closed => break,
        }
    }
    // The line lands after the last prompt, whatever ended the
    // session; a run that opened no session prints nothing.
    if let Some(id) = &opened {
        eprintln!("{}", exit_line(id, runtime.is_latest_session(id)));
    }
    result
}

/// One user turn. Ctrl-C drops the turn future; the engine and the
/// session survive for the next prompt.
async fn run_turn(
    engine: &mut Engine<'_>,
    text: String,
    reader: &Reader,
    sigint: &mut Signal,
    call_limit: u32,
) {
    // The listener is long-lived, so a signal from between turns may
    // still be queued; drop it instead of aborting this turn with it.
    drain(sigint).await;
    let renderer = Renderer::new(true);
    let mut approver = TerminalApprover {
        reader: reader.clone(),
    };
    let outcome = {
        let turn = engine.user_turn(text, &renderer, &mut approver);
        tokio::pin!(turn);
        tokio::select! {
            outcome = &mut turn => Some(outcome),
            _ = sigint.recv() => None,
        }
    };
    renderer.finish();
    match outcome {
        None => eprintln!("[turn aborted]"),
        Some(Ok(TurnOutcome::Completed)) => {}
        Some(Ok(TurnOutcome::Aborted)) => eprintln!("[turn stopped]"),
        Some(Ok(TurnOutcome::MaxTokens)) => {
            eprintln!(
                "demerzel: the model hit a token limit — the output cap or the context window"
            );
        }
        Some(Ok(TurnOutcome::CallLimit)) => {
            eprintln!(
                "demerzel: the turn hit the model-call limit ({} model calls)",
                call_limit
            );
        }
        Some(Ok(TurnOutcome::Refused)) => {
            eprintln!("demerzel: the model refused the request");
        }
        Some(Ok(_)) => {}
        Some(Err(error)) => eprintln!("demerzel: {error}"),
    }
}

/// Discard queued signal notifications.
async fn drain(sigint: &mut Signal) {
    std::future::poll_fn(|cx| {
        while let std::task::Poll::Ready(Some(())) = sigint.poll_recv(cx) {}
        std::task::Poll::Ready(())
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_parse_strictly() {
        assert_eq!(parse_answer("y"), ApprovalAnswer::Allow);
        assert_eq!(parse_answer(" YES \n"), ApprovalAnswer::Allow);
        assert_eq!(parse_answer("a"), ApprovalAnswer::AllowForSession);
        assert_eq!(parse_answer("always"), ApprovalAnswer::AllowForSession);
        assert_eq!(parse_answer(""), ApprovalAnswer::Deny);
        assert_eq!(parse_answer("n"), ApprovalAnswer::Deny);
        assert_eq!(parse_answer("yep"), ApprovalAnswer::Deny);
    }
}
