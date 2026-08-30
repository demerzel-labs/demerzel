//! Conformance tests for the openai-codex adapter against the mock
//! Codex endpoint. Hermetic: no network, no model.

use std::future::poll_fn;
use std::path::PathBuf;
use std::time::Duration;

use demerzel_core::{ContentBlock, Effort, Message, Role, SealedPayload, StopReason};
use demerzel_mock::{MockResponse, MockServer};
use demerzel_providers::auth::AuthStore;
use demerzel_providers::openai_codex::{CodexConfig, OpenAiCodex};
use demerzel_providers::{
    EventStream, Provider, ProviderError, Request, StreamEvent, ToolDefinition,
};
use serde_json::{Value, json};

fn write_store(dir: &std::path::Path, access_token: &str) -> PathBuf {
    let path = dir.join("auth.json");
    std::fs::write(
        &path,
        json!({
            "version": 1,
            "openai_codex": {
                "access_token": access_token,
                "refresh_token": "refresh-1",
                "account_id": "acct_test",
            },
        })
        .to_string(),
    )
    .unwrap();
    path
}

fn provider_for(mock: &MockServer, store_path: PathBuf) -> OpenAiCodex {
    let auth = AuthStore::open(store_path, None)
        .unwrap()
        .with_token_url(format!("{}/oauth/token", mock.url()));
    let config = CodexConfig {
        base_url: mock.url(),
        max_attempts: 3,
        base_delay: Duration::from_millis(1),
        ..CodexConfig::default()
    };
    OpenAiCodex::new(config, auth)
}

fn request(messages: Vec<Message>, tools: Vec<ToolDefinition>) -> Request {
    let mut request = Request::new("gpt-test");
    request.system_prompt = "You are dmz.".to_string();
    request.messages = messages;
    request.tools = tools;
    request
}

fn completed(usage: Value) -> Value {
    json!({"type": "response.completed", "response": {"id": "resp_1", "usage": usage}})
}

async fn collect(mut stream: EventStream) -> Vec<Result<StreamEvent, ProviderError>> {
    let mut out = Vec::new();
    while let Some(item) = poll_fn(|cx| stream.as_mut().poll_next(cx)).await {
        out.push(item);
    }
    out
}

#[tokio::test]
async fn serializes_the_request_per_the_codex_contract() {
    let dir = tempfile::tempdir().unwrap();
    let mock = MockServer::start(
        vec![MockResponse::stream(vec![completed(json!({}))])],
        vec![],
    )
    .await;
    let provider = provider_for(&mock, write_store(dir.path(), "token-1"));

    let sealed_item = json!({
        "type": "reasoning",
        "id": "rs_1",
        "summary": [],
        "encrypted_content": "abc123",
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
                    summary: "think".into(),
                    sealed: Some(SealedPayload {
                        provider: "openai-codex".into(),
                        data: sealed_item.clone(),
                    }),
                },
                ContentBlock::Reasoning {
                    summary: "foreign".into(),
                    sealed: Some(SealedPayload {
                        provider: "anthropic".into(),
                        data: json!({"signature": "zzz"}),
                    }),
                },
                ContentBlock::Text { text: "ok".into() },
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
                content: "a1|[workspace]".into(),
                is_error: false,
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
    assert_eq!(call.authorization.as_deref(), Some("Bearer token-1"));
    assert_eq!(call.account_id.as_deref(), Some("acct_test"));

    let body = &call.body;
    assert_eq!(body["model"], "gpt-test");
    assert_eq!(body["instructions"], "You are dmz.");
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["name"], "read");
    assert_eq!(body["tools"][0]["parameters"]["type"], "object");

    let input = body["input"].as_array().unwrap();
    assert_eq!(input.len(), 5, "foreign sealed reasoning must be dropped");
    assert_eq!(input[0]["type"], "message");
    assert_eq!(input[0]["role"], "user");
    assert_eq!(input[0]["content"][0]["type"], "input_text");
    assert_eq!(
        input[1], sealed_item,
        "sealed payload must round-trip verbatim"
    );
    assert_eq!(input[2]["content"][0]["type"], "output_text");
    assert_eq!(input[3]["type"], "function_call");
    assert_eq!(input[3]["call_id"], "call_1");
    assert_eq!(
        input[3]["arguments"],
        serde_json::to_string(&json!({"path": "Cargo.toml"})).unwrap()
    );
    assert_eq!(input[4]["type"], "function_call_output");
    assert_eq!(input[4]["call_id"], "call_1");
}

#[tokio::test]
async fn reasoning_summary_parts_arrive_as_paragraphs() {
    let dir = tempfile::tempdir().unwrap();
    let part = |index: u64| {
        json!({
            "type": "response.reasoning_summary_part.added",
            "item_id": "rs_1", "summary_index": index,
            "part": {"type": "summary_text", "text": ""}
        })
    };
    let item = |id: &str| json!({"type": "response.output_item.added", "item": {"type": "reasoning", "id": id}});
    let events = vec![
        item("rs_1"),
        part(0),
        json!({"type": "response.reasoning_summary_text.delta", "delta": "**First**"}),
        part(1),
        json!({"type": "response.reasoning_summary_text.delta", "delta": "**Second**"}),
        // A second reasoning item restarts the part index.
        item("rs_2"),
        part(0),
        json!({"type": "response.reasoning_summary_text.delta", "delta": "**Third**"}),
        completed(json!({"input_tokens": 1, "output_tokens": 1})),
    ];
    let mock = MockServer::start(vec![MockResponse::stream(events)], vec![]).await;
    let provider = provider_for(&mock, write_store(dir.path(), "token-1"));

    let stream = provider
        .stream(request(Vec::new(), Vec::new()))
        .await
        .unwrap();
    let reasoning: String = collect(stream)
        .await
        .into_iter()
        .map(Result::unwrap)
        .filter_map(|event| match event {
            StreamEvent::ReasoningDelta(text) => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(reasoning, "**First**\n\n**Second**\n\n**Third**");
}

#[tokio::test]
async fn the_request_effort_lands_in_the_reasoning_field() {
    let dir = tempfile::tempdir().unwrap();
    let done = || {
        MockResponse::stream(vec![
            json!({"type": "response.output_text.delta", "delta": "ok"}),
            completed(json!({"input_tokens": 1, "output_tokens": 1})),
        ])
    };
    let mock = MockServer::start(vec![done(), done()], vec![]).await;
    let provider = provider_for(&mock, write_store(dir.path(), "token-1"));

    let mut with_effort = request(Vec::new(), Vec::new());
    with_effort.effort = Some(Effort::XHigh);
    collect(provider.stream(with_effort).await.unwrap()).await;
    collect(
        provider
            .stream(request(Vec::new(), Vec::new()))
            .await
            .unwrap(),
    )
    .await;

    let received = mock.received();
    assert_eq!(received[0].body["reasoning"]["effort"], "xhigh");
    assert_eq!(received[0].body["reasoning"]["summary"], "auto");
    // No effort on the request: the provider's own default applies,
    // and the summary setting stays.
    assert!(received[1].body["reasoning"].get("effort").is_none());
    assert_eq!(received[1].body["reasoning"]["summary"], "auto");
}

#[tokio::test]
async fn assembles_the_stream_into_neutral_events() {
    let dir = tempfile::tempdir().unwrap();
    let events = vec![
        json!({"type": "response.created", "response": {}}),
        json!({"type": "response.output_item.added", "item": {
            "type": "function_call", "id": "fc_1", "call_id": "call_9", "name": "read", "arguments": ""
        }}),
        json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1", "delta": "{\"pa"}),
        json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1", "delta": "th\":\"x\"}"}),
        json!({"type": "response.output_item.done", "item": {
            "type": "function_call", "id": "fc_1", "call_id": "call_9", "name": "read",
            "arguments": "{\"path\":\"x\"}"
        }}),
        json!({"type": "response.reasoning_summary_text.delta", "delta": "thinking"}),
        json!({"type": "response.output_item.done", "item": {
            "type": "reasoning", "id": "rs_1",
            "summary": [{"type": "summary_text", "text": "thinking"}],
            "encrypted_content": "enc"
        }}),
        json!({"type": "response.output_text.delta", "delta": "Hello "}),
        json!({"type": "response.output_text.delta", "delta": "world"}),
        json!({"type": "response.future_unknown_event", "mystery": true}),
        completed(json!({
            "input_tokens": 100,
            "input_tokens_details": {"cached_tokens": 40},
            "output_tokens": 20,
        })),
    ];
    let mock = MockServer::start(vec![MockResponse::stream(events)], vec![]).await;
    let provider = provider_for(&mock, write_store(dir.path(), "token-1"));

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
            ToolCallStart { id: s_id, name },
            ToolCallDelta { id: d1, .. },
            ToolCallDelta { id: d2, .. },
            ToolCallEnd { id: e_id, input },
            ReasoningDelta(delta),
            ReasoningDone { summary, sealed },
            TextDelta(t1),
            TextDelta(t2),
            Usage(usage),
            Done(stop),
        ] => {
            assert_eq!(s_id, "call_9");
            assert_eq!(name, "read");
            assert_eq!(d1, "call_9");
            assert_eq!(d2, "call_9");
            assert_eq!(e_id, "call_9");
            assert_eq!(input, &json!({"path": "x"}));
            assert_eq!(delta, "thinking");
            assert_eq!(summary, "thinking");
            let sealed = sealed.as_ref().unwrap();
            assert_eq!(sealed.provider, "openai-codex");
            assert_eq!(sealed.data["encrypted_content"], "enc");
            assert_eq!(t1, "Hello ");
            assert_eq!(t2, "world");
            assert_eq!(usage.input_tokens, 100);
            assert_eq!(usage.output_tokens, 20);
            assert_eq!(usage.cache_read_tokens, 40);
            assert_eq!(stop, &StopReason::ToolUse);
        }
        other => panic!("unexpected event sequence: {other:?}"),
    }
}

#[tokio::test]
async fn retries_a_server_error_with_backoff() {
    let dir = tempfile::tempdir().unwrap();
    let mock = MockServer::start(
        vec![
            MockResponse::error(500),
            MockResponse::stream(vec![
                json!({"type": "response.output_text.delta", "delta": "ok"}),
                completed(json!({})),
            ]),
        ],
        vec![],
    )
    .await;
    let provider = provider_for(&mock, write_store(dir.path(), "token-1"));

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
async fn refreshes_once_on_401_and_persists_the_new_token() {
    let dir = tempfile::tempdir().unwrap();
    let store_path = write_store(dir.path(), "stale-token");
    let mock = MockServer::start(
        vec![
            MockResponse::error(401),
            MockResponse::stream(vec![completed(json!({}))]),
        ],
        vec![json!({
            "access_token": "fresh-token",
            "refresh_token": "fresh-refresh",
        })],
    )
    .await;
    let provider = provider_for(&mock, store_path.clone());

    let stream = provider
        .stream(request(Vec::new(), Vec::new()))
        .await
        .unwrap();
    collect(stream).await;

    let received = mock.received();
    assert_eq!(received.len(), 3);
    assert_eq!(received[0].path, "/responses");
    assert_eq!(
        received[0].authorization.as_deref(),
        Some("Bearer stale-token")
    );
    assert_eq!(received[1].path, "/oauth/token");
    assert_eq!(received[1].body["grant_type"], "refresh_token");
    assert_eq!(received[1].body["refresh_token"], "refresh-1");
    assert_eq!(received[2].path, "/responses");
    assert_eq!(
        received[2].authorization.as_deref(),
        Some("Bearer fresh-token")
    );

    let saved: Value =
        serde_json::from_str(&std::fs::read_to_string(&store_path).unwrap()).unwrap();
    assert_eq!(saved["openai_codex"]["access_token"], "fresh-token");
    assert_eq!(saved["openai_codex"]["refresh_token"], "fresh-refresh");
}

#[tokio::test]
async fn gives_up_after_max_attempts_on_rate_limits() {
    let dir = tempfile::tempdir().unwrap();
    let mock = MockServer::start(
        vec![
            MockResponse::error(429),
            MockResponse::error(429),
            MockResponse::error(429),
        ],
        vec![],
    )
    .await;
    let provider = provider_for(&mock, write_store(dir.path(), "token-1"));

    let error = provider
        .stream(request(Vec::new(), Vec::new()))
        .await
        .err()
        .expect("must fail after max attempts");
    assert!(matches!(error, ProviderError::RateLimited { .. }));
    assert_eq!(mock.received().len(), 3);
}

#[tokio::test]
async fn an_incomplete_response_reports_its_usage_before_the_stop() {
    let dir = tempfile::tempdir().unwrap();
    let mock = MockServer::start(
        vec![MockResponse::stream(vec![
            json!({"type": "response.output_text.delta", "delta": "cut"}),
            json!({"type": "response.incomplete", "response": {"usage": {
                "input_tokens": 101_003,
                "output_tokens": 64_000,
                "input_tokens_details": {"cached_tokens": 90_000},
            }}}),
        ])],
        vec![],
    )
    .await;
    let provider = provider_for(&mock, write_store(dir.path(), "token-1"));

    let events: Vec<StreamEvent> = collect(
        provider
            .stream(request(Vec::new(), Vec::new()))
            .await
            .unwrap(),
    )
    .await
    .into_iter()
    .map(Result::unwrap)
    .collect();

    use StreamEvent::*;
    match &events[..] {
        [TextDelta(_), Usage(usage), Done(stop)] => {
            assert_eq!(usage.input_tokens, 101_003);
            assert_eq!(usage.output_tokens, 64_000);
            assert_eq!(usage.cache_read_tokens, 90_000);
            assert_eq!(*stop, StopReason::MaxTokens);
        }
        other => panic!("unexpected events: {other:?}"),
    }
}
