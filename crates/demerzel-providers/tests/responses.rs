//! Conformance tests for the openai-responses adapter against the mock
//! Responses endpoint. Hermetic: no network, no model.

use std::future::poll_fn;
use std::time::Duration;

use demerzel_core::{ContentBlock, Effort, Message, Role, SealedPayload, StopReason, Usage};
use demerzel_mock::responses::{self as fixtures, call_events, closed, reasoning_item};
use demerzel_mock::{MockResponse, MockServer, SseEvent};
use demerzel_providers::openai_responses::{OpenAiResponses, PROVIDER_ID, ResponsesConfig};
use demerzel_providers::{
    EventStream, Provider, ProviderError, Request, StreamEvent, ToolDefinition,
};
use serde_json::{Value, json};

const KEY: &str = "test-responses-key";

fn provider_for(mock: &MockServer, key: Option<&str>) -> OpenAiResponses {
    let mut config = ResponsesConfig::new("hosted", format!("{}/v1/", mock.url()));
    config.max_attempts = 3;
    config.base_delay = Duration::from_millis(1);
    OpenAiResponses::new(config, key.map(str::to_string))
}

fn request(model: &str, messages: Vec<Message>) -> Request {
    let mut request = Request::new(model);
    request.system_prompt = "You are dmz.".to_string();
    request.messages = messages;
    request.tools = vec![ToolDefinition {
        name: "read".into(),
        description: "Read a file".into(),
        input_schema: json!({
            "type": "object",
            "properties": {"path": {"type": "string"}, "offset": {"type": "integer"}},
            "required": ["path"],
        }),
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

async fn events(provider: &OpenAiResponses, request: Request) -> Vec<StreamEvent> {
    let stream = provider.stream(request).await.unwrap();
    collect(stream)
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect()
}

/// The events of one reply, errors included.
async fn raw_events(mock: &MockServer) -> Vec<Result<StreamEvent, ProviderError>> {
    let provider = provider_for(mock, Some(KEY));
    let stream = provider
        .stream(request("m1", vec![user("hi")]))
        .await
        .unwrap();
    collect(stream).await
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

fn reasoning_of(events: &[StreamEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ReasoningDelta(text) => Some(text.as_str()),
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

fn sealed_of(events: &[StreamEvent]) -> Vec<(String, SealedPayload)> {
    events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ReasoningDone {
                summary,
                sealed: Some(sealed),
            } => Some((summary.clone(), sealed.clone())),
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
    assert_eq!(received[0].path, "/v1/responses");
    assert_eq!(
        received[0].authorization.as_deref(),
        Some("Bearer test-responses-key")
    );
    assert_eq!(received[0].account_id, None);
    let body = &received[0].body;
    assert_eq!(body["model"], "m1");
    assert_eq!(body["instructions"], "You are dmz.");
    assert_eq!(body["stream"], true);
    assert_eq!(body["store"], false);
    assert_eq!(
        body["input"],
        json!([{"type": "message", "role": "user",
                "content": [{"type": "input_text", "text": "hi"}]}])
    );
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["name"], "read");
    assert_eq!(body["tools"][0]["strict"], false);
    assert_eq!(body["tools"][0]["parameters"]["required"], json!(["path"]));
    // Local state only, and no field the request did not set.
    // `include` too: some hosted models refuse it, and current servers
    // return the encrypted reasoning without it.
    for field in [
        "include",
        "previous_response_id",
        "conversation",
        "reasoning",
        "max_output_tokens",
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
    request.effort = Some(Effort::XHigh);
    events(&provider, request).await;
    let body = &mock.received()[0].body;
    assert_eq!(body["max_output_tokens"], 512);
    assert_eq!(body["reasoning"], json!({"effort": "xhigh"}));
}

#[tokio::test]
async fn a_tool_call_streams_under_its_call_id_and_ends_with_its_whole_input() {
    let input = json!({"path": "src/main.rs", "offset": 3});
    let mock = MockServer::start(
        vec![fixtures::tool_turn("call_1", "read", input.clone())],
        vec![],
    )
    .await;
    let provider = provider_for(&mock, None);
    let got = events(&provider, request("m1", vec![user("read it")])).await;

    // The wire item id `fc_call_1` routes the fragments; every neutral
    // event carries the call id the result answers.
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
            StreamEvent::ToolCallDelta { id, input_fragment } => {
                assert_eq!(id, "call_1");
                Some(input_fragment.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(fragments, input.to_string());
    assert_eq!(ends(&got), vec![("call_1".to_string(), input)]);
    assert_eq!(got.last(), Some(&StreamEvent::Done(StopReason::ToolUse)));
}

#[tokio::test]
async fn optional_tool_inputs_and_empty_arguments_pass() {
    let mut events_list = call_events("call_1", "read", &json!({"path": "a"}));
    // A call whose arguments are empty takes no input.
    events_list.push(
        json!({"type": "response.output_item.added", "output_index": 2,
        "item": {"type": "function_call", "id": "fc_2", "call_id": "call_2", "name": "read",
                 "arguments": ""}}),
    );
    events_list.push(
        json!({"type": "response.output_item.done", "output_index": 2,
        "item": {"type": "function_call", "id": "fc_2", "call_id": "call_2", "name": "read",
                 "arguments": ""}}),
    );
    let mock = MockServer::start(vec![closed(events_list)], vec![]).await;
    let provider = provider_for(&mock, None);
    let got = events(&provider, request("m1", vec![user("read")])).await;
    assert_eq!(
        ends(&got),
        vec![
            ("call_1".to_string(), json!({"path": "a"})),
            ("call_2".to_string(), json!({})),
        ]
    );
}

#[tokio::test]
async fn two_calls_in_one_reply_end_in_order() {
    let mut events_list = call_events("call_a", "read", &json!({"path": "a"}));
    events_list.extend(call_events("call_b", "grep", &json!({"pattern": "x"})));
    let mock = MockServer::start(vec![closed(events_list)], vec![]).await;
    let provider = provider_for(&mock, None);
    let got = events(&provider, request("m1", vec![user("both")])).await;
    assert_eq!(
        ends(&got),
        vec![
            ("call_a".to_string(), json!({"path": "a"})),
            ("call_b".to_string(), json!({"pattern": "x"})),
        ]
    );
}

#[tokio::test]
async fn reasoning_streams_as_reasoning_and_seals_the_complete_item_with_its_identity() {
    let mock = MockServer::start(
        vec![fixtures::reasoning_tool_turn(
            "Read the file first.",
            "enc_mock_1",
            "call_1",
            "read",
            json!({"path": "a"}),
        )],
        vec![],
    )
    .await;
    let provider = provider_for(&mock, None);
    let got = events(&provider, request("m1", vec![user("read a")])).await;

    assert_eq!(reasoning_of(&got), "Read the file first.");
    let sealed = sealed_of(&got);
    assert_eq!(sealed.len(), 1);
    let (summary, payload) = &sealed[0];
    assert_eq!(summary, "Read the file first.");
    assert_eq!(
        payload.data,
        reasoning_item("rs_mock", "Read the file first.", "enc_mock_1")
    );
    assert!(payload.replays_to(PROVIDER_ID, "hosted", "m1"));
    assert!(!payload.replays_to(PROVIDER_ID, "other", "m1"));
    assert!(!payload.replays_to(PROVIDER_ID, "hosted", "m2"));
    // The reasoning block ends before the call starts.
    let done = got
        .iter()
        .position(|e| matches!(e, StreamEvent::ReasoningDone { .. }));
    let start = got
        .iter()
        .position(|e| matches!(e, StreamEvent::ToolCallStart { .. }));
    assert!(done < start, "{got:?}");
}

#[tokio::test]
async fn reasoning_text_supplies_display_text_once_with_paragraphs() {
    let reasoning = |kind: &str, index_key: &str, index: u64, delta: &str| {
        json!({"type": kind, "item_id": "rs_1", "output_index": 0, index_key: index,
               "delta": delta})
    };
    let mock = MockServer::start(
        vec![closed(vec![
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"type": "reasoning", "id": "rs_1", "summary": []}}),
            reasoning(
                "response.reasoning_text.delta",
                "content_index",
                0,
                "First ",
            ),
            reasoning("response.reasoning_text.delta", "content_index", 0, "part."),
            // The same item's summary is not shown a second time.
            reasoning(
                "response.reasoning_summary_text.delta",
                "summary_index",
                0,
                "Dup.",
            ),
            reasoning(
                "response.reasoning_text.delta",
                "content_index",
                1,
                "Second.",
            ),
            json!({"type": "response.output_item.done", "output_index": 0, "item": {
                "type": "reasoning", "id": "rs_1",
                "summary": [{"type": "summary_text", "text": "Dup."}],
                "content": [{"type": "reasoning_text", "text": "First part."},
                            {"type": "reasoning_text", "text": "Second."}]}}),
            json!({"type": "response.output_item.added", "output_index": 1,
                   "item": {"type": "reasoning", "id": "rs_2", "summary": []}}),
            // An item whose text never streamed shows the text it holds.
            json!({"type": "response.output_item.done", "output_index": 1, "item": {
                "type": "reasoning", "id": "rs_2",
                "summary": [{"type": "summary_text", "text": "Third."}]}}),
        ])],
        vec![],
    )
    .await;
    let provider = provider_for(&mock, None);
    let got = events(&provider, request("m1", vec![user("think")])).await;
    // Each item is a block of its own: only parts within one item
    // are paragraphs.
    assert_eq!(reasoning_of(&got), "First part.\n\nSecond.Third.");
    let summaries: Vec<String> = sealed_of(&got).into_iter().map(|(s, _)| s).collect();
    assert_eq!(summaries, vec!["First part.\n\nSecond.", "Third."]);
}

#[tokio::test]
async fn reasoning_without_text_seals_the_item_and_shows_nothing() {
    let mock = MockServer::start(
        vec![closed(vec![
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"type": "reasoning", "id": "rs_1", "summary": []}}),
            json!({"type": "response.output_item.done", "output_index": 0,
                   "item": {"type": "reasoning", "id": "rs_1", "summary": [],
                            "encrypted_content": "enc_1"}}),
        ])],
        vec![],
    )
    .await;
    let provider = provider_for(&mock, None);
    let got = events(&provider, request("m1", vec![user("think")])).await;
    assert_eq!(reasoning_of(&got), "");
    let sealed = sealed_of(&got);
    assert_eq!(sealed.len(), 1);
    assert_eq!(sealed[0].0, "");
    assert_eq!(sealed[0].1.data["encrypted_content"], "enc_1");
}

/// The strict server's rule: every `function_call` input item follows
/// the reasoning item that produced it, whole, with its encrypted
/// content.
fn requires_reasoning_replay(body: &Value) -> Result<(), String> {
    let input = body["input"].as_array().ok_or("no input")?;
    for (at, item) in input.iter().enumerate() {
        if item["type"] != "function_call" {
            continue;
        }
        let previous = at.checked_sub(1).map(|before| &input[before]);
        let expected = reasoning_item("rs_mock", "Read the file first.", "enc_mock_1");
        if previous != Some(&expected) {
            return Err(format!(
                "Item '{}' of type 'function_call' was provided without its reasoning item",
                item["call_id"]
            ));
        }
    }
    Ok(())
}

/// A tool continuation: the user's ask, the reply with `sealed` and a
/// call, and the call's result.
fn continuation(sealed: SealedPayload) -> Vec<Message> {
    vec![
        user("read a"),
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Reasoning {
                    summary: "Read the file first.".into(),
                    sealed: Some(sealed),
                },
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read".into(),
                    input: json!({"path": "a"}),
                },
            ],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_1".into(),
                content: "alpha".into(),
                is_error: false,
            }],
        },
    ]
}

fn own_payload() -> SealedPayload {
    SealedPayload::new(
        PROVIDER_ID,
        reasoning_item("rs_mock", "Read the file first.", "enc_mock_1"),
    )
    .with_identity("hosted", "m1")
}

#[tokio::test]
async fn a_strict_server_accepts_the_continuation_only_with_the_original_reasoning() {
    let mock = MockServer::start(
        vec![
            fixtures::text_turn("the file holds alpha").with_check(requires_reasoning_replay),
            fixtures::text_turn("never").with_check(requires_reasoning_replay),
            fixtures::text_turn("never").with_check(requires_reasoning_replay),
        ],
        vec![],
    )
    .await;
    let provider = provider_for(&mock, None);
    let got = events(&provider, request("m1", continuation(own_payload()))).await;
    assert_eq!(text_of(&got), "the file holds alpha");
    let input = &mock.received()[0].body["input"];
    assert_eq!(
        input[1],
        reasoning_item("rs_mock", "Read the file first.", "enc_mock_1")
    );
    assert_eq!(
        input[2],
        json!({"type": "function_call", "call_id": "call_1", "name": "read",
               "arguments": "{\"path\":\"a\"}"})
    );
    assert_eq!(
        input[3],
        json!({"type": "function_call_output", "call_id": "call_1", "output": "alpha"})
    );

    // Another instance, or another model, gets no foreign payload, and
    // the strict server refuses the call.
    for (instance, model) in [("other", "m1"), ("hosted", "m2")] {
        let foreign = own_payload().with_identity(instance, model);
        let error = provider
            .stream(request("m1", continuation(foreign)))
            .await
            .err()
            .unwrap();
        assert!(
            matches!(&error, ProviderError::InvalidRequest(message) if message.contains("status 400")),
            "{error:?}"
        );
    }
    for received in &mock.received()[1..] {
        let input = received.body["input"].as_array().unwrap();
        assert!(
            input.iter().all(|item| item["type"] != "reasoning"),
            "{input:?}"
        );
        // The text and tool history stay.
        assert_eq!(input[1]["call_id"], "call_1");
        assert_eq!(input[2]["type"], "function_call_output");
    }
}

#[tokio::test]
async fn a_payload_from_another_adapter_stays_off_the_wire() {
    let mock = MockServer::start(vec![fixtures::text_turn("ok")], vec![]).await;
    let provider = provider_for(&mock, None);
    let codex = SealedPayload::new(
        "openai-codex",
        reasoning_item("rs_codex", "theirs", "enc_theirs"),
    );
    events(&provider, request("m1", continuation(codex))).await;
    let input = mock.received()[0].body["input"].as_array().unwrap().clone();
    assert!(input.iter().all(|item| item["type"] != "reasoning"));
}

#[tokio::test]
async fn assistant_text_replays_as_plain_content() {
    let mock = MockServer::start(vec![fixtures::text_turn("ok")], vec![]).await;
    let provider = provider_for(&mock, None);
    let history = vec![
        user("hi"),
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "hello".into(),
            }],
        },
        user("again"),
    ];
    events(&provider, request("m1", history)).await;
    assert_eq!(
        mock.received()[0].body["input"][1],
        json!({"type": "message", "role": "assistant", "content": "hello"})
    );
}

#[tokio::test]
async fn a_reasoning_item_without_its_following_item_stays_off_the_wire() {
    let mock = MockServer::start(vec![fixtures::text_turn("ok")], vec![]).await;
    let provider = provider_for(&mock, None);
    let reasoning = || ContentBlock::Reasoning {
        summary: String::new(),
        sealed: Some(own_payload()),
    };
    // A cut reply: reasoning alone, and text then reasoning.
    let history = vec![
        user("one"),
        Message {
            role: Role::Assistant,
            content: vec![reasoning()],
        },
        user("two"),
        Message {
            role: Role::Assistant,
            content: vec![
                reasoning(),
                ContentBlock::Text {
                    text: "partial".into(),
                },
                reasoning(),
            ],
        },
        user("three"),
    ];
    events(&provider, request("m1", history)).await;
    let input = mock.received()[0].body["input"].as_array().unwrap().clone();
    let kinds: Vec<&str> = input
        .iter()
        .map(|item| item["type"].as_str().unwrap())
        .collect();
    // Only the reasoning that text follows goes back.
    assert_eq!(
        kinds,
        ["message", "message", "reasoning", "message", "message"]
    );
    assert_eq!(input[3]["content"], "partial");
}

fn refusal_stream(deltas: bool) -> MockResponse {
    let mut events_list = vec![
        json!({"type": "response.output_item.added", "output_index": 0,
        "item": {"type": "message", "id": "msg_1", "role": "assistant", "content": []}}),
    ];
    if deltas {
        events_list.push(json!({"type": "response.refusal.delta", "item_id": "msg_1",
            "output_index": 0, "content_index": 0, "delta": "I can't help "}));
        events_list.push(json!({"type": "response.refusal.delta", "item_id": "msg_1",
            "output_index": 0, "content_index": 0, "delta": "with that."}));
    }
    events_list.push(json!({"type": "response.refusal.done", "item_id": "msg_1",
        "output_index": 0, "content_index": 0, "refusal": "I can't help with that."}));
    closed(events_list)
}

#[tokio::test]
async fn a_refusal_shows_its_text_and_ends_as_a_refusal_after_completion() {
    for deltas in [true, false] {
        let mock = MockServer::start(vec![refusal_stream(deltas)], vec![]).await;
        let provider = provider_for(&mock, None);
        let got = events(&provider, request("m1", vec![user("no")])).await;
        assert_eq!(text_of(&got), "I can't help with that.", "deltas {deltas}");
        assert_eq!(
            got.last(),
            Some(&StreamEvent::Done(StopReason::Refusal)),
            "deltas {deltas}"
        );
    }
}

#[tokio::test]
async fn a_message_that_only_completes_shows_its_text_or_its_refusal() {
    let done = |content: Value| {
        closed(vec![
            json!({"type": "response.output_item.done", "output_index": 0,
            "item": {"type": "message", "id": "msg_1", "role": "assistant",
                     "content": content}}),
        ])
    };
    let mock = MockServer::start(
        vec![
            done(json!([{"type": "output_text", "text": "whole answer"}])),
            done(json!([{"type": "refusal", "refusal": "No."}])),
        ],
        vec![],
    )
    .await;
    let provider = provider_for(&mock, None);
    let got = events(&provider, request("m1", vec![user("hi")])).await;
    assert_eq!(text_of(&got), "whole answer");
    assert_eq!(got.last(), Some(&StreamEvent::Done(StopReason::EndTurn)));
    let got = events(&provider, request("m1", vec![user("hi")])).await;
    assert_eq!(text_of(&got), "No.");
    assert_eq!(got.last(), Some(&StreamEvent::Done(StopReason::Refusal)));
}

#[tokio::test]
async fn a_second_message_item_opens_a_paragraph() {
    let mut events_list = fixtures::message_events("msg_1", "I will check.");
    let mut answer = fixtures::message_events("msg_2", "It is fine.");
    for event in &mut answer {
        event["output_index"] = json!(1);
    }
    events_list.extend(answer);
    let mock = MockServer::start(vec![closed(events_list)], vec![]).await;
    let provider = provider_for(&mock, None);
    let got = events(&provider, request("m1", vec![user("check")])).await;
    assert_eq!(text_of(&got), "I will check.\n\nIt is fine.");
}

#[tokio::test]
async fn only_adjacent_message_items_are_joined_by_a_paragraph() {
    let at = |mut events: Vec<Value>, index: u64| {
        for event in &mut events {
            event["output_index"] = json!(index);
        }
        events
    };
    let mut events_list = at(fixtures::message_events("msg_1", "Before."), 0);
    events_list.extend(at(call_events("call_1", "read", &json!({"path": "a"})), 1));
    events_list.extend(at(fixtures::message_events("msg_2", "After."), 2));
    let mock = MockServer::start(vec![closed(events_list)], vec![]).await;
    let provider = provider_for(&mock, None);
    let got = events(&provider, request("m1", vec![user("go")])).await;
    // The call between them ends the first text block, so the second
    // one opens without a blank line.
    assert_eq!(text_of(&got), "Before.After.");
}

#[tokio::test]
async fn arguments_survive_an_empty_final_item_and_a_missing_added_event() {
    let item = |id: &str, call_id: &str, arguments: Option<&str>| {
        let mut item = json!({"type": "function_call", "id": id, "call_id": call_id,
                              "name": "read"});
        if let Some(arguments) = arguments {
            item["arguments"] = json!(arguments);
        }
        item
    };
    let mock = MockServer::start(
        vec![closed(vec![
            // Fragments, then a final item whose arguments are empty.
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": item("fc_1", "call_1", Some(""))}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1",
                   "output_index": 0, "delta": "{\"path\":\"a\"}"}),
            json!({"type": "response.output_item.done", "output_index": 0,
                   "item": item("fc_1", "call_1", Some(""))}),
            // No added event: the whole arguments, then a final item
            // without them.
            json!({"type": "response.function_call_arguments.done", "item_id": "fc_2",
                   "output_index": 1, "arguments": "{\"path\":\"b\"}"}),
            json!({"type": "response.output_item.done", "output_index": 1,
                   "item": item("fc_2", "call_2", None)}),
        ])],
        vec![],
    )
    .await;
    let provider = provider_for(&mock, None);
    let got = events(&provider, request("m1", vec![user("read")])).await;
    assert_eq!(
        ends(&got),
        vec![
            ("call_1".to_string(), json!({"path": "a"})),
            ("call_2".to_string(), json!({"path": "b"})),
        ]
    );
    // Every end has its start.
    let starts = got
        .iter()
        .filter(|e| matches!(e, StreamEvent::ToolCallStart { .. }))
        .count();
    assert_eq!(starts, 2);
}

#[tokio::test]
async fn a_call_starts_under_its_call_id_even_when_the_added_item_lacks_it() {
    let mock = MockServer::start(
        vec![closed(vec![
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"type": "function_call", "id": "fc_1", "name": "read",
                            "arguments": ""}}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1",
                   "output_index": 0, "delta": "{}"}),
            json!({"type": "response.output_item.done", "output_index": 0,
                   "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1",
                            "name": "read", "arguments": "{}"}}),
        ])],
        vec![],
    )
    .await;
    let provider = provider_for(&mock, None);
    let got = events(&provider, request("m1", vec![user("read")])).await;
    assert_eq!(
        got[0],
        StreamEvent::ToolCallStart {
            id: "call_1".into(),
            name: "read".into()
        }
    );
    assert_eq!(ends(&got), vec![("call_1".to_string(), json!({}))]);
}

#[tokio::test]
async fn a_call_without_a_call_id_ends_under_its_item_id() {
    let mock = MockServer::start(
        vec![closed(vec![
            json!({"type": "response.output_item.done", "output_index": 0,
            "item": {"type": "function_call", "id": "fc_1", "name": "read",
                     "arguments": "{}"}}),
        ])],
        vec![],
    )
    .await;
    let provider = provider_for(&mock, None);
    let got = events(&provider, request("m1", vec![user("read")])).await;
    assert_eq!(
        got[0],
        StreamEvent::ToolCallStart {
            id: "fc_1".into(),
            name: "read".into()
        }
    );
    assert_eq!(ends(&got), vec![("fc_1".to_string(), json!({}))]);
}

#[tokio::test]
async fn an_endpoint_that_does_not_parse_fails_once_and_names_the_instance() {
    let mut config = ResponsesConfig::new("hosted", "not a url");
    config.base_delay = Duration::from_secs(30);
    let provider = OpenAiResponses::new(config, Some(KEY.into()));
    let started = std::time::Instant::now();
    let error = provider
        .stream(request("m1", vec![user("hi")]))
        .await
        .err()
        .unwrap();
    assert!(
        matches!(&error, ProviderError::Network(message) if message.contains("`hosted`")),
        "{error:?}"
    );
    // No retry waited out its delay.
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn items_without_ids_are_told_apart_by_their_output_index() {
    // Two calls with call ids but no item ids, by output index.
    let (a, b) = ((1u64, "call_a".to_string()), (2u64, "call_b".to_string()));
    let added = |(index, call_id): &(u64, String)| {
        json!({"type": "response.output_item.added", "output_index": index,
               "item": {"type": "function_call", "call_id": call_id, "name": "read",
                        "arguments": ""}})
    };
    let fragment = |index: u64, text: &str| {
        json!({"type": "response.function_call_arguments.delta", "output_index": index,
               "delta": text})
    };
    let done = |(index, call_id): &(u64, String), arguments: &str| {
        json!({"type": "response.output_item.done", "output_index": index,
               "item": {"type": "function_call", "call_id": call_id, "name": "read",
                        "arguments": arguments}})
    };
    let mock = MockServer::start(
        vec![closed(vec![
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"type": "reasoning", "summary": []}}),
            json!({"type": "response.reasoning_summary_text.delta", "output_index": 0,
                   "summary_index": 0, "delta": "Two reads."}),
            json!({"type": "response.output_item.done", "output_index": 0,
                   "item": {"type": "reasoning",
                            "summary": [{"type": "summary_text", "text": "Two reads."}]}}),
            added(&a),
            added(&b),
            fragment(1, "{\"path\":"),
            fragment(2, "{\"path\":\"b\"}"),
            fragment(1, "\"a\"}"),
            done(&a, ""),
            done(&b, ""),
        ])],
        vec![],
    )
    .await;
    let provider = provider_for(&mock, None);
    let got = events(&provider, request("m1", vec![user("read two")])).await;
    // The summary shows once.
    assert_eq!(reasoning_of(&got), "Two reads.");
    assert_eq!(
        ends(&got),
        vec![
            ("call_a".to_string(), json!({"path": "a"})),
            ("call_b".to_string(), json!({"path": "b"})),
        ]
    );
}

fn incomplete(reason: Option<&str>) -> Value {
    let details = reason.map_or(Value::Null, |reason| json!({"reason": reason}));
    json!({"type": "response.incomplete", "response": {
        "status": "incomplete",
        "incomplete_details": details,
        "usage": {"input_tokens": 7, "output_tokens": 3},
    }})
}

/// A call that opened and streamed part of its arguments, with no
/// completed item.
fn partial_call() -> Vec<Value> {
    call_events("call_cut", "read", &json!({"path": "a"}))[..2].to_vec()
}

#[tokio::test]
async fn known_incomplete_reasons_map_to_stops_and_a_cut_call_never_ends() {
    for (reason, stop) in [
        ("max_output_tokens", StopReason::MaxTokens),
        ("content_filter", StopReason::Refusal),
    ] {
        let mut events_list = fixtures::message_events("msg_1", "partial answer");
        events_list.extend(partial_call());
        events_list.push(incomplete(Some(reason)));
        let mock = MockServer::start(vec![MockResponse::stream(events_list)], vec![]).await;
        let got: Vec<StreamEvent> = raw_events(&mock)
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();
        assert_eq!(text_of(&got), "partial answer", "{reason}");
        assert!(ends(&got).is_empty(), "{reason}: {got:?}");
        assert!(got.contains(&StreamEvent::Usage(Usage {
            input_tokens: 7,
            output_tokens: 3,
            ..Usage::default()
        })));
        assert_eq!(got.last(), Some(&StreamEvent::Done(stop)), "{reason}");
    }
}

#[tokio::test]
async fn an_unknown_or_missing_incomplete_reason_fails_the_call() {
    for reason in [Some("server_decided"), None] {
        let mock = MockServer::start(
            vec![MockResponse::stream(vec![
                fixtures::message_events("msg_1", "half")[1].clone(),
                incomplete(reason),
            ])],
            vec![],
        )
        .await;
        let got = raw_events(&mock).await;
        assert!(
            matches!(got.last(), Some(Err(ProviderError::Server(_)))),
            "{reason:?}: {got:?}"
        );
        assert!(!got.iter().any(|e| matches!(e, Ok(StreamEvent::Done(_)))));
    }
}

#[tokio::test]
async fn failures_fail_the_call_and_never_end_a_cut_call() {
    let failed = |code: &str| {
        json!({"type": "response.failed", "response": {"status": "failed",
            "error": {"code": code, "message": format!("the model failed near {KEY}")}}})
    };
    let error_event = json!({"type": "error", "code": "server_error",
        "message": "stream broke", "param": null, "sequence_number": 4});
    let cases = [
        (failed("server_error"), "server"),
        (failed("rate_limit_exceeded"), "rate"),
        (error_event, "server"),
    ];
    for (terminal, class) in cases {
        let mut events_list = partial_call();
        events_list.push(terminal);
        let mock = MockServer::start(vec![MockResponse::stream(events_list)], vec![]).await;
        let got = raw_events(&mock).await;
        let last = got.last().unwrap();
        match class {
            "rate" => assert!(
                matches!(last, Err(ProviderError::RateLimited { .. })),
                "{got:?}"
            ),
            _ => assert!(matches!(last, Err(ProviderError::Server(_))), "{got:?}"),
        }
        let text = format!("{last:?}");
        assert!(!text.contains(KEY), "{text}");
        assert!(
            !got.iter()
                .any(|e| matches!(e, Ok(StreamEvent::ToolCallEnd { .. }))),
            "{got:?}"
        );
    }
}

#[tokio::test]
async fn a_stream_cut_before_its_terminal_event_is_a_protocol_error() {
    let mut events_list = fixtures::message_events("msg_1", "half");
    events_list.extend(partial_call());
    let cut = MockResponse::stream(events_list);
    // A Chat Completions end marker is no Responses terminal either.
    let marker = MockResponse::events(vec![
        SseEvent::new(fixtures::message_events("msg_1", "half")[1].clone()),
        SseEvent::raw("[DONE]"),
    ]);
    for script in [cut, marker] {
        let mock = MockServer::start(vec![script], vec![]).await;
        let got = raw_events(&mock).await;
        assert!(
            matches!(got.last(), Some(Err(ProviderError::Protocol(_)))),
            "{got:?}"
        );
        assert!(
            !got.iter()
                .any(|e| matches!(e, Ok(StreamEvent::ToolCallEnd { .. }))),
            "{got:?}"
        );
    }
}

#[tokio::test]
async fn unknown_events_and_fields_and_a_new_model_id_pass() {
    let mut events_list = vec![
        json!({"type": "response.in_progress", "sequence_number": 1,
               "response": {"id": "resp_1", "model": "vendor-model-2031-preview"}}),
        json!({"type": "response.vendor_annotation.added", "sequence_number": 2,
               "annotation": {"kind": "unknown"}}),
    ];
    for mut event in call_events("call_1", "read", &json!({"path": "a"})) {
        event["sequence_number"] = json!(3);
        event["obfuscation"] = json!("pad");
        events_list.push(event);
    }
    let mock = MockServer::start(vec![closed(events_list)], vec![]).await;
    let provider = provider_for(&mock, None);
    let got = events(
        &provider,
        request("vendor-model-2031-preview", vec![user("read")]),
    )
    .await;
    assert_eq!(
        ends(&got),
        vec![("call_1".to_string(), json!({"path": "a"}))]
    );
    assert_eq!(got.last(), Some(&StreamEvent::Done(StopReason::ToolUse)));
    assert_eq!(
        mock.received()[0].body["model"],
        "vendor-model-2031-preview"
    );
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
        matches!(&error, ProviderError::Auth(message) if message.contains("`hosted`")),
        "{error:?}"
    );
    assert_eq!(mock.received().len(), 1);
}

#[tokio::test]
async fn a_retry_before_the_stream_never_repeats_an_event() {
    let mock = MockServer::start(
        vec![
            MockResponse::error_with_retry_after(429, 0),
            MockResponse::error(503),
            fixtures::text_turn("once"),
        ],
        vec![],
    )
    .await;
    let provider = provider_for(&mock, None);
    let got = events(&provider, request("m1", vec![user("hi")])).await;
    assert_eq!(text_of(&got), "once");
    let dones = got
        .iter()
        .filter(|e| matches!(e, StreamEvent::Done(_)))
        .count();
    assert_eq!(dones, 1);
    assert_eq!(mock.received().len(), 3);
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
async fn a_reply_that_is_not_an_event_stream_fails_with_its_type() {
    let reply = json!({"id": "resp_1", "object": "response", "output": []}).to_string();
    let mock = MockServer::start(
        vec![MockResponse::sse_body(reply).with_content_type("application/json")],
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
    assert!(message.contains("application/json"), "{message}");
    assert!(message.contains("`hosted`"), "{message}");
}

/// Replay one fixture file through the adapter and return the events,
/// errors included.
async fn replay_fixture(body: &str) -> Vec<Result<StreamEvent, ProviderError>> {
    let mock = MockServer::start(vec![MockResponse::sse_body(body)], vec![]).await;
    raw_events(&mock).await
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

fn usage(input: u64, output: u64, cached: u64) -> Option<Usage> {
    Some(Usage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cached,
        cache_creation_tokens: 0,
    })
}

#[tokio::test]
async fn fixture_hosted_text_seals_silent_reasoning_before_the_answer() {
    let got = unwrap_all(replay_fixture(include_str!("fixtures/responses/hosted-text.sse")).await);
    assert_eq!(text_of(&got), "A crate is one unit of compiled Rust.");
    // No summary was asked for, so no reasoning text shows.
    assert_eq!(reasoning_of(&got), "");
    let sealed = sealed_of(&got);
    assert_eq!(sealed.len(), 1);
    assert_eq!(
        sealed[0].1.data["encrypted_content"],
        "enc_synthetic_text_1_complete"
    );
    assert_eq!(usage_of(&got), usage(1200, 90, 1024));
    assert_eq!(got.last(), Some(&StreamEvent::Done(StopReason::EndTurn)));
}

#[tokio::test]
async fn fixture_hosted_reasoning_tool_keeps_the_completed_item_and_the_call_id() {
    let got = unwrap_all(
        replay_fixture(include_str!("fixtures/responses/hosted-reasoning-tool.sse")).await,
    );
    assert_eq!(
        reasoning_of(&got),
        "**Reading the manifest**\n\nThe user wants the version. I should read Cargo.toml first."
    );
    // The completed item, not the partial one the added event showed.
    let sealed = sealed_of(&got);
    assert_eq!(
        sealed[0].1.data["encrypted_content"],
        "enc_synthetic_2_complete_blob"
    );
    assert_eq!(sealed[0].1.data["id"], "rs_synthetic_2");
    assert_eq!(
        ends(&got),
        vec![(
            "call_synthetic_2".to_string(),
            json!({"path": "Cargo.toml"})
        )]
    );
    assert_eq!(usage_of(&got), usage(2400, 150, 2048));
    assert_eq!(got.last(), Some(&StreamEvent::Done(StopReason::ToolUse)));
}

#[tokio::test]
async fn fixture_hosted_refusal_ends_as_a_refusal() {
    let got =
        unwrap_all(replay_fixture(include_str!("fixtures/responses/hosted-refusal.sse")).await);
    assert_eq!(text_of(&got), "I can't help with that request.");
    assert_eq!(got.last(), Some(&StreamEvent::Done(StopReason::Refusal)));
}

#[tokio::test]
async fn fixture_hosted_incomplete_keeps_the_text_and_drops_the_cut_call() {
    let got =
        unwrap_all(replay_fixture(include_str!("fixtures/responses/hosted-incomplete.sse")).await);
    assert_eq!(text_of(&got), "I will read the manifest.");
    assert!(ends(&got).is_empty(), "{got:?}");
    assert_eq!(got.last(), Some(&StreamEvent::Done(StopReason::MaxTokens)));
}

#[tokio::test]
async fn fixtures_with_a_failure_fail_the_call() {
    for (name, body) in [
        (
            "hosted-failed",
            include_str!("fixtures/responses/hosted-failed.sse"),
        ),
        (
            "hosted-error-event",
            include_str!("fixtures/responses/hosted-error-event.sse"),
        ),
    ] {
        let got = replay_fixture(body).await;
        assert!(
            matches!(got.last(), Some(Err(ProviderError::Server(_)))),
            "{name}: {got:?}"
        );
        assert!(
            !got.iter()
                .any(|event| matches!(event, Ok(StreamEvent::Done(_)))),
            "{name}"
        );
    }
}

#[tokio::test]
async fn fixture_second_provider_reasons_and_sends_a_whole_call() {
    let got = unwrap_all(
        replay_fixture(include_str!("fixtures/responses/second-reasoning-tool.sse")).await,
    );
    assert_eq!(
        reasoning_of(&got),
        "The user wants the manifest. Call read on Cargo.toml."
    );
    assert_eq!(
        sealed_of(&got)[0].1.data["encrypted_content"],
        "enc_synthetic_second_reasoning_blob"
    );
    assert_eq!(
        ends(&got),
        vec![(
            "call_synthetic_second_1".to_string(),
            json!({"path": "Cargo.toml"})
        )]
    );
    assert_eq!(usage_of(&got), usage(512, 180, 384));
    assert_eq!(got.last(), Some(&StreamEvent::Done(StopReason::ToolUse)));
}

#[tokio::test]
async fn fixture_second_provider_text_ignores_unknown_usage_fields() {
    let got = unwrap_all(replay_fixture(include_str!("fixtures/responses/second-text.sse")).await);
    assert_eq!(text_of(&got), "Hello! How can I help?");
    assert_eq!(usage_of(&got), usage(40, 6, 32));
    assert_eq!(got.last(), Some(&StreamEvent::Done(StopReason::EndTurn)));
}

#[tokio::test]
async fn fixture_local_server_reasons_then_calls_a_tool() {
    let got = unwrap_all(
        replay_fixture(include_str!("fixtures/responses/local-reasoning-tool.sse")).await,
    );
    assert!(reasoning_of(&got).starts_with("Okay, the"), "{got:?}");
    let sealed = sealed_of(&got);
    assert!(
        sealed[0].1.data["encrypted_content"]
            .as_str()
            .unwrap()
            .starts_with("Okay, the user wants me to read")
    );
    assert_eq!(
        ends(&got),
        vec![("call_5tf72qsq".to_string(), json!({"path": "notes.txt"}))]
    );
    assert_eq!(usage_of(&got), usage(136, 89, 0));
    assert_eq!(got.last(), Some(&StreamEvent::Done(StopReason::ToolUse)));
}

#[tokio::test]
async fn fixture_local_server_text_reply() {
    let got = unwrap_all(replay_fixture(include_str!("fixtures/responses/local-text.sse")).await);
    assert_eq!(text_of(&got), "Hello! How can I assist you today?");
    assert_eq!(usage_of(&got), usage(24, 119, 2));
    assert_eq!(got.last(), Some(&StreamEvent::Done(StopReason::EndTurn)));
}
