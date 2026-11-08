//! The agent loop: one user turn in, model calls and tool calls until
//! the model ends the turn.
//!
//! The engine owns the in-memory history and keeps it in lockstep with
//! the session file: every message appends to both before the next
//! model call. Prompt assembly is fixed — system prompt, then history —
//! and volatile state (approvals, display) never enters it, so the
//! prompt prefix stays byte-stable across turns.
//!
//! The engine applies the approval policy itself, before every
//! dispatch, and records every decision in the session file.
//!
//! Cancellation contract: dropping the future returned by
//! [`Engine::user_turn`] aborts the turn. Only complete records reach
//! the session file, so an aborted session file stays valid. A tool
//! call that was recorded but never answered is repaired at the start
//! of the next turn with an error result, so a resumed history is
//! always well-formed.
//!
//! Every turn that returns closes with a turn record in the session
//! file: the outcome, or `error` with the provider's message. A
//! dropped turn writes none, so a turn without one was cut off.
//!
//! The engine holds the clock for the session file's timing fields.
//! It reads the system clock and starts a monotonic timer at each
//! boundary — the request send, the tool run, the user turn — and the
//! usage, tool, and turn records carry the start and the wall time.

use std::collections::HashMap;
use std::future::poll_fn;
use std::io;
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;

use crate::approval::{ApprovalDecision, ApprovalMode, ApprovalPolicy, ApprovalRequest};
use crate::message::{ContentBlock, Message, Role, StopReason, Usage};
use crate::provider::{Effort, Provider, ProviderError, Request, StreamEvent};
use crate::route::{ProviderHost, Route, Target};
use crate::session::{Session, SessionMeta, SessionOpen, Timing};
use crate::tool::{BoxFuture, ToolDefinition, ToolHost, ToolOutput};

/// The fixed base system prompt.
pub const DEFAULT_SYSTEM_PROMPT: &str = "You are Demerzel, a terminal coding agent. \
You work inside the user's project directory with the tools the harness provides. \
Read a file before you edit it. Keep changes small and report what you did.";

/// Per-session loop settings. Non-exhaustive: build one with
/// [`EngineConfig::new`] and set the fields, so a later field stays
/// additive.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct EngineConfig {
    /// The targets of the model calls. A turn's first call tries the
    /// head target first and falls back along the tail; see
    /// [`Route`]. The meta records name the head.
    pub route: Route,
    pub system_prompt: String,
    /// Model calls allowed for one user turn before the loop stops.
    pub max_model_calls: u32,
    pub max_output_tokens: Option<u64>,
    /// The session's own reasoning effort, which every target's
    /// requests carry. `None`: each request carries the level that
    /// [`ProviderHost::effort`] gives its target's instance.
    pub effort: Option<Effort>,
}

/// Model calls one user turn may make before the loop stops. A
/// guard against a runaway tool loop, not a budget; the binary's
/// `max_model_calls` config key overrides it.
pub const DEFAULT_MAX_MODEL_CALLS: u32 = 128;

impl EngineConfig {
    pub fn new(route: Route) -> Self {
        Self {
            route,
            system_prompt: DEFAULT_SYSTEM_PROMPT.to_string(),
            max_model_calls: DEFAULT_MAX_MODEL_CALLS,
            max_output_tokens: None,
            effort: None,
        }
    }
}

/// One display event. The renderer draws these; none of them feed back
/// into the prompt.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum EngineEvent<'a> {
    /// A model call attempt starts on this target: the instance and
    /// the model. Each attempt of a fallback chain sends one, before
    /// its provider builds, so the interface names the target in use.
    /// `attempt` counts the call's attempts from 0: a value above 0
    /// follows a failed attempt, a fallback.
    #[non_exhaustive]
    ModelStart {
        provider: &'a str,
        model: &'a str,
        attempt: u32,
    },
    TextDelta(&'a str),
    ReasoningDelta(&'a str),
    /// One reasoning segment ended: the deltas since the last end, or
    /// since the turn's start, form one block. A segment with no
    /// deltas — a redacted one — ends too.
    #[non_exhaustive]
    ReasoningEnd {},
    /// The model started streaming a tool call.
    #[non_exhaustive]
    ToolStart {
        id: &'a str,
        name: &'a str,
    },
    /// A tool call is about to dispatch, with its complete input. The
    /// renderer uses this for previews and approval display.
    #[non_exhaustive]
    ToolRun {
        id: &'a str,
        name: &'a str,
        input: &'a Value,
    },
    /// A tool call finished, with the tool's own one-line summary and
    /// its output text. The output is borrowed for the call; a
    /// renderer that keeps any of it caps what it keeps.
    #[non_exhaustive]
    ToolEnd {
        id: &'a str,
        name: &'a str,
        summary: Option<&'a str>,
        is_error: bool,
        output: &'a str,
    },
    /// Usage for one model call.
    ModelUsage(Usage),
}

/// Receives display events during a turn. Implementations take `&self`
/// so a renderer can hold the sink while the turn future runs; use
/// interior mutability or a channel to collect.
pub trait EventSink: Send + Sync {
    fn event(&self, event: EngineEvent<'_>);
}

impl<F> EventSink for F
where
    F: Fn(EngineEvent<'_>) + Send + Sync,
{
    fn event(&self, event: EngineEvent<'_>) {
        self(event)
    }
}

/// The user's answer to one approval question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ApprovalAnswer {
    /// Run this call.
    Allow,
    /// Run this call, and grant it for the rest of this run at the
    /// scope the policy sets: the exact input for an execute tool,
    /// the tool name otherwise. The grant lives in the engine's
    /// policy; it does not survive a resume.
    AllowForSession,
    /// Refuse this call; the turn continues.
    Deny,
    /// Stop the turn. The refused call and every later call in the
    /// batch get error results, and no model call follows.
    Abort,
}

/// Answers the approval question for one `AskUser` tool call.
pub trait Approver: Send {
    fn approve<'a>(&'a mut self, tool: &'a str, input: &'a Value) -> BoxFuture<'a, ApprovalAnswer>;

    /// The model-facing message for a declined call.
    fn deny_message(&self, tool: &str) -> String {
        format!("the user declined the {tool} call")
    }
}

/// Non-interactive approver: every ask comes back declined. One-shot
/// runs use it; the REPL replaces it with a real prompt.
pub struct NoPrompt;

impl Approver for NoPrompt {
    fn approve<'a>(
        &'a mut self,
        _tool: &'a str,
        _input: &'a Value,
    ) -> BoxFuture<'a, ApprovalAnswer> {
        Box::pin(std::future::ready(ApprovalAnswer::Deny))
    }

    fn deny_message(&self, tool: &str) -> String {
        format!("the {tool} tool needs user approval and this run has no approval prompt")
    }
}

/// How a user turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TurnOutcome {
    /// The model ended its turn.
    Completed,
    /// The model hit a token limit: the output cap or the context
    /// window.
    MaxTokens,
    /// The model or its safety layer declined the request.
    Refused,
    /// The loop hit `max_model_calls` for this user turn.
    CallLimit,
    /// The user stopped the turn at an approval prompt. The session
    /// stays valid; every unrun call carries an error result.
    Aborted,
}

impl TurnOutcome {
    /// The outcome's name in the session file's turn record: one
    /// source for the vocabulary the record documents.
    pub const fn as_str(self) -> &'static str {
        match self {
            TurnOutcome::Completed => "completed",
            TurnOutcome::MaxTokens => "max_tokens",
            TurnOutcome::Refused => "refused",
            TurnOutcome::CallLimit => "call_limit",
            TurnOutcome::Aborted => "aborted",
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EngineError {
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error("session: {0}")]
    Session(#[from] io::Error),
}

/// The parts of one engine. Non-exhaustive: build one with
/// [`EngineParts::new`] and set the optional fields, so a later seam
/// adds a field, not a parameter.
#[non_exhaustive]
pub struct EngineParts<'a> {
    /// Maps each target's instance to its provider.
    pub providers: &'a dyn ProviderHost,
    pub tools: &'a dyn ToolHost,
    pub policy: ApprovalPolicy,
    pub config: EngineConfig,
    pub session: Session,
    /// The history the session continues: a resumed session's
    /// replayed messages. Empty for a fresh session.
    pub history: Vec<Message>,
}

impl<'a> EngineParts<'a> {
    /// The parts of an engine over a fresh session.
    pub fn new(
        providers: &'a dyn ProviderHost,
        tools: &'a dyn ToolHost,
        policy: ApprovalPolicy,
        config: EngineConfig,
        session: Session,
    ) -> Self {
        Self {
            providers,
            tools,
            policy,
            config,
            session,
            history: Vec::new(),
        }
    }
}

/// The turn engine for one session.
pub struct Engine<'a> {
    providers: &'a dyn ProviderHost,
    tools: &'a dyn ToolHost,
    policy: ApprovalPolicy,
    config: EngineConfig,
    session: Session,
    history: Vec<Message>,
    /// Snapshot taken at construction and at every approval switch:
    /// the tool set holds between switches, and the snapshot keeps
    /// the request tools block byte-identical across model calls.
    tool_definitions: Vec<ToolDefinition>,
}

impl<'a> Engine<'a> {
    /// An engine over its parts: a fresh session, or a resumed one
    /// with its replayed history.
    pub fn new(parts: EngineParts<'a>) -> Self {
        Self {
            tool_definitions: parts.tools.definitions(),
            providers: parts.providers,
            tools: parts.tools,
            policy: parts.policy,
            config: parts.config,
            session: parts.session,
            history: parts.history,
        }
    }

    /// The route the model calls follow.
    pub fn route(&self) -> &Route {
        &self.config.route
    }

    pub fn history(&self) -> &[Message] {
        &self.history
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Switch the head target's model for the turns that follow. See
    /// [`Engine::set_head`].
    pub fn set_model(&mut self, model: &str) -> Result<(), EngineError> {
        let instance = self.config.route.head.instance.clone();
        self.set_head(Target::new(instance, model))
    }

    /// Switch the head target, the instance and the model, for the
    /// turns that follow; the tail stays. An instance the host does
    /// not know is an error, and nothing changes. The switch appends a meta record, so
    /// the session file names the target that continues it, and a
    /// resume continues on it. The history keeps its sealed
    /// reasoning: a payload goes only to the instance and the model
    /// that produced it, so a switch back restores the continuity. A
    /// payload without a model binds to no model: it comes from a
    /// file written before the identity fields, and a real switch
    /// drops it, as before.
    pub fn set_head(&mut self, head: Target) -> Result<(), EngineError> {
        if self.providers.kind(&head.instance).is_none() {
            return Err(EngineError::Provider(ProviderError::InvalidRequest(
                format!("no provider instance `{}` in this run", head.instance),
            )));
        }
        let effort = self.effort_for(&head.instance);
        let meta = self.meta(&head, effort, self.policy.mode);
        self.session.append_meta(&meta)?;
        if head != self.config.route.head {
            for message in &mut self.history {
                for block in &mut message.content {
                    if let ContentBlock::Reasoning { sealed, .. } = block
                        && sealed.as_ref().is_some_and(|sealed| sealed.model.is_none())
                    {
                        *sealed = None;
                    }
                }
            }
        }
        self.config.route.head = head;
        Ok(())
    }

    /// The effort that requests to `instance` carry: the session's
    /// own level, else the level the host gives the instance.
    fn effort_for(&self, instance: &str) -> Option<Effort> {
        self.config
            .effort
            .or_else(|| self.providers.effort(instance))
    }

    /// Run one user turn: repair the history if the last turn was cut,
    /// append the user message, then alternate model calls and tool
    /// calls until the model ends the turn or a limit stops it.
    ///
    /// The session file closes the turn with a turn record: the
    /// outcome, or `error` with the message of a provider failure.
    /// The record is an append like every other, so a turn that
    /// completed returns the session error when that append fails —
    /// the file is the source of truth, and the next append would
    /// fail the same way. After a provider failure the append must
    /// not hide the provider's message: its own failure is dropped
    /// and surfaces on the next append. The message is cut at
    /// `MAX_TURN_ERROR_BYTES`, 1 KiB. A session failure writes no
    /// record; the file is the failure.
    pub async fn user_turn(
        &mut self,
        text: String,
        sink: &dyn EventSink,
        approver: &mut dyn Approver,
    ) -> Result<TurnOutcome, EngineError> {
        let at = SystemTime::now();
        let started = Instant::now();
        let result = self.run_turn(text, sink, approver).await;
        let timing = Timing::new(at, started.elapsed());
        match &result {
            // The record first, the line second: a line that says
            // completed must not stand over a file that does not.
            Ok(outcome) => {
                if let Err(error) = self.session.append_turn(outcome.as_str(), None, timing) {
                    log::error!(
                        "turn {} recorded no turn record: session={error}",
                        outcome.as_str()
                    );
                    return Err(error.into());
                }
                log::info!(
                    "turn {} duration_ms={}",
                    outcome.as_str(),
                    timing.duration_ms()
                );
            }
            Err(EngineError::Provider(error)) => {
                let message = error.to_string();
                log::error!(
                    "turn error duration_ms={} error={}",
                    timing.duration_ms(),
                    capped(&message, MAX_TURN_ERROR_BYTES)
                );
                let _ = self.session.append_turn(
                    "error",
                    Some(capped(&message, MAX_TURN_ERROR_BYTES)),
                    timing,
                );
            }
            Err(EngineError::Session(error)) => {
                log::error!(
                    "turn error duration_ms={} session={error}",
                    timing.duration_ms()
                );
            }
        }
        result
    }

    async fn run_turn(
        &mut self,
        text: String,
        sink: &dyn EventSink,
        approver: &mut dyn Approver,
    ) -> Result<TurnOutcome, EngineError> {
        // Repair results ride in front of the user text, inside one
        // message, so roles keep alternating and the wire order stays
        // results-then-text.
        let mut content = self.dangling_repairs();
        content.push(ContentBlock::Text { text });
        self.push(Message {
            role: Role::User,
            content,
        })?;

        let mut calls = 0u32;
        // The target the turn's next call starts at. After a fallback,
        // the turn's later calls start at the target that answered, so
        // a failing head costs its retries once per turn, not once per
        // tool round; the next turn tries the head again.
        let mut start = 0usize;
        loop {
            if calls >= self.config.max_model_calls {
                return Ok(TurnOutcome::CallLimit);
            }
            calls += 1;

            // The call's clock runs from the first send to the
            // terminal event, retries and failed targets included: the
            // wall time the user waited.
            let at = SystemTime::now();
            let started = Instant::now();
            let (response, answered) = self.call_model(calls, start, sink).await?;
            start = answered;
            let timing = Timing::new(at, started.elapsed());
            let ModelResponse {
                message,
                stop,
                usage,
            } = response;

            if !message.content.is_empty() {
                self.push(message.clone())?;
            }
            if let Some(usage) = usage {
                let target = self
                    .config
                    .route
                    .targets()
                    .nth(answered)
                    .expect("the answering target is on the route");
                self.session.append_usage(&usage, target, timing)?;
                sink.event(EngineEvent::ModelUsage(usage));
            }

            match stop {
                StopReason::ToolUse => {
                    let batch = self.run_tools(&message, sink, approver).await?;
                    if batch.results.is_empty() {
                        return Err(EngineError::Provider(ProviderError::Protocol(
                            "the model stopped for tool use without a tool call".to_string(),
                        )));
                    }
                    self.push(Message {
                        role: Role::User,
                        content: batch.results,
                    })?;
                    // The tool records follow the message that
                    // carries the results, one per call in order.
                    for call in &batch.calls {
                        self.session.append_tool(
                            &call.id,
                            &call.name,
                            call.timing,
                            call.is_error,
                            call.output_bytes,
                            call.truncated,
                        )?;
                    }
                    if batch.aborted {
                        return Ok(TurnOutcome::Aborted);
                    }
                }
                StopReason::EndTurn | StopReason::Aborted => return Ok(TurnOutcome::Completed),
                StopReason::MaxTokens => return Ok(TurnOutcome::MaxTokens),
                StopReason::Refusal => return Ok(TurnOutcome::Refused),
            }
        }
    }

    /// One model call: the targets of the route in order, from
    /// target `start`, until one answers. After the last target the
    /// call tries the ones before `start`. A target equal to one the
    /// call already tried is skipped. An attempt fails over to the
    /// next target only when nothing reached the sink yet and the
    /// failure is one that another endpoint can avoid; see
    /// [`falls_back`]. Returns the response and the index of the
    /// target that answered it.
    async fn call_model(
        &self,
        call: u32,
        start: usize,
        sink: &dyn EventSink,
    ) -> Result<(ModelResponse, usize), EngineError> {
        let route = &self.config.route;
        let targets = 1 + route.tail.len();
        let order = (start..targets).chain(0..start.min(targets));
        let mut tried: Vec<usize> = Vec::new();
        let mut last_error = None;
        for index in order {
            let target = route.targets().nth(index).expect("index on the route");
            if tried
                .iter()
                .any(|&earlier| route.targets().nth(earlier) == Some(target))
            {
                continue;
            }
            let attempt = u32::try_from(tried.len()).unwrap_or(u32::MAX);
            tried.push(index);
            sink.event(EngineEvent::ModelStart {
                provider: &target.instance,
                model: &target.model,
                attempt,
            });
            let effort = self.effort_for(&target.instance);
            let effort_name = effort.map_or("default", Effort::as_str);
            let started = Instant::now();
            let mut emitted = false;
            let result = self.attempt(target, effort, sink, &mut emitted).await;
            let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            match result {
                Ok(response) => {
                    if log::log_enabled!(log::Level::Info) {
                        let tokens = match response.usage {
                            Some(usage) => format!(
                                "input_tokens={} output_tokens={} cache_read_tokens={} \
                                 cache_creation_tokens={}",
                                usage.input_tokens,
                                usage.output_tokens,
                                usage.cache_read_tokens,
                                usage.cache_creation_tokens
                            ),
                            None => "usage=none".to_string(),
                        };
                        log::info!(
                            "model call {call}: provider={} model={} effort={effort_name} \
                             stop={} duration_ms={duration_ms} {tokens}",
                            target.instance,
                            target.model,
                            response.stop.as_str(),
                        );
                    }
                    return Ok((response, index));
                }
                Err(error) => {
                    let fallback = !emitted && falls_back(&error);
                    log::warn!(
                        "model call {call} failed: provider={} model={} effort={effort_name} \
                         attempt={} duration_ms={duration_ms} fallback={fallback} error={}",
                        target.instance,
                        target.model,
                        attempt + 1,
                        capped(&error.to_string(), MAX_TURN_ERROR_BYTES)
                    );
                    if !fallback {
                        return Err(error);
                    }
                    last_error = Some(error);
                }
            }
        }
        // Every target failed before an event reached the sink: the
        // call fails with the last error.
        Err(last_error.expect("a route has at least one target"))
    }

    /// One attempt of a model call on `target`: build its provider,
    /// send the request, and collect the response. `emitted` turns
    /// true once a stream event reaches the sink.
    async fn attempt(
        &self,
        target: &Target,
        effort: Option<Effort>,
        sink: &dyn EventSink,
        emitted: &mut bool,
    ) -> Result<ModelResponse, EngineError> {
        // The provider comes from the target this attempt goes to,
        // and so do its capabilities. A failed build is a failed
        // attempt.
        let provider = self.providers.provider(&target.instance)?;
        let request = self.assemble(provider, target, effort);
        let stream = provider.stream(request).await?;
        collect_response(stream, sink, emitted).await
    }

    /// Fixed prompt assembly for `target`: system prompt, then
    /// history. Nothing volatile enters the request. Tools go along
    /// only when the target's provider declares the capability. A
    /// sealed payload that the target may not receive stays out of
    /// the copy: the history keeps it for a switch back, and the
    /// request does not carry it to an adapter that drops it. A
    /// payload without a model identity, from an older file, goes to
    /// the head target alone: a fallback target on the same adapter
    /// can hold another key or another model.
    fn assemble(
        &self,
        provider: &dyn Provider,
        target: &Target,
        effort: Option<Effort>,
    ) -> Request {
        let is_tail = *target != self.config.route.head;
        let mut request = Request::new(target.model.clone());
        request.system_prompt = self.config.system_prompt.clone();
        request.messages = self
            .history
            .iter()
            .map(|message| Message {
                role: message.role,
                content: message
                    .content
                    .iter()
                    .map(|block| match block {
                        ContentBlock::Reasoning {
                            summary,
                            sealed: Some(sealed),
                        } if !sealed.replays_to(provider.id(), &target.instance, &target.model)
                            || (sealed.model.is_none() && is_tail) =>
                        {
                            ContentBlock::Reasoning {
                                summary: summary.clone(),
                                sealed: None,
                            }
                        }
                        other => other.clone(),
                    })
                    .collect(),
            })
            .collect();
        if provider.capabilities().tool_calls {
            request.tools = self.tool_definitions.clone();
        }
        request.max_output_tokens = self.config.max_output_tokens;
        request.effort = effort;
        request
    }

    /// Set the reasoning effort for the requests that follow. A fresh
    /// meta record names it, the way a model switch leaves its trail.
    pub fn set_effort(&mut self, effort: Effort) -> Result<(), EngineError> {
        // The record first, the switch second: a failed write leaves
        // the engine where the interface still believes it is.
        let meta = self.meta(&self.config.route.head, Some(effort), self.policy.mode);
        self.session.append_meta(&meta)?;
        self.config.effort = Some(effort);
        Ok(())
    }

    /// Set the approval mode for the turns that follow, with the tool
    /// set that goes with it: `read-only` drops the write and execute
    /// tools, `auto` asks nothing, `ask` asks again. `tools` must be
    /// the host for `mode` — the runtime's `tools(mode)` — so the
    /// advertised set and the policy agree. The session's grants
    /// survive the switch; `read-only` has no tool for them to cover.
    /// A fresh meta record names the mode, the way a model or an
    /// effort switch leaves its trail.
    pub fn set_approval(
        &mut self,
        mode: ApprovalMode,
        tools: &'a dyn ToolHost,
    ) -> Result<(), EngineError> {
        let head = &self.config.route.head;
        let meta = self.meta(head, self.effort_for(&head.instance), mode);
        self.session.append_meta(&meta)?;
        self.policy.mode = mode;
        self.tools = tools;
        // Snapshot on every switch: a host may vary its definitions,
        // and the advertised set must be the host's current one. The
        // registries build the same bytes for the same set, so an
        // equal rebuild leaves the request tools block byte-identical
        // and the provider's cached prefix intact.
        self.tool_definitions = tools.definitions();
        Ok(())
    }

    /// A meta record for head target `head` with these settings. It
    /// starts from the session's last record and changes only what a
    /// switch changes, so a field the opening record carries stays on
    /// every later one. The adapter id comes from the host, with no
    /// provider build.
    fn meta(&self, head: &Target, effort: Option<Effort>, approval: ApprovalMode) -> SessionMeta {
        let instance = &head.instance;
        let kind = self.providers.kind(instance).unwrap_or(instance);
        let mut open = SessionOpen::new(self.session.cwd(), instance, kind, &head.model);
        open.effort = effort;
        open.approval = approval;
        let fresh = SessionMeta::now(&open);
        let Some(last) = self.session.meta() else {
            return fresh;
        };
        let mut meta = last.clone();
        meta.provider = fresh.provider;
        meta.kind = fresh.kind;
        meta.model = fresh.model;
        meta.effort = fresh.effort;
        meta.approval = fresh.approval;
        meta.created = fresh.created;
        meta
    }

    /// Append one message to the session file and the history, in that
    /// order, so the file never lags the prompt.
    fn push(&mut self, message: Message) -> Result<(), EngineError> {
        // The conversation itself, at trace only: the docs say the
        // file holds it at that level.
        log::trace!(
            "message {}",
            serde_json::to_string(&message).unwrap_or_default()
        );
        self.session.append_message(&message)?;
        self.history.push(message);
        Ok(())
    }

    /// Error results for tool calls that a cut-off turn left
    /// unanswered. Calls pair with results positionally, so a call id
    /// an adapter reuses later still gets its own answer; duplicate
    /// dangling ids repair once.
    fn dangling_repairs(&self) -> Vec<ContentBlock> {
        let mut unanswered: Vec<&str> = Vec::new();
        for message in &self.history {
            for block in &message.content {
                match block {
                    ContentBlock::ToolUse { id, .. } => unanswered.push(id),
                    ContentBlock::ToolResult { tool_use_id, .. } => {
                        if let Some(position) =
                            unanswered.iter().rposition(|open| *open == tool_use_id)
                        {
                            unanswered.remove(position);
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut seen = std::collections::HashSet::new();
        unanswered
            .into_iter()
            .filter(|id| seen.insert(*id))
            .map(|id| ContentBlock::ToolResult {
                tool_use_id: id.to_string(),
                content: "interrupted: the session ended before this call returned a result; \
                          the call may have run"
                    .to_string(),
                is_error: true,
            })
            .collect()
    }

    /// Run every tool call in the message, in order. The engine checks
    /// the policy against the access class the host declares, and each
    /// decision goes into the session record with its call id. A name
    /// the host does not hold skips the policy: `run_approved` returns
    /// the unknown-tool error and nothing runs.
    ///
    /// Returns the result blocks, whether the user stopped the turn,
    /// and one [`ToolCall`] per result for the session's tool
    /// records. On an abort, the refused call and every later call
    /// get error results, so nothing dangles.
    async fn run_tools(
        &mut self,
        message: &Message,
        sink: &dyn EventSink,
        approver: &mut dyn Approver,
    ) -> Result<ToolBatch, EngineError> {
        let calls: Vec<(String, String, Value)> = message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { id, name, input } => {
                    Some((id.clone(), name.clone(), input.clone()))
                }
                _ => None,
            })
            .collect();

        let mut batch = ToolBatch {
            results: Vec::with_capacity(calls.len()),
            calls: Vec::with_capacity(calls.len()),
            aborted: false,
        };
        for (id, name, input) in calls {
            if batch.aborted {
                let output = ToolOutput::error(format!(
                    "the user stopped the turn; the {name} call did not run"
                ));
                batch.answer(
                    id,
                    name,
                    output,
                    Timing::new(SystemTime::now(), Duration::ZERO),
                );
                continue;
            }
            sink.event(EngineEvent::ToolRun {
                id: &id,
                name: &name,
                input: &input,
            });
            // Decide first, then run on the clock: the wait at an
            // approval prompt is the user's time, not the tool's.
            let verdict = match self.tools.access(&name) {
                None => Verdict::Run,
                Some(access) => {
                    let decision = self.policy.decide(&ApprovalRequest {
                        tool: &name,
                        access,
                        input: &input,
                    });
                    let answer = match decision {
                        ApprovalDecision::Allow => ApprovalAnswer::Allow,
                        ApprovalDecision::Deny => ApprovalAnswer::Deny,
                        ApprovalDecision::AskUser => approver.approve(&name, &input).await,
                    };
                    let approved = matches!(
                        answer,
                        ApprovalAnswer::Allow | ApprovalAnswer::AllowForSession
                    );
                    let answer_name = match answer {
                        ApprovalAnswer::Allow => "allow",
                        ApprovalAnswer::AllowForSession => "allow_for_session",
                        ApprovalAnswer::Deny => "deny",
                        ApprovalAnswer::Abort => "abort",
                    };
                    let asked = matches!(decision, ApprovalDecision::AskUser);
                    log::info!(
                        "approval {name} call_id={id} mode={} decision={} answer={}",
                        self.policy.mode,
                        decision.as_str(),
                        if asked { answer_name } else { "-" }
                    );
                    self.session.append_approval(
                        &name,
                        self.policy.mode,
                        decision,
                        &id,
                        asked.then_some(approved),
                        asked.then_some(answer_name),
                    )?;
                    if answer == ApprovalAnswer::AllowForSession {
                        self.policy.grant(&ApprovalRequest {
                            tool: &name,
                            access,
                            input: &input,
                        });
                    }
                    if approved {
                        Verdict::Run
                    } else if decision == ApprovalDecision::Deny {
                        Verdict::Refuse(format!(
                            "the approval mode refuses the {name} tool in this session"
                        ))
                    } else if answer == ApprovalAnswer::Abort {
                        batch.aborted = true;
                        Verdict::Refuse(format!(
                            "the user stopped the turn; the {name} call did not run"
                        ))
                    } else {
                        Verdict::Refuse(approver.deny_message(&name))
                    }
                }
            };
            let at = SystemTime::now();
            let started = Instant::now();
            let output = match verdict {
                Verdict::Run => self.tools.run_approved(&name, input).await,
                Verdict::Refuse(message) => ToolOutput::error(message),
            };
            let timing = Timing::new(at, started.elapsed());
            // The summary carries the tool's own outcome line — for
            // bash, the exit code.
            log::info!(
                "tool {name} call_id={id} duration_ms={} is_error={} output_bytes={} \
                 truncated={} summary={:?}",
                timing.duration_ms(),
                output.is_error,
                output.content.len(),
                output.truncated,
                output.summary.as_deref().unwrap_or("-")
            );
            sink.event(EngineEvent::ToolEnd {
                id: &id,
                name: &name,
                summary: output.summary.as_deref(),
                is_error: output.is_error,
                output: &output.content,
            });
            batch.answer(id, name, output, timing);
        }
        Ok(batch)
    }
}

/// Whether a tool call runs, after the policy and the user answered.
enum Verdict {
    Run,
    /// The call does not run; the model gets this error text.
    Refuse(String),
}

/// One answered tool call, for its `tool` session record.
struct ToolCall {
    id: String,
    name: String,
    timing: Timing,
    is_error: bool,
    output_bytes: u64,
    truncated: bool,
}

/// The answers to one batch of tool calls: the result blocks for the
/// next user message, and the call records that follow it.
struct ToolBatch {
    results: Vec<ContentBlock>,
    calls: Vec<ToolCall>,
    aborted: bool,
}

impl ToolBatch {
    /// Answer one call with its output: the result block for the
    /// model, and the record for the file.
    fn answer(&mut self, id: String, name: String, output: ToolOutput, timing: Timing) {
        self.calls.push(ToolCall {
            id: id.clone(),
            name,
            timing,
            is_error: output.is_error,
            output_bytes: output.content.len() as u64,
            truncated: output.truncated,
        });
        self.results.push(ContentBlock::ToolResult {
            tool_use_id: id,
            content: output.content,
            is_error: output.is_error,
        });
    }
}

/// Byte cap for the provider message in a turn record. A provider
/// error can carry a whole server response; the file keeps its head,
/// so one bad reply cannot grow every later replay.
const MAX_TURN_ERROR_BYTES: usize = 1024;

/// `text` cut to `cap` bytes at a char boundary.
fn capped(text: &str, cap: usize) -> &str {
    if text.len() <= cap {
        return text;
    }
    let mut end = cap;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

struct ModelResponse {
    message: Message,
    stop: StopReason,
    usage: Option<Usage>,
}

/// Whether a failed attempt moves the call to the next target of the
/// route: a failure that another endpoint or credential can avoid,
/// after the adapter's own retries. A protocol error, a stream the
/// adapter could not read, and a session failure end the call.
fn falls_back(error: &EngineError) -> bool {
    matches!(
        error,
        EngineError::Provider(
            ProviderError::Auth(_)
                | ProviderError::Network(_)
                | ProviderError::Server(_)
                | ProviderError::RateLimited { .. }
                | ProviderError::InvalidRequest(_)
        )
    )
}

/// Drain one response stream into an assistant message, preserving the
/// block order the model produced. `emitted` turns true at the first
/// event that reaches the sink: from then on, a failure cannot fall
/// back without repeating what the screen shows.
async fn collect_response(
    mut stream: crate::provider::EventStream,
    sink: &dyn EventSink,
    emitted: &mut bool,
) -> Result<ModelResponse, EngineError> {
    let mut blocks: Vec<ContentBlock> = Vec::new();
    let mut call_names: HashMap<String, String> = HashMap::new();
    let mut usage = None;

    loop {
        let Some(item) = poll_fn(|cx| stream.as_mut().poll_next(cx)).await else {
            return Err(EngineError::Provider(ProviderError::Protocol(
                "the stream ended without a done event".to_string(),
            )));
        };
        match item? {
            StreamEvent::TextDelta(delta) => {
                *emitted = true;
                sink.event(EngineEvent::TextDelta(&delta));
                if let Some(ContentBlock::Text { text }) = blocks.last_mut() {
                    text.push_str(&delta);
                } else {
                    blocks.push(ContentBlock::Text { text: delta });
                }
            }
            StreamEvent::ReasoningDelta(delta) => {
                *emitted = true;
                sink.event(EngineEvent::ReasoningDelta(&delta));
            }
            StreamEvent::ReasoningDone { summary, sealed } => {
                *emitted = true;
                sink.event(EngineEvent::ReasoningEnd {});
                blocks.push(ContentBlock::Reasoning { summary, sealed });
            }
            StreamEvent::ToolCallStart { id, name } => {
                *emitted = true;
                sink.event(EngineEvent::ToolStart {
                    id: &id,
                    name: &name,
                });
                call_names.insert(id, name);
            }
            StreamEvent::ToolCallDelta { .. } => {}
            StreamEvent::ToolCallEnd { id, input } => {
                let Some(name) = call_names.get(&id).cloned() else {
                    return Err(EngineError::Provider(ProviderError::Protocol(format!(
                        "tool call {id} ended without a start event"
                    ))));
                };
                blocks.push(ContentBlock::ToolUse { id, name, input });
            }
            StreamEvent::Usage(u) => usage = Some(u),
            StreamEvent::Done(stop) => {
                return Ok(ModelResponse {
                    message: Message {
                        role: Role::Assistant,
                        content: blocks,
                    },
                    stop,
                    usage,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::{ApprovalMode, ToolAccess};
    use crate::message::SealedPayload;
    use crate::provider::{Capabilities, EventStream};
    use crate::route::ProviderHost;
    use crate::route::Target;
    use crate::session::{SessionStore, replay};
    use futures_core::Stream;
    use serde_json::json;
    use std::collections::VecDeque;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Mutex;
    use std::task::{Context, Poll, Waker};

    /// Every future in these tests resolves without waiting, so a noop
    /// waker drives the whole engine.
    fn block_on<T>(future: impl Future<Output = T>) -> T {
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        match std::pin::pin!(future).poll(&mut cx) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("engine future must not block in unit tests"),
        }
    }

    struct VecStream(VecDeque<Result<StreamEvent, ProviderError>>);

    impl Stream for VecStream {
        type Item = Result<StreamEvent, ProviderError>;

        fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            Poll::Ready(self.0.pop_front())
        }
    }

    /// Replays scripted event lists and records every request.
    struct ScriptedProvider {
        scripts: Mutex<VecDeque<Vec<StreamEvent>>>,
        requests: Mutex<Vec<Request>>,
        caps: Capabilities,
        /// A wait inside every call, so a test can bound the clock.
        delay: Duration,
    }

    impl ScriptedProvider {
        fn new(scripts: Vec<Vec<StreamEvent>>) -> Self {
            Self {
                scripts: Mutex::new(scripts.into()),
                requests: Mutex::new(Vec::new()),
                caps: Capabilities {
                    streaming: true,
                    tool_calls: true,
                    ..Capabilities::default()
                },
                delay: Duration::ZERO,
            }
        }

        fn with_delay(mut self, delay: Duration) -> Self {
            self.delay = delay;
            self
        }

        fn without_tool_calls(scripts: Vec<Vec<StreamEvent>>) -> Self {
            let mut provider = Self::new(scripts);
            provider.caps.tool_calls = false;
            provider
        }

        fn requests(&self) -> Vec<Request> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Provider for ScriptedProvider {
        fn id(&self) -> &'static str {
            "scripted"
        }

        fn capabilities(&self) -> Capabilities {
            self.caps
        }

        fn stream(&self, request: Request) -> BoxFuture<'_, Result<EventStream, ProviderError>> {
            self.requests.lock().unwrap().push(request);
            // A blocking wait: the noop waker never re-polls, and the
            // call's clock must see the time pass.
            std::thread::sleep(self.delay);
            let script = self
                .scripts
                .lock()
                .unwrap()
                .pop_front()
                .expect("script exhausted");
            Box::pin(std::future::ready(Ok(
                Box::pin(VecStream(script.into_iter().map(Ok).collect())) as EventStream,
            )))
        }
    }

    /// The engine tests run one provider, which serves every instance.
    impl ProviderHost for ScriptedProvider {
        fn provider(&self, _instance: &str) -> Result<&dyn Provider, ProviderError> {
            Ok(self)
        }

        fn kind(&self, _instance: &str) -> Option<&str> {
            Some(self.id())
        }
    }

    /// Three tools: `note` is read-only, `write` writes, `exec`
    /// executes. Any other name is unknown.
    struct FakeTools;

    impl ToolHost for FakeTools {
        fn definitions(&self) -> Vec<ToolDefinition> {
            ["note", "write", "exec"]
                .into_iter()
                .map(|name| ToolDefinition {
                    name: name.to_string(),
                    description: format!("fake {name}"),
                    input_schema: json!({"type": "object"}),
                })
                .collect()
        }

        fn access(&self, name: &str) -> Option<ToolAccess> {
            match name {
                "note" => Some(ToolAccess::ReadOnly),
                "write" => Some(ToolAccess::Write),
                "exec" => Some(ToolAccess::Execute),
                _ => None,
            }
        }

        fn run_approved(&self, name: &str, input: Value) -> BoxFuture<'_, ToolOutput> {
            // `{"sleep_ms": n}` holds the run for n milliseconds, so a
            // test can bound the tool clock.
            if let Some(millis) = input["sleep_ms"].as_u64() {
                std::thread::sleep(Duration::from_millis(millis));
            }
            let output = if self.access(name).is_some() {
                ToolOutput::text(format!("{name} ran: {input}")).with_summary(format!("{name}: ok"))
            } else {
                ToolOutput::error(format!(
                    "unknown tool: {name}; available tools: note, write, exec"
                ))
            };
            Box::pin(std::future::ready(output))
        }
    }

    struct ApproveAll;

    impl Approver for ApproveAll {
        fn approve<'a>(
            &'a mut self,
            _tool: &'a str,
            _input: &'a Value,
        ) -> BoxFuture<'a, ApprovalAnswer> {
            Box::pin(std::future::ready(ApprovalAnswer::Allow))
        }
    }

    /// Answers from a script, so a test can grant or abort.
    struct ScriptedApprover(VecDeque<ApprovalAnswer>);

    impl Approver for ScriptedApprover {
        fn approve<'a>(
            &'a mut self,
            _tool: &'a str,
            _input: &'a Value,
        ) -> BoxFuture<'a, ApprovalAnswer> {
            let answer = self.0.pop_front().expect("approver script exhausted");
            Box::pin(std::future::ready(answer))
        }
    }

    /// The route of a test engine: the scripted instance and `model`.
    fn route(model: &str) -> Route {
        Route::new(Target::new("scripted", model))
    }

    fn new_engine<'a>(
        provider: &'a ScriptedProvider,
        tools: &'a FakeTools,
        policy: ApprovalPolicy,
        config: EngineConfig,
        session: Session,
    ) -> Engine<'a> {
        Engine::new(EngineParts::new(provider, tools, policy, config, session))
    }

    fn engine_with_history<'a>(
        provider: &'a ScriptedProvider,
        tools: &'a FakeTools,
        policy: ApprovalPolicy,
        config: EngineConfig,
        session: Session,
        history: Vec<Message>,
    ) -> Engine<'a> {
        let mut parts = EngineParts::new(provider, tools, policy, config, session);
        parts.history = history;
        Engine::new(parts)
    }

    fn session_in(dir: &tempfile::TempDir) -> Session {
        SessionStore::at(dir.path())
            .create(&SessionOpen::new(
                dir.path(),
                "scripted",
                "scripted",
                "test-model",
            ))
            .unwrap()
    }

    fn engine<'a>(
        provider: &'a ScriptedProvider,
        tools: &'a FakeTools,
        mode: ApprovalMode,
        session: Session,
    ) -> Engine<'a> {
        new_engine(
            provider,
            tools,
            ApprovalPolicy::new(mode),
            EngineConfig::new(route("test-model")),
            session,
        )
    }

    fn tool_call(id: &str, name: &str, input: Value) -> Vec<StreamEvent> {
        vec![
            StreamEvent::ToolCallStart {
                id: id.into(),
                name: name.into(),
            },
            StreamEvent::ToolCallEnd {
                id: id.into(),
                input,
            },
            StreamEvent::Done(StopReason::ToolUse),
        ]
    }

    fn text_done(text: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::TextDelta(text.into()),
            StreamEvent::Usage(Usage {
                input_tokens: 7,
                output_tokens: 3,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            }),
            StreamEvent::Done(StopReason::EndTurn),
        ]
    }

    fn noop_sink() -> impl EventSink {
        |_: EngineEvent<'_>| {}
    }

    #[test]
    fn a_refusal_ends_the_turn_as_refused() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ScriptedProvider::new(vec![vec![StreamEvent::Done(StopReason::Refusal)]]);
        let tools = FakeTools;
        let mut engine = engine(&provider, &tools, ApprovalMode::Auto, session_in(&dir));

        let outcome = block_on(engine.user_turn("hi".into(), &noop_sink(), &mut ApproveAll));
        assert_eq!(outcome.unwrap(), TurnOutcome::Refused);
        assert_eq!(turn_records(&engine), ["refused"]);
    }

    /// The `outcome` of every turn record in the session file, in
    /// order; `error:<message>` for a failed turn. Every line must
    /// parse: a torn or malformed record fails the test.
    fn turn_records(engine: &Engine<'_>) -> Vec<String> {
        let raw = std::fs::read_to_string(engine.session().path()).unwrap();
        raw.lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|record| record["type"] == "turn")
            .map(|record| {
                let outcome = record["outcome"].as_str().unwrap();
                match record["error"].as_str() {
                    Some(error) => format!("{outcome}:{error}"),
                    None => outcome.to_string(),
                }
            })
            .collect()
    }

    /// Every record in the session file, parsed, in file order.
    fn records(engine: &Engine<'_>) -> Vec<Value> {
        let raw = std::fs::read_to_string(engine.session().path()).unwrap();
        raw.lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect()
    }

    /// `at` parses as RFC 3339 and `duration_ms` is a count.
    fn assert_clock(record: &Value) {
        let at = record["at"].as_str().expect("at");
        humantime::parse_rfc3339(at).expect("rfc 3339");
        assert!(record["duration_ms"].is_u64(), "{record}");
    }

    #[test]
    fn usage_and_turn_records_carry_the_clock() {
        let dir = tempfile::tempdir().unwrap();
        // The call takes 20 ms, so the clock must read at least that:
        // a duration measured off the seam reads zero.
        let provider =
            ScriptedProvider::new(vec![text_done("hi")]).with_delay(Duration::from_millis(20));
        let tools = FakeTools;
        let mut engine = engine(&provider, &tools, ApprovalMode::Auto, session_in(&dir));

        let before = SystemTime::now();
        block_on(engine.user_turn("hi".into(), &noop_sink(), &mut ApproveAll)).unwrap();

        let records = records(&engine);
        let usage = records.iter().find(|r| r["type"] == "usage").unwrap();
        assert_clock(usage);
        assert_eq!(usage["input_tokens"], 7);
        assert!(usage["duration_ms"].as_u64().unwrap() >= 15, "{usage}");
        let turn = records.iter().find(|r| r["type"] == "turn").unwrap();
        assert_clock(turn);
        // The turn spans the call, so it lasts at least as long, and
        // it started no later than the call did.
        assert!(turn["duration_ms"].as_u64() >= usage["duration_ms"].as_u64());
        let turn_at = humantime::parse_rfc3339(turn["at"].as_str().unwrap()).unwrap();
        let usage_at = humantime::parse_rfc3339(usage["at"].as_str().unwrap()).unwrap();
        assert!(turn_at <= usage_at);
        // Millisecond precision floors the start, so allow one.
        assert!(turn_at + Duration::from_millis(1) >= before, "{turn}");
    }

    #[test]
    fn a_failed_turn_record_carries_the_clock_too() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ScriptedProvider::new(vec![vec![StreamEvent::Done(StopReason::ToolUse)]]);
        let tools = FakeTools;
        let mut engine = engine(&provider, &tools, ApprovalMode::Auto, session_in(&dir));

        block_on(engine.user_turn("hi".into(), &noop_sink(), &mut ApproveAll)).unwrap_err();

        let records = records(&engine);
        let turn = records.iter().find(|r| r["type"] == "turn").unwrap();
        assert_eq!(turn["outcome"], "error");
        assert_clock(turn);
    }

    #[test]
    fn each_tool_call_gets_a_record_after_the_result_message() {
        let dir = tempfile::tempdir().unwrap();
        // Two calls in one batch: one runs, one the mode refuses.
        let provider = ScriptedProvider::new(vec![
            vec![
                StreamEvent::ToolCallStart {
                    id: "call_1".into(),
                    name: "note".into(),
                },
                StreamEvent::ToolCallEnd {
                    id: "call_1".into(),
                    input: json!({"sleep_ms": 20}),
                },
                StreamEvent::ToolCallStart {
                    id: "call_2".into(),
                    name: "write".into(),
                },
                StreamEvent::ToolCallEnd {
                    id: "call_2".into(),
                    input: json!({}),
                },
                StreamEvent::Done(StopReason::ToolUse),
            ],
            text_done("done"),
        ]);
        let tools = FakeTools;
        let mut engine = engine(&provider, &tools, ApprovalMode::ReadOnly, session_in(&dir));

        block_on(engine.user_turn("go".into(), &noop_sink(), &mut ApproveAll)).unwrap();

        let records = records(&engine);
        // The result message, then one tool record per call, in the
        // batch's order.
        let result_index = records
            .iter()
            .position(|r| r["type"] == "message" && r["content"][0]["type"] == "tool_result")
            .unwrap();
        let tools: Vec<&Value> = records.iter().filter(|r| r["type"] == "tool").collect();
        assert_eq!(tools.len(), 2);
        assert_eq!(records[result_index + 1]["type"], "tool");
        assert_eq!(records[result_index + 2]["type"], "tool");

        let ran = tools[0];
        assert_eq!(ran["call_id"], "call_1");
        assert_eq!(ran["name"], "note");
        assert_eq!(ran["is_error"], false);
        assert_eq!(ran["truncated"], false);
        assert_clock(ran);
        // The byte count is the result text the model receives.
        let content = records[result_index]["content"][0]["content"]
            .as_str()
            .unwrap();
        assert_eq!(ran["output_bytes"], content.len() as u64);

        // The run took 20 ms, so the tool clock reads at least that.
        assert!(ran["duration_ms"].as_u64().unwrap() >= 15, "{ran}");

        let refused = tools[1];
        assert_eq!(refused["call_id"], "call_2");
        assert_eq!(refused["name"], "write");
        assert_eq!(refused["is_error"], true);
        assert_clock(refused);
    }

    #[test]
    fn an_aborted_batch_still_records_every_call() {
        let dir = tempfile::tempdir().unwrap();
        // Three execute calls: the user allows the first, aborts at
        // the second, and the third never asks.
        let mut events = Vec::new();
        for index in 1..=3 {
            events.push(StreamEvent::ToolCallStart {
                id: format!("call_{index}"),
                name: "exec".into(),
            });
            events.push(StreamEvent::ToolCallEnd {
                id: format!("call_{index}"),
                input: json!({"n": index}),
            });
        }
        events.push(StreamEvent::Done(StopReason::ToolUse));
        let provider = ScriptedProvider::new(vec![events]);
        let tools = FakeTools;
        let mut engine = engine(&provider, &tools, ApprovalMode::Ask, session_in(&dir));
        let mut approver = ScriptedApprover(VecDeque::from([
            ApprovalAnswer::Allow,
            ApprovalAnswer::Abort,
        ]));

        let outcome = block_on(engine.user_turn("go".into(), &noop_sink(), &mut approver));
        assert_eq!(outcome.unwrap(), TurnOutcome::Aborted);

        // The result message, then three tool records, then the turn
        // record: the invariant holds on an abort too.
        let records = records(&engine);
        let result_index = records
            .iter()
            .position(|r| r["type"] == "message" && r["content"][0]["type"] == "tool_result")
            .unwrap();
        let kinds: Vec<&str> = records[result_index + 1..]
            .iter()
            .map(|r| r["type"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, ["tool", "tool", "tool", "turn"]);
        let tools: Vec<&Value> = records.iter().filter(|r| r["type"] == "tool").collect();
        assert_eq!(tools[0]["call_id"], "call_1");
        assert_eq!(tools[0]["is_error"], false);
        assert_eq!(tools[1]["call_id"], "call_2");
        assert_eq!(tools[1]["is_error"], true);
        // The skipped call has a record with a zero-length run.
        assert_eq!(tools[2]["call_id"], "call_3");
        assert_eq!(tools[2]["is_error"], true);
        assert_eq!(tools[2]["duration_ms"], 0);
        for record in &tools {
            assert_clock(record);
        }
    }

    #[test]
    fn capped_cuts_at_a_char_boundary_and_keeps_short_text() {
        assert_eq!(capped("short", 8), "short");
        // Two-byte chars: a cut inside one steps back to the boundary.
        let text = "ééééé";
        assert_eq!(capped(text, 5), "éé");
        assert_eq!(capped(text, 4), "éé");
        assert_eq!(capped("", 0), "");
    }

    #[test]
    fn a_token_limit_ends_the_turn_as_max_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ScriptedProvider::new(vec![vec![
            StreamEvent::TextDelta("partial".into()),
            StreamEvent::Done(StopReason::MaxTokens),
        ]]);
        let tools = FakeTools;
        let mut engine = engine(&provider, &tools, ApprovalMode::Auto, session_in(&dir));

        let outcome = block_on(engine.user_turn("hi".into(), &noop_sink(), &mut ApproveAll));
        assert_eq!(outcome.unwrap(), TurnOutcome::MaxTokens);
        assert_eq!(turn_records(&engine), ["max_tokens"]);
    }

    #[test]
    fn a_text_turn_lands_in_history_and_the_session_file() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ScriptedProvider::new(vec![vec![
            StreamEvent::TextDelta("he".into()),
            StreamEvent::TextDelta("llo".into()),
            StreamEvent::Done(StopReason::EndTurn),
        ]]);
        let tools = FakeTools;
        let mut engine = engine(&provider, &tools, ApprovalMode::Auto, session_in(&dir));

        let seen = Mutex::new(String::new());
        let sink = |event: EngineEvent<'_>| {
            if let EngineEvent::TextDelta(delta) = event {
                seen.lock().unwrap().push_str(delta);
            }
        };
        let outcome = block_on(engine.user_turn("hi".into(), &sink, &mut ApproveAll)).unwrap();
        assert_eq!(outcome, TurnOutcome::Completed);
        assert_eq!(*seen.lock().unwrap(), "hello");
        assert_eq!(engine.history().len(), 2);
        assert_eq!(
            engine.history()[1].content,
            vec![ContentBlock::Text {
                text: "hello".into()
            }]
        );

        let replayed = replay(engine.session().path()).unwrap();
        assert_eq!(replayed.messages, engine.history());
        assert_eq!(turn_records(&engine), ["completed"]);
    }

    #[test]
    fn a_tool_turn_runs_the_tool_and_feeds_the_result_back() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ScriptedProvider::new(vec![
            tool_call("call_1", "note", json!({"text": "x"})),
            text_done("done"),
        ]);
        let tools = FakeTools;
        let mut engine = engine(&provider, &tools, ApprovalMode::Auto, session_in(&dir));

        let events = Mutex::new(Vec::new());
        let sink = |event: EngineEvent<'_>| match event {
            EngineEvent::ToolRun { id, name, .. } => {
                events.lock().unwrap().push(format!("run {name} {id}"));
            }
            EngineEvent::ToolEnd {
                id, name, is_error, ..
            } => {
                events
                    .lock()
                    .unwrap()
                    .push(format!("end {name} {id} err={is_error}"));
            }
            _ => {}
        };
        let outcome = block_on(engine.user_turn("go".into(), &sink, &mut ApproveAll)).unwrap();
        assert_eq!(outcome, TurnOutcome::Completed);
        assert_eq!(
            *events.lock().unwrap(),
            vec!["run note call_1", "end note call_1 err=false"]
        );

        // user, assistant tool call, user tool result, assistant text.
        assert_eq!(engine.history().len(), 4);
        let ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } = &engine.history()[2].content[0]
        else {
            panic!("expected a tool result");
        };
        assert_eq!(tool_use_id, "call_1");
        assert!(content.contains("note ran"));
        assert!(!is_error);

        // The second request carried the tool result back to the model.
        let requests = provider.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].messages.len(), 3);

        // The approval record names the call id.
        let raw = std::fs::read_to_string(engine.session().path()).unwrap();
        assert!(raw.contains(r#""tool_use_id":"call_1""#));

        let replayed = replay(engine.session().path()).unwrap();
        assert_eq!(replayed.messages, engine.history());
    }

    #[test]
    fn ask_mode_asks_and_a_declined_call_returns_an_error_result() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ScriptedProvider::new(vec![
            tool_call("call_1", "write", json!({"path": "f"})),
            text_done("gave up"),
        ]);
        let tools = FakeTools;
        let mut engine = engine(&provider, &tools, ApprovalMode::Ask, session_in(&dir));

        let sink = noop_sink();
        block_on(engine.user_turn("go".into(), &sink, &mut NoPrompt)).unwrap();

        let ContentBlock::ToolResult {
            content, is_error, ..
        } = &engine.history()[2].content[0]
        else {
            panic!("expected a tool result");
        };
        assert!(is_error);
        assert!(content.contains("no approval prompt"));

        let text = std::fs::read_to_string(engine.session().path()).unwrap();
        assert!(text.contains(r#""type":"approval""#));
        assert!(text.contains(r#""approved":false"#));
    }

    #[test]
    fn read_only_mode_denies_write_tools_without_asking() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ScriptedProvider::new(vec![
            tool_call("call_1", "write", json!({})),
            text_done("ok"),
        ]);
        let tools = FakeTools;
        let mut engine = engine(&provider, &tools, ApprovalMode::ReadOnly, session_in(&dir));

        let sink = noop_sink();
        block_on(engine.user_turn("go".into(), &sink, &mut ApproveAll)).unwrap();

        let ContentBlock::ToolResult {
            content, is_error, ..
        } = &engine.history()[2].content[0]
        else {
            panic!("expected a tool result");
        };
        assert!(is_error);
        assert!(content.contains("refuses the write tool"));
    }

    #[test]
    fn an_unknown_tool_errors_without_an_approval_record() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ScriptedProvider::new(vec![
            tool_call("call_1", "nope", json!({})),
            text_done("ok"),
        ]);
        let tools = FakeTools;
        let mut engine = engine(&provider, &tools, ApprovalMode::ReadOnly, session_in(&dir));

        let sink = noop_sink();
        block_on(engine.user_turn("go".into(), &sink, &mut ApproveAll)).unwrap();

        let ContentBlock::ToolResult {
            content, is_error, ..
        } = &engine.history()[2].content[0]
        else {
            panic!("expected a tool result");
        };
        assert!(is_error);
        assert!(content.contains("unknown tool: nope"));
        // No policy decision exists for a tool the host does not hold,
        // so the audit log carries none.
        let raw = std::fs::read_to_string(engine.session().path()).unwrap();
        assert!(!raw.contains(r#""type":"approval""#));
    }

    #[test]
    fn a_provider_without_tool_calls_gets_no_tool_definitions() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ScriptedProvider::without_tool_calls(vec![text_done("plain")]);
        let tools = FakeTools;
        let mut engine = engine(&provider, &tools, ApprovalMode::Auto, session_in(&dir));

        let sink = noop_sink();
        block_on(engine.user_turn("go".into(), &sink, &mut ApproveAll)).unwrap();
        assert!(provider.requests()[0].tools.is_empty());
    }

    #[test]
    fn reasoning_blocks_round_trip_into_the_next_request() {
        let dir = tempfile::tempdir().unwrap();
        let sealed = SealedPayload::new("scripted", json!({"blob": 1}));
        let provider = ScriptedProvider::new(vec![
            vec![
                StreamEvent::ReasoningDelta("thinking".into()),
                StreamEvent::ReasoningDone {
                    summary: "a plan".into(),
                    sealed: Some(sealed.clone()),
                },
                StreamEvent::ToolCallStart {
                    id: "call_1".into(),
                    name: "note".into(),
                },
                StreamEvent::ToolCallEnd {
                    id: "call_1".into(),
                    input: json!({}),
                },
                StreamEvent::Done(StopReason::ToolUse),
            ],
            text_done("done"),
        ]);
        let tools = FakeTools;
        let mut engine = engine(&provider, &tools, ApprovalMode::Auto, session_in(&dir));

        let seen = std::sync::Mutex::new(Vec::new());
        let sink = |event: EngineEvent<'_>| {
            let name = match event {
                EngineEvent::ReasoningDelta(_) => "delta",
                EngineEvent::ReasoningEnd { .. } => "end",
                EngineEvent::ToolStart { .. } => "tool",
                _ => return,
            };
            seen.lock().unwrap().push(name);
        };
        block_on(engine.user_turn("go".into(), &sink, &mut ApproveAll)).unwrap();
        // The end event closes the segment before the next block starts.
        assert_eq!(seen.into_inner().unwrap(), vec!["delta", "end", "tool"]);

        let assistant = &engine.history()[1];
        assert_eq!(
            assistant.content[0],
            ContentBlock::Reasoning {
                summary: "a plan".into(),
                sealed: Some(sealed.clone()),
            }
        );
        // The reasoning block went back out on the next request.
        let requests = provider.requests();
        assert_eq!(requests[1].messages[1].content[0], assistant.content[0]);
    }

    #[test]
    fn the_call_limit_stops_a_tool_loop() {
        let dir = tempfile::tempdir().unwrap();
        let scripts = (0..3)
            .map(|i| tool_call(&format!("call_{i}"), "note", json!({})))
            .collect();
        let provider = ScriptedProvider::new(scripts);
        let tools = FakeTools;
        let session = session_in(&dir);
        let mut config = EngineConfig::new(route("test-model"));
        config.max_model_calls = 2;
        let mut engine = new_engine(
            &provider,
            &tools,
            ApprovalPolicy::new(ApprovalMode::Auto),
            config,
            session,
        );

        let sink = noop_sink();
        let outcome = block_on(engine.user_turn("go".into(), &sink, &mut ApproveAll)).unwrap();
        assert_eq!(outcome, TurnOutcome::CallLimit);
        assert_eq!(provider.requests().len(), 2);
        assert_eq!(turn_records(&engine), ["call_limit"]);
    }

    #[test]
    fn a_dangling_tool_call_is_repaired_in_the_next_user_message() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ScriptedProvider::new(vec![text_done("recovered")]);
        let tools = FakeTools;
        let history = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "go".into() }],
            },
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "call_cut".into(),
                    name: "note".into(),
                    input: json!({}),
                }],
            },
        ];
        let mut engine = engine_with_history(
            &provider,
            &tools,
            ApprovalPolicy::new(ApprovalMode::Auto),
            EngineConfig::new(route("test-model")),
            session_in(&dir),
            history,
        );

        let sink = noop_sink();
        block_on(engine.user_turn("again".into(), &sink, &mut ApproveAll)).unwrap();

        // One user message: the repair result first, then the text, so
        // roles keep alternating.
        let repair = &engine.history()[2];
        assert_eq!(repair.role, Role::User);
        assert_eq!(repair.content.len(), 2);
        let ContentBlock::ToolResult {
            tool_use_id,
            is_error,
            content,
        } = &repair.content[0]
        else {
            panic!("expected a repair result");
        };
        assert_eq!(tool_use_id, "call_cut");
        assert!(is_error);
        assert!(content.contains("may have run"));
        assert_eq!(
            repair.content[1],
            ContentBlock::Text {
                text: "again".into()
            }
        );
    }

    #[test]
    fn repair_pairs_calls_and_results_positionally() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ScriptedProvider::new(vec![text_done("ok")]);
        let tools = FakeTools;
        // call_1 was answered in turn one; an adapter that restarts its
        // ids reissued call_1 later and the turn was cut.
        let history = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "go".into() }],
            },
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "note".into(),
                    input: json!({}),
                }],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_1".into(),
                    content: "first answer".into(),
                    is_error: false,
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "note".into(),
                    input: json!({"n": 2}),
                }],
            },
        ];
        let mut engine = engine_with_history(
            &provider,
            &tools,
            ApprovalPolicy::new(ApprovalMode::Auto),
            EngineConfig::new(route("test-model")),
            session_in(&dir),
            history,
        );

        let sink = noop_sink();
        block_on(engine.user_turn("again".into(), &sink, &mut ApproveAll)).unwrap();

        // The reissued call_1 got exactly one repair result.
        let repair = &engine.history()[4];
        let repairs: Vec<_> = repair
            .content
            .iter()
            .filter(|block| matches!(block, ContentBlock::ToolResult { .. }))
            .collect();
        assert_eq!(repairs.len(), 1);
    }

    #[test]
    fn allow_for_session_grants_the_tool_and_skips_later_asks() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ScriptedProvider::new(vec![
            tool_call("call_1", "write", json!({"n": 1})),
            tool_call("call_2", "write", json!({"n": 2})),
            text_done("done"),
        ]);
        let tools = FakeTools;
        let mut engine = engine(&provider, &tools, ApprovalMode::Ask, session_in(&dir));

        // One scripted answer: the second call must not ask again.
        let mut approver = ScriptedApprover(VecDeque::from([ApprovalAnswer::AllowForSession]));
        let sink = noop_sink();
        let outcome = block_on(engine.user_turn("go".into(), &sink, &mut approver)).unwrap();
        assert_eq!(outcome, TurnOutcome::Completed);

        // Both calls ran; the second decided Allow from the grant.
        for index in [2, 4] {
            let ContentBlock::ToolResult {
                content, is_error, ..
            } = &engine.history()[index].content[0]
            else {
                panic!("expected a tool result");
            };
            assert!(!is_error, "call at message {index} failed: {content}");
        }
        let raw = std::fs::read_to_string(engine.session().path()).unwrap();
        assert!(raw.contains(r#""decision":"ask_user""#));
        assert!(raw.contains(r#""decision":"allow""#));
    }

    #[test]
    fn an_execute_grant_skips_the_same_command_and_asks_for_a_new_one() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ScriptedProvider::new(vec![
            tool_call("call_1", "exec", json!({"command": "cargo test"})),
            tool_call("call_2", "exec", json!({"command": "cargo test"})),
            tool_call("call_3", "exec", json!({"command": "rm -rf /"})),
            text_done("done"),
        ]);
        let tools = FakeTools;
        let mut engine = engine(&provider, &tools, ApprovalMode::Ask, session_in(&dir));

        // Two scripted answers: the grant covers the repeat of the
        // same command, and the new command asks again.
        let mut approver = ScriptedApprover(VecDeque::from([
            ApprovalAnswer::AllowForSession,
            ApprovalAnswer::Deny,
        ]));
        let sink = noop_sink();
        let outcome = block_on(engine.user_turn("go".into(), &sink, &mut approver)).unwrap();
        assert_eq!(outcome, TurnOutcome::Completed);

        let result = |index: usize| {
            let ContentBlock::ToolResult {
                content, is_error, ..
            } = &engine.history()[index].content[0]
            else {
                panic!("expected a tool result at message {index}");
            };
            (content.clone(), *is_error)
        };
        // The granted command ran twice; the new command was declined.
        assert!(!result(2).1);
        assert!(!result(4).1);
        let (content, is_error) = result(6);
        assert!(is_error, "the new command must not inherit the grant");
        assert!(content.contains("declined"), "{content}");
    }

    #[test]
    fn set_model_records_a_meta_and_later_requests_carry_it() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ScriptedProvider::new(vec![text_done("one"), text_done("two")]);
        let tools = FakeTools;
        let mut engine = engine(&provider, &tools, ApprovalMode::Auto, session_in(&dir));

        let sink = noop_sink();
        block_on(engine.user_turn("first".into(), &sink, &mut ApproveAll)).unwrap();
        engine.set_model("other-model").unwrap();
        block_on(engine.user_turn("second".into(), &sink, &mut ApproveAll)).unwrap();

        let requests = provider.requests();
        assert_eq!(requests[0].model, "test-model");
        assert_eq!(requests[1].model, "other-model");

        // The session file names the model that continues it, under
        // the same project the session opened with.
        let raw = std::fs::read_to_string(engine.session().path()).unwrap();
        assert!(raw.contains("other-model"), "missing the switch meta");
        let replayed = replay(engine.session().path()).unwrap();
        assert_eq!(replayed.messages, engine.history());
        let canonical = dir.path().canonicalize().unwrap();
        assert_eq!(replayed.meta.unwrap().cwd, canonical.display().to_string());
    }

    #[test]
    fn a_named_instance_names_itself_and_its_adapter_in_every_meta() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ScriptedProvider::new(vec![text_done("one")]);
        let tools = FakeTools;
        let mut config = EngineConfig::new(route("test-model"));
        config.route.head.instance = "local".to_string();
        let mut engine = new_engine(
            &provider,
            &tools,
            ApprovalPolicy::new(ApprovalMode::Auto),
            config,
            session_in(&dir),
        );
        engine.set_model("other-model").unwrap();
        engine.set_effort(Effort::Low).unwrap();

        let replayed = replay(engine.session().path()).unwrap();
        let meta = replayed.meta.unwrap();
        assert_eq!(meta.provider, "local");
        assert_eq!(meta.kind.as_deref(), Some("scripted"));
        assert_eq!(meta.model, "other-model");
        assert_eq!(meta.effort.as_deref(), Some("low"));
    }

    /// A provider whose every call fails: after `events` stream, or
    /// before the stream opens when `events` is empty.
    struct Failing {
        events: Vec<StreamEvent>,
        error: fn() -> ProviderError,
        requests: Mutex<Vec<Request>>,
    }

    impl Failing {
        fn new(error: fn() -> ProviderError) -> Self {
            Self {
                events: Vec::new(),
                error,
                requests: Mutex::new(Vec::new()),
            }
        }

        fn after(events: Vec<StreamEvent>, error: fn() -> ProviderError) -> Self {
            Self {
                events,
                ..Self::new(error)
            }
        }

        fn calls(&self) -> usize {
            self.requests.lock().unwrap().len()
        }

        fn requests(&self) -> Vec<Request> {
            self.requests.lock().unwrap().clone()
        }
    }

    /// The scripted adapter's id: a failing instance and a working
    /// one share an adapter, as two keys of one vendor do.
    impl Provider for Failing {
        fn id(&self) -> &'static str {
            "scripted"
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities {
                streaming: true,
                tool_calls: true,
                ..Capabilities::default()
            }
        }

        fn stream(&self, request: Request) -> BoxFuture<'_, Result<EventStream, ProviderError>> {
            self.requests.lock().unwrap().push(request);
            if self.events.is_empty() {
                return Box::pin(std::future::ready(Err((self.error)())));
            }
            let items: VecDeque<_> = self
                .events
                .iter()
                .cloned()
                .map(Ok)
                .chain(std::iter::once(Err((self.error)())))
                .collect();
            Box::pin(std::future::ready(Ok(
                Box::pin(VecStream(items)) as EventStream
            )))
        }
    }

    /// A host over named slots. A slot without a provider fails its
    /// build with an authentication error, as a missing key does.
    struct Chain<'p> {
        slots: Vec<(&'static str, Option<&'p dyn Provider>, Option<Effort>)>,
    }

    impl ProviderHost for Chain<'_> {
        fn provider(&self, instance: &str) -> Result<&dyn Provider, ProviderError> {
            match self.slots.iter().find(|slot| slot.0 == instance) {
                Some((_, Some(provider), _)) => Ok(*provider),
                Some((_, None, _)) => Err(ProviderError::Auth(format!("no key for {instance}"))),
                None => panic!("unknown instance {instance}"),
            }
        }

        fn kind(&self, instance: &str) -> Option<&str> {
            self.slots
                .iter()
                .find(|slot| slot.0 == instance)
                .map(|slot| slot.1.map_or("none", |provider| provider.id()))
        }

        fn effort(&self, instance: &str) -> Option<Effort> {
            self.slots
                .iter()
                .find(|slot| slot.0 == instance)
                .and_then(|slot| slot.2)
        }
    }

    /// The route `a` → `b`, with models `ma` and `mb`.
    fn chain_route() -> Route {
        let mut route = Route::new(Target::new("a", "ma"));
        route.tail.push(Target::new("b", "mb"));
        route
    }

    /// An engine over `host` in `auto` mode on the route `a` → `b`.
    fn chain_engine<'a>(host: &'a Chain<'a>, tools: &'a FakeTools, session: Session) -> Engine<'a> {
        Engine::new(EngineParts::new(
            host,
            tools,
            ApprovalPolicy::new(ApprovalMode::Auto),
            EngineConfig::new(chain_route()),
            session,
        ))
    }

    /// The targets of the `ModelStart` events a sink saw, as
    /// `instance/model`.
    fn model_starts(events: &Mutex<Vec<String>>) -> Vec<String> {
        events.lock().unwrap().clone()
    }

    fn start_recorder(events: &Mutex<Vec<String>>) -> impl EventSink + '_ {
        move |event: EngineEvent<'_>| match event {
            EngineEvent::ModelStart {
                provider,
                model,
                attempt,
            } => {
                events
                    .lock()
                    .unwrap()
                    .push(format!("{provider}/{model}#{attempt}"));
            }
            EngineEvent::TextDelta(_) => events.lock().unwrap().push("text".to_string()),
            _ => {}
        }
    }

    #[test]
    fn a_failure_before_the_first_event_falls_back_to_the_next_target() {
        let failures: [fn() -> ProviderError; 5] = [
            || ProviderError::Auth("bad key".into()),
            || ProviderError::Network("refused".into()),
            || ProviderError::Server("overloaded".into()),
            || ProviderError::RateLimited { retry_after: None },
            || ProviderError::InvalidRequest("unknown model".into()),
        ];
        for failure in failures {
            let dir = tempfile::tempdir().unwrap();
            let head = Failing::new(failure);
            let tail = ScriptedProvider::new(vec![text_done("from b")]);
            let host = Chain {
                slots: vec![("a", Some(&head), None), ("b", Some(&tail), None)],
            };
            let tools = FakeTools;
            let mut engine = chain_engine(&host, &tools, session_in(&dir));
            let events = Mutex::new(Vec::new());
            let outcome =
                block_on(engine.user_turn("hi".into(), &start_recorder(&events), &mut ApproveAll))
                    .unwrap();
            assert_eq!(outcome, TurnOutcome::Completed, "{}", failure());
            assert_eq!(model_starts(&events), ["a/ma#0", "b/mb#1", "text"]);
            assert_eq!(tail.requests()[0].model, "mb");
            // The usage record names the target that answered.
            let usage = records(&engine)
                .into_iter()
                .find(|record| record["type"] == "usage")
                .unwrap();
            assert_eq!(usage["provider"], "b");
            assert_eq!(usage["model"], "mb");
        }
    }

    #[test]
    fn a_failed_build_falls_back_and_the_turn_stays_on_the_answering_target() {
        let dir = tempfile::tempdir().unwrap();
        let tail = ScriptedProvider::new(vec![
            tool_call("call_1", "note", json!({})),
            text_done("done"),
        ]);
        let host = Chain {
            slots: vec![("a", None, None), ("b", Some(&tail), None)],
        };
        let tools = FakeTools;
        let mut engine = chain_engine(&host, &tools, session_in(&dir));
        let events = Mutex::new(Vec::new());
        let outcome =
            block_on(engine.user_turn("hi".into(), &start_recorder(&events), &mut ApproveAll))
                .unwrap();
        assert_eq!(outcome, TurnOutcome::Completed);
        // The turn's second call starts at `b`, the target that
        // answered; the next turn tries the head again.
        assert_eq!(
            model_starts(&events),
            ["a/ma#0", "b/mb#1", "b/mb#0", "text"]
        );
        let events = Mutex::new(Vec::new());
        let tail_again = ScriptedProvider::new(vec![text_done("again")]);
        let host = Chain {
            slots: vec![("a", None, None), ("b", Some(&tail_again), None)],
        };
        let mut engine = chain_engine(&host, &tools, session_in(&dir));
        block_on(engine.user_turn("next".into(), &start_recorder(&events), &mut ApproveAll))
            .unwrap();
        assert_eq!(model_starts(&events), ["a/ma#0", "b/mb#1", "text"]);
    }

    #[test]
    fn a_target_the_call_already_tried_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let head = Failing::new(|| ProviderError::Server("down".into()));
        let tail = ScriptedProvider::new(vec![text_done("from b")]);
        let host = Chain {
            slots: vec![("a", Some(&head), None), ("b", Some(&tail), None)],
        };
        let tools = FakeTools;
        let mut config = EngineConfig::new(chain_route());
        config.route.tail.insert(0, Target::new("a", "ma"));
        let mut engine = Engine::new(EngineParts::new(
            &host,
            &tools,
            ApprovalPolicy::new(ApprovalMode::Auto),
            config,
            session_in(&dir),
        ));
        block_on(engine.user_turn("hi".into(), &noop_sink(), &mut ApproveAll)).unwrap();
        assert_eq!(head.calls(), 1);
    }

    #[test]
    fn a_payload_without_a_model_goes_to_the_head_alone() {
        let dir = tempfile::tempdir().unwrap();
        let head = Failing::new(|| ProviderError::Auth("bad key".into()));
        let tail = ScriptedProvider::new(vec![text_done("from b")]);
        let host = Chain {
            slots: vec![("a", Some(&head), None), ("b", Some(&tail), None)],
        };
        let tools = FakeTools;
        // A payload from a file written before the identity fields.
        let legacy = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Reasoning {
                summary: "old".into(),
                sealed: Some(SealedPayload::new("scripted", json!({"blob": 1}))),
            }],
        };
        let history = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "q".into() }],
            },
            legacy,
        ];
        let mut parts = EngineParts::new(
            &host,
            &tools,
            ApprovalPolicy::new(ApprovalMode::Auto),
            EngineConfig::new(chain_route()),
            session_in(&dir),
        );
        parts.history = history;
        let mut engine = Engine::new(parts);
        block_on(engine.user_turn("hi".into(), &noop_sink(), &mut ApproveAll)).unwrap();
        let sealed = |request: &Request| {
            request.messages[1].content.iter().any(|block| {
                matches!(
                    block,
                    ContentBlock::Reasoning {
                        sealed: Some(_),
                        ..
                    }
                )
            })
        };
        assert!(sealed(&head.requests()[0]));
        assert!(!sealed(&tail.requests()[0]));
    }

    #[test]
    fn no_fallback_after_an_event_reached_the_sink() {
        let dir = tempfile::tempdir().unwrap();
        let head = Failing::after(vec![StreamEvent::TextDelta("partial".into())], || {
            ProviderError::Network("reset".into())
        });
        let tail = ScriptedProvider::new(vec![text_done("never")]);
        let host = Chain {
            slots: vec![("a", Some(&head), None), ("b", Some(&tail), None)],
        };
        let tools = FakeTools;
        let mut engine = chain_engine(&host, &tools, session_in(&dir));
        let events = Mutex::new(Vec::new());
        let error =
            block_on(engine.user_turn("hi".into(), &start_recorder(&events), &mut ApproveAll))
                .unwrap_err();
        assert!(error.to_string().contains("reset"), "{error}");
        assert_eq!(model_starts(&events), ["a/ma#0", "text"]);
        assert!(tail.requests().is_empty());
    }

    #[test]
    fn a_protocol_error_and_the_last_target_end_the_call() {
        let dir = tempfile::tempdir().unwrap();
        let head = Failing::new(|| ProviderError::Protocol("bad frame".into()));
        let tail = ScriptedProvider::new(vec![text_done("never")]);
        let host = Chain {
            slots: vec![("a", Some(&head), None), ("b", Some(&tail), None)],
        };
        let tools = FakeTools;
        let mut engine = chain_engine(&host, &tools, session_in(&dir));
        let error =
            block_on(engine.user_turn("hi".into(), &noop_sink(), &mut ApproveAll)).unwrap_err();
        assert!(error.to_string().contains("bad frame"), "{error}");
        assert!(tail.requests().is_empty());

        // Both targets fail: the call returns the last one's error.
        let dir = tempfile::tempdir().unwrap();
        let head = Failing::new(|| ProviderError::Auth("bad key".into()));
        let tail = Failing::new(|| ProviderError::Server("down".into()));
        let host = Chain {
            slots: vec![("a", Some(&head), None), ("b", Some(&tail), None)],
        };
        let mut engine = chain_engine(&host, &tools, session_in(&dir));
        let error =
            block_on(engine.user_turn("hi".into(), &noop_sink(), &mut ApproveAll)).unwrap_err();
        assert!(error.to_string().contains("down"), "{error}");
        assert_eq!((head.calls(), tail.calls()), (1, 1));
    }

    #[test]
    fn each_attempt_carries_the_effort_of_its_own_target() {
        let dir = tempfile::tempdir().unwrap();
        let head = Failing::new(|| ProviderError::Auth("bad key".into()));
        let tail = ScriptedProvider::new(vec![text_done("one"), text_done("two")]);
        let host = Chain {
            slots: vec![
                ("a", Some(&head), Some(Effort::High)),
                ("b", Some(&tail), Some(Effort::Medium)),
            ],
        };
        let tools = FakeTools;
        let mut engine = chain_engine(&host, &tools, session_in(&dir));
        block_on(engine.user_turn("hi".into(), &noop_sink(), &mut ApproveAll)).unwrap();
        // The session sets no level: each target gets what the host
        // gives its instance.
        assert_eq!(head.requests()[0].effort, Some(Effort::High));
        assert_eq!(tail.requests()[0].effort, Some(Effort::Medium));
        // A session level reaches every target.
        engine.set_effort(Effort::Low).unwrap();
        block_on(engine.user_turn("again".into(), &noop_sink(), &mut ApproveAll)).unwrap();
        assert_eq!(tail.requests()[1].effort, Some(Effort::Low));
        assert_eq!(head.requests()[1].effort, Some(Effort::Low));
        // The second turn of the same engine tried the head again.
        assert_eq!(head.calls(), 2);
    }

    #[test]
    fn set_head_switches_the_instance_and_keeps_the_tail() {
        let dir = tempfile::tempdir().unwrap();
        let head = Failing::new(|| ProviderError::Auth("bad key".into()));
        let tail = ScriptedProvider::new(vec![text_done("one")]);
        let host = Chain {
            slots: vec![
                ("a", Some(&head), None),
                ("b", Some(&tail), Some(Effort::Medium)),
            ],
        };
        let tools = FakeTools;
        let mut engine = chain_engine(&host, &tools, session_in(&dir));
        let before = records(&engine).len();
        assert!(engine.set_head(Target::new("ghost", "m")).is_err());
        assert_eq!(records(&engine).len(), before);
        engine.set_head(Target::new("b", "mb2")).unwrap();
        assert_eq!(engine.route().head, Target::new("b", "mb2"));
        assert_eq!(engine.route().tail, [Target::new("b", "mb")]);
        let meta = replay(engine.session().path()).unwrap().meta.unwrap();
        assert_eq!(meta.provider, "b");
        assert_eq!(meta.kind.as_deref(), Some("scripted"));
        assert_eq!(meta.model, "mb2");
        assert_eq!(meta.effort.as_deref(), Some("medium"));
        block_on(engine.user_turn("hi".into(), &noop_sink(), &mut ApproveAll)).unwrap();
        assert_eq!(tail.requests()[0].model, "mb2");
        assert_eq!(head.calls(), 0);
    }

    #[test]
    fn set_effort_reaches_later_requests_and_records_a_meta() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ScriptedProvider::new(vec![text_done("one"), text_done("two")]);
        let tools = FakeTools;
        let mut engine = engine(&provider, &tools, ApprovalMode::Auto, session_in(&dir));

        let sink = noop_sink();
        block_on(engine.user_turn("first".into(), &sink, &mut ApproveAll)).unwrap();
        engine.set_effort(Effort::High).unwrap();
        block_on(engine.user_turn("second".into(), &sink, &mut ApproveAll)).unwrap();

        let requests = provider.requests();
        assert_eq!(requests[0].effort, None);
        assert_eq!(requests[1].effort, Some(Effort::High));

        // The switch left a meta record naming the effort, and the
        // last meta describes the run that continues.
        let raw = std::fs::read_to_string(engine.session().path()).unwrap();
        let metas: Vec<serde_json::Value> = raw
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .filter(|record: &serde_json::Value| record["type"] == "meta")
            .collect();
        assert_eq!(metas.len(), 2);
        assert!(metas[0].get("effort").is_none());
        assert_eq!(metas[1]["effort"], "high");
        let replayed = replay(engine.session().path()).unwrap();
        assert_eq!(replayed.meta.unwrap().effort.as_deref(), Some("high"));
    }

    /// A host with the read tool alone, the read-only registry's shape.
    struct ReadTools;

    impl ToolHost for ReadTools {
        fn definitions(&self) -> Vec<ToolDefinition> {
            FakeTools.definitions().into_iter().take(1).collect()
        }

        fn access(&self, name: &str) -> Option<ToolAccess> {
            (name == "note").then_some(ToolAccess::ReadOnly)
        }

        fn run_approved(&self, name: &str, input: Value) -> BoxFuture<'_, ToolOutput> {
            FakeTools.run_approved(name, input)
        }
    }

    #[test]
    fn set_approval_swaps_the_policy_and_the_tools_and_records_a_meta() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ScriptedProvider::new(vec![
            tool_call("c1", "write", json!({})),
            text_done("one"),
            text_done("two"),
            tool_call("c2", "write", json!({})),
            text_done("three"),
            tool_call("c3", "exec", json!({"cmd": "x"})),
            text_done("four"),
        ]);
        let full = FakeTools;
        let read = ReadTools;
        let mut engine = engine(&provider, &full, ApprovalMode::Ask, session_in(&dir));
        let sink = noop_sink();

        // Ask mode asks; the grant covers the write tool for the
        // session.
        let mut approver = ScriptedApprover(VecDeque::from([ApprovalAnswer::AllowForSession]));
        block_on(engine.user_turn("first".into(), &sink, &mut approver)).unwrap();
        // Read-only: the next request advertises the read tool alone.
        engine.set_approval(ApprovalMode::ReadOnly, &read).unwrap();
        block_on(engine.user_turn("second".into(), &sink, &mut ApproveAll)).unwrap();
        // Ask again: the full set is back, and the grant carries the
        // write call with no question — an empty script panics when
        // asked.
        engine.set_approval(ApprovalMode::Ask, &full).unwrap();
        let mut none = ScriptedApprover(VecDeque::new());
        block_on(engine.user_turn("third".into(), &sink, &mut none)).unwrap();
        // Auto: an execute call runs with no question either.
        engine.set_approval(ApprovalMode::Auto, &full).unwrap();
        block_on(engine.user_turn("fourth".into(), &sink, &mut none)).unwrap();

        let requests = provider.requests();
        let names = |index: usize| -> Vec<String> {
            requests[index]
                .tools
                .iter()
                .map(|tool| tool.name.clone())
                .collect()
        };
        assert_eq!(names(0), ["note", "write", "exec"]);
        assert_eq!(names(2), ["note"]);
        assert_eq!(names(3), ["note", "write", "exec"]);
        // The tools block is the same bytes after a round trip: the
        // provider's cached prefix holds.
        assert_eq!(requests[0].tools, requests[3].tools);
        assert_eq!(requests[3].tools, requests[5].tools);

        // Each switch left a meta record naming the mode; the last
        // one describes the run that continues.
        let raw = std::fs::read_to_string(engine.session().path()).unwrap();
        let modes: Vec<String> = raw
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .filter(|record| record["type"] == "meta")
            .map(|record| record["approval"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(modes, ["ask", "read-only", "ask", "auto"]);
        let replayed = replay(engine.session().path()).unwrap();
        assert_eq!(replayed.meta.unwrap().approval.as_deref(), Some("auto"));
    }

    #[test]
    fn a_model_switch_keeps_sealed_reasoning_in_the_history() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ScriptedProvider::new(vec![text_done("later"), text_done("back")]);
        let tools = FakeTools;
        let sealed = SealedPayload::new("scripted", json!({"sig": "model-a-only"}))
            .with_identity("scripted", "model-a");
        let reasoning = ContentBlock::Reasoning {
            summary: "thought".into(),
            sealed: Some(sealed),
        };
        let history = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "hi".into() }],
            },
            Message {
                role: Role::Assistant,
                content: vec![reasoning.clone(), ContentBlock::Text { text: "hey".into() }],
            },
        ];
        let mut engine = engine_with_history(
            &provider,
            &tools,
            ApprovalPolicy::new(ApprovalMode::Auto),
            EngineConfig::new(route("model-a")),
            session_in(&dir),
            history,
        );

        // The switch keeps the payload; the adapter, not the engine,
        // keeps it off another model's wire.
        engine.set_model("model-b").unwrap();
        assert_eq!(engine.route().head.model, "model-b");
        let sink = noop_sink();
        block_on(engine.user_turn("next".into(), &sink, &mut ApproveAll)).unwrap();
        // A switch back finds the payload where it was.
        engine.set_model("model-a").unwrap();
        block_on(engine.user_turn("again".into(), &sink, &mut ApproveAll)).unwrap();

        // The request to the other model leaves the payload out; the
        // request after the switch back carries it again.
        let requests = provider.requests();
        assert_eq!(requests[0].model, "model-b");
        assert_eq!(
            requests[0].messages[1].content[0],
            ContentBlock::Reasoning {
                summary: "thought".into(),
                sealed: None,
            }
        );
        assert_eq!(requests[1].model, "model-a");
        assert_eq!(requests[1].messages[1].content[0], reasoning);
        assert_eq!(engine.history()[1].content[0], reasoning);
    }

    #[test]
    fn a_model_switch_drops_payloads_without_a_model_identity() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ScriptedProvider::new(Vec::new());
        let tools = FakeTools;
        // A payload from a file written before the identity fields.
        let legacy = ContentBlock::Reasoning {
            summary: "thought".into(),
            sealed: Some(SealedPayload::new("scripted", json!({"sig": "old"}))),
        };
        let history = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "hi".into() }],
            },
            Message {
                role: Role::Assistant,
                content: vec![legacy.clone()],
            },
        ];
        let mut engine = engine_with_history(
            &provider,
            &tools,
            ApprovalPolicy::new(ApprovalMode::Auto),
            EngineConfig::new(route("model-a")),
            session_in(&dir),
            history,
        );
        // The same name is no switch: the payload stays.
        engine.set_model("model-a").unwrap();
        assert_eq!(engine.history()[1].content[0], legacy);
        // A real switch drops it; the summary stays readable.
        engine.set_model("model-b").unwrap();
        assert_eq!(
            engine.history()[1].content[0],
            ContentBlock::Reasoning {
                summary: "thought".into(),
                sealed: None,
            }
        );
    }

    #[test]
    fn an_abort_answer_stops_the_turn_and_answers_every_call() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ScriptedProvider::new(vec![vec![
            StreamEvent::ToolCallStart {
                id: "call_1".into(),
                name: "write".into(),
            },
            StreamEvent::ToolCallEnd {
                id: "call_1".into(),
                input: json!({"n": 1}),
            },
            StreamEvent::ToolCallStart {
                id: "call_2".into(),
                name: "write".into(),
            },
            StreamEvent::ToolCallEnd {
                id: "call_2".into(),
                input: json!({"n": 2}),
            },
            StreamEvent::Done(StopReason::ToolUse),
        ]]);
        let tools = FakeTools;
        let mut engine = engine(&provider, &tools, ApprovalMode::Ask, session_in(&dir));

        let mut approver = ScriptedApprover(VecDeque::from([ApprovalAnswer::Abort]));
        let sink = noop_sink();
        let outcome = block_on(engine.user_turn("go".into(), &sink, &mut approver)).unwrap();
        assert_eq!(outcome, TurnOutcome::Aborted);

        // One model call only; both calls got error results.
        assert_eq!(provider.requests().len(), 1);
        let results = &engine.history()[2].content;
        assert_eq!(results.len(), 2);
        for block in results {
            let ContentBlock::ToolResult {
                content, is_error, ..
            } = block
            else {
                panic!("expected a tool result");
            };
            assert!(is_error);
            assert!(content.contains("stopped the turn"));
        }
        // The session replays to the same well-formed history.
        let replayed = replay(engine.session().path()).unwrap();
        assert_eq!(replayed.messages, engine.history());
        assert_eq!(turn_records(&engine), ["aborted"]);
    }

    #[test]
    fn a_tool_stop_without_calls_is_a_protocol_error() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ScriptedProvider::new(vec![vec![
            StreamEvent::TextDelta("hm".into()),
            StreamEvent::Done(StopReason::ToolUse),
        ]]);
        let tools = FakeTools;
        let mut engine = engine(&provider, &tools, ApprovalMode::Auto, session_in(&dir));

        let sink = noop_sink();
        let error = block_on(engine.user_turn("go".into(), &sink, &mut ApproveAll)).unwrap_err();
        assert!(error.to_string().contains("without a tool call"));
        // The failed turn still closes in the file, with the error's
        // own message.
        assert_eq!(turn_records(&engine), [format!("error:{error}")]);
    }

    #[test]
    fn a_tool_call_end_without_a_start_is_a_protocol_error() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ScriptedProvider::new(vec![vec![
            StreamEvent::ToolCallEnd {
                id: "ghost".into(),
                input: json!({}),
            },
            StreamEvent::Done(StopReason::ToolUse),
        ]]);
        let tools = FakeTools;
        let mut engine = engine(&provider, &tools, ApprovalMode::Auto, session_in(&dir));

        let sink = noop_sink();
        let error = block_on(engine.user_turn("go".into(), &sink, &mut ApproveAll)).unwrap_err();
        assert!(error.to_string().contains("without a start event"));
        // The history holds only the user message; nothing partial.
        assert_eq!(engine.history().len(), 1);
    }

    #[test]
    fn the_prompt_prefix_is_stable_across_turns() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ScriptedProvider::new(vec![
            tool_call("call_1", "write", json!({"path": "f"})),
            text_done("first answer"),
            text_done("second answer"),
        ]);
        let tools = FakeTools;
        // Ask mode, with an approval happening mid-turn: the approval
        // must not disturb the prompt.
        let mut engine = engine(&provider, &tools, ApprovalMode::Ask, session_in(&dir));

        let sink = noop_sink();
        block_on(engine.user_turn("one".into(), &sink, &mut ApproveAll)).unwrap();
        block_on(engine.user_turn("two".into(), &sink, &mut ApproveAll)).unwrap();

        let requests = provider.requests();
        assert_eq!(requests.len(), 3);
        for pair in requests.windows(2) {
            assert_eq!(pair[0].system_prompt, pair[1].system_prompt);
            assert_eq!(pair[0].tools, pair[1].tools);
            let prefix = &pair[1].messages[..pair[0].messages.len()];
            assert_eq!(prefix, &pair[0].messages[..]);
            // Byte-identical, not only structurally equal.
            assert_eq!(
                serde_json::to_string(prefix).unwrap(),
                serde_json::to_string(&pair[0].messages).unwrap()
            );
        }
        // Approval state never enters a request.
        for request in &requests {
            let text = serde_json::to_string(&request.messages).unwrap();
            assert!(!text.contains("approval"));
        }
    }
}
