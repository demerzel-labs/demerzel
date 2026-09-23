//! Fixture builders for the Chat Completions stream shapes the
//! `openai-compat` adapter consumes. Test and bench scripts build
//! replies from these instead of re-deriving the wire shapes.
//!
//! The shapes follow the Chat Completions streaming reference: each
//! chunk is a `chat.completion.chunk` with one choice, a usage chunk
//! with empty `choices` follows the finish chunk, and `[DONE]` ends
//! the stream.

use serde_json::{Value, json};

use crate::{MockResponse, SseEvent};

/// One chunk whose only choice carries `delta`.
pub fn chunk(delta: Value) -> Value {
    json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion.chunk",
        "created": 1_758_000_000,
        "model": "mock-model",
        "choices": [{"index": 0, "delta": delta, "finish_reason": null}],
    })
}

/// The finish chunk: an empty delta and the reason.
pub fn finish(reason: &str) -> Value {
    json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion.chunk",
        "created": 1_758_000_000,
        "model": "mock-model",
        "choices": [{"index": 0, "delta": {}, "finish_reason": reason}],
    })
}

/// The usage chunk with fixed numbers: the same totals as the other
/// fixtures, 10 prompt tokens with 2 read from the cache, 5 output.
pub fn usage() -> Value {
    json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion.chunk",
        "created": 1_758_000_000,
        "model": "mock-model",
        "choices": [],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15,
            "prompt_tokens_details": {"cached_tokens": 2},
        },
    })
}

/// The `[DONE]` marker that ends every stream.
pub fn done() -> SseEvent {
    SseEvent::raw("[DONE]")
}

/// A stream of JSON chunks, closed by the usage chunk and `[DONE]`.
pub fn closed(mut chunks: Vec<Value>) -> MockResponse {
    chunks.push(usage());
    let mut events: Vec<SseEvent> = chunks.into_iter().map(SseEvent::new).collect();
    events.push(done());
    MockResponse::events(events)
}

/// A reply that streams `text` and stops.
pub fn text_turn(text: &str) -> MockResponse {
    closed(vec![
        chunk(json!({"role": "assistant", "content": ""})),
        chunk(json!({"content": text})),
        finish("stop"),
    ])
}

/// The fragments of one tool call at wire index `index`: the first
/// carries the id, the type, and the name; the arguments follow in
/// two pieces.
pub fn tool_call_fragments(index: u64, id: &str, name: &str, input: &Value) -> Vec<Value> {
    let arguments = input.to_string();
    let (head, tail) = arguments.split_at(arguments.len() / 2);
    vec![
        chunk(json!({"tool_calls": [{
            "index": index,
            "id": id,
            "type": "function",
            "function": {"name": name, "arguments": ""},
        }]})),
        chunk(json!({"tool_calls": [{"index": index, "function": {"arguments": head}}]})),
        chunk(json!({"tool_calls": [{"index": index, "function": {"arguments": tail}}]})),
    ]
}

/// A reply that streams one tool call in fragments.
pub fn tool_turn(id: &str, name: &str, input: Value) -> MockResponse {
    let mut chunks = vec![chunk(json!({"role": "assistant", "content": null}))];
    chunks.extend(tool_call_fragments(0, id, name, &input));
    chunks.push(finish("tool_calls"));
    closed(chunks)
}

/// A reply that reasons in `field` — `reasoning_content` or
/// `reasoning` — then streams one tool call.
pub fn reasoning_tool_turn(
    field: &str,
    reasoning: &str,
    id: &str,
    name: &str,
    input: Value,
) -> MockResponse {
    let mut chunks = vec![chunk(json!({"role": "assistant", "content": null}))];
    let (head, tail) = reasoning.split_at(reasoning.len() / 2);
    chunks.push(chunk(json!({ field: head })));
    chunks.push(chunk(json!({ field: tail })));
    chunks.extend(tool_call_fragments(0, id, name, &input));
    chunks.push(finish("tool_calls"));
    closed(chunks)
}
