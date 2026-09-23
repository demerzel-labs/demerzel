//! The full-screen interface wired to the runtime. The interface loop
//! lives in `demerzel-tui`; this module is its driver: it opens the
//! session on the first prompt, runs turns, and maps commands and
//! outcomes to messages. The REPL and this driver share the command
//! registry, the previews, and the message wording. The driver builds
//! every preview, so the interface crate needs no knowledge of tools.
//! A resumed session's past turns go to the screen the same way: the
//! driver turns the session file into display entries, previews
//! included, and the interface shows them as blocks.

use std::path::PathBuf;

use demerzel_core::{
    ApprovalAnswer, Approver, BoxFuture, Engine, EngineEvent, EventSink, TurnOutcome,
};
use demerzel_tui::{ApprovalChannel, Driver, EventSender, Options, StatusPatch, Submission};
use serde_json::Value;

use crate::commands::{
    Effect, NEW_SESSION_NOTE, approval_note, dispatch, effort_note, exit_line, model_note,
    session_note,
};
use crate::git::git_branch;
use crate::preview::{ask_preview, run_preview};
use crate::{READY_MARKER, Resume, RunConfig, RunError, Runtime, SessionSettings};

/// Run the full-screen interface to completion. The caller owns the
/// tokio runtime. A run that opened a session prints the exit line
/// on stderr after the screen restores, on every exit path.
pub async fn run(config: &RunConfig) -> Result<(), RunError> {
    let runtime = Runtime::new(config.clone())?;
    let mode = config.approval;
    // The first line on stderr, before the alternate screen. The PTY
    // boot probe waits for it.
    eprintln!("{READY_MARKER} — {} ({mode})", config.model);
    crate::trace::announce();
    let mut options = Options::default();
    options.history_path = config.history_path.clone();
    options.banner = Some(format!(
        "{} ({mode}) — /help lists the commands; /exit, Ctrl-D, or Ctrl-C twice ends the session",
        config.model
    ));
    options.model = config.model.clone();
    options.mode = mode;
    // A compatible endpoint receives no effort until one is set; the
    // status says so rather than naming a level it does not send.
    options.effort = match SessionSettings::from_config(config).request_effort() {
        Some(effort) => effort.to_string(),
        None => "default".to_string(),
    };
    options.context_window = runtime.context_window(&config.model);
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
    // No credential for the provider: the screen says so before the
    // prompt, and the provider builds at the first turn, so a login
    // that lands meanwhile takes effect.
    options.first_run = runtime.first_run();
    let mut driver = TuiDriver {
        runtime: &runtime,
        engine: None,
        settings: SessionSettings::from_config(config),
        resume: config.resume.clone(),
        opened: None,
        project_dir,
        branch,
    };
    let result = demerzel_tui::run(&mut driver, options).await;
    // The interface gave the screen back; the line lands on the
    // shell's own stream, whatever ended the session.
    if let Some(id) = &driver.opened {
        eprintln!("{}", exit_line(id, runtime.is_latest_session(id)));
    }
    result.map_err(RunError::Io)
}

struct TuiDriver<'r> {
    runtime: &'r Runtime,
    /// Opens on the first submitted prompt, so an idle boot reads and
    /// writes no session file — the REPL rule.
    engine: Option<Engine<'r>>,
    /// The session settings: `/model`, `/effort`, and `/approval`
    /// (with Shift-Tab) switch them.
    settings: SessionSettings,
    /// Which session the next open resumes. `/new` makes it a fresh
    /// one.
    resume: Resume,
    /// The id of the last session this run opened, for the exit
    /// line. `/new` drops the engine, not this.
    opened: Option<String>,
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
                self.resume = Resume::Fresh;
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
                // The window follows the model; an unknown window
                // takes the share away.
                patch.context_window = Some(self.runtime.context_window(&name));
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
                self.settings.effort_set = true;
                let mut patch = StatusPatch::default();
                patch.effort = Some(effort.to_string());
                events.status(patch);
                return Submission::Note(effort_note(Some(effort)));
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
        // The session opens at the first prompt, before the turn
        // starts: the events land above the prompt line's echo, so
        // a resumed session's past turns show first, the notice
        // under them, and the new prompt under the notice — the
        // screen shows what the model receives.
        if self.engine.is_none() {
            let (session, replay) = match self.runtime.open_session(&self.resume, &self.settings) {
                Ok(opened) => opened,
                Err(error) => return Submission::Error(format!("demerzel: {error}")),
            };
            let id = session.id().to_string();
            let history = (!replay.messages.is_empty()).then(|| {
                (
                    crate::replay::entries(&replay),
                    format!(
                        "continuing session {id} ({} messages)",
                        replay.messages.len()
                    ),
                )
            });
            // The state changes after the engine builds: a failure
            // leaves no opened session behind it.
            let engine = match self
                .runtime
                .engine(session, replay.messages, &self.settings)
            {
                Ok(engine) => engine,
                Err(error) => return Submission::Error(format!("demerzel: {error}")),
            };
            self.engine = Some(engine);
            self.opened = Some(id);
            events.session_changed();
            if let Some((entries, notice)) = history {
                events.replay(entries);
                events.notice(notice);
            }
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
