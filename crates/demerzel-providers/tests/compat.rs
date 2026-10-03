//! Conformance tests for the openai-compat adapter against the mock
//! Chat Completions endpoint. Hermetic: no network, no model.

use std::future::poll_fn;
use std::time::Duration;

use demerzel_core::{ContentBlock, Effort, Message, Role, SealedPayload, StopReason, Usage};
use demerzel_mock::compat::{self as fixtures, chunk, finish};
use demerzel_mock::{MockResponse, MockServer, SseEvent};
use demerzel_providers::openai_compat::{CompatConfig, OpenAiCompat, PROVIDER_ID};
use demerzel_providers::{
    EventStream, Provider, ProviderError, Request, StreamEvent, ToolDefinition,
};
use serde_json::{Value, json};

const KEY: &str = "test-compat-key";

fn provider_for(mock: &MockServer, key: Option<&str>) -> OpenAiCompat {
    let mut config = CompatConfig::new("local", format!("{}/v1/", mock.url()));
    config.max_attempts = 3;
    config.base_delay = Duration::from_millis(1);
    OpenAiCompat::new(config, key.map(str::to_string))
}

fn request(model: &str, messages: Vec<Message>) -> Request {
    let mut request = Request::new(model);
    request.system_prompt = "You are dmz.".to_string();
    request.messages = messages;
    request.tools = vec![ToolDefinition {
        name: "read".into(),
        description: "Read a file".into(),
        input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
    }];
    request
}

fn user(text: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![ContentBlock::Text { text: text.into() }],
    }
}

async fn collect(mut stream: EventStream) -> Vec<Result<StreamEvent, ProviderError>> {
    let mut out = Vec::new();
    while let Some(item) = poll_fn(|cx| stream.as_mut().poll_next(cx)).await {
        out.push(item);
    }
    out
}

async fn events(provider: &OpenAiCompat, request: Request) -> Vec<StreamEvent> {
    let stream = provider.stream(request).await.unwrap();
    collect(stream)
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect()
}

fn text_of(events: &[StreamEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::TextDelta(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn ends(events: &[StreamEvent]) -> Vec<(String, Value)> {
    events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ToolCallEnd { id, input } => Some((id.clone(), input.clone())),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_text_turn_maps_text_usage_and_the_stop() {
    let mock = MockServer::start(vec![fixtures::text_turn("hello there")], vec![]).await;
    let provider = provider_for(&mock, Some(KEY));
    let got = events(&provider, request("m1", vec![user("hi")])).await;

    assert_eq!(text_of(&got), "hello there");
    assert!(got.contains(&StreamEvent::Usage(Usage {
        input_tokens: 10,
        output_tokens: 5,
        cache_read_tokens: 2,
        cache_creation_tokens: 0,
    })));
    assert_eq!(got.last(), Some(&StreamEvent::Done(StopReason::EndTurn)));

    let received = mock.received();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].path, "/v1/chat/completions");
    assert_eq!(
        received[0].authorization.as_deref(),
        Some("Bearer test-compat-key")
    );
    let body = &received[0].body;
    assert_eq!(body["model"], "m1");
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert_eq!(
        body["messages"],
        json!([
            {"role": "system", "content": "You are dmz."},
            {"role": "user", "content": "hi"},
        ])
    );
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["function"]["name"], "read");
    // A minimal body: no field the request did not set.
    for field in [
        "max_tokens",
        "reasoning_effort",
        "tool_choice",
        "parallel_tool_calls",
    ] {
        assert!(body.get(field).is_none(), "{field} must be absent");
    }
}

#[tokio::test]
async fn an_instance_without_a_key_sends_no_authorization_header() {
    let mock = MockServer::start(vec![fixtures::text_turn("ok")], vec![]).await;
    let provider = provider_for(&mock, None);
    events(&provider, request("m1", vec![user("hi")])).await;
    assert_eq!(mock.received()[0].authorization, None);
}

#[tokio::test]
async fn set_fields_go_out_as_named() {
    let mock = MockServer::start(vec![fixtures::text_turn("ok")], vec![]).await;
    let provider = provider_for(&mock, None);
    let mut request = request("m1", vec![user("hi")]);
    request.max_output_tokens = Some(512);
    request.effort = Some(Effort::Max);
    events(&provider, request).await;
    let body = &mock.received()[0].body;
    assert_eq!(body["max_tokens"], 512);
    // The level goes out as named; the adapter does not clamp it.
    assert_eq!(body["reasoning_effort"], "max");
}

#[tokio::test]
async fn a_tool_call_streamed_in_fragments_ends_with_its_whole_input() {
    let mock = MockServer::start(
        vec![fixtures::tool_turn(
            "call_1",
            "read",
            json!({"path": "Cargo.toml"}),
        )],
        vec![],
    )
    .await;
    let provider = provider_for(&mock, None);
    let got = events(&provider, request("m1", vec![user("read it")])).await;

    assert_eq!(
        got[0],
        StreamEvent::ToolCallStart {
            id: "call_1".into(),
            name: "read".into()
        }
    );
    let fragments: String = got
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ToolCallDelta { input_fragment, .. } => Some(input_fragment.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(fragments, json!({"path": "Cargo.toml"}).to_string());
    assert_eq!(
        ends(&got),
        vec![("call_1".to_string(), json!({"path": "Cargo.toml"}))]
    );
    assert_eq!(got.last(), Some(&StreamEvent::Done(StopReason::ToolUse)));
}

#[tokio::test]
async fn two_calls_in_one_reply_end_in_index_order() {
    let mut chunks = vec![chunk(json!({"role": "assistant", "content": null}))];
    // The second call's fragments interleave with the first's.
    let first = fixtures::tool_call_fragments(0, "call_a", "read", &json!({"path": "a"}));
    let second = fixtures::tool_call_fragments(1, "call_b", "grep", &json!({"pattern": "b"}));
    for (a, b) in first.into_iter().zip(second) {
        chunks.push(a);
        chunks.push(b);
    }
    chunks.push(finish("tool_calls"));
    let mock = MockServer::start(vec![fixtures::closed(chunks)], vec![]).await;
    let provider = provider_for(&mock, None);
    let got = events(&provider, request("m1", vec![user("go")])).await;
    assert_eq!(
        ends(&got),
        vec![
            ("call_a".to_string(), json!({"path": "a"})),
            ("call_b".to_string(), json!({"pattern": "b"})),
        ]
    );
}

#[tokio::test]
async fn a_stop_after_a_call_still_means_tool_use() {
    let mut chunks = vec![];
    chunks.extend(fixtures::tool_call_fragments(
        0,
        "call_1",
        "read",
        &json!({"path": "a"}),
    ));
    chunks.push(finish("stop"));
    let mock = MockServer::start(vec![fixtures::closed(chunks)], vec![]).await;
    let provider = provider_for(&mock, None);
    let got = events(&provider, request("m1", vec![user("go")])).await;
    assert_eq!(ends(&got).len(), 1);
    assert_eq!(got.last(), Some(&StreamEvent::Done(StopReason::ToolUse)));
}

#[tokio::test]
async fn finish_reasons_map_to_stop_reasons() {
    let reply =
        |reason: &str| fixtures::closed(vec![chunk(json!({"content": "partial"})), finish(reason)]);
    let mock = MockServer::start(vec![reply("length"), reply("content_filter")], vec![]).await;
    let provider = provider_for(&mock, None);
    let length = events(&provider, request("m1", vec![user("go")])).await;
    assert_eq!(
        length.last(),
        Some(&StreamEvent::Done(StopReason::MaxTokens))
    );
    let filtered = events(&provider, request("m1", vec![user("go")])).await;
    assert_eq!(
        filtered.last(),
        Some(&StreamEvent::Done(StopReason::Refusal))
    );
}

#[tokio::test]
async fn a_stream_without_done_is_a_protocol_error() {
    let mock = MockServer::start(
        vec![MockResponse::stream(vec![
            chunk(json!({"content": "cut"})),
            finish("stop"),
            fixtures::usage(),
        ])],
        vec![],
    )
    .await;
    let provider = provider_for(&mock, None);
    let stream = provider
        .stream(request("m1", vec![user("go")]))
        .await
        .unwrap();
    let got = collect(stream).await;
    assert!(
        matches!(got.last(), Some(Err(ProviderError::Protocol(message))) if message.contains("[DONE]")),
        "{got:?}"
    );
}

#[tokio::test]
async fn reasoning_streams_as_reasoning_and_seals_with_its_identity() {
    for field in ["reasoning_content", "reasoning"] {
        let mock = MockServer::start(
            vec![fixtures::reasoning_tool_turn(
                field,
                "look at the manifest",
                "call_1",
                "read",
                json!({"path": "Cargo.toml"}),
            )],
            vec![],
        )
        .await;
        let provider = provider_for(&mock, None);
        let got = events(&provider, request("m1", vec![user("go")])).await;

        let shown: String = got
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ReasoningDelta(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(shown, "look at the manifest", "{field}");
        let sealed = got
            .iter()
            .find_map(|event| match event {
                StreamEvent::ReasoningDone { summary, sealed } => {
                    assert_eq!(summary, "look at the manifest");
                    sealed.clone()
                }
                _ => None,
            })
            .expect("a sealed reasoning segment");
        assert_eq!(sealed.provider, PROVIDER_ID);
        assert_eq!(sealed.instance.as_deref(), Some("local"));
        assert_eq!(sealed.model.as_deref(), Some("m1"));
        assert_eq!(sealed.data, json!({ field: "look at the manifest" }));
        // The segment closes before the call starts.
        let done_at = got
            .iter()
            .position(|event| matches!(event, StreamEvent::ReasoningDone { .. }))
            .unwrap();
        let start_at = got
            .iter()
            .position(|event| matches!(event, StreamEvent::ToolCallStart { .. }))
            .unwrap();
        assert!(done_at < start_at, "{field}");
    }
}

/// The strict server: every assistant message that carries tool calls
/// must carry `reasoning_content` back, the rule a thinking-mode
/// server applies to tool continuations.
fn requires_reasoning_replay(body: &Value) -> Result<(), String> {
    let messages = body["messages"].as_array().ok_or("no messages")?;
    for message in messages {
        if message["role"] == "assistant"
            && message.get("tool_calls").is_some()
            && message["reasoning_content"]
                .as_str()
                .is_none_or(str::is_empty)
        {
            return Err("missing `reasoning_content` in the thinking mode".to_string());
        }
    }
    Ok(())
}

/// The history after one reasoning tool turn: the assistant message
/// with its sealed reasoning and call, then the call's result.
fn continuation(sealed: SealedPayload) -> Vec<Message> {
    vec![
        user("go"),
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Reasoning {
                    summary: "look at the manifest".into(),
                    sealed: Some(sealed),
                },
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read".into(),
                    input: json!({"path": "Cargo.toml"}),
                },
            ],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_1".into(),
                content: "[workspace]".into(),
                is_error: false,
            }],
        },
    ]
}

#[tokio::test]
async fn a_strict_server_accepts_the_continuation_only_with_the_original_reasoning() {
    let sealed = SealedPayload::new(
        PROVIDER_ID,
        json!({"reasoning_content": "look at the manifest"}),
    )
    .with_identity("local", "m1");
    let strict = || fixtures::text_turn("done").with_check(requires_reasoning_replay);
    let mock = MockServer::start(vec![strict(), strict(), strict(), strict()], vec![]).await;
    let provider = provider_for(&mock, None);

    // The same instance and model: the field goes back verbatim, on
    // the message that carries the call.
    let got = events(&provider, request("m1", continuation(sealed.clone()))).await;
    assert_eq!(text_of(&got), "done");
    let body = &mock.received()[0].body;
    let assistant = &body["messages"][2];
    assert_eq!(assistant["role"], "assistant");
    assert_eq!(assistant["content"], Value::Null);
    assert_eq!(assistant["reasoning_content"], "look at the manifest");
    assert_eq!(assistant["tool_calls"][0]["id"], "call_1");
    assert_eq!(
        assistant["tool_calls"][0]["function"]["arguments"],
        json!({"path": "Cargo.toml"}).to_string()
    );
    assert_eq!(
        body["messages"][3],
        json!({"role": "tool", "tool_call_id": "call_1", "content": "[workspace]"})
    );

    // Without the payload the strict server refuses the request.
    let bare = continuation(sealed.clone())
        .into_iter()
        .map(|mut message| {
            message
                .content
                .retain(|block| !matches!(block, ContentBlock::Reasoning { .. }));
            message
        })
        .collect();
    let refused = provider.stream(request("m1", bare)).await.err().unwrap();
    assert!(
        matches!(&refused, ProviderError::InvalidRequest(message) if message.contains("reasoning_content")),
        "{refused:?}"
    );

    // Another model, or another instance, gets no foreign payload.
    provider
        .stream(request("m2", continuation(sealed.clone())))
        .await
        .err()
        .unwrap();
    assert!(
        mock.received()[2].body["messages"][2]
            .get("reasoning_content")
            .is_none()
    );
    let other = SealedPayload::new(PROVIDER_ID, json!({"reasoning_content": "theirs"}))
        .with_identity("hosted", "m1");
    provider
        .stream(request("m1", continuation(other)))
        .await
        .err()
        .unwrap();
    assert!(
        mock.received()[3].body["messages"][2]
            .get("reasoning_content")
            .is_none()
    );
}

#[tokio::test]
async fn a_payload_from_another_adapter_stays_off_the_wire() {
    let mock = MockServer::start(vec![fixtures::text_turn("ok")], vec![]).await;
    let provider = provider_for(&mock, None);
    let foreign = SealedPayload::new("anthropic", json!({"reasoning_content": "not ours"}));
    events(&provider, request("m1", continuation(foreign))).await;
    let assistant = &mock.received()[0].body["messages"][2];
    assert!(assistant.get("reasoning_content").is_none());
    assert_eq!(assistant["tool_calls"][0]["id"], "call_1");
}

#[tokio::test]
async fn status_401_fails_at_once_and_names_the_instance() {
    let mock = MockServer::start(vec![MockResponse::error(401)], vec![]).await;
    let provider = provider_for(&mock, Some(KEY));
    let error = provider
        .stream(request("m1", vec![user("hi")]))
        .await
        .err()
        .unwrap();
    assert!(
        matches!(&error, ProviderError::Auth(message) if message.contains("`local`")),
        "{error:?}"
    );
    assert_eq!(mock.received().len(), 1);
}

#[tokio::test]
async fn status_429_retries_after_the_server_delay() {
    let mock = MockServer::start(
        vec![
            MockResponse::error_with_retry_after(429, 0),
            fixtures::text_turn("after the wait"),
        ],
        vec![],
    )
    .await;
    let provider = provider_for(&mock, None);
    let got = events(&provider, request("m1", vec![user("hi")])).await;
    assert_eq!(text_of(&got), "after the wait");
    assert_eq!(mock.received().len(), 2);
}

#[tokio::test]
async fn server_errors_retry_and_then_fail() {
    let mock = MockServer::start(
        vec![
            MockResponse::error(503),
            MockResponse::error(500),
            MockResponse::error(502),
        ],
        vec![],
    )
    .await;
    let provider = provider_for(&mock, None);
    let error = provider
        .stream(request("m1", vec![user("hi")]))
        .await
        .err()
        .unwrap();
    assert!(matches!(error, ProviderError::Server(_)), "{error:?}");
    assert_eq!(mock.received().len(), 3);
}

#[tokio::test]
async fn a_rejected_request_does_not_echo_the_key() {
    let mock = MockServer::start(vec![MockResponse::error(400)], vec![]).await;
    let provider = provider_for(&mock, Some(KEY));
    let error = provider
        .stream(request("m1", vec![user("hi")]))
        .await
        .err()
        .unwrap();
    let text = error.to_string();
    assert!(text.contains("status 400"), "{text}");
    assert!(!text.contains(KEY), "{text}");
}

#[tokio::test]
async fn a_mid_stream_error_payload_fails_the_call() {
    let mock = MockServer::start(
        vec![MockResponse::events(vec![
            SseEvent::new(chunk(json!({"content": "par"}))),
            SseEvent::new(json!({"error": {"message": "the model crashed", "code": 500}})),
        ])],
        vec![],
    )
    .await;
    let provider = provider_for(&mock, None);
    let stream = provider
        .stream(request("m1", vec![user("hi")]))
        .await
        .unwrap();
    let got = collect(stream).await;
    assert!(
        matches!(got.last(), Some(Err(ProviderError::Server(message))) if message.contains("crashed")),
        "{got:?}"
    );
}

/// Replay one fixture file through the adapter and return the events,
/// errors included.
async fn replay_fixture(body: &str) -> Vec<Result<StreamEvent, ProviderError>> {
    let mock = MockServer::start(vec![MockResponse::sse_body(body)], vec![]).await;
    let provider = provider_for(&mock, None);
    let stream = provider
        .stream(request("m1", vec![user("go")]))
        .await
        .unwrap();
    collect(stream).await
}

fn unwrap_all(events: Vec<Result<StreamEvent, ProviderError>>) -> Vec<StreamEvent> {
    events.into_iter().map(Result::unwrap).collect()
}

fn usage_of(events: &[StreamEvent]) -> Option<Usage> {
    events.iter().find_map(|event| match event {
        StreamEvent::Usage(usage) => Some(*usage),
        _ => None,
    })
}

fn sealed_of(events: &[StreamEvent]) -> Option<SealedPayload> {
    events.iter().find_map(|event| match event {
        StreamEvent::ReasoningDone { sealed, .. } => sealed.clone(),
        _ => None,
    })
}

#[tokio::test]
async fn fixture_hosted_thinking_mode_reasons_then_calls_a_tool() {
    let got = unwrap_all(
        replay_fixture(include_str!(
            "fixtures/chat-completions/hosted-thinking-tool.sse"
        ))
        .await,
    );
    let sealed = sealed_of(&got).expect("sealed reasoning");
    assert_eq!(
        sealed.data,
        json!({"reasoning_content": "The user wants the manifest. Read Cargo.toml first."})
    );
    assert_eq!(
        ends(&got),
        vec![(
            "call_synthetic_1".to_string(),
            json!({"path": "Cargo.toml"})
        )]
    );
    // Usage rides the finish chunk; the cache count comes from the
    // standard field.
    assert_eq!(
        usage_of(&got),
        Some(Usage {
            input_tokens: 120,
            output_tokens: 30,
            cache_read_tokens: 64,
            cache_creation_tokens: 0,
        })
    );
    assert_eq!(got.last(), Some(&StreamEvent::Done(StopReason::ToolUse)));
}

#[tokio::test]
async fn fixture_gateway_repeats_the_finish_chunk_and_carries_reasoning() {
    let got = unwrap_all(
        replay_fixture(include_str!(
            "fixtures/chat-completions/gateway-reasoning-text.sse"
        ))
        .await,
    );
    assert_eq!(text_of(&got), "Four.");
    // `reasoning_details` is structured, not text: only `reasoning`
    // seals.
    assert_eq!(
        sealed_of(&got).unwrap().data,
        json!({"reasoning": "Check the question."})
    );
    assert_eq!(usage_of(&got).unwrap().input_tokens, 40);
    assert_eq!(usage_of(&got).unwrap().cache_read_tokens, 8);
    let done: Vec<_> = got
        .iter()
        .filter(|event| matches!(event, StreamEvent::Done(_)))
        .collect();
    assert_eq!(done, vec![&StreamEvent::Done(StopReason::EndTurn)]);
}

#[tokio::test]
async fn fixture_local_server_grows_the_tool_name_before_its_arguments() {
    let got = unwrap_all(
        replay_fixture(include_str!(
            "fixtures/chat-completions/local-grown-name-tool.sse"
        ))
        .await,
    );
    assert_eq!(
        sealed_of(&got).unwrap().data,
        json!({"reasoning_content": "List the files."})
    );
    let starts: Vec<_> = got
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ToolCallStart { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(starts, vec!["glob"]);
    assert_eq!(ends(&got)[0].1, json!({"pattern": "*.rs"}));
    assert_eq!(usage_of(&got).unwrap().input_tokens, 80);
    assert_eq!(got.last(), Some(&StreamEvent::Done(StopReason::ToolUse)));
}

#[tokio::test]
async fn fixture_local_server_sends_a_whole_call_in_one_delta() {
    let got = unwrap_all(
        replay_fixture(include_str!(
            "fixtures/chat-completions/local-whole-call-tool.sse"
        ))
        .await,
    );
    assert_eq!(
        sealed_of(&got).unwrap().data,
        json!({"reasoning": "Search for the symbol."})
    );
    assert_eq!(
        ends(&got),
        vec![(
            "call_synthetic_17".to_string(),
            json!({"pattern": "fn main"})
        )]
    );
    assert_eq!(usage_of(&got).unwrap().cache_read_tokens, 10);
    assert_eq!(got.last(), Some(&StreamEvent::Done(StopReason::ToolUse)));
}

#[tokio::test]
async fn fixture_baseline_text_reply_ignores_unknown_fields() {
    let got = unwrap_all(
        replay_fixture(include_str!("fixtures/chat-completions/baseline-text.sse")).await,
    );
    assert_eq!(text_of(&got), "Hello");
    assert_eq!(usage_of(&got).unwrap().input_tokens, 12);
    assert_eq!(got.last(), Some(&StreamEvent::Done(StopReason::EndTurn)));
}

#[tokio::test]
async fn fixtures_with_a_mid_stream_error_fail_the_call() {
    for (name, body, needle) in [
        (
            "self-hosted-error",
            include_str!("fixtures/chat-completions/self-hosted-error.sse"),
            "synthetic generation failure",
        ),
        (
            "gateway-error",
            include_str!("fixtures/chat-completions/gateway-error.sse"),
            "Provider disconnected unexpectedly",
        ),
    ] {
        let got = replay_fixture(body).await;
        assert!(
            matches!(got.last(), Some(Err(ProviderError::Server(message))) if message.contains(needle)),
            "{name}: {got:?}"
        );
        // Nothing after the failure: no done event.
        assert!(
            !got.iter()
                .any(|event| matches!(event, Ok(StreamEvent::Done(_)))),
            "{name}"
        );
    }
}

#[tokio::test]
async fn a_reply_that_is_not_an_event_stream_fails_with_its_type() {
    let page = "<html>".to_string() + &"x".repeat(4096) + "</html>";
    let mock = MockServer::start(
        vec![MockResponse::sse_body(page).with_content_type("text/html; charset=utf-8")],
        vec![],
    )
    .await;
    let provider = provider_for(&mock, Some(KEY));
    let error = provider
        .stream(request("m1", vec![user("hi")]))
        .await
        .err()
        .unwrap();
    let ProviderError::Protocol(message) = &error else {
        panic!("expected a protocol error, got {error:?}");
    };
    assert!(message.contains("text/html"), "{message}");
    assert!(message.contains("base_url"), "{message}");
    // Only the head of the body reaches the message.
    assert!(message.len() < 1024, "{}", message.len());
}

#[tokio::test]
async fn the_key_check_accepts_refuses_and_leaves_the_rest_unconfirmed() {
    use demerzel_providers::endpoint::{KeyCheck, verify_key};

    let mock = MockServer::start(
        vec![
            MockResponse::stream(Vec::new()),
            MockResponse::error(401),
            MockResponse::error(404),
            MockResponse::error(500),
        ],
        Vec::new(),
    )
    .await;
    let http = reqwest::Client::new();
    // A trailing slash on the base URL does not double the separator.
    let base_url = format!("{}/v1/", mock.url());

    let check = verify_key(&http, "local", &base_url, KEY).await.unwrap();
    assert_eq!(check, KeyCheck::Accepted);
    let error = verify_key(&http, "local", &base_url, KEY)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, ProviderError::Auth(message) if message.contains("401") && message.contains("`local`")),
        "{error:?}"
    );
    for status in ["404", "500"] {
        let check = verify_key(&http, "local", &base_url, KEY).await.unwrap();
        assert!(
            matches!(&check, KeyCheck::Unconfirmed(reason) if reason.contains(status)),
            "{check:?}"
        );
    }

    let received = mock.received();
    assert_eq!(received.len(), 4);
    for request in &received {
        assert_eq!(request.path, "/v1/models");
        assert_eq!(
            request.authorization.as_deref(),
            Some(format!("Bearer {KEY}").as_str())
        );
    }

    // A request that does not build is refused: no session could
    // send it, and the error never carries the key.
    for (base_url, key) in [("not a url", KEY), (base_url.as_str(), "bad\nkey")] {
        let error = verify_key(&http, "local", base_url, key).await.unwrap_err();
        assert!(
            matches!(&error, ProviderError::InvalidRequest(message) if !message.contains("bad\nkey")),
            "{error:?}"
        );
    }

    // An endpoint that does not answer leaves the key unconfirmed.
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let check = verify_key(
        &http,
        "local",
        &format!("http://127.0.0.1:{closed}/v1"),
        KEY,
    )
    .await
    .unwrap();
    assert!(matches!(&check, KeyCheck::Unconfirmed(_)), "{check:?}");
}
