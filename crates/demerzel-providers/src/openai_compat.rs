//! The `openai-compat` adapter: the Chat Completions streaming
//! contract against a configured endpoint.
//!
//! One adapter serves every server that speaks the format: hosted
//! vendors, gateways, and local servers. A named instance supplies the
//! endpoint, the model, and an optional key. The adapter carries no
//! default endpoint and no model catalog: a model id is a string the
//! config names and the server accepts or refuses.
//!
//! The request body stays minimal, because many servers refuse fields
//! they do not know. The parser ignores unknown fields and events, but
//! it requires the `[DONE]` marker: a stream that ends without it is a
//! protocol error, as with the other adapters.
//!
//! Reasoning text arrives in `reasoning_content` or `reasoning`,
//! depending on the server. The adapter keeps each field under its
//! original name and returns it on the assistant message of later
//! requests, because some servers refuse a tool continuation without
//! it. The payload carries the instance and the model, so it goes back
//! only to the endpoint and the model that wrote it.

use std::collections::BTreeMap;
use std::time::Duration;

use demerzel_core::{ContentBlock, Role, SealedPayload, StopReason, Usage};
use serde_json::{Map, Value, json};

use crate::endpoint::KeyedEndpoint;
use crate::retry::redact;
use crate::sse::{EventCounts, PayloadMapper, spawn_event_stream};
use crate::{BoxFuture, Capabilities, EventStream, Provider, ProviderError, Request, StreamEvent};

/// Stable adapter id. Sealed payloads carry this id.
pub const PROVIDER_ID: &str = "openai-compat";

/// The request route under the base URL. A base URL names the API
/// root, such as `http://127.0.0.1:11434/v1`.
const ROUTE: &str = "/chat/completions";

/// The reasoning fields a server can stream, in display preference.
/// A server that sends both in one chunk shows the first one once.
const REASONING_FIELDS: [&str; 2] = ["reasoning_content", "reasoning"];

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CompatConfig {
    /// The instance name. Sealed payloads carry it.
    pub instance: String,
    /// The API root, without the route.
    pub base_url: String,
    /// Total attempts for retryable failures (429, 5xx, network).
    pub max_attempts: u32,
    pub base_delay: Duration,
}

impl CompatConfig {
    /// The config for instance `instance` at `base_url`, with the
    /// default retry policy. A trailing `/` on the URL is dropped.
    pub fn new(instance: impl Into<String>, base_url: impl Into<String>) -> Self {
        let mut base_url = base_url.into();
        while base_url.ends_with('/') {
            base_url.pop();
        }
        Self {
            instance: instance.into(),
            base_url,
            max_attempts: 4,
            base_delay: Duration::from_millis(500),
        }
    }
}

pub struct OpenAiCompat {
    config: CompatConfig,
    http: reqwest::Client,
    /// The bearer key. `None` sends no `Authorization` header: a local
    /// server needs none.
    api_key: Option<String>,
}

impl OpenAiCompat {
    pub fn new(config: CompatConfig, api_key: Option<String>) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
            api_key: api_key.filter(|key| !key.is_empty()),
        }
    }
}

impl Provider for OpenAiCompat {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            tool_calls: true,
            ..Capabilities::default()
        }
    }

    fn stream(&self, request: Request) -> BoxFuture<'_, Result<EventStream, ProviderError>> {
        // Build the wire body eagerly and drop the request, as the
        // other adapters do: one packed copy of the history lives
        // across the round trip.
        let body = serde_json::to_vec(&build_body(&request, &self.config.instance))
            .expect("a JSON value serializes");
        let model = request.model.clone();
        drop(request);
        Box::pin(async move {
            let url = format!("{}{ROUTE}", self.config.base_url);
            let key = self.api_key.as_deref();
            let endpoint = KeyedEndpoint {
                http: &self.http,
                instance: &self.config.instance,
                url: &url,
                route: ROUTE,
                key,
                max_attempts: self.config.max_attempts,
                base_delay: self.config.base_delay,
            };
            let response = endpoint.post(body).await?;
            let state = StreamState {
                secret: key.unwrap_or("").to_string(),
                instance: self.config.instance.clone(),
                model,
                ..StreamState::default()
            };
            Ok(spawn_event_stream(response, state))
        })
    }
}

/// Build the Chat Completions request body from the neutral request.
///
/// The system prompt leads as a `system` message. A neutral user
/// message splits into one `tool` message per result, then one `user`
/// message with its text; adjacent user text merges, for servers whose
/// templates require alternating roles. An assistant message carries
/// its text, its tool calls, and the reasoning fields of the payloads
/// that replay to this instance and model.
fn build_body(request: &Request, instance: &str) -> Value {
    let mut messages: Vec<Value> = Vec::with_capacity(request.messages.len() + 1);
    if !request.system_prompt.is_empty() {
        messages.push(json!({"role": "system", "content": request.system_prompt}));
    }
    for message in &request.messages {
        match message.role {
            Role::User => {
                let mut text = String::new();
                for block in &message.content {
                    match block {
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error: _,
                        } => messages.push(json!({
                            "role": "tool",
                            "tool_call_id": tool_use_id,
                            "content": content,
                        })),
                        ContentBlock::Text { text: part } => {
                            if !text.is_empty() {
                                text.push_str("\n\n");
                            }
                            text.push_str(part);
                        }
                        ContentBlock::ToolUse { .. } | ContentBlock::Reasoning { .. } => {}
                    }
                }
                if !text.is_empty() {
                    push_user_text(&mut messages, text);
                }
            }
            Role::Assistant => {
                if let Some(wire) = assistant_message(&message.content, instance, &request.model) {
                    messages.push(wire);
                }
            }
        }
    }

    let mut body = json!({
        "model": request.model,
        "messages": messages,
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    if !request.tools.is_empty() {
        let tools: Vec<Value> = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.input_schema,
                    },
                })
            })
            .collect();
        body["tools"] = Value::Array(tools);
    }
    if let Some(max) = request.max_output_tokens {
        body["max_tokens"] = json!(max);
    }
    // The level goes out as named. The adapter does not guess a
    // server's range: a server that refuses the level says so.
    if let Some(effort) = request.effort {
        body["reasoning_effort"] = json!(effort.as_str());
    }
    body
}

/// Append user text, merged into the previous message when that one
/// is user text too.
fn push_user_text(messages: &mut Vec<Value>, text: String) {
    if let Some(last) = messages.last_mut()
        && last["role"] == "user"
        && let Some(Value::String(previous)) = last.get_mut("content")
    {
        // In place: a run of user messages costs its length once.
        previous.push_str("\n\n");
        previous.push_str(&text);
        return;
    }
    messages.push(json!({"role": "user", "content": text}));
}

/// The wire form of one neutral assistant message, or `None` when
/// nothing of it goes on the wire.
fn assistant_message(content: &[ContentBlock], instance: &str, model: &str) -> Option<Value> {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    let mut reasoning: Map<String, Value> = Map::new();
    for block in content {
        match block {
            ContentBlock::Text { text: part } => text.push_str(part),
            ContentBlock::ToolUse { id, name, input } => tool_calls.push(json!({
                "id": id,
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": serde_json::to_string(input).unwrap_or_default(),
                },
            })),
            // Only a payload bound to this adapter, instance, and model
            // replays; the readable summary never replaces it.
            ContentBlock::Reasoning {
                sealed: Some(sealed),
                ..
            } if sealed.replays_to(PROVIDER_ID, instance, model) => {
                if let Some(fields) = sealed.data.as_object() {
                    for (name, value) in fields {
                        let Some(part) = value.as_str() else {
                            continue;
                        };
                        match reasoning.get_mut(name) {
                            Some(Value::String(joined)) => joined.push_str(part),
                            _ => {
                                reasoning.insert(name.clone(), Value::String(part.to_string()));
                            }
                        }
                    }
                }
            }
            ContentBlock::Reasoning { .. } | ContentBlock::ToolResult { .. } => {}
        }
    }
    if text.is_empty() && tool_calls.is_empty() {
        return None;
    }
    let mut message = Map::with_capacity(3 + reasoning.len());
    message.insert("role".to_string(), json!("assistant"));
    message.insert(
        "content".to_string(),
        if text.is_empty() {
            Value::Null
        } else {
            Value::String(text)
        },
    );
    if !tool_calls.is_empty() {
        message.insert("tool_calls".to_string(), Value::Array(tool_calls));
    }
    message.extend(reasoning);
    Some(Value::Object(message))
}

/// One tool call in flight, by its wire index.
#[derive(Default)]
struct Call {
    id: Option<String>,
    name: Option<String>,
    /// Argument text received before the start event went out.
    pending: String,
    /// Every argument fragment, joined.
    arguments: String,
    started: bool,
}

#[derive(Default)]
struct StreamState {
    /// Reasoning text of the open segment, by original field name.
    reasoning: BTreeMap<String, String>,
    /// The readable text of the open segment.
    reasoning_summary: String,
    calls: BTreeMap<u64, Call>,
    /// A tool call arrived in this reply. It outlives the call map:
    /// a server can repeat the finish chunk after the calls ended.
    saw_call: bool,
    /// The model declined in `delta.refusal`.
    refused: bool,
    stop: Option<StopReason>,
    usage: Option<Usage>,
    terminal: bool,
    counts: EventCounts,
    /// The request's key, so a failure message that echoes it is
    /// redacted before it becomes an error.
    secret: String,
    instance: String,
    model: String,
}

impl PayloadMapper for StreamState {
    fn map(&mut self, payload: &str) -> Vec<Result<StreamEvent, ProviderError>> {
        map_payload(payload, self)
    }

    fn terminal(&self) -> bool {
        self.terminal
    }

    fn eof_error(&self) -> ProviderError {
        ProviderError::Protocol("stream ended before [DONE]".to_string())
    }

    fn counts(&self) -> &EventCounts {
        &self.counts
    }
}

impl StreamState {
    /// Close the open reasoning segment, if one is open.
    fn close_reasoning(&mut self, out: &mut Vec<Result<StreamEvent, ProviderError>>) {
        if self.reasoning.is_empty() {
            return;
        }
        let fields: Map<String, Value> = std::mem::take(&mut self.reasoning)
            .into_iter()
            .map(|(name, text)| (name, Value::String(text)))
            .collect();
        let sealed = SealedPayload::new(PROVIDER_ID, Value::Object(fields))
            .with_identity(self.instance.clone(), self.model.clone());
        out.push(Ok(StreamEvent::ReasoningDone {
            summary: std::mem::take(&mut self.reasoning_summary),
            sealed: Some(sealed),
        }));
    }

    /// End every accumulated tool call, in index order.
    fn end_calls(&mut self, out: &mut Vec<Result<StreamEvent, ProviderError>>) {
        for (index, mut call) in std::mem::take(&mut self.calls) {
            let id = call.id.take().unwrap_or_else(|| format!("call_{index}"));
            if !call.started {
                out.push(Ok(StreamEvent::ToolCallStart {
                    id: id.clone(),
                    name: call.name.take().unwrap_or_default(),
                }));
                if !call.pending.is_empty() {
                    out.push(Ok(StreamEvent::ToolCallDelta {
                        id: id.clone(),
                        input_fragment: std::mem::take(&mut call.pending),
                    }));
                }
            }
            let input = if call.arguments.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(&call.arguments).unwrap_or(Value::String(call.arguments))
            };
            out.push(Ok(StreamEvent::ToolCallEnd { id, input }));
        }
    }

    /// The wire index of a tool-call fragment. A fragment without an
    /// index — some local servers omit it — continues the call with its
    /// id, or starts a new one when its id is new, or continues the
    /// last call.
    fn call_index(&self, fragment: &Value) -> u64 {
        if let Some(index) = fragment["index"].as_u64() {
            return index;
        }
        let last = self.calls.iter().next_back();
        let next = last.map_or(0, |(index, _)| index + 1);
        if let Some(id) = fragment["id"].as_str().filter(|id| !id.is_empty()) {
            return self
                .calls
                .iter()
                .find(|(_, call)| call.id.as_deref() == Some(id))
                .map_or(next, |(index, _)| *index);
        }
        // Neither index nor id: a named fragment after a call that
        // already has its name and arguments is a call of its own.
        let named = fragment["function"]["name"]
            .as_str()
            .is_some_and(|name| !name.is_empty());
        match last {
            Some((_, call)) if named && call.name.is_some() && !call.arguments.is_empty() => next,
            Some((index, _)) => *index,
            None => 0,
        }
    }

    /// Apply one tool-call fragment.
    ///
    /// The start event waits for the first argument text: a server
    /// that parses the name as it streams re-sends the grown id and
    /// name, and each repeat before the start overwrites the last.
    /// After the start a repeat changes nothing.
    fn tool_fragment(
        &mut self,
        fragment: &Value,
        out: &mut Vec<Result<StreamEvent, ProviderError>>,
    ) {
        self.saw_call = true;
        let index = self.call_index(fragment);
        let call = self.calls.entry(index).or_default();
        let id = fragment["id"].as_str().filter(|id| !id.is_empty());
        let function = &fragment["function"];
        let name = function["name"].as_str().filter(|name| !name.is_empty());
        let text = function["arguments"].as_str().unwrap_or("");
        call.arguments.push_str(text);
        if call.started {
            if name.is_some_and(|name| call.name.as_deref() != Some(name)) {
                log::warn!("tool call {index} changed its name after its start; kept the first");
            }
            if !text.is_empty() {
                out.push(Ok(StreamEvent::ToolCallDelta {
                    id: call.id.clone().unwrap_or_default(),
                    input_fragment: text.to_string(),
                }));
            }
            return;
        }
        if let Some(id) = id {
            call.id = Some(id.to_string());
        }
        if let Some(name) = name {
            call.name = Some(name.to_string());
        }
        call.pending.push_str(text);
        if call.pending.is_empty() {
            return;
        }
        if let (Some(id), Some(name)) = (&call.id, &call.name) {
            call.started = true;
            out.push(Ok(StreamEvent::ToolCallStart {
                id: id.clone(),
                name: name.clone(),
            }));
            out.push(Ok(StreamEvent::ToolCallDelta {
                id: id.clone(),
                input_fragment: std::mem::take(&mut call.pending),
            }));
        }
    }

    /// The stop for a reply that named no reason, or named `stop` or
    /// a reason this adapter does not know.
    fn default_stop(&self) -> StopReason {
        if self.refused {
            StopReason::Refusal
        } else if self.saw_call {
            StopReason::ToolUse
        } else {
            StopReason::EndTurn
        }
    }
}

/// Map one SSE payload to neutral events. Unknown fields map to
/// nothing.
fn map_payload(payload: &str, state: &mut StreamState) -> Vec<Result<StreamEvent, ProviderError>> {
    let mut out = Vec::new();
    if payload.trim() == "[DONE]" {
        state.counts.count("[DONE]");
        state.terminal = true;
        state.close_reasoning(&mut out);
        state.end_calls(&mut out);
        let stop = state.stop.unwrap_or_else(|| state.default_stop());
        log::info!("stream ended: [DONE] stop_reason={}", stop.as_str());
        if let Some(usage) = state.usage {
            out.push(Ok(StreamEvent::Usage(usage)));
        }
        out.push(Ok(StreamEvent::Done(stop)));
        return out;
    }
    let Ok(value) = serde_json::from_str::<Value>(payload) else {
        state.terminal = true;
        return vec![Err(ProviderError::Protocol(format!(
            "sse payload is not JSON: {payload}"
        )))];
    };
    if value.get("error").is_some_and(|error| !error.is_null()) {
        state.counts.count("error");
        state.terminal = true;
        let error = &value["error"];
        let message = redact(
            error["message"]
                .as_str()
                .or_else(|| error.as_str())
                .unwrap_or("error event"),
            &state.secret,
        );
        log::warn!("stream failed: error message={message}");
        let rate_limited = error["code"].as_u64() == Some(429)
            || error["code"].as_str() == Some("rate_limit_exceeded")
            || error["type"].as_str() == Some("rate_limit_error");
        let failure = if rate_limited {
            ProviderError::RateLimited { retry_after: None }
        } else {
            ProviderError::Server(message)
        };
        return vec![Err(failure)];
    }
    state
        .counts
        .count(value["object"].as_str().unwrap_or("chunk"));

    // Usage can ride a chunk of its own, with empty `choices`, or the
    // last chunk with a choice. The last one seen wins.
    if let Some(usage) = value.get("usage").filter(|usage| usage.is_object()) {
        let count = |path: &[&str]| {
            path.iter()
                .try_fold(usage, |value, key| value.get(key))
                .and_then(Value::as_u64)
        };
        state.usage = Some(Usage {
            input_tokens: count(&["prompt_tokens"]).unwrap_or(0),
            output_tokens: count(&["completion_tokens"]).unwrap_or(0),
            cache_read_tokens: count(&["prompt_tokens_details", "cached_tokens"])
                .or_else(|| count(&["prompt_cache_hit_tokens"]))
                .unwrap_or(0),
            cache_creation_tokens: 0,
        });
    }

    let Some(choice) = value["choices"].as_array().and_then(|choices| {
        choices
            .iter()
            .find(|choice| choice["index"].as_u64().is_none_or(|index| index == 0))
    }) else {
        return out;
    };
    let delta = &choice["delta"];

    // Reasoning first: a chunk that carries reasoning and content
    // keeps that order on screen.
    let mut shown = false;
    for field in REASONING_FIELDS {
        let Some(part) = delta[field].as_str().filter(|part| !part.is_empty()) else {
            continue;
        };
        // One key allocation per segment, not per chunk.
        match state.reasoning.get_mut(field) {
            Some(joined) => joined.push_str(part),
            None => {
                state.reasoning.insert(field.to_string(), part.to_string());
            }
        }
        if !shown {
            shown = true;
            state.reasoning_summary.push_str(part);
            out.push(Ok(StreamEvent::ReasoningDelta(part.to_string())));
        }
    }

    if let Some(text) = delta["content"].as_str().filter(|text| !text.is_empty()) {
        state.close_reasoning(&mut out);
        out.push(Ok(StreamEvent::TextDelta(text.to_string())));
    }
    // A refusal streams as text the user reads, and ends the turn as
    // a refusal whatever the finish reason says.
    if let Some(text) = delta["refusal"].as_str().filter(|text| !text.is_empty()) {
        state.close_reasoning(&mut out);
        state.refused = true;
        out.push(Ok(StreamEvent::TextDelta(text.to_string())));
    }

    if let Some(fragments) = delta["tool_calls"].as_array() {
        state.close_reasoning(&mut out);
        for fragment in fragments {
            state.tool_fragment(fragment, &mut out);
        }
    }

    if let Some(reason) = choice["finish_reason"].as_str() {
        // The terminal choice ends every call it accumulated, whatever
        // the reason says. The usage chunk and `[DONE]` can follow, and
        // a gateway can repeat the finish chunk.
        state.close_reasoning(&mut out);
        state.end_calls(&mut out);
        let stop = match reason {
            // The server ended the reply for its own failure: the text
            // so far is no answer, and the call fails.
            "error" | "insufficient_system_resource" | "abort" | "aborted" => {
                state.terminal = true;
                out.push(Err(ProviderError::Server(format!(
                    "the server ended the reply early: finish_reason {reason}"
                ))));
                return out;
            }
            // A refusal ends the turn as a refusal, whatever the
            // reason says.
            _ if state.refused => StopReason::Refusal,
            "tool_calls" | "function_call" => StopReason::ToolUse,
            "length" => StopReason::MaxTokens,
            "content_filter" => StopReason::Refusal,
            // `stop` and any other value.
            _ => state.default_stop(),
        };
        // A repeated finish chunk keeps the first chunk's reason.
        state.stop.get_or_insert(stop);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use demerzel_core::{Message, ToolDefinition};

    fn events(state: &mut StreamState, payloads: &[&str]) -> Vec<StreamEvent> {
        payloads
            .iter()
            .flat_map(|payload| map_payload(payload, state))
            .map(Result::unwrap)
            .collect()
    }

    fn chunk(delta: Value) -> String {
        json!({"object": "chat.completion.chunk", "choices": [{"index": 0, "delta": delta}]})
            .to_string()
    }

    fn finish(reason: &str) -> String {
        json!({"choices": [{"index": 0, "delta": {}, "finish_reason": reason}]}).to_string()
    }

    fn state() -> StreamState {
        StreamState {
            instance: "local".into(),
            model: "m1".into(),
            ..StreamState::default()
        }
    }

    #[test]
    fn a_call_without_an_index_continues_by_id_or_starts_anew() {
        let mut state = state();
        let got = events(
            &mut state,
            &[
                &chunk(
                    json!({"tool_calls": [{"id": "a", "function": {"name": "read", "arguments": "{\"pa"}}]}),
                ),
                &chunk(json!({"tool_calls": [{"function": {"arguments": "th\":1}"}}]})),
                &chunk(
                    json!({"tool_calls": [{"id": "b", "function": {"name": "grep", "arguments": "{}"}}]}),
                ),
                &finish("tool_calls"),
                "[DONE]",
            ],
        );
        let ends: Vec<_> = got
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ToolCallEnd { id, input } => Some((id.clone(), input.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            ends,
            vec![
                ("a".to_string(), json!({"path": 1})),
                ("b".to_string(), json!({}))
            ]
        );
    }

    #[test]
    fn whole_calls_without_index_or_id_stay_apart() {
        let mut state = state();
        let got = events(
            &mut state,
            &[
                &chunk(
                    json!({"tool_calls": [{"function": {"name": "read", "arguments": "{\"path\":\"a\"}"}}]}),
                ),
                &chunk(
                    json!({"tool_calls": [{"function": {"name": "grep", "arguments": "{\"pattern\":\"x\"}"}}]}),
                ),
                &finish("tool_calls"),
                "[DONE]",
            ],
        );
        let ends: Vec<_> = got
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ToolCallEnd { id, input } => Some((id.clone(), input.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            ends,
            vec![
                ("call_0".to_string(), json!({"path": "a"})),
                ("call_1".to_string(), json!({"pattern": "x"})),
            ]
        );
    }

    #[test]
    fn arguments_before_the_name_wait_for_the_start() {
        let mut state = state();
        let got = events(
            &mut state,
            &[
                &chunk(
                    json!({"tool_calls": [{"index": 0, "id": "a", "function": {"arguments": "{\"x\""}}]}),
                ),
                &chunk(
                    json!({"tool_calls": [{"index": 0, "function": {"name": "read", "arguments": ":1}"}}]}),
                ),
                &finish("tool_calls"),
                "[DONE]",
            ],
        );
        assert_eq!(
            got[..2],
            [
                StreamEvent::ToolCallStart {
                    id: "a".into(),
                    name: "read".into()
                },
                StreamEvent::ToolCallDelta {
                    id: "a".into(),
                    input_fragment: "{\"x\":1}".into()
                },
            ]
        );
    }

    #[test]
    fn a_name_that_grows_before_the_arguments_starts_the_call_whole() {
        let mut state = state();
        let got = events(
            &mut state,
            &[
                &chunk(
                    json!({"tool_calls": [{"index": 0, "id": "a", "type": "function", "function": {"name": "rea", "arguments": ""}}]}),
                ),
                &chunk(
                    json!({"tool_calls": [{"index": 0, "id": "a", "type": "function", "function": {"name": "read"}}]}),
                ),
                &chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": "{}"}}]})),
                &finish("tool_calls"),
                "[DONE]",
            ],
        );
        assert_eq!(
            got[0],
            StreamEvent::ToolCallStart {
                id: "a".into(),
                name: "read".into()
            }
        );
    }

    #[test]
    fn a_repeated_finish_chunk_keeps_the_tool_use_stop() {
        let mut state = state();
        let got = events(
            &mut state,
            &[
                &chunk(
                    json!({"tool_calls": [{"index": 0, "id": "a", "function": {"name": "read", "arguments": "{}"}}]}),
                ),
                &finish("stop"),
                &finish("stop"),
                "[DONE]",
            ],
        );
        assert_eq!(got.last(), Some(&StreamEvent::Done(StopReason::ToolUse)));
    }

    #[test]
    fn a_refusal_streams_as_text_and_ends_as_a_refusal() {
        let mut state = state();
        let got = events(
            &mut state,
            &[
                &chunk(json!({"refusal": "I can't help with that."})),
                &finish("stop"),
                "[DONE]",
            ],
        );
        assert_eq!(
            got[0],
            StreamEvent::TextDelta("I can't help with that.".into())
        );
        assert_eq!(got.last(), Some(&StreamEvent::Done(StopReason::Refusal)));
    }

    #[test]
    fn the_first_finish_reason_holds_and_a_refusal_wins() {
        let mut filtered = state();
        let got = events(
            &mut filtered,
            &[
                &chunk(json!({"content": "part"})),
                &finish("content_filter"),
                &finish("stop"),
                "[DONE]",
            ],
        );
        assert_eq!(got.last(), Some(&StreamEvent::Done(StopReason::Refusal)));

        let mut refusing = state();
        let got = events(
            &mut refusing,
            &[
                &chunk(json!({"refusal": "No."})),
                &finish("length"),
                "[DONE]",
            ],
        );
        assert_eq!(got.last(), Some(&StreamEvent::Done(StopReason::Refusal)));
    }

    #[test]
    fn an_empty_id_continues_the_call() {
        let mut state = state();
        let got = events(
            &mut state,
            &[
                &chunk(
                    json!({"tool_calls": [{"id": "a", "function": {"name": "read", "arguments": "{\"pa"}}]}),
                ),
                &chunk(json!({"tool_calls": [{"id": "", "function": {"arguments": "th\":1}"}}]})),
                &finish("tool_calls"),
                "[DONE]",
            ],
        );
        let ends: Vec<_> = got
            .iter()
            .filter(|event| matches!(event, StreamEvent::ToolCallEnd { .. }))
            .collect();
        assert_eq!(
            ends,
            vec![&StreamEvent::ToolCallEnd {
                id: "a".into(),
                input: json!({"path": 1})
            }]
        );
    }

    #[test]
    fn a_failure_finish_reason_fails_the_call() {
        for reason in ["insufficient_system_resource", "error"] {
            let mut state = state();
            let mut got = Vec::new();
            for payload in [chunk(json!({"content": "half"})), finish(reason)] {
                got.extend(map_payload(&payload, &mut state));
            }
            assert!(state.terminal, "{reason}");
            assert!(
                matches!(got.last(), Some(Err(ProviderError::Server(message))) if message.contains(reason)),
                "{reason}: {got:?}"
            );
        }
    }

    #[test]
    fn a_chunk_with_both_reasoning_fields_shows_the_text_once_and_keeps_both() {
        let mut state = state();
        let got = events(
            &mut state,
            &[
                &chunk(json!({"reasoning_content": "think", "reasoning": "think"})),
                &chunk(json!({"content": "done"})),
                &finish("stop"),
                "[DONE]",
            ],
        );
        assert_eq!(got[0], StreamEvent::ReasoningDelta("think".into()));
        let StreamEvent::ReasoningDone { summary, sealed } = &got[1] else {
            panic!("expected the segment end, got {:?}", got[1]);
        };
        assert_eq!(summary, "think");
        let sealed = sealed.as_ref().unwrap();
        assert_eq!(
            sealed.data,
            json!({"reasoning": "think", "reasoning_content": "think"})
        );
        assert!(sealed.replays_to(PROVIDER_ID, "local", "m1"));
    }

    #[test]
    fn the_body_merges_adjacent_user_text_and_puts_results_first() {
        let mut request = Request::new("m1");
        request.messages = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "a".into() }],
            },
            Message {
                role: Role::User,
                content: vec![
                    ContentBlock::ToolResult {
                        tool_use_id: "c1".into(),
                        content: "out".into(),
                        is_error: true,
                    },
                    ContentBlock::Text { text: "b".into() },
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "c".into() }],
            },
        ];
        request.tools = vec![ToolDefinition {
            name: "read".into(),
            description: "Read".into(),
            input_schema: json!({"type": "object"}),
        }];
        let body = build_body(&request, "local");
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0], json!({"role": "user", "content": "a"}));
        assert_eq!(
            messages[1],
            json!({"role": "tool", "tool_call_id": "c1", "content": "out"})
        );
        assert_eq!(messages[2], json!({"role": "user", "content": "b\n\nc"}));
        assert_eq!(body["tools"][0]["function"]["name"], "read");
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("reasoning_effort").is_none());
    }
}
