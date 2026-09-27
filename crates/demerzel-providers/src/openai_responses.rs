//! The `openai-responses` adapter: the Responses API streaming
//! contract against a configured endpoint with an optional API key.
//!
//! A named instance supplies the endpoint, the model, and the key
//! variable. The adapter carries no default endpoint and no model
//! catalog. Subscription logins and their account headers belong to
//! `openai-codex`; this adapter never reads them.
//!
//! The request holds the whole local conversation and asks the server
//! to store nothing: no `previous_response_id`, `store: false`. Current
//! servers return each reasoning item with opaque encrypted content
//! unasked; the adapter does not send `include`, because some hosted
//! models refuse it. The adapter seals the complete item with the
//! instance and the model that wrote it. Later requests to the same
//! instance and model send the item back verbatim, in its original
//! position; a switch sends the text and the tool history without it.
//!
//! The parser ignores unknown events and fields. A stream ends only
//! with `response.completed` or a known `response.incomplete`; a
//! failure, an error event, an unknown incomplete reason, or a stream
//! cut before the terminal event fails the call. A tool call ends only
//! when its item completes, so a cut call never runs.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use demerzel_core::{ContentBlock, Role, SealedPayload, StopReason, Usage};
use serde_json::{Map, Value, json};

use crate::endpoint::KeyedEndpoint;
use crate::retry::redact;
use crate::sse::{EventCounts, PayloadMapper, spawn_event_stream};
use crate::{BoxFuture, Capabilities, EventStream, Provider, ProviderError, Request, StreamEvent};

/// Stable adapter id. Sealed payloads carry this id.
pub const PROVIDER_ID: &str = "openai-responses";

/// The request route under the base URL. A base URL names the API
/// root, such as `https://api.example.com/v1`.
const ROUTE: &str = "/responses";

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ResponsesConfig {
    /// The instance name. Sealed payloads carry it.
    pub instance: String,
    /// The API root, without the route.
    pub base_url: String,
    /// Total attempts for retryable failures (429, 5xx, network).
    pub max_attempts: u32,
    pub base_delay: Duration,
}

impl ResponsesConfig {
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

pub struct OpenAiResponses {
    config: ResponsesConfig,
    http: reqwest::Client,
    /// The bearer key. `None` sends no `Authorization` header.
    api_key: Option<String>,
}

impl OpenAiResponses {
    pub fn new(config: ResponsesConfig, api_key: Option<String>) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
            api_key: api_key.filter(|key| !key.is_empty()),
        }
    }
}

impl Provider for OpenAiResponses {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: true,
            tool_calls: true,
            parallel_tool_calls: true,
            usage_in_stream: true,
            cache_control: false,
            sealed_reasoning: true,
        }
    }

    fn stream(&self, request: Request) -> BoxFuture<'_, Result<EventStream, ProviderError>> {
        // The body takes the request apart. The model, the system
        // prompt, and the reasoning payloads move into the JSON tree;
        // each other block is copied and then freed, so the history is
        // not held twice before the one serialized copy that lives
        // across the round trip.
        let model = request.model.clone();
        let body = serde_json::to_vec(&build_body(request, &self.config.instance))
            .expect("a JSON value serializes");
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

/// Build the Responses request body from the neutral request.
///
/// Each neutral block becomes one input item, in order: user text a
/// `message`, a tool result a `function_call_output`, assistant text a
/// `message` with plain content, a tool call a `function_call`, and a
/// reasoning payload that replays to this instance and model its
/// original item. A reasoning item goes back only when an item it
/// produced follows it in the same reply: a server refuses a reasoning
/// item without its following item, the shape a cut reply leaves. The
/// body sets only what the request sets, besides the fields the
/// stateless contract needs.
fn build_body(request: Request, instance: &str) -> Value {
    let Request {
        model,
        system_prompt,
        messages,
        tools,
        max_output_tokens,
        effort,
        ..
    } = request;
    let mut input = Vec::with_capacity(messages.len());
    for message in messages {
        let assistant = message.role == Role::Assistant;
        // The last block that goes out as an output item.
        let last_output = message.content.iter().rposition(|block| {
            matches!(
                block,
                ContentBlock::Text { .. } | ContentBlock::ToolUse { .. }
            )
        });
        for (at, block) in message.content.into_iter().enumerate() {
            match block {
                ContentBlock::Text { text } if assistant => input.push(json!({
                    "type": "message",
                    "role": "assistant",
                    "content": text,
                })),
                ContentBlock::Text { text } => input.push(json!({
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": text}],
                })),
                ContentBlock::ToolUse {
                    id,
                    name,
                    input: arguments,
                } if assistant => input.push(json!({
                    "type": "function_call",
                    "call_id": id,
                    "name": name,
                    "arguments": arguments.to_string(),
                })),
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error: _,
                } if !assistant => input.push(json!({
                    "type": "function_call_output",
                    "call_id": tool_use_id,
                    "output": content,
                })),
                // Only a payload bound to this adapter, instance, and
                // model replays; the readable summary never replaces it.
                ContentBlock::Reasoning {
                    sealed: Some(sealed),
                    ..
                } if assistant
                    && last_output.is_some_and(|last| last > at)
                    && sealed.replays_to(PROVIDER_ID, instance, &model) =>
                {
                    input.push(sealed.data);
                }
                _ => {}
            }
        }
    }

    let mut body = Map::new();
    body.insert("model".into(), Value::String(model));
    if !system_prompt.is_empty() {
        body.insert("instructions".into(), Value::String(system_prompt));
    }
    body.insert("input".into(), Value::Array(input));
    if !tools.is_empty() {
        let tools = tools
            .into_iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                    "strict": false,
                })
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
    }
    body.insert("stream".into(), Value::Bool(true));
    body.insert("store".into(), Value::Bool(false));
    if let Some(max) = max_output_tokens {
        body.insert("max_output_tokens".into(), json!(max));
    }
    // The level goes out as named. The adapter does not guess a
    // server's range: a server that refuses the level says so.
    if let Some(effort) = effort {
        body.insert("reasoning".into(), json!({"effort": effort.as_str()}));
    }
    Value::Object(body)
}

/// One tool call in flight, by its item key.
#[derive(Default)]
struct Call {
    /// The id the call's result answers; distinct from the item id.
    /// Empty until the wire names it.
    call_id: String,
    name: String,
    /// Every argument fragment, joined.
    arguments: String,
    /// The start event went out.
    started: bool,
}

/// Which stream supplies an item's readable reasoning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Summary,
    Text,
}

/// The readable text of one reasoning item in flight.
#[derive(Default)]
struct Thought {
    text: String,
    /// The first stream that supplied text; the other one is not
    /// shown, so a server that sends both shows the text once.
    source: Option<Source>,
}

/// The item an event belongs to: its output index, which every
/// documented event carries, or its id for a server that sends no
/// index.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ItemKey {
    Index(u64),
    Id(String),
}

impl ItemKey {
    /// The key of an event with `output_index` and item id `id`.
    fn of(output_index: &Value, id: &Value) -> Self {
        match output_index.as_u64() {
            Some(index) => ItemKey::Index(index),
            None => ItemKey::Id(id.as_str().unwrap_or("").to_string()),
        }
    }
}

/// State across one response.
#[derive(Default)]
struct StreamState {
    calls: HashMap<ItemKey, Call>,
    thoughts: HashMap<ItemKey, Thought>,
    /// The part of the open reasoning item that the last text came
    /// from: source and index. Text from another part opens a
    /// paragraph.
    last_part: Option<(Source, u64)>,
    /// The message items that showed text or a refusal.
    shown_messages: HashSet<ItemKey>,
    /// The message item that showed text last, while no other item
    /// came between. Text from the next message item opens a
    /// paragraph, so a preamble and an answer stay apart.
    last_message: Option<ItemKey>,
    /// A tool call ended with its complete input.
    saw_call: bool,
    /// The reply declined in refusal content.
    refused: bool,
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
        ProviderError::Protocol("stream ended before response.completed".to_string())
    }

    fn counts(&self) -> &EventCounts {
        &self.counts
    }
}

impl StreamState {
    /// Show message text from item `key`.
    fn message_text(
        &mut self,
        key: &ItemKey,
        text: &str,
        out: &mut Vec<Result<StreamEvent, ProviderError>>,
    ) {
        if text.is_empty() {
            return;
        }
        let mut delta = String::new();
        if self.last_message.as_ref() != Some(key) {
            if self.last_message.is_some() {
                delta.push_str("\n\n");
            }
            self.last_message = Some(key.clone());
            self.shown_messages.insert(key.clone());
        }
        delta.push_str(text);
        out.push(Ok(StreamEvent::TextDelta(delta)));
    }

    /// End a message item. An item whose text never streamed shows the
    /// text it holds; a refusal part ends the turn as a refusal.
    fn end_message(
        &mut self,
        key: &ItemKey,
        item: &Value,
        out: &mut Vec<Result<StreamEvent, ProviderError>>,
    ) {
        let mut unshown = String::new();
        for part in item["content"].as_array().into_iter().flatten() {
            let text = match part["type"].as_str() {
                Some("output_text") => part["text"].as_str(),
                Some("refusal") => {
                    self.refused = true;
                    part["refusal"].as_str()
                }
                _ => None,
            };
            if let Some(text) = text.filter(|text| !text.is_empty()) {
                if !unshown.is_empty() {
                    unshown.push_str("\n\n");
                }
                unshown.push_str(text);
            }
        }
        if !self.shown_messages.contains(key) {
            self.message_text(key, &unshown, out);
        }
    }

    /// Show reasoning text from one part of item `key`. The first
    /// source that supplies an item's text is the one shown.
    fn reasoning_text(
        &mut self,
        key: &ItemKey,
        source: Source,
        index: u64,
        text: &str,
        out: &mut Vec<Result<StreamEvent, ProviderError>>,
    ) {
        if text.is_empty() {
            return;
        }
        self.last_message = None;
        // One key clone per item, not per delta.
        if !self.thoughts.contains_key(key) {
            self.thoughts.insert(key.clone(), Thought::default());
        }
        let thought = self.thoughts.get_mut(key).expect("inserted above");
        if *thought.source.get_or_insert(source) != source {
            return;
        }
        let mut delta = String::new();
        if self.last_part != Some((source, index)) {
            // A new part of an item with shown text is a paragraph of
            // its own. Each item is a block of its own, so its first
            // part needs no break.
            if !thought.text.is_empty() {
                delta.push_str("\n\n");
            }
            self.last_part = Some((source, index));
        }
        delta.push_str(text);
        thought.text.push_str(&delta);
        out.push(Ok(StreamEvent::ReasoningDelta(delta)));
    }

    /// End reasoning item `key`: seal the complete item with this
    /// instance and model. An item whose text never streamed shows the
    /// text the item holds.
    fn end_reasoning(
        &mut self,
        key: &ItemKey,
        item: Value,
        out: &mut Vec<Result<StreamEvent, ProviderError>>,
    ) {
        if self
            .thoughts
            .get(key)
            .is_none_or(|thought| thought.source.is_none())
        {
            for (source, field, kind) in [
                (Source::Summary, "summary", "summary_text"),
                (Source::Text, "content", "reasoning_text"),
            ] {
                let parts = item[field].as_array().into_iter().flatten();
                for (index, part) in parts.enumerate() {
                    if part["type"].as_str().is_none_or(|t| t == kind)
                        && let Some(text) = part["text"].as_str()
                    {
                        self.reasoning_text(key, source, index as u64, text, out);
                    }
                }
            }
        }
        let summary = self
            .thoughts
            .remove(key)
            .map(|thought| thought.text)
            .unwrap_or_default();
        self.last_part = None;
        self.last_message = None;
        // The item moves into the payload: its encrypted content can
        // run to kilobytes.
        let sealed = SealedPayload::new(PROVIDER_ID, item)
            .with_identity(self.instance.clone(), self.model.clone());
        out.push(Ok(StreamEvent::ReasoningDone {
            summary,
            sealed: Some(sealed),
        }));
    }

    /// The call of item `key`, opened on first sight: fragments can
    /// arrive before the item's own event.
    fn call_mut(&mut self, key: &ItemKey) -> &mut Call {
        self.last_message = None;
        // One key clone per item, not per fragment.
        if !self.calls.contains_key(key) {
            self.calls.insert(key.clone(), Call::default());
        }
        self.calls.get_mut(key).expect("inserted above")
    }

    /// Open the call of item `key`. The start goes out once the wire
    /// names both the call id and the tool.
    fn start_call(
        &mut self,
        key: &ItemKey,
        item: &Value,
        out: &mut Vec<Result<StreamEvent, ProviderError>>,
    ) {
        let call = self.call_mut(key);
        if let Some(call_id) = item["call_id"].as_str().filter(|id| !id.is_empty()) {
            call.call_id = call_id.to_string();
        }
        if let Some(name) = item["name"].as_str().filter(|name| !name.is_empty()) {
            call.name = name.to_string();
        }
        if let Some(arguments) = item["arguments"].as_str() {
            call.arguments.push_str(arguments);
        }
        if !call.started && !call.call_id.is_empty() && !call.name.is_empty() {
            call.started = true;
            out.push(Ok(StreamEvent::ToolCallStart {
                id: call.call_id.clone(),
                name: call.name.clone(),
            }));
        }
    }

    /// End the call of item `key` with the complete item's input.
    fn end_call(
        &mut self,
        key: &ItemKey,
        item: &Value,
        out: &mut Vec<Result<StreamEvent, ProviderError>>,
    ) {
        self.last_message = None;
        let mut call = self.calls.remove(key).unwrap_or_default();
        if !call.started {
            if let Some(call_id) = item["call_id"].as_str().filter(|id| !id.is_empty()) {
                call.call_id = call_id.to_string();
            }
            if call.call_id.is_empty() {
                // No call id on the wire: the item id stands in, or a
                // name from the output index.
                call.call_id = match (item["id"].as_str().filter(|id| !id.is_empty()), key) {
                    (Some(id), _) => id.to_string(),
                    (None, ItemKey::Index(index)) => format!("call_{index}"),
                    (None, ItemKey::Id(id)) => id.clone(),
                };
            }
            if let Some(name) = item["name"].as_str().filter(|name| !name.is_empty()) {
                call.name = name.to_string();
            }
            out.push(Ok(StreamEvent::ToolCallStart {
                id: call.call_id.clone(),
                name: std::mem::take(&mut call.name),
            }));
        }
        // The complete item is authoritative; the joined fragments
        // stand in for a server that leaves its arguments empty.
        let arguments = item["arguments"]
            .as_str()
            .filter(|arguments| !arguments.trim().is_empty())
            .unwrap_or(&call.arguments);
        let input = if arguments.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(arguments).unwrap_or_else(|_| Value::String(arguments.into()))
        };
        self.saw_call = true;
        out.push(Ok(StreamEvent::ToolCallEnd {
            id: call.call_id,
            input,
        }));
    }

    /// Drop the calls that never completed: a cut call never runs.
    fn drop_partial_calls(&mut self) {
        for call in std::mem::take(&mut self.calls).into_values() {
            log::warn!(
                "tool call {} ({}) did not complete; it does not run",
                call.call_id,
                call.name
            );
        }
    }

    /// Fail the call with `failure`, as the terminal event.
    fn fail(&mut self, failure: ProviderError) -> Vec<Result<StreamEvent, ProviderError>> {
        self.terminal = true;
        self.drop_partial_calls();
        vec![Err(failure)]
    }
}

/// Map one SSE payload to neutral events. Unknown events map to
/// nothing.
fn map_payload(payload: &str, state: &mut StreamState) -> Vec<Result<StreamEvent, ProviderError>> {
    if payload.trim() == "[DONE]" {
        // A Chat Completions end marker, not a Responses terminal.
        state.counts.count("[DONE]");
        return state.fail(state.eof_error());
    }
    let Ok(mut value) = serde_json::from_str::<Value>(payload) else {
        return state.fail(ProviderError::Protocol(format!(
            "sse payload is not JSON: {payload}"
        )));
    };
    let kind = value["type"].as_str().unwrap_or("");
    state.counts.count(kind);
    let mut out = Vec::new();
    let delta = value["delta"].as_str().unwrap_or("");
    match kind {
        "response.output_text.delta" => {
            let key = ItemKey::of(&value["output_index"], &value["item_id"]);
            state.message_text(&key, delta, &mut out);
        }
        // A refusal streams as text the user reads, and ends the turn
        // as a refusal whatever the terminal event says.
        "response.refusal.delta" => {
            state.refused = true;
            let key = ItemKey::of(&value["output_index"], &value["item_id"]);
            state.message_text(&key, delta, &mut out);
        }
        "response.refusal.done" => {
            state.refused = true;
            let key = ItemKey::of(&value["output_index"], &value["item_id"]);
            if !state.shown_messages.contains(&key) {
                let refusal = value["refusal"].as_str().unwrap_or("");
                state.message_text(&key, refusal, &mut out);
            }
        }
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            let key = ItemKey::of(&value["output_index"], &value["item_id"]);
            let (source, index) = if kind == "response.reasoning_text.delta" {
                (Source::Text, &value["content_index"])
            } else {
                (Source::Summary, &value["summary_index"])
            };
            let index = index.as_u64().unwrap_or(0);
            state.reasoning_text(&key, source, index, delta, &mut out);
        }
        "response.output_item.added" => {
            let item = &value["item"];
            if item["type"] == "function_call" {
                let key = ItemKey::of(&value["output_index"], &item["id"]);
                state.start_call(&key, item, &mut out);
            }
        }
        "response.function_call_arguments.delta" => {
            let key = ItemKey::of(&value["output_index"], &value["item_id"]);
            let call = state.call_mut(&key);
            if !delta.is_empty() {
                call.arguments.push_str(delta);
                if call.started {
                    out.push(Ok(StreamEvent::ToolCallDelta {
                        id: call.call_id.clone(),
                        input_fragment: delta.to_string(),
                    }));
                }
            }
        }
        // The whole arguments, for a server that sends the call in one
        // piece instead of fragments.
        "response.function_call_arguments.done" => {
            let key = ItemKey::of(&value["output_index"], &value["item_id"]);
            if let Some(arguments) = value["arguments"].as_str().filter(|a| !a.is_empty()) {
                let call = state.call_mut(&key);
                call.arguments.clear();
                call.arguments.push_str(arguments);
            }
        }
        "response.output_item.done" => {
            let item = value["item"].take();
            let key = ItemKey::of(&value["output_index"], &item["id"]);
            match item["type"].as_str().unwrap_or("") {
                "function_call" => state.end_call(&key, &item, &mut out),
                "reasoning" => state.end_reasoning(&key, item, &mut out),
                "message" => state.end_message(&key, &item, &mut out),
                _ => {}
            }
        }
        "response.completed" => {
            state.terminal = true;
            state.drop_partial_calls();
            let stop = if state.refused {
                StopReason::Refusal
            } else if state.saw_call {
                StopReason::ToolUse
            } else {
                StopReason::EndTurn
            };
            log::info!(
                "stream ended: response.completed stop_reason={}",
                stop.as_str()
            );
            out.extend(
                usage_from(&value["response"]["usage"])
                    .map(StreamEvent::Usage)
                    .map(Ok),
            );
            out.push(Ok(StreamEvent::Done(stop)));
        }
        "response.incomplete" => {
            let reason = value["response"]["incomplete_details"]["reason"]
                .as_str()
                .unwrap_or("");
            let stop = match reason {
                _ if state.refused => StopReason::Refusal,
                "max_output_tokens" => StopReason::MaxTokens,
                "content_filter" => StopReason::Refusal,
                _ => {
                    log::warn!("stream failed: response.incomplete reason={reason}");
                    return state.fail(ProviderError::Server(format!(
                        "the response ended incomplete for an unknown reason: `{reason}`"
                    )));
                }
            };
            state.terminal = true;
            state.drop_partial_calls();
            log::info!("stream ended: response.incomplete reason={reason}");
            out.extend(
                usage_from(&value["response"]["usage"])
                    .map(StreamEvent::Usage)
                    .map(Ok),
            );
            out.push(Ok(StreamEvent::Done(stop)));
        }
        "response.failed" => {
            let error = &value["response"]["error"];
            return state.fail(failure("response.failed", error, &state.secret));
        }
        "error" => {
            // The top-level error event carries its fields inline;
            // some servers nest them under `error`.
            let error = value
                .get("error")
                .filter(|e| e.is_object())
                .unwrap_or(&value);
            return state.fail(failure("error", error, &state.secret));
        }
        _ => {}
    }
    out
}

/// The provider error for a failure event's error object. The message
/// is server text and is redacted.
fn failure(event: &str, error: &Value, secret: &str) -> ProviderError {
    let code = error["code"].as_str().unwrap_or("-");
    let message = redact(error["message"].as_str().unwrap_or(event), secret);
    log::warn!("stream failed: {event} code={code} message={message}");
    if code == "rate_limit_exceeded" || error["type"] == "rate_limit_error" {
        ProviderError::RateLimited { retry_after: None }
    } else {
        ProviderError::Server(message)
    }
}

/// The neutral usage for a response's `usage` block; `None` when the
/// server sent none. The wire `input_tokens` already includes the
/// cached prompt parts; `cached_tokens` and `cache_write_tokens` are
/// the subset details. A server without a write count reports none.
fn usage_from(usage: &Value) -> Option<Usage> {
    usage.is_object().then(|| Usage {
        input_tokens: usage["input_tokens"].as_u64().unwrap_or(0),
        output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
        cache_read_tokens: usage["input_tokens_details"]["cached_tokens"]
            .as_u64()
            .unwrap_or(0),
        cache_creation_tokens: usage["input_tokens_details"]["cache_write_tokens"]
            .as_u64()
            .unwrap_or(0),
    })
}
