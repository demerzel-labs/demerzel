//! Hermetic sessions through a named `openai-compat` instance: the
//! runtime, the engine, the real tools, and the adapter against the
//! mock Chat Completions route. No network, no model.

use demerzel::{AuthPaths, Resume, RunConfig, RunError, Runtime, SessionSettings};
use demerzel_core::{ContentBlock, Effort, EngineEvent, NoPrompt, SessionStore, Target, replay};
use demerzel_mock::MockServer;
use demerzel_mock::compat::{reasoning_tool_turn, text_turn};
use demerzel_providers::{EffortWire, ProviderError};
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
        config.instance.name = "local".into();
        config.instance.kind = "openai-compat".into();
        config.model = "m1".into();
        config.instance.base_url = Some(format!("{}/v1", self.mock.url()));
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

    // A flag names the model: it wins over the session's own.
    config.model = "m2".into();
    config.route_origin = demerzel::config::Origin::Flag;
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
    let mut engine = runtime.open(&Resume::Fresh, &settings, "").unwrap().engine;
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
    config.effort_origin = demerzel::config::Origin::Flag;
    let runtime = Runtime::new(config).unwrap();
    runtime.one_shot("third".into(), &noop).await.unwrap();
    assert_eq!(bed.mock.received()[2].body["reasoning_effort"], "medium");
}

#[tokio::test]
async fn an_unset_key_variable_stops_the_turn_with_its_name() {
    let bed = Bed::start(vec![text_turn("never")]).await;
    let mut config = bed.config();
    // A name no test and no machine sets.
    config.instance.api_key_env = Some("DMZ_TEST_COMPAT_KEY_NEVER_SET".into());
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
    config.instance.name = "local".into();
    config.instance.kind = "openai-compat".into();
    config.model = "m1".into();
    config.data_root = Some(data.path().to_path_buf());
    let error = Runtime::new(config.clone()).err().unwrap();
    assert!(error.to_string().contains("needs `base_url`"), "{error}");

    config.instance.base_url = Some("http://127.0.0.1:9/v1".into());
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
    assert_eq!(runtime.effort_wire(&settings), EffortWire::Default);
    settings.set_effort(Effort::Low);
    assert_eq!(
        runtime.effort_wire(&settings),
        EffortWire::Sent(Effort::Low)
    );
    let mut engine = runtime.open(&Resume::Fresh, &settings, "").unwrap().engine;
    engine
        .user_turn("first".into(), &noop, &mut NoPrompt)
        .await
        .unwrap();
    assert_eq!(bed.mock.received()[0].body["reasoning_effort"], "low");
    let meta = replay_meta(&bed);
    assert_eq!(meta.effort.as_deref(), Some("low"));
}

/// Every instance of `user` except the run's head `head`, as `main`
/// resolves them.
fn others(head: &str, user: &demerzel::config::Layer) -> Vec<demerzel::config::Resolved> {
    demerzel::config::run_instances(
        &demerzel::config::Flags::default(),
        &|_| None,
        None,
        Some(user),
    )
    .into_iter()
    .filter(|resolved| resolved.instance.name != head)
    .collect()
}

fn replay_meta(bed: &Bed) -> demerzel_core::SessionMeta {
    replay(&bed.latest_session()).unwrap().meta.unwrap()
}

/// The user file of the resume tests: two compatible instances at the
/// mock's root, each with its own model. `alpha` sets its effort.
fn two_instances(bed: &Bed) -> demerzel::config::Layer {
    let url = format!("{}/v1", bed.mock.url());
    let text = format!(
        "[providers.alpha]\nkind = \"openai-compat\"\nbase_url = \"{url}\"\nmodel = \"m1\"\n\
         effort = \"low\"\n\n\
         [providers.beta]\nkind = \"openai-compat\"\nbase_url = \"{url}\"\nmodel = \"m2\"\n"
    );
    demerzel::config::Layer::parse(&text).unwrap().0
}

/// A run whose head is instance `name` with `model`, at the mock.
fn head(bed: &Bed, name: &str, model: &str) -> RunConfig {
    let mut config = bed.config();
    config.instance.name = name.into();
    config.model = model.into();
    config
}

/// The first turn of the resume tests: a tool turn with reasoning on
/// instance `alpha`, model `m1`.
async fn alpha_session(bed: &Bed) {
    Runtime::new(head(bed, "alpha", "m1"))
        .unwrap()
        .one_shot("read f.txt".into(), &noop)
        .await
        .unwrap();
}

fn reasoning_turn() -> demerzel_mock::MockResponse {
    reasoning_tool_turn(
        "reasoning_content",
        "read the file first",
        "call_1",
        "read",
        json!({"path": "f.txt"}),
    )
}

#[tokio::test]
async fn a_resume_keeps_the_sessions_model_over_a_config_change() {
    let bed = Bed::start(vec![
        reasoning_turn(),
        text_turn("done"),
        text_turn("still m1").with_check(requires_reasoning_replay),
    ])
    .await;
    alpha_session(&bed).await;

    // The config file now names another model; no flag names one.
    let mut config = head(&bed, "alpha", "m9");
    config.resume = Resume::Latest;
    let runtime = Runtime::new(config).unwrap();
    let settings = SessionSettings::from_config(runtime.config());
    let mut opened = runtime.open(&Resume::Latest, &settings, "").unwrap();
    assert!(opened.notice.is_none());
    assert_eq!(opened.settings.route.head.model, "m1");
    opened
        .engine
        .user_turn("and now?".into(), &noop, &mut NoPrompt)
        .await
        .unwrap();
    assert_eq!(bed.mock.received()[2].body["model"], "m1");
    let meta = replay(&bed.latest_session()).unwrap().meta.unwrap();
    assert_eq!(
        (meta.provider.as_str(), meta.model.as_str()),
        ("alpha", "m1")
    );
}

#[tokio::test]
async fn a_resume_adopts_the_sessions_instance_and_its_effort() {
    let bed = Bed::start(vec![
        reasoning_turn(),
        text_turn("done"),
        text_turn("back on alpha").with_check(requires_reasoning_replay),
    ])
    .await;
    alpha_session(&bed).await;

    // The run's head is `beta`; config still defines `alpha`.
    let user = two_instances(&bed);
    let mut config = head(&bed, "beta", "m2");
    config.resume = Resume::Latest;
    config.other_instances = others("beta", &user);
    let runtime = Runtime::new(config).unwrap();
    let settings = SessionSettings::from_config(runtime.config());
    assert_eq!(runtime.effort_wire(&settings), EffortWire::Default);
    let mut opened = runtime.open(&Resume::Latest, &settings, "").unwrap();
    assert!(opened.notice.is_none());
    assert_eq!(opened.settings.route.head.instance, "alpha");
    assert_eq!(opened.settings.route.head.model, "m1");
    // The adopted instance brings the effort its table sets.
    assert_eq!(
        runtime.effort_wire(&opened.settings),
        EffortWire::Sent(Effort::Low)
    );
    opened
        .engine
        .user_turn("and now?".into(), &noop, &mut NoPrompt)
        .await
        .unwrap();
    let body = &bed.mock.received()[2].body;
    assert_eq!(body["model"], "m1");
    assert_eq!(body["reasoning_effort"], "low");
    assert_eq!(
        body["messages"][2]["reasoning_content"],
        "read the file first"
    );
}

#[tokio::test]
async fn a_resume_falls_back_with_a_notice_when_config_drops_the_instance() {
    let bed = Bed::start(vec![
        reasoning_turn(),
        text_turn("done"),
        demerzel_mock::responses::text_turn("on the head"),
    ])
    .await;
    alpha_session(&bed).await;

    // Config no longer defines `alpha`. The head is another protocol:
    // the history crosses it, the reasoning does not.
    let mut config = head(&bed, "hosted", "r1");
    config.instance.kind = "openai-responses".into();
    config.resume = Resume::Latest;
    let runtime = Runtime::new(config).unwrap();
    let settings = SessionSettings::from_config(runtime.config());
    let mut opened = runtime.open(&Resume::Latest, &settings, "").unwrap();
    let notice = opened.notice.clone().expect("a fallback notice");
    assert!(
        notice.contains("alpha") && notice.contains("hosted"),
        "{notice}"
    );
    assert_eq!(opened.settings.route.head.instance, "hosted");
    opened
        .engine
        .user_turn("and now?".into(), &noop, &mut NoPrompt)
        .await
        .unwrap();

    let received = bed.mock.received();
    assert_eq!(received[2].path, "/v1/responses");
    let input = received[2].body["input"].as_array().unwrap();
    let kinds: Vec<&str> = input
        .iter()
        .map(|item| item["type"].as_str().unwrap_or("message"))
        .collect();
    assert!(!kinds.contains(&"reasoning"), "{kinds:?}");
    let call = input.iter().find(|item| item["type"] == "function_call");
    assert_eq!(call.unwrap()["call_id"], "call_1");
    let output = input
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .unwrap();
    assert!(output["output"].as_str().unwrap().contains("alpha"));
    let meta = replay(&bed.latest_session()).unwrap().meta.unwrap();
    assert_eq!(meta.provider, "hosted");
    assert_eq!(meta.kind.as_deref(), Some("openai-responses"));
}

#[tokio::test]
async fn a_model_switch_keeps_reasoning_off_the_other_model_and_back_on_return() {
    let bed = Bed::start(vec![
        reasoning_turn(),
        text_turn("done"),
        text_turn("on m2"),
        text_turn("back on m1").with_check(requires_reasoning_replay),
    ])
    .await;
    let runtime = Runtime::new(bed.config()).unwrap();
    let settings = SessionSettings::from_config(runtime.config());
    let mut engine = runtime.open(&Resume::Fresh, &settings, "").unwrap().engine;
    engine
        .user_turn("read f.txt".into(), &noop, &mut NoPrompt)
        .await
        .unwrap();

    engine.set_model("m2").unwrap();
    engine
        .user_turn("other model".into(), &noop, &mut NoPrompt)
        .await
        .unwrap();
    engine.set_model("m1").unwrap();
    engine
        .user_turn("back".into(), &noop, &mut NoPrompt)
        .await
        .unwrap();

    let received = bed.mock.received();
    let switched = &received[2].body;
    assert_eq!(switched["model"], "m2");
    assert_eq!(switched["messages"][2]["tool_calls"][0]["id"], "call_1");
    assert!(switched["messages"][2].get("reasoning_content").is_none());
    assert_eq!(received[3].body["model"], "m1");
    // The file keeps the payload, so a resume restores it too.
    let replayed = replay(&bed.latest_session()).unwrap();
    assert!(replayed.messages.iter().any(|message| {
        message.content.iter().any(|block| {
            matches!(
                block,
                ContentBlock::Reasoning {
                    sealed: Some(_),
                    ..
                }
            )
        })
    }));
    assert_eq!(replayed.meta.unwrap().model, "m1");
}

/// A resume run with head `beta` and the instances of `user`.
fn resume_on_beta(bed: &Bed, user: &demerzel::config::Layer) -> Runtime {
    let mut config = head(bed, "beta", "m2");
    config.resume = Resume::Latest;
    config.other_instances = others("beta", user);
    Runtime::new(config).unwrap()
}

fn meta_records(bed: &Bed) -> usize {
    std::fs::read_to_string(bed.latest_session())
        .unwrap()
        .lines()
        .filter(|line| line.contains(r#""type":"meta""#))
        .count()
}

#[tokio::test]
async fn model_before_the_first_prompt_keeps_its_model_on_the_adopted_instance() {
    let bed = Bed::start(vec![reasoning_turn(), text_turn("done"), text_turn("m7")]).await;
    alpha_session(&bed).await;
    let runtime = resume_on_beta(&bed, &two_instances(&bed));
    let mut settings = SessionSettings::from_config(runtime.config());
    settings.set_model("m7");
    let mut opened = runtime.open(&Resume::Latest, &settings, "").unwrap();
    assert_eq!(opened.settings.route.head.instance, "alpha");
    assert_eq!(opened.settings.route.head.model, "m7");
    opened
        .engine
        .user_turn("go".into(), &noop, &mut NoPrompt)
        .await
        .unwrap();
    assert_eq!(bed.mock.received()[2].body["model"], "m7");
}

#[tokio::test]
async fn a_resume_falls_back_when_the_instance_changed_its_adapter() {
    let bed = Bed::start(vec![
        reasoning_turn(),
        text_turn("done"),
        text_turn("on beta"),
    ])
    .await;
    alpha_session(&bed).await;
    // `alpha` now names a Responses server: the recorded model
    // belongs to another protocol.
    let url = format!("{}/v1", bed.mock.url());
    let text = format!(
        "[providers.alpha]\nkind = \"openai-responses\"\nbase_url = \"{url}\"\nmodel = \"r1\"\n"
    );
    let user = demerzel::config::Layer::parse(&text).unwrap().0;
    let runtime = resume_on_beta(&bed, &user);
    let settings = SessionSettings::from_config(runtime.config());
    let opened = runtime.open(&Resume::Latest, &settings, "").unwrap();
    let notice = opened.notice.expect("a fallback notice");
    assert!(notice.contains("openai-responses"), "{notice}");
    assert_eq!(opened.settings.route.head.instance, "beta");
}

#[tokio::test]
async fn a_missing_login_on_the_adopted_instance_fails_before_a_record() {
    let bed = Bed::start(vec![reasoning_turn(), text_turn("done")]).await;
    alpha_session(&bed).await;
    let url = format!("{}/v1", bed.mock.url());
    let text = format!(
        "[providers.alpha]\nkind = \"openai-compat\"\nbase_url = \"{url}\"\nmodel = \"m1\"\n\
         api_key_env = \"DMZ_TEST_ALPHA_KEY_NEVER_SET\"\n"
    );
    let user = demerzel::config::Layer::parse(&text).unwrap().0;
    let runtime = resume_on_beta(&bed, &user);
    let before = meta_records(&bed);
    let settings = SessionSettings::from_config(runtime.config());
    let Err(error) = runtime.open(&Resume::Latest, &settings, "") else {
        panic!("the open must fail on the missing key");
    };
    let error = error.to_string();
    assert!(
        error.contains("alpha") && error.contains("DMZ_TEST_ALPHA_KEY_NEVER_SET"),
        "{error}"
    );
    assert_eq!(meta_records(&bed), before);
}

/// The user file of the chain tests: `alpha` speaks Chat Completions
/// and sets its effort; `beta` speaks the Responses API and sets none.
fn cross_protocol(bed: &Bed) -> demerzel::config::Layer {
    let url = format!("{}/v1", bed.mock.url());
    let text = format!(
        "[providers.alpha]\nkind = \"openai-compat\"\nbase_url = \"{url}\"\nmodel = \"m1\"\n\
         effort = \"low\"\n\n\
         [providers.beta]\nkind = \"openai-responses\"\nbase_url = \"{url}\"\nmodel = \"r1\"\n"
    );
    demerzel::config::Layer::parse(&text).unwrap().0
}

/// A run with head `alpha`, model `m1`, the chain's tail, and every
/// instance of `user`.
fn chain_run(bed: &Bed, user: &demerzel::config::Layer, tail: Vec<Target>) -> RunConfig {
    let mut config = head(bed, "alpha", "m1");
    config.effort = Effort::Low;
    config.effort_origin = demerzel::config::Origin::User;
    config.tail = tail;
    config.other_instances = others("alpha", user);
    config
}

/// The `usage` records of the latest session, as JSON.
fn usage_records(bed: &Bed) -> Vec<Value> {
    std::fs::read_to_string(bed.latest_session())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|record| record["type"] == "usage")
        .collect()
}

#[tokio::test]
async fn a_chain_falls_back_across_protocols_and_records_the_answering_target() {
    let bed = Bed::start(vec![
        demerzel_mock::MockResponse::error(401),
        demerzel_mock::responses::tool_turn("call_1", "read", json!({"path": "f.txt"})),
        demerzel_mock::responses::text_turn("the file holds alpha and beta"),
    ])
    .await;
    let user = cross_protocol(&bed);
    let config = chain_run(&bed, &user, vec![Target::new("beta", "r1")]);
    let starts = std::sync::Mutex::new(Vec::new());
    let sink = |event: EngineEvent<'_>| {
        if let EngineEvent::ModelStart {
            provider,
            model,
            attempt,
            ..
        } = event
        {
            starts
                .lock()
                .unwrap()
                .push(format!("{provider}/{model}#{attempt}"));
        }
    };
    let outcome = Runtime::new(config)
        .unwrap()
        .one_shot("read f.txt".into(), &sink)
        .await
        .unwrap();
    assert_eq!(outcome, demerzel_core::TurnOutcome::Completed);
    // The first call falls back; the turn's next call starts at the
    // target that answered.
    assert_eq!(
        *starts.lock().unwrap(),
        ["alpha/m1#0", "beta/r1#1", "beta/r1#0"]
    );
    let received = bed.mock.received();
    let paths: Vec<&str> = received.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(
        paths,
        ["/v1/chat/completions", "/v1/responses", "/v1/responses"]
    );
    // Each attempt carries the effort of its own target: `alpha` sets
    // a level, and `beta` sets none.
    assert_eq!(received[0].body["reasoning_effort"], "low");
    assert!(
        received[1].body.get("reasoning").is_none(),
        "{}",
        received[1].body
    );
    // The tool ran through the same tools: its output reached `beta`.
    assert!(received[2].body.to_string().contains("alpha"));
    // Each usage record names the target that answered; the meta
    // record keeps the head.
    let usage = usage_records(&bed);
    assert_eq!(usage.len(), 2);
    for record in &usage {
        assert_eq!(record["provider"], "beta");
        assert_eq!(record["model"], "r1");
    }
    assert_eq!(replay_meta(&bed).provider, "alpha");
}

#[tokio::test]
async fn a_head_without_its_key_falls_back_at_the_open_and_the_turn() {
    let bed = Bed::start(vec![text_turn("from beta")]).await;
    let url = format!("{}/v1", bed.mock.url());
    let text = format!(
        "[providers.alpha]\nkind = \"openai-compat\"\nbase_url = \"{url}\"\nmodel = \"m1\"\n\
         api_key_env = \"DMZ_TEST_ALPHA_KEY_NEVER_SET\"\n\n\
         [providers.beta]\nkind = \"openai-compat\"\nbase_url = \"{url}\"\nmodel = \"m2\"\n"
    );
    let user = demerzel::config::Layer::parse(&text).unwrap().0;
    let mut config = chain_run(&bed, &user, vec![Target::new("beta", "m2")]);
    config.instance.api_key_env = Some("DMZ_TEST_ALPHA_KEY_NEVER_SET".into());
    Runtime::new(config)
        .unwrap()
        .one_shot("hi".into(), &noop)
        .await
        .unwrap();
    let received = bed.mock.received();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].body["model"], "m2");
    assert_eq!(usage_records(&bed)[0]["provider"], "beta");
}

#[tokio::test]
async fn model_with_two_tokens_moves_the_head_to_another_instance() {
    let bed = Bed::start(vec![text_turn("on beta")]).await;
    let user = two_instances(&bed);
    let runtime = Runtime::new(chain_run(&bed, &user, Vec::new())).unwrap();
    let settings = SessionSettings::from_config(runtime.config());
    let mut opened = runtime.open(&Resume::Fresh, &settings, "").unwrap();

    let error = runtime
        .switch_head(&opened.settings, "ghost", "m9")
        .unwrap_err();
    assert!(error.contains("ghost") && error.contains("beta"), "{error}");

    let next = runtime.switch_head(&opened.settings, "beta", "m9").unwrap();
    // `beta` sets no effort: the switch takes its own.
    assert_eq!(runtime.effort_wire(&next), EffortWire::Default);
    opened.engine.set_head(next.route.head.clone()).unwrap();
    opened
        .engine
        .user_turn("go".into(), &noop, &mut NoPrompt)
        .await
        .unwrap();
    let body = &bed.mock.received()[0].body;
    assert_eq!(body["model"], "m9");
    assert!(body.get("reasoning_effort").is_none(), "{body}");
    let meta = replay_meta(&bed);
    assert_eq!(
        (meta.provider.as_str(), meta.model.as_str()),
        ("beta", "m9")
    );
    assert_eq!(usage_records(&bed)[0]["provider"], "beta");
}

#[tokio::test]
async fn a_two_token_switch_before_the_first_prompt_keeps_the_whole_head_on_resume() {
    let bed = Bed::start(vec![reasoning_turn(), text_turn("done"), text_turn("m7")]).await;
    alpha_session(&bed).await;
    let runtime = resume_on_beta(&bed, &two_instances(&bed));
    let settings = SessionSettings::from_config(runtime.config());
    let settings = runtime.switch_head(&settings, "beta", "m7").unwrap();
    let opened = runtime.open(&Resume::Latest, &settings, "").unwrap();
    assert_eq!(opened.settings.route.head, Target::new("beta", "m7"));
}

#[test]
fn a_tail_on_an_undefined_instance_stops_the_run_at_boot() {
    let project = tempfile::tempdir().unwrap();
    let mut config = RunConfig::new(project.path().to_path_buf());
    config.tail = vec![Target::new("ghost", "m")];
    let Err(RunError::Config(error)) = Runtime::new(config) else {
        panic!("an undefined fallback instance must stop the run");
    };
    assert!(error.contains("ghost"), "{error}");
}

#[tokio::test]
async fn the_first_run_screen_shows_only_when_no_target_of_the_route_can_run() {
    let bed = Bed::start(Vec::new()).await;
    let url = format!("{}/v1", bed.mock.url());
    let text = format!(
        "[providers.keyed]\nkind = \"openai-compat\"\nbase_url = \"{url}\"\nmodel = \"m1\"\n\
         api_key_env = \"DMZ_TEST_KEYED_KEY_NEVER_SET\"\n\n\
         [providers.open]\nkind = \"openai-compat\"\nbase_url = \"{url}\"\nmodel = \"m2\"\n"
    );
    let user = demerzel::config::Layer::parse(&text).unwrap().0;
    // A Codex head without a login: the store is empty and no import
    // file exists.
    let run = |tail: Vec<Target>| {
        let mut config = bed.config();
        config.instance = demerzel::config::Instance::new("openai-codex", "openai-codex");
        config.model = "gpt-test".into();
        config.tail = tail;
        config.other_instances = others("openai-codex", &user);
        Runtime::new(config).unwrap()
    };
    // A tail whose key variable is unset cannot run either.
    assert!(run(vec![Target::new("keyed", "m1")]).first_run().is_some());
    // A keyless tail can run: no screen.
    assert!(run(vec![Target::new("open", "m2")]).first_run().is_none());
}
