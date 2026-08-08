//! The plain-terminal REPL: line editor, streamed tokens, one status
//! line per tool call, approval prompts. No TUI in M1.
//!
//! One dedicated thread owns the line editor and stdin; the async loop
//! asks it for lines over a channel. Approval prompts use the same
//! thread, so no two readers ever hold stdin. During a prompt the
//! terminal is in raw mode and Ctrl-C answers the prompt: it clears
//! the main prompt, and it stops the turn at an approval prompt.
//! During a running turn Ctrl-C is a signal that aborts the turn by
//! dropping its future. The session file stays valid across an abort,
//! and the next turn repairs any cut tool call.

use std::io::Write as _;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc as std_mpsc;

use demerzel_core::{
    ApprovalAnswer, ApprovalMode, Approver, BoxFuture, Engine, EngineEvent, EventSink, TurnOutcome,
};
use serde_json::Value;
use tokio::signal::unix::{Signal, SignalKind};
use tokio::sync::oneshot;

use crate::{READY_MARKER, RunConfig, RunError, Runtime};

/// Byte cap for the tool input shown in an approval prompt.
const PREVIEW_BYTES: usize = 1200;

/// One request to the reader thread.
struct ReadRequest {
    prompt: String,
    /// Add the answered line to the editor history.
    remember: bool,
    reply: oneshot::Sender<ReadOutcome>,
}

enum ReadOutcome {
    Line(String),
    /// Ctrl-C at the prompt.
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
    /// start, instead of exiting silently later.
    fn spawn() -> std::io::Result<Self> {
        let (requests, receiver) = std_mpsc::channel::<ReadRequest>();
        let (init_tx, init_rx) = std_mpsc::channel::<Result<(), String>>();
        std::thread::spawn(move || {
            // PreferTerm prompts on /dev/tty, so stdout stays model
            // text even when it is a pipe.
            let config = rustyline::Config::builder()
                .behavior(rustyline::config::Behavior::PreferTerm)
                .build();
            let mut editor =
                match rustyline::Editor::<(), rustyline::history::MemHistory>::with_history(
                    config,
                    rustyline::history::MemHistory::new(),
                ) {
                    Ok(editor) => {
                        let _ = init_tx.send(Ok(()));
                        editor
                    }
                    Err(error) => {
                        let _ = init_tx.send(Err(error.to_string()));
                        return;
                    }
                };
            // A line whose asker went away mid-read; it answers the
            // next request instead of vanishing.
            let mut carried: Option<String> = None;
            while let Ok(request) = receiver.recv() {
                if request.reply.is_closed() {
                    continue;
                }
                let outcome = match carried.take() {
                    Some(line) => ReadOutcome::Line(line),
                    None => match editor.readline(&request.prompt) {
                        Ok(line) => ReadOutcome::Line(line),
                        Err(rustyline::error::ReadlineError::Interrupted) => {
                            ReadOutcome::Interrupted
                        }
                        Err(_) => ReadOutcome::Closed,
                    },
                };
                if let ReadOutcome::Line(line) = &outcome
                    && request.remember
                    && !line.trim().is_empty()
                {
                    let _ = editor.add_history_entry(line.as_str());
                }
                if let Err(unsent) = request.reply.send(outcome)
                    && let ReadOutcome::Line(line) = unsent
                {
                    carried = Some(line);
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
}

impl Renderer {
    pub fn new(show_reasoning: bool) -> Self {
        use std::io::IsTerminal as _;
        let dim = std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        Self {
            show_reasoning,
            dim,
            line_open: AtomicU8::new(LINE_NONE),
        }
    }

    /// Close any line the stream left open.
    pub fn finish(&self) {
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
        self.write_err(text);
        self.write_err("\n");
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
            EngineEvent::ToolStart { name, .. } => {
                self.status_line(&format!("· {name} …"));
            }
            EngineEvent::ToolEnd {
                name,
                summary,
                is_error,
                ..
            } => {
                let mark = if is_error { "✗" } else { "·" };
                self.status_line(&format!("{mark} {}", summary.unwrap_or(name)));
            }
            EngineEvent::ModelUsage(usage) if self.show_reasoning => {
                let line = self.dimmed(&format!(
                    "· {} in · {} out · {} cached",
                    usage.input_tokens, usage.output_tokens, usage.cache_read_tokens
                ));
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
            eprintln!("\n{tool} wants to run:");
            eprintln!("{}", input_preview(input));
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

/// Serializes into a capped buffer, so a preview of a large input
/// never materializes the whole document.
struct CappedWriter {
    buf: Vec<u8>,
    cap: usize,
}

impl std::io::Write for CappedWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.buf.len() >= self.cap {
            return Err(std::io::Error::other("preview cap reached"));
        }
        let take = data.len().min(self.cap - self.buf.len());
        self.buf.extend_from_slice(&data[..take]);
        if take < data.len() {
            return Err(std::io::Error::other("preview cap reached"));
        }
        Ok(take)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Pretty tool input, capped for the terminal.
fn input_preview(input: &Value) -> String {
    let mut writer = CappedWriter {
        buf: Vec::with_capacity(PREVIEW_BYTES.min(256)),
        cap: PREVIEW_BYTES,
    };
    let complete = serde_json::to_writer_pretty(&mut writer, input).is_ok();
    let text = String::from_utf8_lossy(&writer.buf).into_owned();
    if complete {
        text
    } else {
        format!("{text}\n[input truncated]")
    }
}

/// Run the interactive REPL to completion. The caller owns the tokio
/// runtime.
pub async fn run(config: &RunConfig) -> Result<(), RunError> {
    let runtime = Runtime::new(config.clone())?;
    repl(&runtime).await
}

async fn repl(runtime: &Runtime) -> Result<(), RunError> {
    // Install the SIGINT listener before the first prompt, so an
    // external signal cannot kill the process while the terminal is
    // raw.
    let mut sigint = tokio::signal::unix::signal(SignalKind::interrupt())?;

    // `-c` opens its session now, to report what it continues and to
    // fail fast. A fresh session is created on the first turn, so an
    // idle boot leaves no file behind.
    let mut engine: Option<Engine<'_>> = None;
    if runtime.config().continue_last {
        let (session, history) = runtime.open_session()?;
        if !history.is_empty() {
            eprintln!(
                "continuing session {} ({} messages)",
                session.id(),
                history.len()
            );
        }
        engine = Some(runtime.engine(session, history));
    }

    let reader = Reader::spawn()?;
    let mode = match runtime.config().approval {
        ApprovalMode::ReadOnly => "read-only",
        ApprovalMode::Ask => "ask",
        ApprovalMode::Auto => "auto",
    };
    // The PTY boot probe waits for this marker line.
    eprintln!("{READY_MARKER} — {} ({mode})", runtime.config().model);
    eprintln!("Type a prompt. /exit or Ctrl-D ends the session.");

    loop {
        match reader.read("dmz> ", true).await {
            ReadOutcome::Line(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Some(command) = line.strip_prefix('/') {
                    match command.trim() {
                        "exit" | "quit" => break,
                        other => {
                            eprintln!(
                                "unknown command: /{other} — /exit and /quit work today; \
                                 more commands arrive in M2"
                            );
                            continue;
                        }
                    }
                }
                if engine.is_none() {
                    let (session, history) = runtime.open_session()?;
                    engine = Some(runtime.engine(session, history));
                }
                let engine = engine.as_mut().expect("engine opened above");
                run_turn(engine, line.to_string(), &reader, &mut sigint).await;
            }
            ReadOutcome::Interrupted => continue,
            ReadOutcome::Closed => break,
        }
    }
    Ok(())
}

/// One user turn. Ctrl-C drops the turn future; the engine and the
/// session survive for the next prompt.
async fn run_turn(engine: &mut Engine<'_>, text: String, reader: &Reader, sigint: &mut Signal) {
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
            eprintln!("demerzel: the model hit the output-token limit");
        }
        Some(Ok(TurnOutcome::CallLimit)) => {
            eprintln!("demerzel: the turn hit the model-call limit");
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
    use serde_json::json;

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

    #[test]
    fn the_input_preview_caps_long_inputs_without_serializing_them() {
        let short = json!({"path": "f.txt"});
        assert_eq!(
            input_preview(&short),
            serde_json::to_string_pretty(&short).unwrap()
        );

        let long = json!({"content": "é".repeat(200_000)});
        let preview = input_preview(&long);
        assert!(preview.len() < PREVIEW_BYTES + 100);
        assert!(preview.ends_with("[input truncated]"));
    }
}
