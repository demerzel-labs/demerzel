//! The plain-terminal REPL: line editor, streamed tokens, one status
//! line per tool call, y/n approval prompts. No TUI in M1.
//!
//! One dedicated thread owns the line editor and stdin; the async loop
//! asks it for lines over a channel. Approval prompts use the same
//! thread, so no two readers ever hold stdin. During a prompt the
//! terminal is in raw mode and Ctrl-C answers the prompt (clear or
//! decline); during a running turn Ctrl-C is a signal that aborts the
//! turn by dropping its future. The session file stays valid across
//! an abort, and the next turn repairs any cut tool call.

use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;

use demerzel_core::{
    ApprovalMode, Approver, BoxFuture, Engine, EngineEvent, EventSink, TurnOutcome,
};
use serde_json::Value;
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
    fn spawn() -> Self {
        let (requests, receiver) = std_mpsc::channel::<ReadRequest>();
        std::thread::spawn(move || {
            let config = rustyline::Config::builder().build();
            let Ok(mut editor) =
                rustyline::Editor::<(), rustyline::history::MemHistory>::with_history(
                    config,
                    rustyline::history::MemHistory::new(),
                )
            else {
                return;
            };
            while let Ok(request) = receiver.recv() {
                let outcome = match editor.readline(&request.prompt) {
                    Ok(line) => {
                        if request.remember && !line.trim().is_empty() {
                            let _ = editor.add_history_entry(line.as_str());
                        }
                        ReadOutcome::Line(line)
                    }
                    Err(rustyline::error::ReadlineError::Interrupted) => ReadOutcome::Interrupted,
                    Err(_) => ReadOutcome::Closed,
                };
                let _ = request.reply.send(outcome);
            }
        });
        Self { requests }
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

/// Streams engine events to the terminal: model text to stdout, tool
/// status and reasoning to stderr. Reasoning shows dim; one-shot mode
/// hides it.
pub struct Renderer {
    show_reasoning: bool,
    text_open: AtomicBool,
    reasoning_open: AtomicBool,
}

impl Renderer {
    pub fn new(show_reasoning: bool) -> Self {
        Self {
            show_reasoning,
            text_open: AtomicBool::new(false),
            reasoning_open: AtomicBool::new(false),
        }
    }

    /// Close any line the stream left open.
    pub fn finish(&self) {
        self.close_reasoning();
        if self.text_open.swap(false, Ordering::Relaxed) {
            println!();
        }
    }

    fn close_reasoning(&self) {
        if self.reasoning_open.swap(false, Ordering::Relaxed) {
            eprintln!();
        }
    }
}

impl EventSink for Renderer {
    fn event(&self, event: EngineEvent<'_>) {
        match event {
            EngineEvent::TextDelta(delta) => {
                self.close_reasoning();
                print!("{delta}");
                let _ = std::io::stdout().flush();
                self.text_open
                    .store(!delta.ends_with('\n'), Ordering::Relaxed);
            }
            EngineEvent::ReasoningDelta(delta) => {
                if self.show_reasoning {
                    eprint!("\x1b[2m{delta}\x1b[0m");
                    self.reasoning_open
                        .store(!delta.ends_with('\n'), Ordering::Relaxed);
                }
            }
            EngineEvent::ToolStart { name, .. } => {
                self.close_reasoning();
                eprintln!("· {name} …");
            }
            EngineEvent::ToolEnd {
                name,
                summary,
                is_error,
                ..
            } => {
                let mark = if is_error { "✗" } else { "·" };
                eprintln!("{mark} {}", summary.unwrap_or(name));
            }
            _ => {}
        }
    }
}

/// Asks y/N on the terminal for every gated tool call.
struct TerminalApprover {
    reader: Reader,
}

impl Approver for TerminalApprover {
    fn approve<'a>(&'a mut self, tool: &'a str, input: &'a Value) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            eprintln!("\n{tool} wants to run:");
            eprintln!("{}", input_preview(input));
            match self
                .reader
                .read(&format!("approve {tool}? [y/N] "), false)
                .await
            {
                ReadOutcome::Line(line) => parse_answer(&line),
                ReadOutcome::Interrupted | ReadOutcome::Closed => false,
            }
        })
    }
}

/// True only for an explicit yes.
fn parse_answer(line: &str) -> bool {
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Pretty tool input, capped for the terminal.
fn input_preview(input: &Value) -> String {
    let pretty = serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string());
    if pretty.len() <= PREVIEW_BYTES {
        return pretty;
    }
    let mut end = PREVIEW_BYTES;
    while !pretty.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[truncated: {} bytes total]",
        &pretty[..end],
        pretty.len()
    )
}

/// Run the interactive REPL to completion.
pub fn run(config: &RunConfig) -> Result<(), RunError> {
    let runtime = Runtime::new(config.clone())?;
    let tokio_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build the tokio runtime");
    tokio_runtime.block_on(repl(&runtime))
}

async fn repl(runtime: &Runtime) -> Result<(), RunError> {
    let (session, history) = runtime.open_session()?;
    let resumed = history.len();
    let session_id = session.id().to_string();
    let mut engine = runtime.engine(session, history);

    let mode = match runtime.config().approval {
        ApprovalMode::ReadOnly => "read-only",
        ApprovalMode::Ask => "ask",
        ApprovalMode::Auto => "auto",
    };
    // The PTY boot probe waits for this marker line.
    println!("{READY_MARKER} — {} ({mode})", runtime.config().model);
    if resumed > 0 {
        eprintln!("continuing session {session_id} ({resumed} messages)");
    }
    eprintln!("Type a prompt. exit or Ctrl-D ends the session.");

    let reader = Reader::spawn();
    loop {
        match reader.read("dmz> ", true).await {
            ReadOutcome::Line(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if line == "exit" || line == "quit" {
                    break;
                }
                run_turn(&mut engine, line.to_string(), &reader).await;
            }
            ReadOutcome::Interrupted => continue,
            ReadOutcome::Closed => break,
        }
    }
    Ok(())
}

/// One user turn. Ctrl-C drops the turn future; the engine and the
/// session survive for the next prompt.
async fn run_turn(engine: &mut Engine<'_>, text: String, reader: &Reader) {
    let renderer = Renderer::new(true);
    let mut approver = TerminalApprover {
        reader: reader.clone(),
    };
    let outcome = {
        let turn = engine.user_turn(text, &renderer, &mut approver);
        tokio::pin!(turn);
        tokio::select! {
            outcome = &mut turn => Some(outcome),
            _ = tokio::signal::ctrl_c() => None,
        }
    };
    renderer.finish();
    match outcome {
        None => eprintln!("[turn aborted]"),
        Some(Ok(TurnOutcome::Completed)) => {}
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_an_explicit_yes_approves() {
        assert!(parse_answer("y"));
        assert!(parse_answer(" YES \n"));
        assert!(!parse_answer(""));
        assert!(!parse_answer("n"));
        assert!(!parse_answer("yep"));
        assert!(!parse_answer("no"));
    }

    #[test]
    fn the_input_preview_caps_long_inputs_at_a_char_boundary() {
        let short = json!({"path": "f.txt"});
        assert_eq!(
            input_preview(&short),
            serde_json::to_string_pretty(&short).unwrap()
        );

        let long = json!({"content": "é".repeat(2000)});
        let preview = input_preview(&long);
        assert!(preview.len() < 1400);
        assert!(preview.contains("[truncated:"));
    }
}
