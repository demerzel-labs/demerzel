//! Fixture builders for the Responses stream shapes the
//! `openai-responses` adapter consumes. Test and bench scripts build
//! replies from these instead of re-deriving the wire shapes.
//!
//! The shapes follow the public Responses streaming reference: named
//! item events around each output item, a function call whose item id
//! differs from its call id, arguments in fragments, reasoning items
//! that carry encrypted content, and `response.completed` with usage.

use serde_json::{Value, json};

use crate::MockResponse;

/// The completed event with fixed usage numbers: the same totals as
/// the other fixtures, 10 input tokens with 2 read from the cache, 5
/// output.
pub fn completed() -> Value {
    json!({
        "type": "response.completed",
        "response": {
            "id": "resp_mock",
            "object": "response",
            "status": "completed",
            "usage": {
                "input_tokens": 10,
                "input_tokens_details": {"cached_tokens": 2},
                "output_tokens": 5,
                "output_tokens_details": {"reasoning_tokens": 1},
                "total_tokens": 15,
            },
        },
    })
}

/// The events of one assistant message item that streams `text`.
pub fn message_events(item_id: &str, text: &str) -> Vec<Value> {
    let (head, tail) = text.split_at(text.len() / 2);
    let item = |status: &str, content: Value| {
        json!({
            "type": "message",
            "id": item_id,
            "status": status,
            "role": "assistant",
            "content": content,
        })
    };
    vec![
        json!({"type": "response.output_item.added", "output_index": 0,
               "item": item("in_progress", json!([]))}),
        json!({"type": "response.output_text.delta", "item_id": item_id,
               "output_index": 0, "content_index": 0, "delta": head}),
        json!({"type": "response.output_text.delta", "item_id": item_id,
               "output_index": 0, "content_index": 0, "delta": tail}),
        json!({"type": "response.output_item.done", "output_index": 0,
               "item": item("completed", json!([{"type": "output_text", "text": text}]))}),
    ]
}

/// The events of one function call item: the item id is `fc_` and the
/// call id, and the arguments stream in two fragments.
pub fn call_events(call_id: &str, name: &str, input: &Value) -> Vec<Value> {
    let item_id = format!("fc_{call_id}");
    let arguments = input.to_string();
    let (head, tail) = arguments.split_at(arguments.len() / 2);
    let item = |status: &str, arguments: &str| {
        json!({
            "type": "function_call",
            "id": item_id,
            "call_id": call_id,
            "name": name,
            "arguments": arguments,
            "status": status,
        })
    };
    vec![
        json!({"type": "response.output_item.added", "output_index": 1,
               "item": item("in_progress", "")}),
        json!({"type": "response.function_call_arguments.delta", "item_id": item_id,
               "output_index": 1, "delta": head}),
        json!({"type": "response.function_call_arguments.delta", "item_id": item_id,
               "output_index": 1, "delta": tail}),
        json!({"type": "response.function_call_arguments.done", "item_id": item_id,
               "output_index": 1, "arguments": arguments}),
        json!({"type": "response.output_item.done", "output_index": 1,
               "item": item("completed", &arguments)}),
    ]
}

/// The complete reasoning item: `summary` as one part, and an opaque
/// `encrypted` blob.
pub fn reasoning_item(item_id: &str, summary: &str, encrypted: &str) -> Value {
    json!({
        "type": "reasoning",
        "id": item_id,
        "summary": [{"type": "summary_text", "text": summary}],
        "encrypted_content": encrypted,
    })
}

/// The events of one reasoning item whose summary streams in two
/// deltas.
pub fn reasoning_events(item_id: &str, summary: &str, encrypted: &str) -> Vec<Value> {
    let (head, tail) = summary.split_at(summary.len() / 2);
    vec![
        json!({"type": "response.output_item.added", "output_index": 0,
               "item": {"type": "reasoning", "id": item_id, "summary": []}}),
        json!({"type": "response.reasoning_summary_part.added", "item_id": item_id,
               "output_index": 0, "summary_index": 0,
               "part": {"type": "summary_text", "text": ""}}),
        json!({"type": "response.reasoning_summary_text.delta", "item_id": item_id,
               "output_index": 0, "summary_index": 0, "delta": head}),
        json!({"type": "response.reasoning_summary_text.delta", "item_id": item_id,
               "output_index": 0, "summary_index": 0, "delta": tail}),
        json!({"type": "response.output_item.done", "output_index": 0,
               "item": reasoning_item(item_id, summary, encrypted)}),
    ]
}

/// A stream of `events`, opened by `response.created` and closed by
/// `response.completed`.
pub fn closed(events: Vec<Value>) -> MockResponse {
    let mut all = vec![json!({
        "type": "response.created",
        "response": {"id": "resp_mock", "object": "response", "status": "in_progress"},
    })];
    all.extend(events);
    all.push(completed());
    MockResponse::stream(all)
}

/// A reply that streams `text` and completes.
pub fn text_turn(text: &str) -> MockResponse {
    closed(message_events("msg_mock", text))
}

/// A reply that streams one tool call in fragments.
pub fn tool_turn(call_id: &str, name: &str, input: Value) -> MockResponse {
    closed(call_events(call_id, name, &input))
}

/// A reply that reasons, then streams one tool call.
pub fn reasoning_tool_turn(
    summary: &str,
    encrypted: &str,
    call_id: &str,
    name: &str,
    input: Value,
) -> MockResponse {
    let mut events = reasoning_events("rs_mock", summary, encrypted);
    events.extend(call_events(call_id, name, &input));
    closed(events)
}
