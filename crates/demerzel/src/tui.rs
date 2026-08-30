//! The full-screen interface wired to the runtime. The interface loop
//! lives in `demerzel-tui`; this module is its driver: it opens the
//! session on the first prompt, runs turns, and maps commands and
//! outcomes to messages. The REPL and this driver share the command
//! registry, the previews, and the message wording. The driver builds
//! every preview, so the interface crate needs no knowledge of tools.

use std::path::PathBuf;

use demerzel_core::{
    ApprovalAnswer, Approver, BoxFuture, Engine, EngineEvent, EventSink, TurnOutcome,
};
use demerzel_tui::{ApprovalChannel, Driver, EventSender, Options, StatusPatch, Submission};
use serde_json::Value;

use crate::commands::{
    Effect, NEW_SESSION_NOTE, approval_note, dispatch, effort_note, model_note, session_note,
};
use crate::git::git_branch;
use crate::preview::{ask_preview, run_preview};
use crate::{READY_MARKER, RunConfig, RunError, Runtime, SessionSettings};

/// Run the full-screen interface to completion. The caller owns the
/// tokio runtime.
pub async fn run(config: &RunConfig) -> Result<(), RunError> {
    let runtime = Runtime::new(config.clone())?;
    let mode = config.approval;
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
    options.mode = mode;
    options.effort = config.effort.to_string();
    // The config takes any path; the name and the branch need a
    // real one, so `.` and `..` name their directory and the walk
    // up visits true ancestors only. The session record's `cwd`
    // resolves the same way.
    let project_dir = config
        .project_dir
        .canonicalize()
        .or_else(|_| std::path::absolute(&config.project_dir))
        .unwrap_or_else(|_| config.project_dir.clone());
    options.project = project_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let branch = git_branch(&project_dir).unwrap_or_default();
    options.branch = branch.clone();
    options.mouse = config.mouse;
    options.markdown = config.markdown;
    options.thinking = config.thinking;
    options.theme = config.theme.clone();
    options.commands = crate::commands::specs();
    let mut driver = TuiDriver {
        runtime: &runtime,
        engine: None,
        settings: SessionSettings::from_config(config),
        resume: config.continue_last,
        project_dir,
        branch,
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
    /// The session settings: `/model`, `/effort`, and `/approval`
    /// (with Shift-Tab) switch them.
    settings: SessionSettings,
    /// Whether the next open resumes the latest session. `/new`
    /// clears it.
    resume: bool,
    /// The project directory, resolved: the branch read walks up
    /// from it.
    project_dir: PathBuf,
    /// The branch the status shows; empty for none. Read again at
    /// each turn's start, so a checkout between turns shows at the
    /// next turn.
    branch: String,
}

/// The engine's event sink: it forwards every display event and
/// renders the dispatch preview — the edit diff, the bash command —
/// onto the call's card. The preview is built here, next to the
/// tools.
struct PreviewSink {
    events: EventSender,
}

impl EventSink for PreviewSink {
    fn event(&self, event: EngineEvent<'_>) {
        if let EngineEvent::ToolRun {
            id, name, input, ..
        } = &event
            && let Some(preview) = run_preview(name, input)
        {
            self.events.tool_preview(id, preview);
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
        match dispatch(&line, &self.settings) {
            Some(Effect::Exit) => return Submission::Exit,
            Some(Effect::Note(note)) => return Submission::Note(note),
            Some(Effect::NewSession) => {
                self.engine = None;
                self.resume = false;
                events.session_changed();
                return Submission::Note(NEW_SESSION_NOTE.to_string());
            }
            Some(Effect::Session) => {
                let session = self
                    .engine
                    .as_ref()
                    .map(|engine| (engine.session().id(), engine.session().path()));
                return Submission::Note(session_note(session));
            }
            Some(Effect::SetModel(name)) => {
                if let Some(engine) = self.engine.as_mut()
                    && let Err(error) = engine.set_model(&name)
                {
                    return Submission::Error(format!("demerzel: {error}"));
                }
                self.settings.model = name.clone();
                let mut patch = StatusPatch::default();
                patch.model = Some(name);
                events.status(patch);
                return Submission::Note(model_note(&self.settings.model));
            }
            Some(Effect::SetEffort(effort)) => {
                if let Some(engine) = self.engine.as_mut()
                    && let Err(error) = engine.set_effort(effort)
                {
                    return Submission::Error(format!("demerzel: {error}"));
                }
                self.settings.effort = effort;
                let mut patch = StatusPatch::default();
                patch.effort = Some(effort.to_string());
                events.status(patch);
                return Submission::Note(effort_note(effort));
            }
            Some(Effect::SetApproval(mode)) => {
                if let Some(engine) = self.engine.as_mut()
                    && let Err(error) = engine.set_approval(mode, self.runtime.tools(mode))
                {
                    return Submission::Error(format!("demerzel: {error}"));
                }
                self.settings.approval = mode;
                let mut patch = StatusPatch::default();
                patch.mode = Some(mode);
                events.status(patch);
                return Submission::Note(approval_note(mode));
            }
            None => {}
        }
        Submission::Run(Box::pin(async move {
            // The branch on the status follows a checkout between
            // turns: one read at each turn's start, no polling, and
            // a patch only on a change.
            let branch = git_branch(&self.project_dir).unwrap_or_default();
            if branch != self.branch {
                self.branch = branch;
                let mut patch = StatusPatch::default();
                patch.branch = Some(self.branch.clone());
                events.status(patch);
            }
            if self.engine.is_none() {
                match self.runtime.open_session(self.resume, &self.settings) {
                    Ok((session, history)) => {
                        events.session_changed();
                        if !history.is_empty() {
                            events.notice(format!(
                                "continuing session {} ({} messages)",
                                session.id(),
                                history.len()
                            ));
                        }
                        self.engine = Some(self.runtime.engine(session, history, &self.settings));
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
                Ok(TurnOutcome::CallLimit) => events.error(format!(
                    "demerzel: the turn hit the model-call limit ({} model calls)",
                    self.runtime.config().max_model_calls
                )),
                Ok(TurnOutcome::Refused) => events.error("demerzel: the model refused the request"),
                Ok(_) => {}
                Err(error) => events.error(format!("demerzel: {error}")),
            }
        }))
    }
}
