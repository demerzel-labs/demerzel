//! The `openai-codex` adapter: the Responses API against the
//! ChatGPT-subscription Codex endpoint.
//!
//! Contract reference: the open-source Codex CLI client (`codex-rs`).
//! The backend is not a public API, so the parser ignores unknown
//! events and fields instead of failing on them.

use std::collections::HashMap;
use std::time::Duration;

use demerzel_core::{ContentBlock, Role, SealedPayload, StopReason, Usage};
use serde_json::{Value, json};

use crate::auth::AuthStore;
use crate::retry::{backoff, retry_after, retry_delay};
use crate::sse::{PayloadMapper, spawn_event_stream};
use crate::{BoxFuture, Capabilities, EventStream, Provider, ProviderError, Request, StreamEvent};

/// Stable adapter id. Sealed payloads carry this id.
pub const PROVIDER_ID: &str = "openai-codex";

/// Default Codex backend base URL.
pub const DEFAULT_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

/// Default model until per-role model routing arrives (M3).
pub const DEFAULT_MODEL: &str = "gpt-5.6-sol";

/// The context windows of the models this adapter knows, in tokens.
/// The numbers are the standard-session budgets from the Codex
/// client's model catalog (2026-08). The model card advertises a
/// larger window for `gpt-5.6-sol`; the endpoint grants it only to
/// sessions that opt in, which this adapter does not send. The
/// `context_window` config key overrides a stale row.
pub(crate) const CONTEXT_WINDOWS: &[(&str, u64)] = &[("gpt-5.6-sol", 272_000)];

#[derive(Debug, Clone)]
pub struct CodexConfig {
    pub base_url: String,
    /// `originator` header value. The backend expects a known client
    /// name; the Codex CLI default keeps requests accepted.
    pub originator: String,
    /// Total attempts for retryable failures (429, 5xx, network).
    pub max_attempts: u32,
    pub base_delay: Duration,
    /// `reasoning` request field, sent as-is when set. A request that
    /// carries an effort adds `effort` to it, or sends `reasoning`
    /// with the effort alone when this is unset or not an object.
    pub reasoning: Option<Value>,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            originator: "codex_cli_rs".to_string(),
            max_attempts: 4,
            base_delay: Duration::from_millis(500),
            reasoning: Some(json!({"summary": "auto"})),
        }
    }
}

pub struct OpenAiCodex {
    config: CodexConfig,
    http: reqwest::Client,
    auth: tokio::sync::Mutex<AuthStore>,
}

impl OpenAiCodex {
    pub fn new(config: CodexConfig, auth: AuthStore) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
            auth: tokio::sync::Mutex::new(auth),
        }
    }
}

impl Provider for OpenAiCodex {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: true,
            tool_calls: true,
            parallel_tool_calls: false,
            usage_in_stream: true,
            cache_control: false,
            sealed_reasoning: true,
        }
    }

    fn stream(&self, request: Request) -> BoxFuture<'_, Result<EventStream, ProviderError>> {
        // Build the wire body eagerly and drop the request, so one
        // copy of the history — not two — lives across the round trip.
        let body = build_body(&request, self.config.reasoning.as_ref());
        drop(request);
        Box::pin(async move {
            let url = format!("{}/responses", self.config.base_url);
            let mut refreshed = false;
            let mut attempt: u32 = 0;
            let response = loop {
                let (token, account_id) = {
                    let mut store = self.auth.lock().await;
                    let creds = store.ensure()?;
                    (creds.access_token.clone(), store.account_id())
                };
                let mut builder = self
                    .http
                    .post(&url)
                    .bearer_auth(&token)
                    .header("accept", "text/event-stream")
                    .header("originator", &self.config.originator)
                    .json(&body);
                if let Some(account_id) = &account_id {
                    builder = builder.header("chatgpt-account-id", account_id);
                }
                match builder.send().await {
                    Err(error) => {
                        attempt += 1;
                        if attempt >= self.config.max_attempts {
                            return Err(ProviderError::Network(error.to_string()));
                        }
                        tokio::time::sleep(backoff(self.config.base_delay, attempt)).await;
                    }
                    Ok(response) => {
                        let status = response.status();
                        if status.is_success() {
                            break response;
                        }
                        if status.as_u16() == 401 {
                            if refreshed {
                                return Err(ProviderError::Auth(
                                    "unauthorized after token refresh".to_string(),
                                ));
                            }
                            refreshed = true;
                            self.auth.lock().await.refresh(&self.http).await?;
                            continue;
                        }
                        if status.as_u16() == 429 || status.is_server_error() {
                            attempt += 1;
                            if attempt >= self.config.max_attempts {
                                return Err(if status.as_u16() == 429 {
                                    ProviderError::RateLimited {
                                        retry_after: retry_after(&response),
                                    }
                                } else {
                                    ProviderError::Server(format!("status {status}"))
                                });
                            }
                            let delay = retry_delay(&response, self.config.base_delay, attempt);
                            tokio::time::sleep(delay).await;
                            continue;
                        }
                        let text = response.text().await.unwrap_or_default();
                        return Err(ProviderError::InvalidRequest(format!(
                            "status {status}: {text}"
                        )));
                    }
                }
            };
            Ok(spawn_event_stream(response, StreamState::default()))
        })
    }
}

/// Build the Responses API request body from the neutral request.
fn build_body(request: &Request, reasoning: Option<&Value>) -> Value {
    let mut input = Vec::new();
    for message in &request.messages {
        let (role, text_type) = match message.role {
            Role::User => ("user", "input_text"),
            Role::Assistant => ("assistant", "output_text"),
        };
        for block in &message.content {
            match block {
                ContentBlock::Text { text } => input.push(json!({
                    "type": "message",
                    "role": role,
                    "content": [{"type": text_type, "text": text}],
                })),
                ContentBlock::ToolUse {
                    id,
                    name,
                    input: args,
                } => input.push(json!({
                    "type": "function_call",
                    "name": name,
                    "arguments": serde_json::to_string(args).unwrap_or_default(),
                    "call_id": id,
                })),
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error: _,
                } => input.push(json!({
                    "type": "function_call_output",
                    "call_id": tool_use_id,
                    "output": content,
                })),
                // Same-provider sealed payloads round-trip verbatim.
                // Foreign or missing payloads are dropped: the readable
                // summary is for humans, not for the wire.
                ContentBlock::Reasoning { sealed, .. } => {
                    if let Some(sealed) = sealed
                        && sealed.provider == PROVIDER_ID
                    {
                        input.push(sealed.data.clone());
                    }
                }
            }
        }
    }
    let tools: Vec<Value> = request
        .tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "strict": false,
                "parameters": tool.input_schema,
            })
        })
        .collect();
    let mut body = json!({
        "model": request.model,
        "input": input,
        "tool_choice": "auto",
        "parallel_tool_calls": false,
        "store": false,
        "stream": true,
        "include": ["reasoning.encrypted_content"],
    });
    if !request.system_prompt.is_empty() {
        body["instructions"] = json!(request.system_prompt);
    }
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    if let Some(reasoning) = reasoning {
        body["reasoning"] = reasoning.clone();
    }
    if let Some(effort) = request.effort {
        match body["reasoning"].as_object_mut() {
            Some(reasoning) => {
                reasoning.insert("effort".to_string(), json!(effort.as_str()));
            }
            None => body["reasoning"] = json!({"effort": effort.as_str()}),
        }
    }
    if let Some(max) = request.max_output_tokens {
        body["max_output_tokens"] = json!(max);
    }
    body
}

#[derive(Default)]
struct StreamState {
    saw_tool_call: bool,
    terminal: bool,
    /// Maps wire item ids to call ids for argument-delta events.
    call_ids: HashMap<String, String>,
    /// A reasoning item opened; the next one starts a paragraph.
    saw_reasoning: bool,
}

impl PayloadMapper for StreamState {
    fn map(&mut self, payload: &str) -> Vec<Result<StreamEvent, ProviderError>> {
        map_payload(payload, self)
    }

    fn terminal(&self) -> bool {
        self.terminal
    }

    fn eof_error(&self) -> ProviderError {
        ProviderError::Protocol("stream ended before response.completed".to_string())
    }
}

/// Map one SSE payload to neutral events. Unknown payload types map to
/// nothing.
fn map_payload(payload: &str, state: &mut StreamState) -> Vec<Result<StreamEvent, ProviderError>> {
    let Ok(value) = serde_json::from_str::<Value>(payload) else {
        state.terminal = true;
        return vec![Err(ProviderError::Protocol(format!(
            "sse payload is not JSON: {payload}"
        )))];
    };
    match value["type"].as_str().unwrap_or("") {
        "response.output_text.delta" => text_delta(&value)
            .map(StreamEvent::TextDelta)
            .map(|e| vec![Ok(e)])
            .unwrap_or_default(),
        "response.reasoning_summary_text.delta" => text_delta(&value)
            .map(StreamEvent::ReasoningDelta)
            .map(|e| vec![Ok(e)])
            .unwrap_or_default(),
        // The summary arrives in parts; each part after the first is
        // a paragraph of its own. The part index restarts with each
        // reasoning item, so a second item opens a paragraph too.
        "response.reasoning_summary_part.added" => {
            if value["summary_index"].as_u64().unwrap_or(0) > 0 {
                vec![Ok(StreamEvent::ReasoningDelta("\n\n".to_string()))]
            } else {
                Vec::new()
            }
        }
        "response.output_item.added" => {
            let item = &value["item"];
            if item["type"] == "reasoning" {
                let part = std::mem::replace(&mut state.saw_reasoning, true);
                return if part {
                    vec![Ok(StreamEvent::ReasoningDelta("\n\n".to_string()))]
                } else {
                    Vec::new()
                };
            }
            if item["type"] == "function_call" {
                state.saw_tool_call = true;
                let call_id = item["call_id"].as_str().unwrap_or_default().to_string();
                if let Some(item_id) = item["id"].as_str() {
                    state.call_ids.insert(item_id.to_string(), call_id.clone());
                }
                vec![Ok(StreamEvent::ToolCallStart {
                    id: call_id,
                    name: item["name"].as_str().unwrap_or_default().to_string(),
                })]
            } else {
                Vec::new()
            }
        }
        "response.function_call_arguments.delta" => {
            let item_id = value["item_id"].as_str().unwrap_or_default();
            let id = state
                .call_ids
                .get(item_id)
                .cloned()
                .unwrap_or_else(|| item_id.to_string());
            text_delta(&value)
                .map(|fragment| {
                    vec![Ok(StreamEvent::ToolCallDelta {
                        id,
                        input_fragment: fragment,
                    })]
                })
                .unwrap_or_default()
        }
        "response.output_item.done" => {
            let item = &value["item"];
            match item["type"].as_str().unwrap_or("") {
                "function_call" => {
                    let arguments = item["arguments"].as_str().unwrap_or("");
                    let input = serde_json::from_str(arguments)
                        .unwrap_or_else(|_| Value::String(arguments.to_string()));
                    vec![Ok(StreamEvent::ToolCallEnd {
                        id: item["call_id"].as_str().unwrap_or_default().to_string(),
                        input,
                    })]
                }
                "reasoning" => {
                    let summary = item["summary"]
                        .as_array()
                        .map(|parts| {
                            parts
                                .iter()
                                .filter_map(|part| part["text"].as_str())
                                .collect::<Vec<_>>()
                                .join("")
                        })
                        .unwrap_or_default();
                    vec![Ok(StreamEvent::ReasoningDone {
                        summary,
                        sealed: Some(SealedPayload {
                            provider: PROVIDER_ID.to_string(),
                            data: item.clone(),
                        }),
                    })]
                }
                _ => Vec::new(),
            }
        }
        "response.completed" => {
            state.terminal = true;
            let stop = if state.saw_tool_call {
                StopReason::ToolUse
            } else {
                StopReason::EndTurn
            };
            vec![
                Ok(StreamEvent::Usage(usage_from(&value["response"]["usage"]))),
                Ok(StreamEvent::Done(stop)),
            ]
        }
        "response.incomplete" => {
            state.terminal = true;
            // An incomplete response carries a usage block too; the
            // call at a token limit is the one whose context reading
            // matters most.
            vec![
                Ok(StreamEvent::Usage(usage_from(&value["response"]["usage"]))),
                Ok(StreamEvent::Done(StopReason::MaxTokens)),
            ]
        }
        "response.failed" => {
            state.terminal = true;
            let error = &value["response"]["error"];
            let message = error["message"].as_str().unwrap_or("response.failed");
            let failure = if error["code"] == "rate_limit_exceeded" {
                ProviderError::RateLimited { retry_after: None }
            } else {
                ProviderError::Server(message.to_string())
            };
            vec![Err(failure)]
        }
        _ => Vec::new(),
    }
}

/// The neutral usage for a response's `usage` block. The wire
/// `input_tokens` already includes the cached prompt parts;
/// `cached_tokens` is the subset detail. The endpoint reports no
/// cache-write count.
fn usage_from(usage: &Value) -> Usage {
    Usage {
        input_tokens: usage["input_tokens"].as_u64().unwrap_or(0),
        output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
        cache_read_tokens: usage["input_tokens_details"]["cached_tokens"]
            .as_u64()
            .unwrap_or(0),
        cache_creation_tokens: 0,
    }
}

fn text_delta(value: &Value) -> Option<String> {
    value["delta"].as_str().map(str::to_string)
}
