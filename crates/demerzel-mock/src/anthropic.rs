//! Fixture builders for the Messages-API event shapes the adapter
//! consumes. Test and bench scripts build replies from these instead
//! of re-deriving the wire shapes.

use serde_json::{Value, json};

use crate::MockResponse;

/// A message_start event with fixed usage numbers. The wire
/// `input_tokens` field excludes the cached parts, so the neutral
/// input count is 7 + 1 + 2 = 10 — the same totals as the codex
/// fixtures, with one token written into the cache.
pub fn message_start() -> Value {
    json!({
        "type": "message_start",
        "message": {"usage": {
            "input_tokens": 7,
            "cache_creation_input_tokens": 1,
            "cache_read_input_tokens": 2,
            "output_tokens": 1,
        }},
    })
}

/// One complete text block at index 0: start, one delta, stop.
pub fn text_block(text: &str) -> Vec<Value> {
    vec![
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": ""},
        }),
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": text},
        }),
        json!({"type": "content_block_stop", "index": 0}),
    ]
}

/// The closing pair: a message_delta with the stop reason and final
/// output tokens, then message_stop.
pub fn message_end(stop_reason: &str) -> Vec<Value> {
    vec![
        json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop_reason},
            "usage": {"output_tokens": 5},
        }),
        json!({"type": "message_stop"}),
    ]
}

/// A reply that streams `text` and ends the turn.
pub fn text_turn(text: &str) -> MockResponse {
    let mut events = vec![message_start()];
    events.extend(text_block(text));
    events.extend(message_end("end_turn"));
    MockResponse::stream(events)
}

/// A reply that emits one complete tool call at index 0.
pub fn tool_turn(id: &str, name: &str, input: Value) -> MockResponse {
    let mut events = vec![
        message_start(),
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}},
        }),
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "input_json_delta", "partial_json": input.to_string()},
        }),
        json!({"type": "content_block_stop", "index": 0}),
    ];
    events.extend(message_end("tool_use"));
    MockResponse::stream(events)
}
