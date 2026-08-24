//! The full-screen interface wired to the runtime. The interface loop
//! lives in `demerzel-tui`; this module is its driver: it opens the
//! session on the first prompt, runs turns, and maps commands and
//! outcomes to messages. The REPL and this driver share the command
//! registry, the previews, and the message wording. The driver builds
//! every preview, so the interface crate needs no knowledge of tools.

use demerzel_core::{
    ApprovalAnswer, Approver, BoxFuture, Effort, Engine, EngineEvent, EventSink, TurnOutcome,
};
use demerzel_tui::{ApprovalChannel, Driver, EventSender, Options, Submission};
use serde_json::Value;

use crate::commands::{Effect, NEW_SESSION_NOTE, dispatch, effort_note, model_note};
use crate::preview::{ask_preview, run_preview};
use crate::{READY_MARKER, RunConfig, RunError, Runtime, mode_name};

/// Run the full-screen interface to completion. The caller owns the
/// tokio runtime.
pub async fn run(config: &RunConfig) -> Result<(), RunError> {
    let runtime = Runtime::new(config.clone())?;
    let mode = mode_name(config.approval);
    // The first line on stderr, before the alternate screen. The PTY
    // boot probe waits for it.
    eprintln!("{READY_MARKER} — {} ({mode})", config.model);
    let mut options = Options::default();
    options.history_path = config.history_path.clone();
    options.banner = Some(format!(
        "{} ({mode}) — /help lists the commands; /exit or Ctrl-D ends the session",
        config.model
    ));
    options.model = config.model.clone();
    options.mode = mode.to_string();
    options.mouse = config.mouse;
    options.markdown = config.markdown;
    let mut driver = TuiDriver {
        runtime: &runtime,
        engine: None,
        model: config.model.clone(),
        effort: config.effort,
        resume: config.continue_last,
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
    /// The session model. `/model` switches it.
    model: String,
    /// The session effort; `/effort` sets it.
    effort: Effort,
    /// Whether the next open resumes the latest session. `/new`
    /// clears it.
    resume: bool,
}

/// The engine's event sink: it forwards every display event and
/// renders the dispatch preview — the edit diff, the bash command —
/// into the transcript. The preview is built here, next to the tools.
struct PreviewSink {
    events: EventSender,
}

impl EventSink for PreviewSink {
    fn event(&self, event: EngineEvent<'_>) {
        if let EngineEvent::ToolRun { name, input, .. } = &event
            && let Some(preview) = run_preview(name, input)
        {
            self.events.notice(preview);
        }
        self.events.event(event);
    }
}

/// The engine's approver over the interface's question channel. A
/// call whose preview [`PreviewSink`] already rendered sends no
/// preview: the question row is enough. This pairing is the
/// contract: a turn that uses this approver must use [`PreviewSink`]
/// as its sink, or a covered call would ask with nothing on screen.
/// The PTY tests pin both halves.
struct TuiApprover {
    channel: ApprovalChannel,
}

impl Approver for TuiApprover {
    fn approve<'a>(&'a mut self, tool: &'a str, input: &'a Value) -> BoxFuture<'a, ApprovalAnswer> {
        Box::pin(async move { self.channel.ask(tool, ask_preview(tool, input)).await })
    }
}

impl Driver for TuiDriver<'_> {
    fn submit(
        &mut self,
        line: String,
        events: EventSender,
        approvals: ApprovalChannel,
    ) -> Submission<'_> {
        match dispatch(&line, &self.model, self.effort) {
            Some(Effect::Exit) => return Submission::Exit,
            Some(Effect::Note(note)) => return Submission::Note(note),
            Some(Effect::NewSession) => {
                self.engine = None;
                self.resume = false;
                events.session(None);
                return Submission::Note(NEW_SESSION_NOTE.to_string());
            }
            Some(Effect::SetModel(name)) => {
                if let Some(engine) = self.engine.as_mut()
                    && let Err(error) = engine.set_model(&name)
                {
                    return Submission::Error(format!("demerzel: {error}"));
                }
                self.model = name.clone();
                events.model(name);
                return Submission::Note(model_note(&self.model));
            }
            Some(Effect::SetEffort(effort)) => {
                if let Some(engine) = self.engine.as_mut()
                    && let Err(error) = engine.set_effort(effort)
                {
                    return Submission::Error(format!("demerzel: {error}"));
                }
                self.effort = effort;
                return Submission::Note(effort_note(effort));
            }
            None => {}
        }
        Submission::Run(Box::pin(async move {
            if self.engine.is_none() {
                match self
                    .runtime
                    .open_session(self.resume, &self.model, self.effort)
                {
                    Ok((session, history)) => {
                        events.session(Some(session.id().to_string()));
                        if !history.is_empty() {
                            events.notice(format!(
                                "continuing session {} ({} messages)",
                                session.id(),
                                history.len()
                            ));
                        }
                        self.engine =
                            Some(
                                self.runtime
                                    .engine(session, history, &self.model, self.effort),
                            );
                    }
                    Err(error) => {
                        events.error(format!("demerzel: {error}"));
                        return;
                    }
                }
            }
            let engine = self.engine.as_mut().expect("engine opened above");
            let sink = PreviewSink {
                events: events.clone(),
            };
            let mut approver = TuiApprover { channel: approvals };
            match engine.user_turn(line, &sink, &mut approver).await {
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
