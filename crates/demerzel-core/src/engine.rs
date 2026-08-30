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

use std::collections::HashMap;
use std::future::poll_fn;
use std::io;

use serde_json::Value;

use crate::approval::{ApprovalDecision, ApprovalMode, ApprovalPolicy, ApprovalRequest};
use crate::message::{ContentBlock, Message, Role, StopReason, Usage};
use crate::provider::{Capabilities, Effort, Provider, ProviderError, Request, StreamEvent};
use crate::session::{Session, SessionMeta};
use crate::tool::{BoxFuture, ToolDefinition, ToolHost, ToolOutput};

/// The fixed base system prompt for M1. Config replaces it in M3.
pub const DEFAULT_SYSTEM_PROMPT: &str = "You are Demerzel, a terminal coding agent. \
You work inside the user's project directory with the tools the harness provides. \
Read a file before you edit it. Keep changes small and report what you did.";

/// Per-session loop settings.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub model: String,
    pub system_prompt: String,
    /// Model calls allowed for one user turn before the loop stops.
    pub max_model_calls: u32,
    pub max_output_tokens: Option<u64>,
    /// The reasoning effort every request carries; `None` leaves the
    /// provider's default.
    pub effort: Option<Effort>,
}

impl EngineConfig {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            system_prompt: DEFAULT_SYSTEM_PROMPT.to_string(),
            max_model_calls: 128,
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

/// The turn engine for one session.
pub struct Engine<'a> {
    provider: &'a dyn Provider,
    tools: &'a dyn ToolHost,
    policy: ApprovalPolicy,
    config: EngineConfig,
    session: Session,
    history: Vec<Message>,
    /// Snapshot taken at construction and at every approval switch:
    /// the tool set holds between switches, and the snapshot keeps
    /// the request tools block byte-identical across model calls.
    tool_definitions: Vec<ToolDefinition>,
    capabilities: Capabilities,
}

impl<'a> Engine<'a> {
    /// An engine over a fresh session.
    pub fn new(
        provider: &'a dyn Provider,
        tools: &'a dyn ToolHost,
        policy: ApprovalPolicy,
        config: EngineConfig,
        session: Session,
    ) -> Self {
        Self::with_history(provider, tools, policy, config, session, Vec::new())
    }

    /// An engine over a resumed session with its replayed history.
    pub fn with_history(
        provider: &'a dyn Provider,
        tools: &'a dyn ToolHost,
        policy: ApprovalPolicy,
        config: EngineConfig,
        session: Session,
        history: Vec<Message>,
    ) -> Self {
        Self {
            tool_definitions: tools.definitions(),
            capabilities: provider.capabilities(),
            provider,
            tools,
            policy,
            config,
            session,
            history,
        }
    }

    pub fn history(&self) -> &[Message] {
        &self.history
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Switch the model for the turns that follow. The switch appends
    /// a meta record, so the session file names the model that
    /// continues it — an audit trail; resume still takes its model
    /// from config. A real switch also drops the sealed reasoning
    /// payloads from the in-memory history: a sealed payload is bound
    /// to the model that produced it, and resending it to another
    /// model fails every later turn. The readable summaries stay.
    pub fn set_model(&mut self, model: &str) -> Result<(), EngineError> {
        self.session.append_meta(&SessionMeta::now(
            self.session.cwd(),
            self.provider.id(),
            model,
            self.config.effort,
            self.policy.mode,
        ))?;
        if model != self.config.model {
            for message in &mut self.history {
                for block in &mut message.content {
                    if let ContentBlock::Reasoning { sealed, .. } = block {
                        *sealed = None;
                    }
                }
            }
        }
        self.config.model = model.to_string();
        Ok(())
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
        let result = self.run_turn(text, sink, approver).await;
        match &result {
            Ok(outcome) => self.session.append_turn(outcome.as_str(), None)?,
            Err(EngineError::Provider(error)) => {
                let message = error.to_string();
                let _ = self
                    .session
                    .append_turn("error", Some(capped(&message, MAX_TURN_ERROR_BYTES)));
            }
            Err(EngineError::Session(_)) => {}
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
        loop {
            if calls >= self.config.max_model_calls {
                return Ok(TurnOutcome::CallLimit);
            }
            calls += 1;

            let request = self.assemble();
            let stream = self.provider.stream(request).await?;
            let ModelResponse {
                message,
                stop,
                usage,
            } = collect_response(stream, sink).await?;

            if !message.content.is_empty() {
                self.push(message.clone())?;
            }
            if let Some(usage) = usage {
                self.session.append_usage(&usage)?;
                sink.event(EngineEvent::ModelUsage(usage));
            }

            match stop {
                StopReason::ToolUse => {
                    let (results, aborted) = self.run_tools(&message, sink, approver).await?;
                    if results.is_empty() {
                        return Err(EngineError::Provider(ProviderError::Protocol(
                            "the model stopped for tool use without a tool call".to_string(),
                        )));
                    }
                    self.push(Message {
                        role: Role::User,
                        content: results,
                    })?;
                    if aborted {
                        return Ok(TurnOutcome::Aborted);
                    }
                }
                StopReason::EndTurn | StopReason::Aborted => return Ok(TurnOutcome::Completed),
                StopReason::MaxTokens => return Ok(TurnOutcome::MaxTokens),
                StopReason::Refusal => return Ok(TurnOutcome::Refused),
            }
        }
    }

    /// Fixed prompt assembly: system prompt, then history. Nothing
    /// volatile enters the request. Tools go along only when the
    /// provider declares the capability.
    fn assemble(&self) -> Request {
        let mut request = Request::new(self.config.model.clone());
        request.system_prompt = self.config.system_prompt.clone();
        request.messages = self.history.clone();
        if self.capabilities.tool_calls {
            request.tools = self.tool_definitions.clone();
        }
        request.max_output_tokens = self.config.max_output_tokens;
        request.effort = self.config.effort;
        request
    }

    /// Set the reasoning effort for the requests that follow. A fresh
    /// meta record names it, the way a model switch leaves its trail.
    pub fn set_effort(&mut self, effort: Effort) -> Result<(), EngineError> {
        // The record first, the switch second: a failed write leaves
        // the engine where the interface still believes it is.
        self.session.append_meta(&SessionMeta::now(
            self.session.cwd(),
            self.provider.id(),
            &self.config.model,
            Some(effort),
            self.policy.mode,
        ))?;
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
        self.session.append_meta(&SessionMeta::now(
            self.session.cwd(),
            self.provider.id(),
            &self.config.model,
            self.config.effort,
            mode,
        ))?;
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

    /// Append one message to the session file and the history, in that
    /// order, so the file never lags the prompt.
    fn push(&mut self, message: Message) -> Result<(), EngineError> {
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
    /// Returns the result blocks and whether the user stopped the
    /// turn. On an abort, the refused call and every later call get
    /// error results, so nothing dangles.
    async fn run_tools(
        &mut self,
        message: &Message,
        sink: &dyn EventSink,
        approver: &mut dyn Approver,
    ) -> Result<(Vec<ContentBlock>, bool), EngineError> {
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

        let mut results = Vec::with_capacity(calls.len());
        let mut aborted = false;
        for (id, name, input) in calls {
            if aborted {
                results.push(ContentBlock::ToolResult {
                    tool_use_id: id,
                    content: format!("the user stopped the turn; the {name} call did not run"),
                    is_error: true,
                });
                continue;
            }
            sink.event(EngineEvent::ToolRun {
                id: &id,
                name: &name,
                input: &input,
            });
            let output = match self.tools.access(&name) {
                None => self.tools.run_approved(&name, input).await,
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
                        self.tools.run_approved(&name, input).await
                    } else if decision == ApprovalDecision::Deny {
                        ToolOutput::error(format!(
                            "the approval mode refuses the {name} tool in this session"
                        ))
                    } else if answer == ApprovalAnswer::Abort {
                        aborted = true;
                        ToolOutput::error(format!(
                            "the user stopped the turn; the {name} call did not run"
                        ))
                    } else {
                        ToolOutput::error(approver.deny_message(&name))
                    }
                }
            };
            sink.event(EngineEvent::ToolEnd {
                id: &id,
                name: &name,
                summary: output.summary.as_deref(),
                is_error: output.is_error,
                output: &output.content,
            });
            results.push(ContentBlock::ToolResult {
                tool_use_id: id,
                content: output.content,
                is_error: output.is_error,
            });
        }
        Ok((results, aborted))
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

/// Drain one response stream into an assistant message, preserving the
/// block order the model produced.
async fn collect_response(
    mut stream: crate::provider::EventStream,
    sink: &dyn EventSink,
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
                sink.event(EngineEvent::TextDelta(&delta));
                if let Some(ContentBlock::Text { text }) = blocks.last_mut() {
                    text.push_str(&delta);
                } else {
                    blocks.push(ContentBlock::Text { text: delta });
                }
            }
            StreamEvent::ReasoningDelta(delta) => {
                sink.event(EngineEvent::ReasoningDelta(&delta));
            }
            StreamEvent::ReasoningDone { summary, sealed } => {
                sink.event(EngineEvent::ReasoningEnd {});
                blocks.push(ContentBlock::Reasoning { summary, sealed });
            }
            StreamEvent::ToolCallStart { id, name } => {
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
    use crate::provider::EventStream;
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
            }
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

    fn session_in(dir: &tempfile::TempDir) -> Session {
        SessionStore::at(dir.path())
            .create(
                dir.path(),
                "scripted",
                "test-model",
                None,
                ApprovalMode::Ask,
            )
            .unwrap()
    }

    fn engine<'a>(
        provider: &'a ScriptedProvider,
        tools: &'a FakeTools,
        mode: ApprovalMode,
        session: Session,
    ) -> Engine<'a> {
        Engine::new(
            provider,
            tools,
            ApprovalPolicy::new(mode),
            EngineConfig::new("test-model"),
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
        let sealed = SealedPayload {
            provider: "scripted".into(),
            data: json!({"blob": 1}),
        };
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
        let mut config = EngineConfig::new("test-model");
        config.max_model_calls = 2;
        let mut engine = Engine::new(
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
        let mut engine = Engine::with_history(
            &provider,
            &tools,
            ApprovalPolicy::new(ApprovalMode::Auto),
            EngineConfig::new("test-model"),
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
        let mut engine = Engine::with_history(
            &provider,
            &tools,
            ApprovalPolicy::new(ApprovalMode::Auto),
            EngineConfig::new("test-model"),
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
    fn a_model_switch_drops_sealed_reasoning_from_the_history() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ScriptedProvider::new(vec![text_done("later")]);
        let tools = FakeTools;
        let history = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "hi".into() }],
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Reasoning {
                        summary: "thought".into(),
                        sealed: Some(SealedPayload {
                            provider: "scripted".into(),
                            data: json!({"sig": "model-a-only"}),
                        }),
                    },
                    ContentBlock::Text { text: "hey".into() },
                ],
            },
        ];
        let mut engine = Engine::with_history(
            &provider,
            &tools,
            ApprovalPolicy::new(ApprovalMode::Auto),
            EngineConfig::new("model-a"),
            session_in(&dir),
            history,
        );

        // A switch to the same name keeps the payloads.
        engine.set_model("model-a").unwrap();
        assert!(matches!(
            &engine.history()[1].content[0],
            ContentBlock::Reasoning {
                sealed: Some(_),
                ..
            }
        ));

        // A real switch drops them; the summary stays readable.
        engine.set_model("model-b").unwrap();
        let ContentBlock::Reasoning { summary, sealed } = &engine.history()[1].content[0] else {
            panic!("expected the reasoning block");
        };
        assert_eq!(summary, "thought");
        assert!(sealed.is_none(), "a sealed payload crossed a model switch");
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
