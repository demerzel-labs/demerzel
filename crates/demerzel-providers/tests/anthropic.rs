//! Conformance tests for the anthropic adapter against the mock
//! Messages endpoint. Hermetic: no network, no model.

use std::future::poll_fn;
use std::time::Duration;

use demerzel_core::{ContentBlock, Effort, Message, Role, SealedPayload, StopReason};
use demerzel_mock::{MockResponse, MockServer, anthropic as fixtures};
use demerzel_providers::anthropic::{Anthropic, AnthropicConfig};
use demerzel_providers::{
    EventStream, Provider, ProviderError, Request, StreamEvent, ToolDefinition,
};
use serde_json::json;

fn provider_for(mock: &MockServer) -> Anthropic {
    let config = AnthropicConfig {
        base_url: mock.url(),
        max_attempts: 3,
        base_delay: Duration::from_millis(1),
        ..AnthropicConfig::default()
    };
    Anthropic::new(config, "sk-test-key".to_string())
}

fn request(messages: Vec<Message>, tools: Vec<ToolDefinition>) -> Request {
    let mut request = Request::new("claude-test");
    request.system_prompt = "You are dmz.".to_string();
    request.messages = messages;
    request.tools = tools;
    request
}

async fn collect(mut stream: EventStream) -> Vec<Result<StreamEvent, ProviderError>> {
    let mut out = Vec::new();
    while let Some(item) = poll_fn(|cx| stream.as_mut().poll_next(cx)).await {
        out.push(item);
    }
    out
}

#[tokio::test]
async fn the_request_effort_lands_in_output_config() {
    let done = || {
        MockResponse::stream(vec![
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 1, "output_tokens": 0}}}),
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 1}}),
            json!({"type": "message_stop"}),
        ])
    };
    let mock = MockServer::start(vec![done(), done(), done(), done()], vec![]).await;
    let provider = provider_for(&mock);

    for effort in [
        Some(Effort::Low),
        None,
        Some(Effort::Ultra),
        Some(Effort::None),
    ] {
        let mut request = request(Vec::new(), Vec::new());
        request.effort = effort;
        collect(provider.stream(request).await.unwrap()).await;
    }

    let received = mock.received();
    assert_eq!(received[0].body["output_config"]["effort"], "low");
    assert_eq!(received[0].body["thinking"]["type"], "adaptive");
    assert!(received[1].body.get("output_config").is_none());
    // The Messages API takes `low` through `max`: the ends clamp.
    assert_eq!(received[2].body["output_config"]["effort"], "max");
    assert_eq!(received[3].body["output_config"]["effort"], "low");
}

#[tokio::test]
async fn serializes_the_request_per_the_messages_contract() {
    let mock = MockServer::start(vec![fixtures::text_turn("ok")], vec![]).await;
    let provider = provider_for(&mock);

    let sealed_block = json!({
        "type": "thinking",
        "thinking": "check the manifest first",
        "signature": "sig-abc",
    });
    let messages = vec![
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: "hi".into() }],
        },
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Reasoning {
                    summary: "check the manifest first".into(),
                    sealed: Some(SealedPayload::new("anthropic", sealed_block.clone())),
                },
                ContentBlock::Reasoning {
                    summary: "foreign".into(),
                    sealed: Some(SealedPayload::new(
                        "openai-codex",
                        json!({"encrypted_content": "zzz"}),
                    )),
                },
                ContentBlock::Text { text: "ok".into() },
                ContentBlock::ToolUse {
                    id: "toolu_1".into(),
                    name: "read".into(),
                    input: json!({"path": "Cargo.toml"}),
                },
            ],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "toolu_1".into(),
                content: "a1|[workspace]".into(),
                is_error: false,
            }],
        },
        // A repaired turn can put two user messages in a row; the wire
        // body must merge them into one strict-alternation turn.
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "carry on".into(),
            }],
        },
    ];
    let tools = vec![ToolDefinition {
        name: "read".into(),
        description: "Read a file".into(),
        input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
    }];

    let stream = provider.stream(request(messages, tools)).await.unwrap();
    collect(stream).await;

    let received = mock.received();
    assert_eq!(received.len(), 1);
    let call = &received[0];
    assert_eq!(call.path, "/v1/messages");
    assert_eq!(call.api_key.as_deref(), Some("sk-test-key"));
    assert_eq!(call.anthropic_version.as_deref(), Some("2023-06-01"));
    assert_eq!(call.content_type.as_deref(), Some("application/json"));
    assert_eq!(call.authorization, None);

    let body = &call.body;
    assert_eq!(body["model"], "claude-test");
    assert_eq!(body["stream"], true);
    assert!(body["max_tokens"].as_u64().unwrap() > 0);
    assert_eq!(body["system"][0]["type"], "text");
    assert_eq!(body["system"][0]["text"], "You are dmz.");
    assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(body["thinking"]["type"], "adaptive");
    assert_eq!(body["tools"][0]["name"], "read");
    assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
    assert_eq!(body["tools"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(body["tool_choice"]["type"], "auto");
    assert_eq!(body["tool_choice"]["disable_parallel_tool_use"], true);

    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3, "consecutive user turns must merge");
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"][0]["type"], "text");

    let assistant = messages[1]["content"].as_array().unwrap();
    assert_eq!(assistant.len(), 3, "foreign sealed reasoning must drop");
    assert_eq!(
        assistant[0], sealed_block,
        "sealed thinking must round-trip verbatim"
    );
    assert_eq!(assistant[1]["type"], "text");
    assert_eq!(assistant[2]["type"], "tool_use");
    assert_eq!(assistant[2]["id"], "toolu_1");
    assert_eq!(assistant[2]["input"], json!({"path": "Cargo.toml"}));

    let tail = messages[2]["content"].as_array().unwrap();
    assert_eq!(tail[0]["type"], "tool_result");
    assert_eq!(tail[0]["tool_use_id"], "toolu_1");
    assert_eq!(tail[0]["is_error"], false);
    assert_eq!(tail[1]["type"], "text");
    assert_eq!(
        tail[1]["cache_control"]["type"], "ephemeral",
        "the history tail carries the moving cache breakpoint"
    );
    assert!(
        tail[0].get("cache_control").is_none(),
        "only the last block carries the tail breakpoint"
    );
}

#[tokio::test]
async fn assembles_the_stream_into_neutral_events() {
    let events = vec![
        fixtures::message_start(),
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "thinking", "thinking": ""},
        }),
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "thinking_delta", "thinking": "thin"},
        }),
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "thinking_delta", "thinking": "king"},
        }),
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "signature_delta", "signature": "sig-1"},
        }),
        json!({"type": "content_block_stop", "index": 0}),
        json!({
            "type": "content_block_start",
            "index": 1,
            "content_block": {"type": "tool_use", "id": "toolu_9", "name": "read", "input": {}},
        }),
        json!({
            "type": "content_block_delta",
            "index": 1,
            "delta": {"type": "input_json_delta", "partial_json": "{\"pa"},
        }),
        json!({
            "type": "content_block_delta",
            "index": 1,
            "delta": {"type": "input_json_delta", "partial_json": "th\":\"x\"}"},
        }),
        json!({"type": "content_block_stop", "index": 1}),
        json!({
            "type": "content_block_start",
            "index": 2,
            "content_block": {"type": "text", "text": ""},
        }),
        json!({
            "type": "content_block_delta",
            "index": 2,
            "delta": {"type": "text_delta", "text": "Hello "},
        }),
        json!({
            "type": "content_block_delta",
            "index": 2,
            "delta": {"type": "text_delta", "text": "world"},
        }),
        json!({"type": "content_block_stop", "index": 2}),
        json!({"type": "future_unknown_event", "mystery": true}),
        json!({"type": "ping"}),
        json!({
            "type": "message_delta",
            "delta": {"stop_reason": "tool_use"},
            "usage": {"output_tokens": 20},
        }),
        json!({"type": "message_stop"}),
    ];
    let mock = MockServer::start(vec![MockResponse::stream(events)], vec![]).await;
    let provider = provider_for(&mock);

    let stream = provider
        .stream(request(Vec::new(), Vec::new()))
        .await
        .unwrap();
    let events: Vec<StreamEvent> = collect(stream)
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();

    use StreamEvent::*;
    match &events[..] {
        [
            ReasoningDelta(r1),
            ReasoningDelta(r2),
            ReasoningDone { summary, sealed },
            ToolCallStart { id: s_id, name },
            ToolCallDelta { id: d1, .. },
            ToolCallDelta { id: d2, .. },
            ToolCallEnd { id: e_id, input },
            TextDelta(t1),
            TextDelta(t2),
            Usage(usage),
            Done(stop),
        ] => {
            assert_eq!(r1, "thin");
            assert_eq!(r2, "king");
            assert_eq!(summary, "thinking");
            let sealed = sealed.as_ref().unwrap();
            assert_eq!(sealed.provider, "anthropic");
            assert_eq!(sealed.data["type"], "thinking");
            assert_eq!(sealed.data["thinking"], "thinking");
            assert_eq!(sealed.data["signature"], "sig-1");
            assert_eq!(s_id, "toolu_9");
            assert_eq!(name, "read");
            assert_eq!(d1, "toolu_9");
            assert_eq!(d2, "toolu_9");
            assert_eq!(e_id, "toolu_9");
            assert_eq!(input, &json!({"path": "x"}));
            assert_eq!(t1, "Hello ");
            assert_eq!(t2, "world");
            assert_eq!(usage.input_tokens, 10);
            assert_eq!(usage.output_tokens, 20);
            assert_eq!(usage.cache_read_tokens, 2);
            assert_eq!(usage.cache_creation_tokens, 1);
            assert_eq!(stop, &StopReason::ToolUse);
        }
        other => panic!("unexpected event sequence: {other:?}"),
    }
}

#[tokio::test]
async fn a_redacted_thinking_block_round_trips_verbatim() {
    let events = vec![
        fixtures::message_start(),
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "redacted_thinking", "data": "opaque-bytes"},
        }),
        json!({"type": "content_block_stop", "index": 0}),
        json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn"},
            "usage": {"output_tokens": 3},
        }),
        json!({"type": "message_stop"}),
    ];
    let mock = MockServer::start(vec![MockResponse::stream(events)], vec![]).await;
    let provider = provider_for(&mock);

    let stream = provider
        .stream(request(Vec::new(), Vec::new()))
        .await
        .unwrap();
    let events: Vec<StreamEvent> = collect(stream)
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();

    let StreamEvent::ReasoningDone { summary, sealed } = &events[0] else {
        panic!("expected ReasoningDone, got {events:?}");
    };
    assert_eq!(summary, "");
    let sealed = sealed.as_ref().unwrap().clone();
    assert_eq!(sealed.data["type"], "redacted_thinking");
    assert_eq!(sealed.data["data"], "opaque-bytes");

    // The block goes back onto the wire unchanged.
    let history = vec![
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: "go".into() }],
        },
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Reasoning {
                    summary: String::new(),
                    sealed: Some(sealed),
                },
                ContentBlock::Text {
                    text: "done".into(),
                },
            ],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "more".into(),
            }],
        },
    ];
    let mock = MockServer::start(vec![fixtures::text_turn("ok")], vec![]).await;
    let provider = provider_for(&mock);
    let stream = provider.stream(request(history, Vec::new())).await.unwrap();
    collect(stream).await;

    let body = &mock.received()[0].body;
    let assistant = body["messages"][1]["content"].as_array().unwrap();
    assert_eq!(
        assistant[0],
        json!({"type": "redacted_thinking", "data": "opaque-bytes"})
    );
}

#[tokio::test]
async fn an_empty_tool_input_parses_as_an_empty_object() {
    let events = vec![
        fixtures::message_start(),
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "tool_use", "id": "toolu_0", "name": "glob", "input": {}},
        }),
        json!({"type": "content_block_stop", "index": 0}),
        json!({
            "type": "message_delta",
            "delta": {"stop_reason": "tool_use"},
            "usage": {"output_tokens": 2},
        }),
        json!({"type": "message_stop"}),
    ];
    let mock = MockServer::start(vec![MockResponse::stream(events)], vec![]).await;
    let provider = provider_for(&mock);

    let stream = provider
        .stream(request(Vec::new(), Vec::new()))
        .await
        .unwrap();
    let events: Vec<StreamEvent> = collect(stream)
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();

    assert!(matches!(
        &events[1],
        StreamEvent::ToolCallEnd { input, .. } if input == &json!({})
    ));
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Done(StopReason::ToolUse))
    ));
}

#[tokio::test]
async fn a_refusal_does_not_end_the_turn_as_completed() {
    let mut events = vec![fixtures::message_start()];
    events.extend(fixtures::message_end("refusal"));
    let mock = MockServer::start(vec![MockResponse::stream(events)], vec![]).await;
    let provider = provider_for(&mock);

    let stream = provider
        .stream(request(Vec::new(), Vec::new()))
        .await
        .unwrap();
    let events: Vec<StreamEvent> = collect(stream)
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();

    assert!(matches!(
        events.last(),
        Some(StreamEvent::Done(StopReason::Refusal))
    ));
}

#[tokio::test]
async fn a_thinking_block_without_a_signature_does_not_seal() {
    let events = vec![
        fixtures::message_start(),
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "thinking", "thinking": ""},
        }),
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "thinking_delta", "thinking": "plan"},
        }),
        json!({"type": "content_block_stop", "index": 0}),
        json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn"},
            "usage": {"output_tokens": 3},
        }),
        json!({"type": "message_stop"}),
    ];
    let mock = MockServer::start(vec![MockResponse::stream(events)], vec![]).await;
    let provider = provider_for(&mock);

    let stream = provider
        .stream(request(Vec::new(), Vec::new()))
        .await
        .unwrap();
    let events: Vec<StreamEvent> = collect(stream)
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();

    // A sealed block without a signature would poison the session:
    // the API rejects it on replay. The summary alone comes back.
    let StreamEvent::ReasoningDone { summary, sealed } = &events[1] else {
        panic!("expected ReasoningDone, got {events:?}");
    };
    assert_eq!(summary, "plan");
    assert!(sealed.is_none());
}

#[tokio::test]
async fn retries_a_server_error_with_backoff() {
    let mock = MockServer::start(
        vec![MockResponse::error(529), fixtures::text_turn("ok")],
        vec![],
    )
    .await;
    let provider = provider_for(&mock);

    let stream = provider
        .stream(request(Vec::new(), Vec::new()))
        .await
        .unwrap();
    let events: Vec<StreamEvent> = collect(stream)
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();

    assert_eq!(mock.received().len(), 2);
    assert!(matches!(&events[0], StreamEvent::TextDelta(t) if t == "ok"));
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Done(StopReason::EndTurn))
    ));
}

#[tokio::test]
async fn honors_retry_after_between_attempts() {
    let mock = MockServer::start(
        vec![
            MockResponse::error_with_retry_after(429, 1),
            fixtures::text_turn("ok"),
        ],
        vec![],
    )
    .await;
    let provider = provider_for(&mock);

    let started = std::time::Instant::now();
    let stream = provider
        .stream(request(Vec::new(), Vec::new()))
        .await
        .unwrap();
    collect(stream).await;

    assert_eq!(mock.received().len(), 2);
    // The test config's backoff is one millisecond; a full second
    // proves the header set the wait.
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "the retry must wait the server's retry-after"
    );
}

#[tokio::test]
async fn a_signature_on_the_block_start_seals_without_deltas() {
    let events = vec![
        fixtures::message_start(),
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "thinking", "thinking": "plan", "signature": "sig-seed"},
        }),
        json!({"type": "content_block_stop", "index": 0}),
        json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn"},
            "usage": {"output_tokens": 3},
        }),
        json!({"type": "message_stop"}),
    ];
    let mock = MockServer::start(vec![MockResponse::stream(events)], vec![]).await;
    let provider = provider_for(&mock);

    let stream = provider
        .stream(request(Vec::new(), Vec::new()))
        .await
        .unwrap();
    let events: Vec<StreamEvent> = collect(stream)
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();

    let StreamEvent::ReasoningDone { summary, sealed } = &events[0] else {
        panic!("expected ReasoningDone, got {events:?}");
    };
    assert_eq!(summary, "plan");
    let sealed = sealed.as_ref().unwrap();
    assert_eq!(sealed.data["signature"], "sig-seed");
    assert_eq!(sealed.data["thinking"], "plan");
}

#[tokio::test]
async fn gives_up_after_max_attempts_on_rate_limits() {
    let mock = MockServer::start(
        vec![
            MockResponse::error(429),
            MockResponse::error(429),
            MockResponse::error(429),
        ],
        vec![],
    )
    .await;
    let provider = provider_for(&mock);

    let error = provider
        .stream(request(Vec::new(), Vec::new()))
        .await
        .err()
        .expect("must fail after max attempts");
    assert!(matches!(error, ProviderError::RateLimited { .. }));
    assert_eq!(mock.received().len(), 3);
}

#[tokio::test]
async fn fails_fast_on_401_and_points_at_the_key() {
    let mock = MockServer::start(vec![MockResponse::error(401)], vec![]).await;
    let provider = provider_for(&mock);

    let error = provider
        .stream(request(Vec::new(), Vec::new()))
        .await
        .err()
        .expect("a 401 must not retry");
    assert!(matches!(error, ProviderError::Auth(_)));
    assert!(error.to_string().contains("ANTHROPIC_API_KEY"));
    assert_eq!(mock.received().len(), 1, "no retry and no refresh");
}

#[tokio::test]
async fn an_error_event_mid_stream_becomes_a_provider_error() {
    let events = vec![
        fixtures::message_start(),
        json!({
            "type": "error",
            "error": {"type": "overloaded_error", "message": "Overloaded"},
        }),
    ];
    let mock = MockServer::start(vec![MockResponse::stream(events)], vec![]).await;
    let provider = provider_for(&mock);

    let stream = provider
        .stream(request(Vec::new(), Vec::new()))
        .await
        .unwrap();
    let events = collect(stream).await;
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0], Err(ProviderError::Server(m)) if m == "Overloaded"));
}

#[tokio::test]
async fn a_stream_cut_before_message_stop_is_a_protocol_error() {
    let events = vec![fixtures::message_start()];
    let mock = MockServer::start(vec![MockResponse::stream(events)], vec![]).await;
    let provider = provider_for(&mock);

    let stream = provider
        .stream(request(Vec::new(), Vec::new()))
        .await
        .unwrap();
    let events = collect(stream).await;
    assert!(matches!(
        events.last(),
        Some(Err(ProviderError::Protocol(m))) if m.contains("message_stop")
    ));
}

#[tokio::test]
async fn verify_key_maps_the_status_to_the_outcome() {
    // 200 accepts, 401 rejects, 500 is a server error.
    let mock = MockServer::start(
        vec![
            MockResponse::stream(vec![]),
            MockResponse::error(401),
            MockResponse::error(403),
            MockResponse::error(500),
        ],
        vec![],
    )
    .await;
    let http = reqwest::Client::new();

    demerzel_providers::anthropic::verify_key(&http, &mock.url(), "sk-good")
        .await
        .unwrap();
    let rejected = demerzel_providers::anthropic::verify_key(&http, &mock.url(), "sk-bad")
        .await
        .unwrap_err();
    assert!(matches!(rejected, ProviderError::Auth(_)), "{rejected}");
    let no_access = demerzel_providers::anthropic::verify_key(&http, &mock.url(), "sk-403")
        .await
        .unwrap_err();
    assert!(matches!(no_access, ProviderError::Auth(_)), "{no_access}");
    let broken = demerzel_providers::anthropic::verify_key(&http, &mock.url(), "sk-any")
        .await
        .unwrap_err();
    assert!(matches!(broken, ProviderError::Server(_)), "{broken}");

    let received = mock.received();
    assert_eq!(received.len(), 4);
    assert_eq!(received[0].path, "/v1/models");
    assert_eq!(received[0].api_key.as_deref(), Some("sk-good"));
    assert_eq!(received[0].anthropic_version.as_deref(), Some("2023-06-01"));
}
