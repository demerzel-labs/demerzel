//! Hermetic loop tests: the real engine, the real tools, and the real
//! adapter against the mock server. No network, no model.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use demerzel::{AuthPaths, RunConfig, run_one_shot};
use demerzel_core::{
    ApprovalMode, ApprovalPolicy, ContentBlock, Engine, EngineConfig, EngineEvent, NoPrompt, Role,
    Session, SessionStore, TurnOutcome, replay,
};
use demerzel_mock::{MockResponse, MockServer};
use demerzel_providers::auth::AuthStore;
use demerzel_providers::openai_codex::{CodexConfig, OpenAiCodex};
use demerzel_tools::{Registry, file_tag};
use serde_json::{Value, json};

fn text_delta(text: &str) -> Value {
    json!({"type": "response.output_text.delta", "delta": text})
}

fn call_added(item_id: &str, call_id: &str, name: &str) -> Value {
    json!({
        "type": "response.output_item.added",
        "item": {"type": "function_call", "id": item_id, "call_id": call_id, "name": name},
    })
}

fn call_done(call_id: &str, input: Value) -> Value {
    json!({
        "type": "response.output_item.done",
        "item": {
            "type": "function_call",
            "call_id": call_id,
            "arguments": input.to_string(),
        },
    })
}

fn completed() -> Value {
    json!({
        "type": "response.completed",
        "response": {"usage": {
            "input_tokens": 10,
            "output_tokens": 5,
            "input_tokens_details": {"cached_tokens": 2},
        }},
    })
}

fn tool_turn(index: usize, name: &str, input: Value) -> MockResponse {
    MockResponse::stream(vec![
        call_added(&format!("item_{index}"), &format!("call_{index}"), name),
        call_done(&format!("call_{index}"), input),
        completed(),
    ])
}

fn text_turn(text: &str) -> MockResponse {
    MockResponse::stream(vec![text_delta(text), completed()])
}

fn noop(_: EngineEvent<'_>) {}

/// A project dir, a data dir with a filled auth store, and the adapter
/// pointed at the mock server.
struct Bed {
    mock: MockServer,
    project: tempfile::TempDir,
    data: tempfile::TempDir,
    provider: OpenAiCodex,
    registry: Registry,
}

impl Bed {
    async fn start(responses: Vec<MockResponse>) -> Self {
        let mock = MockServer::start(responses, Vec::new()).await;
        let project = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let auth = auth_store(data.path());
        let provider = OpenAiCodex::new(
            CodexConfig {
                base_url: mock.url(),
                ..CodexConfig::default()
            },
            auth,
        );
        let registry = Registry::full(project.path());
        Self {
            mock,
            project,
            data,
            provider,
            registry,
        }
    }

    fn engine(&self, mode: ApprovalMode) -> Engine<'_> {
        let session = SessionStore::at(self.data.path())
            .create(self.project.path(), "openai-codex", "test-model", None)
            .unwrap();
        Engine::new(
            &self.provider,
            &self.registry,
            ApprovalPolicy::new(mode),
            EngineConfig::new("test-model"),
            session,
        )
    }

    fn write_file(&self, name: &str, content: &str) {
        std::fs::write(self.project.path().join(name), content).unwrap();
    }

    fn read_file(&self, name: &str) -> String {
        std::fs::read_to_string(self.project.path().join(name)).unwrap()
    }
}

fn auth_store(data_dir: &Path) -> AuthStore {
    let path = auth_path(data_dir);
    std::fs::write(
        &path,
        json!({
            "version": 1,
            "openai_codex": {"access_token": "test-token", "refresh_token": "test-refresh"},
        })
        .to_string(),
    )
    .unwrap();
    AuthStore::open(path, None).unwrap()
}

fn auth_path(data_dir: &Path) -> PathBuf {
    data_dir.join("auth.json")
}

#[tokio::test]
async fn a_read_edit_bash_session_completes_end_to_end() {
    let content = "alpha\nbeta\n";
    let tag = file_tag(content);
    let bed = Bed::start(vec![
        tool_turn(1, "read", json!({"path": "f.txt"})),
        tool_turn(
            2,
            "edit",
            json!({
                "path": "f.txt",
                "tag": tag,
                "edits": [{"op": "replace", "start": 1, "end": 1, "content": "ALPHA"}],
            }),
        ),
        tool_turn(3, "bash", json!({"command": "cat f.txt"})),
        text_turn("all done"),
    ])
    .await;
    bed.write_file("f.txt", content);
    let mut engine = bed.engine(ApprovalMode::Auto);

    let outcome = engine
        .user_turn("update f.txt".into(), &noop, &mut NoPrompt)
        .await
        .unwrap();

    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(bed.read_file("f.txt"), "ALPHA\nbeta\n");
    // The final assistant message carries the text.
    let last = engine.history().last().unwrap();
    assert_eq!(
        last.content,
        vec![ContentBlock::Text {
            text: "all done".into()
        }]
    );

    // Four model calls; the first sent all six tool definitions.
    let received = bed.mock.received();
    assert_eq!(received.len(), 4);
    let names: Vec<&str> = received[0].body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["read", "edit", "create", "bash", "grep", "glob"]);
    // The bash output went back to the model on the last request.
    let last_body = received[3].body.to_string();
    assert!(last_body.contains("ALPHA"));

    // The session file rebuilds the exact history and holds usage and
    // approvals with their call ids.
    let replayed = replay(engine.session().path()).unwrap();
    assert_eq!(replayed.messages, engine.history());
    let raw = std::fs::read_to_string(engine.session().path()).unwrap();
    assert!(raw.contains(r#""type":"usage""#));
    assert!(raw.contains(r#""type":"approval""#));
    assert!(raw.contains(r#""tool_use_id":"call_2""#));
}

#[tokio::test]
async fn a_stale_tag_error_reaches_the_model_and_recovery_succeeds() {
    let content = "one\ntwo\n";
    let tag = file_tag(content);
    let bed = Bed::start(vec![
        tool_turn(
            1,
            "edit",
            json!({
                "path": "f.txt",
                "tag": "000000",
                "edits": [{"op": "replace", "start": 1, "end": 1, "content": "ONE"}],
            }),
        ),
        tool_turn(
            2,
            "edit",
            json!({
                "path": "f.txt",
                "tag": tag,
                "edits": [{"op": "replace", "start": 1, "end": 1, "content": "ONE"}],
            }),
        ),
        text_turn("fixed"),
    ])
    .await;
    bed.write_file("f.txt", content);
    let mut engine = bed.engine(ApprovalMode::Auto);

    let outcome = engine
        .user_turn("edit f.txt".into(), &noop, &mut NoPrompt)
        .await
        .unwrap();

    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(bed.read_file("f.txt"), "ONE\ntwo\n");

    // The stale-tag error went back to the model as a tool result.
    let ContentBlock::ToolResult {
        content, is_error, ..
    } = &engine.history()[2].content[0]
    else {
        panic!("expected a tool result");
    };
    assert!(is_error);
    assert!(content.contains("stale tag"));
    let second_request = bed.mock.received()[1].body.to_string();
    assert!(second_request.contains("stale tag"));
}

#[tokio::test]
async fn the_wire_prefix_is_byte_stable_across_turns() {
    let bed = Bed::start(vec![
        tool_turn(1, "bash", json!({"command": "echo hi"})),
        text_turn("one done"),
        text_turn("two done"),
    ])
    .await;
    let mut engine = bed.engine(ApprovalMode::Auto);

    engine
        .user_turn("one".into(), &noop, &mut NoPrompt)
        .await
        .unwrap();
    engine
        .user_turn("two".into(), &noop, &mut NoPrompt)
        .await
        .unwrap();

    let received = bed.mock.received();
    assert_eq!(received.len(), 3);
    for pair in received.windows(2) {
        assert_eq!(pair[0].body["instructions"], pair[1].body["instructions"]);
        assert_eq!(pair[0].body["tools"], pair[1].body["tools"]);
        let earlier = pair[0].body["input"].as_array().unwrap();
        let later = pair[1].body["input"].as_array().unwrap();
        assert!(earlier.len() <= later.len());
        for (index, item) in earlier.iter().enumerate() {
            // Byte-identical, not only structurally equal.
            assert_eq!(item.to_string(), later[index].to_string());
        }
    }
    // Approval state never enters a request body.
    for request in &received {
        assert!(!request.body.to_string().contains("approval"));
    }
}

#[tokio::test]
async fn an_aborted_turn_leaves_a_valid_resumable_session() {
    let bed = Bed::start(vec![
        MockResponse::stream_then_stall(vec![text_delta("par")]),
        text_turn("recovered"),
    ])
    .await;
    let mut engine = bed.engine(ApprovalMode::Auto);
    let session_path = engine.session().path().to_path_buf();

    // Abort by dropping the turn future mid-stream, the Ctrl-C shape.
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let tx = Mutex::new(Some(tx));
    let sink = |event: EngineEvent<'_>| {
        if let EngineEvent::TextDelta(_) = event
            && let Some(tx) = tx.lock().unwrap().take()
        {
            let _ = tx.send(());
        }
    };
    {
        let mut approver = NoPrompt;
        let turn = engine.user_turn("go".into(), &sink, &mut approver);
        tokio::pin!(turn);
        tokio::select! {
            _ = &mut turn => panic!("the stalled turn must not complete"),
            _ = rx => {}
        }
    }
    drop(engine);

    // Only complete records reached the file: the user message alone.
    let replayed = replay(&session_path).unwrap();
    assert_eq!(replayed.messages.len(), 1);
    assert_eq!(replayed.messages[0].role, Role::User);

    // Resume rebuilds that history and the session continues.
    let (session, history) = {
        let (session, replayed) = Session::resume(&session_path).unwrap();
        (session, replayed.messages)
    };
    let mut resumed = Engine::with_history(
        &bed.provider,
        &bed.registry,
        ApprovalPolicy::new(ApprovalMode::Auto),
        EngineConfig::new("test-model"),
        session,
        history,
    );
    let outcome = resumed
        .user_turn("again".into(), &noop, &mut NoPrompt)
        .await
        .unwrap();
    assert_eq!(outcome, TurnOutcome::Completed);
    let replayed = replay(&session_path).unwrap();
    assert_eq!(replayed.messages, resumed.history());
    assert_eq!(replayed.messages.len(), 3);
}

#[tokio::test]
async fn one_shot_read_only_drops_write_tools_and_streams_text() {
    let bed = Bed::start(vec![text_turn("hi there")]).await;
    let mut config = RunConfig::new(bed.project.path().to_path_buf());
    config.model = "test-model".into();
    config.data_root = Some(bed.data.path().join("store"));
    config.base_url = Some(bed.mock.url());
    config.auth = Some(AuthPaths {
        store: auth_path(bed.data.path()),
        codex_cli: None,
    });

    let text = Mutex::new(String::new());
    let sink = |event: EngineEvent<'_>| {
        if let EngineEvent::TextDelta(delta) = event {
            text.lock().unwrap().push_str(delta);
        }
    };
    let outcome = run_one_shot(&config, "hello".into(), &sink).await.unwrap();

    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(*text.lock().unwrap(), "hi there");
    // Read-only: the write tools are not advertised at all.
    let received = bed.mock.received();
    let names: Vec<&str> = received[0].body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["read", "grep", "glob"]);
    // The session landed under the injected data root.
    let store = SessionStore::at(bed.data.path().join("store"));
    assert!(store.latest(bed.project.path()).unwrap().is_some());
}

#[tokio::test]
async fn one_shot_dispatches_to_the_anthropic_provider() {
    let mock = MockServer::start(
        vec![demerzel_mock::anthropic::text_turn("claude here")],
        Vec::new(),
    )
    .await;
    let project = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    // The key sits in the store; nothing else writes that file.
    let store_path = data.path().join("auth.json");
    std::fs::write(
        &store_path,
        json!({"version": 2, "anthropic": {"api_key": "sk-store-key"}}).to_string(),
    )
    .unwrap();

    let mut config = RunConfig::new(project.path().to_path_buf());
    config.provider = "anthropic".into();
    config.model = "claude-test".into();
    config.data_root = Some(data.path().join("store"));
    config.base_url = Some(mock.url());
    config.auth = Some(AuthPaths {
        store: store_path,
        codex_cli: None,
    });

    let text = Mutex::new(String::new());
    let sink = |event: EngineEvent<'_>| {
        if let EngineEvent::TextDelta(delta) = event {
            text.lock().unwrap().push_str(delta);
        }
    };
    let outcome = run_one_shot(&config, "hello".into(), &sink).await.unwrap();

    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(*text.lock().unwrap(), "claude here");
    let received = mock.received();
    assert_eq!(received[0].path, "/v1/messages");
    // The credential order (environment over store) is covered by
    // `credential_order_prefers_the_environment_over_the_store` in
    // tests/doctor.rs, through the real binary with a controlled
    // environment. In process, the ambient environment can supply the
    // key, so this test only asserts a key was sent.
    assert!(received[0].api_key.is_some());
    assert_eq!(received[0].body["model"], "claude-test");
    // The session records the anthropic provider id.
    let store = SessionStore::at(data.path().join("store"));
    let path = store.latest(project.path()).unwrap().unwrap();
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.contains(r#""provider":"anthropic""#), "{raw}");
}

#[tokio::test]
async fn one_shot_continue_resumes_the_latest_session() {
    let bed = Bed::start(vec![text_turn("first"), text_turn("second")]).await;
    let mut config = RunConfig::new(bed.project.path().to_path_buf());
    config.model = "test-model".into();
    config.data_root = Some(bed.data.path().join("store"));
    config.base_url = Some(bed.mock.url());
    config.auth = Some(AuthPaths {
        store: auth_path(bed.data.path()),
        codex_cli: None,
    });

    run_one_shot(&config, "one".into(), &noop).await.unwrap();
    config.continue_last = true;
    run_one_shot(&config, "two".into(), &noop).await.unwrap();

    // One session file, holding both turns.
    let store = SessionStore::at(bed.data.path().join("store"));
    let sessions: Vec<_> = std::fs::read_dir(store.sessions_dir(bed.project.path()))
        .unwrap()
        .collect();
    assert_eq!(sessions.len(), 1);
    let path = store.latest(bed.project.path()).unwrap().unwrap();
    let replayed = replay(&path).unwrap();
    assert_eq!(replayed.messages.len(), 4);
    // Resume appended a fresh meta record for the continuing run.
    let raw = std::fs::read_to_string(&path).unwrap();
    assert_eq!(raw.matches(r#""type":"meta""#).count(), 2);
    // The second request resent the first turn's history.
    let second_body = bed.mock.received()[1].body.to_string();
    assert!(second_body.contains("first"));
    assert!(second_body.contains("one"));
}

/// The measurement seams of the evaluation contract, as the session
/// file carries them: the environment in the meta record, every tool
/// step in the messages and the approval records, the token counts
/// with the cached part in the usage records, and how each user turn
/// ended in the turn records.
#[tokio::test]
async fn the_session_file_carries_the_measurement_seams() {
    let bed = Bed::start(vec![
        tool_turn(1, "read", json!({"path": "f.txt"})),
        tool_turn(2, "bash", json!({"command": "true"})),
        text_turn("done"),
    ])
    .await;
    bed.write_file("f.txt", "alpha\n");
    let mut engine = bed.engine(ApprovalMode::Auto);
    let outcome = engine
        .user_turn("look around".into(), &noop, &mut NoPrompt)
        .await
        .unwrap();
    assert_eq!(outcome, TurnOutcome::Completed);

    let raw = std::fs::read_to_string(engine.session().path()).unwrap();
    let records: Vec<Value> = raw
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let of = |kind: &str| -> Vec<&Value> {
        records
            .iter()
            .filter(|record| record["type"] == kind)
            .collect()
    };

    // Environment: the project directory, the provider, the model.
    let meta = of("meta");
    assert_eq!(meta.len(), 1);
    for field in ["cwd", "provider", "model", "created", "schema"] {
        assert!(meta[0].get(field).is_some(), "meta lacks {field}");
    }
    assert_eq!(meta[0]["provider"], "openai-codex");

    // Tool steps: each call and its result, in the messages; the
    // approval decision for each, with the call id.
    let blocks: Vec<&Value> = of("message")
        .into_iter()
        .flat_map(|message| message["content"].as_array().unwrap())
        .collect();
    let uses = blocks.iter().filter(|b| b["type"] == "tool_use").count();
    let results = blocks.iter().filter(|b| b["type"] == "tool_result").count();
    assert_eq!((uses, results), (2, 2));
    let approvals = of("approval");
    assert_eq!(approvals.len(), 2);
    assert!(approvals.iter().all(|a| a["tool_use_id"].is_string()));

    // Usage with the cached part, one record per model call.
    let usage = of("usage");
    assert_eq!(usage.len(), 3);
    for record in &usage {
        for field in ["input_tokens", "output_tokens", "cache_read_tokens"] {
            assert!(record[field].is_u64(), "usage lacks {field}");
        }
    }
    assert_eq!(usage[0]["cache_read_tokens"], 2);

    // Termination: the turn record closes the turn with its outcome.
    let turns = of("turn");
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0]["outcome"], "completed");
    assert!(turns[0].get("error").is_none());
}
