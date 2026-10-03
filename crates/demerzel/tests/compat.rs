//! Hermetic sessions through a named `openai-compat` instance: the
//! runtime, the engine, the real tools, and the adapter against the
//! mock Chat Completions route. No network, no model.

use demerzel::{AuthPaths, Resume, RunConfig, RunError, Runtime, SessionSettings};
use demerzel_core::{ContentBlock, Effort, EngineEvent, NoPrompt, SessionStore, replay};
use demerzel_mock::MockServer;
use demerzel_mock::compat::{reasoning_tool_turn, text_turn};
use demerzel_providers::ProviderError;
use serde_json::{Value, json};

fn noop(_: EngineEvent<'_>) {}

/// The strict server's rule: every assistant message with tool calls
/// carries its `reasoning_content` back.
fn requires_reasoning_replay(body: &Value) -> Result<(), String> {
    for message in body["messages"].as_array().ok_or("no messages")? {
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

struct Bed {
    mock: MockServer,
    project: tempfile::TempDir,
    data: tempfile::TempDir,
}

impl Bed {
    async fn start(script: Vec<demerzel_mock::MockResponse>) -> Self {
        let bed = Self {
            mock: MockServer::start(script, Vec::new()).await,
            project: tempfile::tempdir().unwrap(),
            data: tempfile::tempdir().unwrap(),
        };
        std::fs::write(bed.project.path().join("f.txt"), "alpha\nbeta\n").unwrap();
        bed
    }

    /// A run on instance `local` of the compatible adapter, in `auto`
    /// mode, at the mock's `/v1` root.
    fn config(&self) -> RunConfig {
        let mut config = RunConfig::new(self.project.path().to_path_buf());
        config.provider = "local".into();
        config.kind = Some("openai-compat".into());
        config.model = "m1".into();
        config.base_url = Some(format!("{}/v1", self.mock.url()));
        config.approval = demerzel_core::ApprovalMode::Auto;
        config.data_root = Some(self.data.path().join("store"));
        // The run reads its own store, never the developer's.
        config.auth = Some(AuthPaths {
            store: self.data.path().join("auth.json"),
            codex_cli: None,
        });
        config
    }

    fn latest_session(&self) -> std::path::PathBuf {
        SessionStore::at(self.data.path().join("store"))
            .latest(self.project.path())
            .unwrap()
            .unwrap()
    }
}

#[tokio::test]
async fn a_tool_session_replays_reasoning_and_names_the_instance() {
    let bed = Bed::start(vec![
        reasoning_tool_turn(
            "reasoning_content",
            "read the file first",
            "call_1",
            "read",
            json!({"path": "f.txt"}),
        ),
        text_turn("the file holds two lines").with_check(requires_reasoning_replay),
    ])
    .await;
    let runtime = Runtime::new(bed.config()).unwrap();
    let outcome = runtime.one_shot("read f.txt".into(), &noop).await.unwrap();
    assert_eq!(outcome, demerzel_core::TurnOutcome::Completed);

    let received = bed.mock.received();
    assert_eq!(received.len(), 2);
    assert!(received.iter().all(|r| r.path == "/v1/chat/completions"));
    // No key variable: no Authorization header.
    assert!(received.iter().all(|r| r.authorization.is_none()));
    // The default effort level stays off the wire.
    assert!(
        received
            .iter()
            .all(|r| r.body.get("reasoning_effort").is_none())
    );
    // The continuation carries the tool's result and the reasoning.
    let messages = received[1].body["messages"].as_array().unwrap();
    let assistant = &messages[2];
    assert_eq!(assistant["reasoning_content"], "read the file first");
    assert_eq!(messages[3]["role"], "tool");
    assert_eq!(messages[3]["tool_call_id"], "call_1");
    assert!(messages[3]["content"].as_str().unwrap().contains("alpha"));

    // The session file names the instance and its adapter, and the
    // sealed reasoning carries its identity.
    let replayed = replay(&bed.latest_session()).unwrap();
    let meta = replayed.meta.unwrap();
    assert_eq!(meta.provider, "local");
    assert_eq!(meta.kind.as_deref(), Some("openai-compat"));
    assert_eq!(meta.effort, None);
    let sealed = replayed
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .find_map(|block| match block {
            ContentBlock::Reasoning { sealed, .. } => sealed.clone(),
            _ => None,
        })
        .expect("a sealed reasoning block in the file");
    assert_eq!(sealed.instance.as_deref(), Some("local"));
    assert_eq!(sealed.model.as_deref(), Some("m1"));
}

#[tokio::test]
async fn resume_keeps_the_reasoning_for_the_same_model_only() {
    let bed = Bed::start(vec![
        reasoning_tool_turn(
            "reasoning_content",
            "read the file first",
            "call_1",
            "read",
            json!({"path": "f.txt"}),
        ),
        text_turn("done").with_check(requires_reasoning_replay),
        // After resume, the strict server still sees the reasoning.
        text_turn("still here").with_check(requires_reasoning_replay),
        // Another model gets no foreign payload.
        text_turn("new model"),
    ])
    .await;
    let mut config = bed.config();
    Runtime::new(config.clone())
        .unwrap()
        .one_shot("read f.txt".into(), &noop)
        .await
        .unwrap();

    config.resume = Resume::Latest;
    Runtime::new(config.clone())
        .unwrap()
        .one_shot("and now?".into(), &noop)
        .await
        .unwrap();
    let resumed = &bed.mock.received()[2].body;
    assert_eq!(
        resumed["messages"][2]["reasoning_content"],
        "read the file first"
    );

    config.model = "m2".into();
    Runtime::new(config)
        .unwrap()
        .one_shot("switch".into(), &noop)
        .await
        .unwrap();
    let switched = &bed.mock.received()[3].body;
    assert_eq!(switched["model"], "m2");
    let assistant = &switched["messages"][2];
    assert_eq!(assistant["tool_calls"][0]["id"], "call_1");
    assert!(assistant.get("reasoning_content").is_none(), "{assistant}");
}

#[tokio::test]
async fn effort_reaches_a_compat_endpoint_only_when_set() {
    let bed = Bed::start(vec![text_turn("one"), text_turn("two"), text_turn("three")]).await;
    let runtime = Runtime::new(bed.config()).unwrap();
    let settings = SessionSettings::from_config(runtime.config());
    let (session, replay) = runtime.open_session(&Resume::Fresh, &settings).unwrap();
    let mut engine = runtime.engine(session, replay.messages, &settings).unwrap();
    engine
        .user_turn("first".into(), &noop, &mut NoPrompt)
        .await
        .unwrap();
    engine.set_effort(Effort::Low).unwrap();
    engine
        .user_turn("second".into(), &noop, &mut NoPrompt)
        .await
        .unwrap();
    let received = bed.mock.received();
    assert!(received[0].body.get("reasoning_effort").is_none());
    assert_eq!(received[1].body["reasoning_effort"], "low");

    // A level the config sets goes out from the first call.
    let mut config = bed.config();
    config.effort = Effort::Medium;
    config.effort_set = true;
    let runtime = Runtime::new(config).unwrap();
    runtime.one_shot("third".into(), &noop).await.unwrap();
    assert_eq!(bed.mock.received()[2].body["reasoning_effort"], "medium");
}

#[tokio::test]
async fn an_unset_key_variable_stops_the_turn_with_its_name() {
    let bed = Bed::start(vec![text_turn("never")]).await;
    let mut config = bed.config();
    // A name no test and no machine sets.
    config.api_key_env = Some("DMZ_TEST_COMPAT_KEY_NEVER_SET".into());
    let error = Runtime::new(config)
        .unwrap()
        .one_shot("hi".into(), &noop)
        .await
        .err()
        .unwrap();
    assert!(
        matches!(&error, RunError::Auth(ProviderError::Auth(message)) if message.contains("DMZ_TEST_COMPAT_KEY_NEVER_SET")),
        "{error}"
    );
    assert!(bed.mock.received().is_empty());
}

#[test]
fn a_compat_run_config_without_an_endpoint_or_model_stops_at_boot() {
    let project = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let mut config = RunConfig::new(project.path().to_path_buf());
    config.provider = "local".into();
    config.kind = Some("openai-compat".into());
    config.model = "m1".into();
    config.data_root = Some(data.path().to_path_buf());
    let error = Runtime::new(config.clone()).err().unwrap();
    assert!(error.to_string().contains("needs `base_url`"), "{error}");

    config.base_url = Some("http://127.0.0.1:9/v1".into());
    config.model = String::new();
    let error = Runtime::new(config).err().unwrap();
    assert!(error.to_string().contains("needs `model`"), "{error}");
}

#[tokio::test]
async fn a_level_set_before_the_first_prompt_reaches_the_first_request() {
    // The state `/effort low` leaves before a session opens: the
    // interfaces set the level and mark it set.
    let bed = Bed::start(vec![text_turn("one")]).await;
    let runtime = Runtime::new(bed.config()).unwrap();
    let mut settings = SessionSettings::from_config(runtime.config());
    assert_eq!(settings.request_effort(), None);
    settings.effort = Effort::Low;
    settings.effort_set = true;
    assert_eq!(settings.request_effort(), Some(Effort::Low));
    let (session, replay) = runtime.open_session(&Resume::Fresh, &settings).unwrap();
    let mut engine = runtime.engine(session, replay.messages, &settings).unwrap();
    engine
        .user_turn("first".into(), &noop, &mut NoPrompt)
        .await
        .unwrap();
    assert_eq!(bed.mock.received()[0].body["reasoning_effort"], "low");
    let meta = replay_meta(&bed);
    assert_eq!(meta.effort.as_deref(), Some("low"));
}

fn replay_meta(bed: &Bed) -> demerzel_core::SessionMeta {
    replay(&bed.latest_session()).unwrap().meta.unwrap()
}
