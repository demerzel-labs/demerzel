//! The agent loop: one user turn in, model calls and tool calls until
//! the model ends the turn.
//!
//! The engine owns the in-memory history and keeps it in lockstep with
//! the session file: every message appends to both before the next
//! model call. Prompt assembly is fixed — system prompt, then history —
//! and volatile state (approvals, display) never enters it, so the
//! prompt prefix stays byte-stable across turns.
//!
//! Cancellation contract: dropping the future returned by
//! [`Engine::user_turn`] aborts the turn. Only complete records reach
//! the session file, so an aborted session file stays valid. A tool
//! call that was recorded but never answered is repaired at the start
//! of the next turn with an error result, so a resumed history is
//! always well-formed.

use std::collections::HashMap;
use std::future::poll_fn;
use std::io;

use serde_json::Value;

use crate::approval::{ApprovalDecision, ApprovalPolicy};
use crate::message::{ContentBlock, Message, Role, StopReason, Usage};
use crate::provider::Request;
use crate::provider::{Provider, ProviderError, StreamEvent};
use crate::session::{Session, SessionRecord};
use crate::tool::{BoxFuture, ToolHost, ToolOutput};

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
}

impl EngineConfig {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            system_prompt: DEFAULT_SYSTEM_PROMPT.to_string(),
            max_model_calls: 32,
            max_output_tokens: None,
        }
    }
}

/// One display event. The renderer draws these; none of them feed back
/// into the prompt.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent<'a> {
    TextDelta(&'a str),
    ReasoningDelta(&'a str),
    /// The model started a tool call.
    ToolStart {
        id: &'a str,
        name: &'a str,
    },
    /// A tool call finished, with the tool's own one-line summary.
    ToolEnd {
        name: &'a str,
        summary: Option<&'a str>,
        is_error: bool,
    },
    /// Usage for one model call.
    ModelUsage(Usage),
}

/// Receives display events during a turn.
pub trait EventSink {
    fn event(&mut self, event: EngineEvent<'_>);
}

impl<F> EventSink for F
where
    F: FnMut(EngineEvent<'_>),
{
    fn event(&mut self, event: EngineEvent<'_>) {
        self(event)
    }
}

/// Answers the approval question for one `AskUser` tool call.
pub trait Approver: Send {
    /// Return true to run the call.
    fn approve<'a>(&'a mut self, tool: &'a str, input: &'a Value) -> BoxFuture<'a, bool>;

    /// The model-facing message for a declined call.
    fn deny_message(&self, tool: &str) -> String {
        format!("the user declined the {tool} call")
    }
}

/// Non-interactive approver: every ask comes back declined. One-shot
/// runs use it; the REPL replaces it with a real prompt.
pub struct NoPrompt;

impl Approver for NoPrompt {
    fn approve<'a>(&'a mut self, _tool: &'a str, _input: &'a Value) -> BoxFuture<'a, bool> {
        Box::pin(std::future::ready(false))
    }

    fn deny_message(&self, tool: &str) -> String {
        format!("the {tool} tool needs user approval and this run has no approval prompt")
    }
}

/// How a user turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnOutcome {
    /// The model ended its turn.
    Completed,
    /// The model hit the output-token limit.
    MaxTokens,
    /// The loop hit `max_model_calls` for this user turn.
    CallLimit,
}

#[derive(Debug, thiserror::Error)]
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

    /// Run one user turn: repair the history if the last turn was cut,
    /// append the user message, then alternate model calls and tool
    /// calls until the model ends the turn or a limit stops it.
    pub async fn user_turn(
        &mut self,
        text: String,
        sink: &mut dyn EventSink,
        approver: &mut dyn Approver,
    ) -> Result<TurnOutcome, EngineError> {
        self.repair_dangling_calls()?;
        self.push(Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text }],
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
                self.session.append(&SessionRecord::Usage(usage))?;
                sink.event(EngineEvent::ModelUsage(usage));
            }

            match stop {
                StopReason::ToolUse => {
                    let results = self.run_tools(&message, sink, approver).await?;
                    if results.is_empty() {
                        return Err(EngineError::Provider(ProviderError::Protocol(
                            "the model stopped for tool use without a tool call".to_string(),
                        )));
                    }
                    self.push(Message {
                        role: Role::User,
                        content: results,
                    })?;
                }
                StopReason::EndTurn | StopReason::Aborted => return Ok(TurnOutcome::Completed),
                StopReason::MaxTokens => return Ok(TurnOutcome::MaxTokens),
            }
        }
    }

    /// Fixed prompt assembly: system prompt, then history. Nothing
    /// volatile enters the request.
    fn assemble(&self) -> Request {
        Request {
            model: self.config.model.clone(),
            system_prompt: self.config.system_prompt.clone(),
            messages: self.history.clone(),
            tools: self.tools.definitions(),
            max_output_tokens: self.config.max_output_tokens,
        }
    }

    /// Append one message to the session file and the history, in that
    /// order, so the file never lags the prompt.
    fn push(&mut self, message: Message) -> Result<(), EngineError> {
        self.session
            .append(&SessionRecord::Message(message.clone()))?;
        self.history.push(message);
        Ok(())
    }

    /// Answer tool calls that a cut-off turn left unanswered, so the
    /// next request never carries a dangling call. The repair appends a
    /// normal message record; the prefix stays stable afterward.
    fn repair_dangling_calls(&mut self) -> Result<(), EngineError> {
        let mut answered = std::collections::HashSet::new();
        for message in &self.history {
            for block in &message.content {
                if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                    answered.insert(tool_use_id.clone());
                }
            }
        }
        let mut repairs = Vec::new();
        for message in &self.history {
            for block in &message.content {
                if let ContentBlock::ToolUse { id, .. } = block
                    && !answered.contains(id)
                {
                    repairs.push(ContentBlock::ToolResult {
                        tool_use_id: id.clone(),
                        content: "interrupted: the session ended before this call ran".to_string(),
                        is_error: true,
                    });
                }
            }
        }
        if !repairs.is_empty() {
            self.push(Message {
                role: Role::User,
                content: repairs,
            })?;
        }
        Ok(())
    }

    /// Run every tool call in the message, in order, through the
    /// approval policy. Each decision goes into the session record.
    async fn run_tools(
        &mut self,
        message: &Message,
        sink: &mut dyn EventSink,
        approver: &mut dyn Approver,
    ) -> Result<Vec<ContentBlock>, EngineError> {
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
        for (id, name, input) in calls {
            let decision = self.tools.decide(&self.policy, &name, &input);
            let approved = match decision {
                ApprovalDecision::Allow => true,
                ApprovalDecision::Deny => false,
                ApprovalDecision::AskUser => approver.approve(&name, &input).await,
            };
            self.session.append(&SessionRecord::Approval {
                tool: name.clone(),
                mode: self.policy.mode,
                decision,
                approved: matches!(decision, ApprovalDecision::AskUser).then_some(approved),
            })?;

            let output = if approved {
                self.tools.run_approved(&name, input).await
            } else if decision == ApprovalDecision::Deny {
                ToolOutput::error(format!(
                    "the approval mode refuses the {name} tool in this session"
                ))
            } else {
                ToolOutput::error(approver.deny_message(&name))
            };
            sink.event(EngineEvent::ToolEnd {
                name: &name,
                summary: output.summary.as_deref(),
                is_error: output.is_error,
            });
            results.push(ContentBlock::ToolResult {
                tool_use_id: id,
                content: output.content,
                is_error: output.is_error,
            });
        }
        Ok(results)
    }
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
    sink: &mut dyn EventSink,
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
                let name = call_names.get(&id).cloned().unwrap_or_default();
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
    use crate::session::{SessionStore, replay};
    use crate::tool::ToolDefinition;
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
    }

    impl ScriptedProvider {
        fn new(scripts: Vec<Vec<StreamEvent>>) -> Self {
            Self {
                scripts: Mutex::new(scripts.into()),
                requests: Mutex::new(Vec::new()),
            }
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
            Capabilities {
                streaming: true,
                tool_calls: true,
                ..Capabilities::default()
            }
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

    /// Two tools: `note` is read-only, `write` writes.
    struct FakeTools;

    impl ToolHost for FakeTools {
        fn definitions(&self) -> Vec<ToolDefinition> {
            ["note", "write"]
                .into_iter()
                .map(|name| ToolDefinition {
                    name: name.to_string(),
                    description: format!("fake {name}"),
                    input_schema: json!({"type": "object"}),
                })
                .collect()
        }

        fn decide(&self, policy: &ApprovalPolicy, name: &str, input: &Value) -> ApprovalDecision {
            let access = if name == "write" {
                ToolAccess::Write
            } else {
                ToolAccess::ReadOnly
            };
            policy.decide(&crate::approval::ApprovalRequest {
                tool: name,
                access,
                input,
            })
        }

        fn run_approved(&self, name: &str, input: Value) -> BoxFuture<'_, ToolOutput> {
            let output = ToolOutput::text(format!("{name} ran: {input}"))
                .with_summary(format!("{name}: ok"));
            Box::pin(std::future::ready(output))
        }
    }

    struct ApproveAll;

    impl Approver for ApproveAll {
        fn approve<'a>(&'a mut self, _tool: &'a str, _input: &'a Value) -> BoxFuture<'a, bool> {
            Box::pin(std::future::ready(true))
        }
    }

    fn session_in(dir: &tempfile::TempDir) -> Session {
        SessionStore::at(dir.path())
            .create(dir.path(), "scripted", "test-model")
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
            }),
            StreamEvent::Done(StopReason::EndTurn),
        ]
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

        let mut seen = String::new();
        let mut sink = |event: EngineEvent<'_>| {
            if let EngineEvent::TextDelta(delta) = event {
                seen.push_str(delta);
            }
        };
        let outcome = block_on(engine.user_turn("hi".into(), &mut sink, &mut ApproveAll)).unwrap();
        assert_eq!(outcome, TurnOutcome::Completed);
        assert_eq!(seen, "hello");
        assert_eq!(engine.history().len(), 2);
        assert_eq!(
            engine.history()[1].content,
            vec![ContentBlock::Text {
                text: "hello".into()
            }]
        );

        let replayed = replay(engine.session().path()).unwrap();
        assert_eq!(replayed.messages, engine.history());
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

        let mut events = Vec::new();
        let mut sink = |event: EngineEvent<'_>| {
            if let EngineEvent::ToolEnd { name, is_error, .. } = event {
                events.push((name.to_string(), is_error));
            }
        };
        let outcome = block_on(engine.user_turn("go".into(), &mut sink, &mut ApproveAll)).unwrap();
        assert_eq!(outcome, TurnOutcome::Completed);
        assert_eq!(events, vec![("note".to_string(), false)]);

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

        let mut sink = |_: EngineEvent<'_>| {};
        block_on(engine.user_turn("go".into(), &mut sink, &mut NoPrompt)).unwrap();

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

        let mut sink = |_: EngineEvent<'_>| {};
        block_on(engine.user_turn("go".into(), &mut sink, &mut ApproveAll)).unwrap();

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

        let mut sink = |_: EngineEvent<'_>| {};
        block_on(engine.user_turn("go".into(), &mut sink, &mut ApproveAll)).unwrap();

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

        let mut sink = |_: EngineEvent<'_>| {};
        let outcome = block_on(engine.user_turn("go".into(), &mut sink, &mut ApproveAll)).unwrap();
        assert_eq!(outcome, TurnOutcome::CallLimit);
        assert_eq!(provider.requests().len(), 2);
    }

    #[test]
    fn a_dangling_tool_call_is_repaired_before_the_next_turn() {
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

        let mut sink = |_: EngineEvent<'_>| {};
        block_on(engine.user_turn("again".into(), &mut sink, &mut ApproveAll)).unwrap();

        // Repair message first, then the new user text.
        let repair = &engine.history()[2];
        assert_eq!(repair.role, Role::User);
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
        assert!(content.contains("interrupted"));
        assert_eq!(
            engine.history()[3].content,
            vec![ContentBlock::Text {
                text: "again".into()
            }]
        );
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

        let mut sink = |_: EngineEvent<'_>| {};
        let error =
            block_on(engine.user_turn("go".into(), &mut sink, &mut ApproveAll)).unwrap_err();
        assert!(error.to_string().contains("without a tool call"));
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

        let mut sink = |_: EngineEvent<'_>| {};
        block_on(engine.user_turn("one".into(), &mut sink, &mut ApproveAll)).unwrap();
        block_on(engine.user_turn("two".into(), &mut sink, &mut ApproveAll)).unwrap();

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
