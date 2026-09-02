//! The `anthropic` adapter: the native Messages API with an API key.
//!
//! Contract reference: the Anthropic Messages API
//! (<https://platform.claude.com/docs/en/api/messages>). The endpoint is
//! public and versioned; the version header below pins the wire shape.
//! The parser still ignores unknown events and fields, so additive
//! server changes never break a session.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use demerzel_core::{ContentBlock, Effort, Role, SealedPayload, StopReason, Usage};
use serde_json::{Value, json};

use crate::retry::{backoff, request_id, retry_after, retry_delay};
use crate::sse::{PayloadMapper, spawn_event_stream};
use crate::{BoxFuture, Capabilities, EventStream, Provider, ProviderError, Request, StreamEvent};

/// Stable adapter id. Sealed payloads carry this id.
pub const PROVIDER_ID: &str = "anthropic";

/// Default Anthropic API base URL.
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// Default model (M2 step-3 decision, checked against the live
/// `/v1/models` list).
pub const DEFAULT_MODEL: &str = "claude-sonnet-5";

/// The context windows of the models this adapter knows, in tokens,
/// from the provider's models documentation (2026-08). The rows name
/// the exact ids the provider publishes; where an id has an alias
/// and a dated form, each gets a row.
pub(crate) const CONTEXT_WINDOWS: &[(&str, u64)] = &[
    ("claude-opus-5", 1_000_000),
    ("claude-sonnet-5", 1_000_000),
    ("claude-haiku-4-5", 200_000),
    ("claude-haiku-4-5-20251001", 200_000),
];

/// The environment variable that holds the API key. It wins over the
/// auth store.
pub const ENV_API_KEY: &str = "ANTHROPIC_API_KEY";

/// The message for a run with no API key. `doctor` prints it too.
pub const NO_CREDENTIALS_HELP: &str =
    "no Anthropic API key: run `dmz login anthropic` or set ANTHROPIC_API_KEY";

/// Pinned `anthropic-version` header value.
const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Debug, Clone)]
pub struct AnthropicConfig {
    pub base_url: String,
    /// Total attempts for retryable failures (429, 5xx, network).
    pub max_attempts: u32,
    pub base_delay: Duration,
    /// `max_tokens` when the request does not set one. The Messages
    /// API requires the field.
    pub max_output_tokens: u64,
    /// `thinking` request field, sent as-is when set.
    pub thinking: Option<Value>,
}

impl Default for AnthropicConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            max_attempts: 4,
            base_delay: Duration::from_millis(500),
            max_output_tokens: 32_000,
            thinking: Some(json!({"type": "adaptive", "display": "summarized"})),
        }
    }
}

/// Check an API key with one cheap call: list one model. `Ok` means
/// the endpoint accepted the key. `dmz login anthropic` verifies a
/// key with this before it stores it.
pub async fn verify_key(
    http: &reqwest::Client,
    base_url: &str,
    api_key: &str,
) -> Result<(), ProviderError> {
    let url = format!("{base_url}/v1/models?limit=1");
    let response = http
        .get(&url)
        .header("x-api-key", api_key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .send()
        .await
        .map_err(|e| ProviderError::Network(format!("key check: {e}")))?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    // Every 4xx except 429 means the endpoint refused this check —
    // a bad key, or a wrong endpoint URL. Not an outage.
    if status.is_client_error() && status.as_u16() != 429 {
        return Err(ProviderError::Auth(format!(
            "the endpoint rejected the key check (status {status}) — \
             check the key and the endpoint URL"
        )));
    }
    Err(ProviderError::Server(format!(
        "key check failed with status {status}"
    )))
}

pub struct Anthropic {
    config: AnthropicConfig,
    http: reqwest::Client,
    api_key: String,
}

impl Anthropic {
    pub fn new(config: AnthropicConfig, api_key: String) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
            api_key,
        }
    }
}

impl Provider for Anthropic {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: true,
            tool_calls: true,
            parallel_tool_calls: false,
            usage_in_stream: true,
            cache_control: true,
            sealed_reasoning: true,
        }
    }

    fn stream(&self, request: Request) -> BoxFuture<'_, Result<EventStream, ProviderError>> {
        // Build the wire body eagerly and drop the request, so one
        // copy of the history — not two — lives across the round trip.
        // Serialized once: every attempt sends the same bytes, and
        // the trace reads the size and the body from them.
        let body = serde_json::to_vec(&build_body(&request, &self.config))
            .expect("a JSON value serializes");
        drop(request);
        Box::pin(async move {
            let url = format!("{}/v1/messages", self.config.base_url);
            let max_attempts = self.config.max_attempts;
            log::debug!("request POST {url} body_bytes={}", body.len());
            log::trace!("request body {}", String::from_utf8_lossy(&body));
            let mut attempt: u32 = 0;
            let response = loop {
                let builder = self
                    .http
                    .post(&url)
                    .header("x-api-key", &self.api_key)
                    .header("anthropic-version", ANTHROPIC_VERSION)
                    .header("accept", "text/event-stream")
                    .header("content-type", "application/json")
                    .body(body.clone());
                match builder.send().await {
                    Err(error) => {
                        attempt += 1;
                        if attempt >= max_attempts {
                            log::warn!(
                                "request failed after {attempt} of {max_attempts} attempts: \
                                 network error: {error}"
                            );
                            return Err(ProviderError::Network(error.to_string()));
                        }
                        let delay = backoff(self.config.base_delay, attempt);
                        log::info!(
                            "retry in {}ms after network error: {error} (attempt {attempt} of \
                             {max_attempts})",
                            delay.as_millis()
                        );
                        tokio::time::sleep(delay).await;
                    }
                    Ok(response) => {
                        let status = response.status();
                        log::info!(
                            "POST /v1/messages attempt {} of {max_attempts}: status {} \
                             request-id={}",
                            attempt + 1,
                            status.as_u16(),
                            request_id(&response)
                        );
                        if status.is_success() {
                            break response;
                        }
                        // API keys have no refresh flow: a 401 fails
                        // at once and points at the key.
                        if status.as_u16() == 401 {
                            return Err(ProviderError::Auth(format!(
                                "the Anthropic API rejected the key (status 401): \
                                 check {ENV_API_KEY} or run `dmz login anthropic`"
                            )));
                        }
                        if status.as_u16() == 429 || status.is_server_error() {
                            attempt += 1;
                            if attempt >= max_attempts {
                                log::warn!(
                                    "request failed after {attempt} of {max_attempts} attempts: \
                                     status {}",
                                    status.as_u16()
                                );
                                return Err(if status.as_u16() == 429 {
                                    ProviderError::RateLimited {
                                        retry_after: retry_after(&response),
                                    }
                                } else {
                                    ProviderError::Server(format!("status {status}"))
                                });
                            }
                            let delay = retry_delay(&response, self.config.base_delay, attempt);
                            log::info!(
                                "retry in {}ms after status {} (attempt {attempt} of \
                                 {max_attempts})",
                                delay.as_millis(),
                                status.as_u16()
                            );
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

/// Build the Messages API request body from the neutral request.
///
/// Prompt caching uses fixed breakpoint positions so the prefix stays
/// stable: the end of the tool definitions, the end of the system
/// prompt, and the tail of the history. The tail breakpoint moves
/// forward each turn, which is the standard incremental pattern — the
/// previous tail stays a valid read point.
fn build_body(request: &Request, config: &AnthropicConfig) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    for message in &request.messages {
        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        let blocks = wire_blocks(&message.content);
        if blocks.is_empty() {
            // A message can lose all content (foreign reasoning only);
            // the API rejects an empty content array.
            continue;
        }
        // Merge consecutive same-role messages: the neutral history
        // allows them, strict role alternation does not.
        match messages.last_mut() {
            Some(last) if last["role"] == role => {
                last["content"]
                    .as_array_mut()
                    .expect("built here")
                    .extend(blocks);
            }
            _ => {
                // Move the blocks in; `json!` would deep-copy them.
                let mut message = serde_json::Map::with_capacity(2);
                message.insert("role".to_string(), Value::String(role.to_string()));
                message.insert("content".to_string(), Value::Array(blocks));
                messages.push(Value::Object(message));
            }
        }
    }
    if let Some(last) = messages
        .last_mut()
        .and_then(|message| message["content"].as_array_mut())
        .and_then(|content| content.last_mut())
        && cacheable(last)
    {
        last["cache_control"] = json!({"type": "ephemeral"});
    }

    let mut tools: Vec<Value> = request
        .tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": tool.input_schema,
            })
        })
        .collect();
    if let Some(last) = tools.last_mut() {
        last["cache_control"] = json!({"type": "ephemeral"});
    }

    let mut body = json!({
        "model": request.model,
        "max_tokens": request.max_output_tokens.unwrap_or(config.max_output_tokens),
        "messages": messages,
        "stream": true,
    });
    if !request.system_prompt.is_empty() {
        body["system"] = json!([{
            "type": "text",
            "text": request.system_prompt,
            "cache_control": {"type": "ephemeral"},
        }]);
    }
    if !tools.is_empty() {
        body["tools"] = json!(tools);
        // Execution stays sequential in M2, so ask for one call at a
        // time; the capability flag says the same.
        body["tool_choice"] = json!({"type": "auto", "disable_parallel_tool_use": true});
    }
    if let Some(thinking) = &config.thinking {
        body["thinking"] = thinking.clone();
    }
    // Effort steers the depth of adaptive thinking; the two fields go
    // together, and the effort sets how much of the budget the model
    // spends. The Messages API takes `low` through `max`; the levels
    // outside that range clamp to its ends.
    if let Some(effort) = request.effort {
        let effort = effort.clamp(Effort::Low, Effort::Max);
        match body["output_config"].as_object_mut() {
            Some(output) => {
                output.insert("effort".to_string(), json!(effort.as_str()));
            }
            None => body["output_config"] = json!({"effort": effort.as_str()}),
        }
    }
    body
}

/// Whether a wire block accepts a `cache_control` marker. Thinking
/// blocks do not; they also never sit on the history tail in practice,
/// because a request always ends with a user message.
fn cacheable(block: &Value) -> bool {
    !matches!(
        block["type"].as_str(),
        Some("thinking") | Some("redacted_thinking")
    )
}

fn wire_blocks(content: &[ContentBlock]) -> Vec<Value> {
    let mut blocks = Vec::with_capacity(content.len());
    for block in content {
        match block {
            ContentBlock::Text { text } => blocks.push(json!({"type": "text", "text": text})),
            ContentBlock::ToolUse { id, name, input } => blocks.push(json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input,
            })),
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => blocks.push(json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": content,
                "is_error": is_error,
            })),
            // Same-provider sealed payloads round-trip verbatim — the
            // payload is the complete wire block, signature included.
            // Foreign or missing payloads are dropped: the readable
            // summary is for humans, not for the wire.
            ContentBlock::Reasoning { sealed, .. } => {
                if let Some(sealed) = sealed
                    && sealed.provider == PROVIDER_ID
                {
                    blocks.push(sealed.data.clone());
                }
            }
        }
    }
    blocks
}

/// One in-flight content block, keyed by its wire index.
enum Block {
    Text,
    Thinking { thinking: String, signature: String },
    Redacted { data: Value },
    ToolUse { id: String, input_json: String },
}

#[derive(Default)]
struct StreamState {
    blocks: HashMap<u64, Block>,
    saw_tool_call: bool,
    stop: Option<StopReason>,
    usage: Usage,
    terminal: bool,
    /// Events per wire type, for the debug trace. Counted only when
    /// the trace runs at `debug`.
    counts: BTreeMap<String, u32>,
}

impl StreamState {
    /// Count one wire event for the trace; free when the trace is
    /// below `debug`.
    fn count(&mut self, kind: &str) {
        if log::log_enabled!(log::Level::Debug) {
            *self.counts.entry(kind.to_string()).or_default() += 1;
        }
    }

    /// The per-type counts, one line, at the stream's end.
    fn report(&self) {
        if log::log_enabled!(log::Level::Debug) {
            let counts: Vec<String> = self
                .counts
                .iter()
                .map(|(kind, count)| format!("{kind}={count}"))
                .collect();
            log::debug!("sse events {}", counts.join(" "));
        }
    }
}

impl PayloadMapper for StreamState {
    fn map(&mut self, payload: &str) -> Vec<Result<StreamEvent, ProviderError>> {
        map_payload(payload, self)
    }

    fn terminal(&self) -> bool {
        self.terminal
    }

    fn eof_error(&self) -> ProviderError {
        ProviderError::Protocol("stream ended before message_stop".to_string())
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
    let kind = value["type"].as_str().unwrap_or("");
    state.count(kind);
    match kind {
        "message_start" => {
            let usage = &value["message"]["usage"];
            let count = |field: &str| usage[field].as_u64().unwrap_or(0);
            // The wire `input_tokens` excludes the cached prompt parts.
            // The neutral count is the whole prompt, with the tokens
            // read from the cache and the tokens written into it as
            // its two cache parts.
            state.usage.input_tokens = count("input_tokens")
                + count("cache_creation_input_tokens")
                + count("cache_read_input_tokens");
            state.usage.cache_read_tokens = count("cache_read_input_tokens");
            state.usage.cache_creation_tokens = count("cache_creation_input_tokens");
            state.usage.output_tokens = count("output_tokens");
            Vec::new()
        }
        "content_block_start" => {
            let Some(index) = value["index"].as_u64() else {
                return Vec::new();
            };
            let block = &value["content_block"];
            match block["type"].as_str().unwrap_or("") {
                "text" => {
                    state.blocks.insert(index, Block::Text);
                    Vec::new()
                }
                "thinking" => {
                    state.blocks.insert(
                        index,
                        Block::Thinking {
                            thinking: block["thinking"].as_str().unwrap_or("").to_string(),
                            signature: block["signature"].as_str().unwrap_or("").to_string(),
                        },
                    );
                    Vec::new()
                }
                "redacted_thinking" => {
                    state.blocks.insert(
                        index,
                        Block::Redacted {
                            data: block.clone(),
                        },
                    );
                    Vec::new()
                }
                "tool_use" => {
                    state.saw_tool_call = true;
                    let id = block["id"].as_str().unwrap_or_default().to_string();
                    let name = block["name"].as_str().unwrap_or_default().to_string();
                    state.blocks.insert(
                        index,
                        Block::ToolUse {
                            id: id.clone(),
                            input_json: String::new(),
                        },
                    );
                    vec![Ok(StreamEvent::ToolCallStart { id, name })]
                }
                _ => Vec::new(),
            }
        }
        "content_block_delta" => {
            let Some(index) = value["index"].as_u64() else {
                return Vec::new();
            };
            let delta = &value["delta"];
            // Deltas apply only to a block the adapter tracks; a delta
            // on an unknown block type maps to nothing, like the block
            // itself.
            match delta["type"].as_str().unwrap_or("") {
                "text_delta" => {
                    let Some(Block::Text) = state.blocks.get(&index) else {
                        return Vec::new();
                    };
                    delta["text"]
                        .as_str()
                        .map(|text| vec![Ok(StreamEvent::TextDelta(text.to_string()))])
                        .unwrap_or_default()
                }
                "thinking_delta" => {
                    let Some(Block::Thinking { thinking, .. }) = state.blocks.get_mut(&index)
                    else {
                        return Vec::new();
                    };
                    let Some(fragment) = delta["thinking"].as_str() else {
                        return Vec::new();
                    };
                    thinking.push_str(fragment);
                    vec![Ok(StreamEvent::ReasoningDelta(fragment.to_string()))]
                }
                "signature_delta" => {
                    if let Some(Block::Thinking { signature, .. }) = state.blocks.get_mut(&index)
                        && let Some(fragment) = delta["signature"].as_str()
                    {
                        signature.push_str(fragment);
                    }
                    Vec::new()
                }
                "input_json_delta" => {
                    let Some(fragment) = delta["partial_json"].as_str() else {
                        return Vec::new();
                    };
                    let Some(Block::ToolUse { id, input_json }) = state.blocks.get_mut(&index)
                    else {
                        return Vec::new();
                    };
                    input_json.push_str(fragment);
                    vec![Ok(StreamEvent::ToolCallDelta {
                        id: id.clone(),
                        input_fragment: fragment.to_string(),
                    })]
                }
                _ => Vec::new(),
            }
        }
        "content_block_stop" => {
            let Some(index) = value["index"].as_u64() else {
                return Vec::new();
            };
            match state.blocks.remove(&index) {
                Some(Block::ToolUse { id, input_json }) => {
                    let input = if input_json.is_empty() {
                        json!({})
                    } else {
                        serde_json::from_str(&input_json).unwrap_or(Value::String(input_json))
                    };
                    vec![Ok(StreamEvent::ToolCallEnd { id, input })]
                }
                Some(Block::Thinking {
                    thinking,
                    signature,
                }) => {
                    // A block without a signature must not seal: the
                    // API rejects a replayed thinking block with a
                    // missing signature, and a sealed one would sit in
                    // the session file and fail every later turn. The
                    // summary still reaches the user.
                    let sealed = (!signature.is_empty()).then(|| SealedPayload {
                        provider: PROVIDER_ID.to_string(),
                        data: json!({
                            "type": "thinking",
                            "thinking": thinking,
                            "signature": signature,
                        }),
                    });
                    vec![Ok(StreamEvent::ReasoningDone {
                        summary: thinking,
                        sealed,
                    })]
                }
                Some(Block::Redacted { data }) => vec![Ok(StreamEvent::ReasoningDone {
                    summary: String::new(),
                    sealed: Some(SealedPayload {
                        provider: PROVIDER_ID.to_string(),
                        data,
                    }),
                })],
                Some(Block::Text) | None => Vec::new(),
            }
        }
        "message_delta" => {
            if let Some(stop) = value["delta"]["stop_reason"].as_str() {
                state.stop = Some(match stop {
                    "tool_use" => StopReason::ToolUse,
                    // The context window is a token limit too; both
                    // tell the user the turn ran out of room.
                    "max_tokens" | "model_context_window_exceeded" => StopReason::MaxTokens,
                    "refusal" => StopReason::Refusal,
                    // `end_turn`, `stop_sequence`, and any future value
                    // end the turn. `pause_turn` lands here too: it
                    // needs server-side tools, which M2 does not send.
                    _ => StopReason::EndTurn,
                });
            }
            if let Some(output) = value["usage"]["output_tokens"].as_u64() {
                state.usage.output_tokens = output;
            }
            Vec::new()
        }
        "message_stop" => {
            state.terminal = true;
            state.report();
            let stop = state.stop.unwrap_or(if state.saw_tool_call {
                StopReason::ToolUse
            } else {
                StopReason::EndTurn
            });
            log::info!("stream ended: message_stop stop_reason={stop:?}");
            vec![
                Ok(StreamEvent::Usage(state.usage)),
                Ok(StreamEvent::Done(stop)),
            ]
        }
        "error" => {
            state.terminal = true;
            state.report();
            let error = &value["error"];
            let message = error["message"].as_str().unwrap_or("error event");
            log::warn!(
                "stream failed: error type={} message={message}",
                error["type"].as_str().unwrap_or("-")
            );
            let failure = match error["type"].as_str().unwrap_or("") {
                "rate_limit_error" => ProviderError::RateLimited { retry_after: None },
                _ => ProviderError::Server(message.to_string()),
            };
            vec![Err(failure)]
        }
        // `ping` and unknown events map to nothing.
        _ => Vec::new(),
    }
}
