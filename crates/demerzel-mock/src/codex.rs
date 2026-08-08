//! Fixture builders for the Responses-API event shapes the adapter
//! consumes. Test and bench scripts build replies from these instead
//! of re-deriving the wire shapes.

use serde_json::{Value, json};

use crate::MockResponse;

/// A text-delta stream event.
pub fn text_delta(text: &str) -> Value {
    json!({"type": "response.output_text.delta", "delta": text})
}

/// A completed event with fixed usage numbers.
pub fn completed() -> Value {
    json!({
        "type": "response.completed",
        "response": {"usage": {
            "input_tokens": 10,
            "output_tokens": 5,
            "input_tokens_details": {"cached_tokens": 2},
        }},
    })
}

/// A reply that streams `text` and completes.
pub fn text_turn(text: &str) -> MockResponse {
    MockResponse::stream(vec![text_delta(text), completed()])
}

/// A reply that emits one complete tool call and completes.
pub fn tool_turn(call_id: &str, name: &str, input: Value) -> MockResponse {
    MockResponse::stream(vec![
        json!({
            "type": "response.output_item.added",
            "item": {
                "type": "function_call",
                "id": format!("item_{call_id}"),
                "call_id": call_id,
                "name": name,
            },
        }),
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "call_id": call_id,
                "arguments": input.to_string(),
            },
        }),
        completed(),
    ])
}
