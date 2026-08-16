//! The full-screen interface wired to the runtime. The interface loop
//! lives in `demerzel-tui`; this module is its driver: it opens the
//! session on the first prompt, runs turns, and maps commands and
//! outcomes to messages. The REPL and this driver share the command
//! parsing and the message wording.

use demerzel_core::{Engine, TurnOutcome};
use demerzel_tui::{ChannelApprover, Driver, EventSender, Options, Submission};

use crate::repl::{slash_command, unknown_command};
use crate::{READY_MARKER, RunConfig, RunError, Runtime, mode_name};

/// Run the full-screen interface to completion. The caller owns the
/// tokio runtime.
pub async fn run(config: &RunConfig) -> Result<(), RunError> {
    let runtime = Runtime::new(config.clone())?;
    let mode = mode_name(config.approval);
    // The first line on stderr, before the alternate screen. The PTY
    // boot probe waits for it.
    eprintln!("{READY_MARKER} — {} ({mode})", config.model);
    let options = Options {
        history_path: config.history_path.clone(),
        banner: Some(format!(
            "{} ({mode}) — /exit or Ctrl-D ends the session",
            config.model
        )),
    };
    let mut driver = TuiDriver {
        runtime: &runtime,
        engine: None,
    };
    demerzel_tui::run(&mut driver, options)
        .await
        .map_err(RunError::Io)
}

struct TuiDriver<'r> {
    runtime: &'r Runtime,
    /// Opens on the first submitted prompt, so an idle boot reads and
    /// writes no session file — the REPL rule.
    engine: Option<Engine<'r>>,
}

impl Driver for TuiDriver<'_> {
    fn submit(
        &mut self,
        line: String,
        events: EventSender,
        mut approvals: ChannelApprover,
    ) -> Submission<'_> {
        if let Some(command) = slash_command(&line) {
            return match command {
                "exit" | "quit" => Submission::Exit,
                other => Submission::Note(unknown_command(other)),
            };
        }
        Submission::Run(Box::pin(async move {
            if self.engine.is_none() {
                match self.runtime.open_session() {
                    Ok((session, history)) => {
                        if !history.is_empty() {
                            events.notice(format!(
                                "continuing session {} ({} messages)",
                                session.id(),
                                history.len()
                            ));
                        }
                        self.engine = Some(self.runtime.engine(session, history));
                    }
                    Err(error) => {
                        events.error(format!("demerzel: {error}"));
                        return;
                    }
                }
            }
            let engine = self.engine.as_mut().expect("engine opened above");
            match engine.user_turn(line, &events, &mut approvals).await {
                Ok(TurnOutcome::Completed) => {}
                Ok(TurnOutcome::Aborted) => events.notice("[turn stopped]"),
                Ok(TurnOutcome::MaxTokens) => events.error(
                    "demerzel: the model hit a token limit — the output cap or the context window",
                ),
                Ok(TurnOutcome::CallLimit) => {
                    events.error("demerzel: the turn hit the model-call limit")
                }
                Ok(TurnOutcome::Refused) => events.error("demerzel: the model refused the request"),
                Ok(_) => {}
                Err(error) => events.error(format!("demerzel: {error}")),
            }
        }))
    }
}
