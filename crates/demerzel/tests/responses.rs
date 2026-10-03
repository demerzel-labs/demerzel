//! Hermetic sessions through a named `openai-responses` instance: the
//! runtime, the engine, the real tools, and the adapter against the
//! mock Responses route. No network, no model.

use demerzel::{AuthPaths, Resume, RunConfig, RunError, Runtime, SessionSettings};
use demerzel_core::{ContentBlock, Effort, EngineEvent, NoPrompt, SessionStore, replay};
use demerzel_mock::MockServer;
use demerzel_mock::responses::{reasoning_item, reasoning_tool_turn, text_turn};
use demerzel_providers::ProviderError;
use serde_json::{Value, json};

fn noop(_: EngineEvent<'_>) {}

/// The strict server's rule: every `function_call` input item follows
/// its reasoning item, whole, with the encrypted content.
fn requires_reasoning_replay(body: &Value) -> Result<(), String> {
    let input = body["input"].as_array().ok_or("no input")?;
    for (at, item) in input.iter().enumerate() {
        if item["type"] == "function_call"
            && at
                .checked_sub(1)
                .is_none_or(|before| input[before] != own_item())
        {
            return Err("a function_call was provided without its reasoning item".to_string());
        }
    }
    Ok(())
}

fn own_item() -> Value {
    reasoning_item("rs_mock", "read the file first", "enc_mock_1")
}

fn reasoning_call() -> demerzel_mock::MockResponse {
    reasoning_tool_turn(
        "read the file first",
        "enc_mock_1",
        "call_1",
        "read",
        json!({"path": "f.txt"}),
    )
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

    /// A run on instance `hosted` of the Responses adapter, in `auto`
    /// mode, at the mock's `/v1` root.
    fn config(&self) -> RunConfig {
        let mut config = RunConfig::new(self.project.path().to_path_buf());
        config.provider = "hosted".into();
        config.kind = Some("openai-responses".into());
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
        reasoning_call(),
        text_turn("the file holds two lines").with_check(requires_reasoning_replay),
    ])
    .await;
    let runtime = Runtime::new(bed.config()).unwrap();
    let outcome = runtime.one_shot("read f.txt".into(), &noop).await.unwrap();
    assert_eq!(outcome, demerzel_core::TurnOutcome::Completed);

    let received = bed.mock.received();
    assert_eq!(received.len(), 2);
    assert!(received.iter().all(|r| r.path == "/v1/responses"));
    // No key variable: no Authorization header, and no account header.
    assert!(received.iter().all(|r| r.authorization.is_none()));
    assert!(received.iter().all(|r| r.account_id.is_none()));
    // The default effort level stays off the wire.
    assert!(received.iter().all(|r| r.body.get("reasoning").is_none()));
    // The continuation carries the reasoning item, the call, and the
    // tool's result, in the order the reply produced them.
    let input = received[1].body["input"].as_array().unwrap();
    assert_eq!(input[1], own_item());
    assert_eq!(input[2]["type"], "function_call");
    assert_eq!(input[2]["call_id"], "call_1");
    assert_eq!(input[3]["type"], "function_call_output");
    assert_eq!(input[3]["call_id"], "call_1");
    assert!(input[3]["output"].as_str().unwrap().contains("alpha"));

    // The session file names the instance and its adapter, and the
    // sealed reasoning carries its identity.
    let replayed = replay(&bed.latest_session()).unwrap();
    let meta = replayed.meta.unwrap();
    assert_eq!(meta.provider, "hosted");
    assert_eq!(meta.kind.as_deref(), Some("openai-responses"));
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
    assert_eq!(sealed.provider, "openai-responses");
    assert_eq!(sealed.instance.as_deref(), Some("hosted"));
    assert_eq!(sealed.model.as_deref(), Some("m1"));
    assert_eq!(sealed.data, own_item());
}

#[tokio::test]
async fn resume_keeps_the_reasoning_for_the_same_model_only() {
    let bed = Bed::start(vec![
        reasoning_call(),
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
    assert_eq!(bed.mock.received()[2].body["input"][1], own_item());

    config.model = "m2".into();
    Runtime::new(config)
        .unwrap()
        .one_shot("switch".into(), &noop)
        .await
        .unwrap();
    let switched = &bed.mock.received()[3].body;
    assert_eq!(switched["model"], "m2");
    let input = switched["input"].as_array().unwrap();
    assert!(
        input.iter().all(|item| item["type"] != "reasoning"),
        "{input:?}"
    );
    // The text and tool history stay usable after the switch.
    assert_eq!(input[1]["type"], "function_call");
    assert_eq!(input[1]["call_id"], "call_1");
    assert_eq!(input[2]["type"], "function_call_output");
}

#[tokio::test]
async fn effort_reaches_the_endpoint_only_when_set() {
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
    assert!(received[0].body.get("reasoning").is_none());
    assert_eq!(received[1].body["reasoning"], json!({"effort": "low"}));

    // A level the config sets goes out from the first call.
    let mut config = bed.config();
    config.effort = Effort::Medium;
    config.effort_set = true;
    let runtime = Runtime::new(config).unwrap();
    runtime.one_shot("third".into(), &noop).await.unwrap();
    assert_eq!(
        bed.mock.received()[2].body["reasoning"],
        json!({"effort": "medium"})
    );
}

#[tokio::test]
async fn an_instance_reads_only_its_own_key_from_the_store() {
    let bed = Bed::start(vec![text_turn("one"), text_turn("two")]).await;
    // The adapters' own credentials and another instance's key never
    // reach this instance's endpoint.
    let foreign = bed.data.path().join("foreign.json");
    std::fs::write(
        &foreign,
        json!({
            "version": 3,
            "openai_codex": {"access_token": "login-token", "refresh_token": "refresh"},
            "anthropic": {"api_key": "sk-fake-anthropic"},
            "keys": {"elsewhere": {"api_key": "fake-other-key"}},
        })
        .to_string(),
    )
    .unwrap();
    // The instance's own entry, checked at its endpoint, goes out as
    // the bearer key.
    let own = bed.data.path().join("own.json");
    let endpoint = format!("{}/v1", bed.mock.url());
    std::fs::write(
        &own,
        json!({"version": 3, "keys": {"hosted": {"api_key": "fake-own-key", "base_url": endpoint}}})
            .to_string(),
    )
    .unwrap();
    for path in [foreign, own] {
        let mut config = bed.config();
        config.auth = Some(AuthPaths {
            store: path,
            codex_cli: None,
        });
        Runtime::new(config)
            .unwrap()
            .one_shot("hi".into(), &noop)
            .await
            .unwrap();
    }
    let received = bed.mock.received();
    assert_eq!(received.len(), 2);
    assert_eq!(received[0].authorization, None);
    assert_eq!(received[0].account_id, None);
    assert_eq!(
        received[1].authorization.as_deref(),
        Some("Bearer fake-own-key")
    );

    // A key checked at another endpoint stops the turn: it goes only
    // to the endpoint its login named.
    let moved = bed.data.path().join("moved.json");
    std::fs::write(
        &moved,
        json!({"version": 3, "keys": {"hosted": {
            "api_key": "fake-moved-key",
            "base_url": "https://api.example.test/v1",
        }}})
        .to_string(),
    )
    .unwrap();
    let mut config = bed.config();
    config.auth = Some(AuthPaths {
        store: moved,
        codex_cli: None,
    });
    let error = Runtime::new(config)
        .unwrap()
        .one_shot("hi".into(), &noop)
        .await
        .err()
        .unwrap();
    assert!(
        matches!(&error, RunError::Auth(ProviderError::Auth(message)) if message.contains("dmz login hosted")),
        "{error:?}"
    );

    // A store that does not parse can hold the key, so it stops the
    // turn before a request goes out.
    let broken = bed.data.path().join("broken.json");
    std::fs::write(&broken, "{ not json").unwrap();
    let mut config = bed.config();
    config.auth = Some(AuthPaths {
        store: broken,
        codex_cli: None,
    });
    let error = Runtime::new(config.clone())
        .unwrap()
        .one_shot("hi".into(), &noop)
        .await
        .err()
        .unwrap();
    assert!(
        matches!(&error, RunError::Auth(ProviderError::Auth(message)) if message.contains("auth store")),
        "{error:?}"
    );
    // A key variable decides before the store: an unset one names the
    // variable, not the store. The real binary covers a set one in
    // tests/doctor.rs.
    config.api_key_env = Some("DMZ_TEST_RESPONSES_KEY_NEVER_SET".into());
    let error = Runtime::new(config)
        .unwrap()
        .one_shot("hi".into(), &noop)
        .await
        .err()
        .unwrap();
    assert!(
        error
            .to_string()
            .contains("DMZ_TEST_RESPONSES_KEY_NEVER_SET"),
        "{error:?}"
    );
    assert_eq!(bed.mock.received().len(), 2);
}

#[tokio::test]
async fn an_unset_key_variable_stops_the_turn_with_its_name() {
    let bed = Bed::start(vec![text_turn("never")]).await;
    let mut config = bed.config();
    // A name no test and no machine sets.
    config.api_key_env = Some("DMZ_TEST_RESPONSES_KEY_NEVER_SET".into());
    let error = Runtime::new(config)
        .unwrap()
        .one_shot("hi".into(), &noop)
        .await
        .err()
        .unwrap();
    assert!(
        matches!(&error, RunError::Auth(ProviderError::Auth(message)) if message.contains("DMZ_TEST_RESPONSES_KEY_NEVER_SET")),
        "{error}"
    );
    assert!(bed.mock.received().is_empty());
}

#[test]
fn a_run_config_without_an_endpoint_or_model_stops_at_boot() {
    let project = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let mut config = RunConfig::new(project.path().to_path_buf());
    config.provider = "hosted".into();
    config.kind = Some("openai-responses".into());
    config.model = "m1".into();
    config.data_root = Some(data.path().to_path_buf());
    // The error names the missing key.
    let error = Runtime::new(config.clone()).err().unwrap();
    assert!(
        matches!(&error, RunError::Config(message) if message.contains("base_url")),
        "{error}"
    );

    config.base_url = Some("http://127.0.0.1:9/v1".into());
    config.model = String::new();
    let error = Runtime::new(config).err().unwrap();
    assert!(
        matches!(&error, RunError::Config(message) if message.contains("model")),
        "{error}"
    );
}
