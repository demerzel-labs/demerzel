//! The system prompt on the wire: the runtime composes it at the
//! session open, and the adapters send it. Hermetic: the mock server,
//! a private project, a private user directory. No network, no model.
//!
//! The contract these tests hold to: the segment order, equal bytes
//! for equal inputs, one prompt for every call and every target, and
//! a replaced base prompt sent as it is. The heading words are not
//! contract, so no assertion names one.

use std::path::{Path, PathBuf};
use std::process::Command;

use demerzel::{AuthPaths, Resume, RunConfig, RunError, Runtime, SessionSettings};
use demerzel_core::{DEFAULT_SYSTEM_PROMPT, EngineEvent, NoPrompt, Target};
use demerzel_mock::compat::text_turn;
use demerzel_mock::{MockResponse, MockServer, Received};
use serde_json::json;

fn noop(_: EngineEvent<'_>) {}

struct Bed {
    mock: MockServer,
    project: tempfile::TempDir,
    data: tempfile::TempDir,
    /// The user's config directory.
    user: tempfile::TempDir,
}

impl Bed {
    async fn start(script: Vec<MockResponse>) -> Self {
        Self {
            mock: MockServer::start(script, Vec::new()).await,
            project: tempfile::tempdir().unwrap(),
            data: tempfile::tempdir().unwrap(),
            user: tempfile::tempdir().unwrap(),
        }
    }

    /// A run on instance `local` of the compatible adapter at the
    /// mock's `/v1` root, with the bed's user directory.
    fn config(&self) -> RunConfig {
        let mut config = RunConfig::new(self.project.path().to_path_buf());
        config.instance.name = "local".into();
        config.instance.kind = "openai-compat".into();
        config.model = "m1".into();
        config.instance.base_url = Some(format!("{}/v1", self.mock.url()));
        config.data_root = Some(self.data.path().join("store"));
        // The run reads its own store, never the developer's.
        config.auth = Some(AuthPaths {
            store: self.data.path().join("auth.json"),
            codex_cli: None,
        });
        config.config_dir = Some(self.user.path().to_path_buf());
        config
    }

    fn user_file(&self, name: &str, content: &str) -> PathBuf {
        let path = self.user.path().join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    fn project_file(&self, name: &str, content: &str) {
        std::fs::write(self.project.path().join(name), content).unwrap();
    }

    /// The profile, the user's instruction file, and the project's.
    fn write_all(&self) {
        self.user_file("personality.md", "PROFILE-TEXT\n");
        self.user_file("AGENTS.md", "USER-RULE\n");
        self.project_file("AGENTS.md", "PROJECT-RULE\n");
    }

    async fn one_shot(&self, config: RunConfig, prompt: &str) {
        Runtime::new(config)
            .unwrap()
            .one_shot(prompt.into(), &noop)
            .await
            .unwrap();
    }
}

/// The system prompt of a Chat Completions request: the content of
/// its system message.
fn system(received: &Received) -> &str {
    let first = &received.body["messages"][0];
    assert_eq!(first["role"], "system", "{first}");
    first["content"].as_str().unwrap()
}

#[tokio::test]
async fn a_project_without_instruction_files_sends_the_base_prompt_alone() {
    let bed = Bed::start(vec![text_turn("one")]).await;
    bed.one_shot(bed.config(), "hello").await;
    assert_eq!(system(&bed.mock.received()[0]), DEFAULT_SYSTEM_PROMPT);
}

#[tokio::test]
async fn the_files_join_the_prompt_in_the_segment_order() {
    let bed = Bed::start(vec![text_turn("one")]).await;
    bed.write_all();
    bed.one_shot(bed.config(), "hello").await;
    let received = bed.mock.received();
    let prompt = system(&received[0]);
    assert!(prompt.starts_with(DEFAULT_SYSTEM_PROMPT));
    let order = ["PROFILE-TEXT", "USER-RULE", "PROJECT-RULE"].map(|text| {
        prompt
            .find(text)
            .unwrap_or_else(|| panic!("{text} in {prompt}"))
    });
    assert!(order.is_sorted(), "{prompt}");
}

#[tokio::test]
async fn two_sessions_with_equal_inputs_send_equal_bytes() {
    let bed = Bed::start(vec![text_turn("one"), text_turn("two")]).await;
    bed.write_all();
    bed.one_shot(bed.config(), "the first prompt").await;
    // Another run, another first prompt, and another head model: none
    // of them is an input of the prompt.
    let mut config = bed.config();
    config.model = "m2".into();
    bed.one_shot(config, "another prompt").await;
    let received = bed.mock.received();
    assert_eq!(received[1].body["model"], "m2");
    assert_eq!(system(&received[0]), system(&received[1]));
    assert!(system(&received[0]).contains("PROJECT-RULE"));
}

#[tokio::test]
async fn a_session_keeps_its_prompt_and_a_resume_composes_again() {
    let bed = Bed::start(vec![text_turn("one"), text_turn("two"), text_turn("three")]).await;
    bed.project_file("AGENTS.md", "RULE-ONE\n");
    let runtime = Runtime::new(bed.config()).unwrap();
    let settings = SessionSettings::from_config(runtime.config());
    let mut engine = runtime
        .open(&Resume::Fresh, &settings, "first")
        .unwrap()
        .engine;
    engine
        .user_turn("first".into(), &noop, &mut NoPrompt)
        .await
        .unwrap();
    // An edit during the session does not reach it.
    bed.project_file("AGENTS.md", "RULE-TWO\n");
    engine
        .user_turn("second".into(), &noop, &mut NoPrompt)
        .await
        .unwrap();
    drop(engine);
    let mut engine = runtime
        .open(&Resume::Latest, &settings, "third")
        .unwrap()
        .engine;
    engine
        .user_turn("third".into(), &noop, &mut NoPrompt)
        .await
        .unwrap();

    let received = bed.mock.received();
    assert_eq!(system(&received[0]), system(&received[1]));
    assert!(system(&received[1]).contains("RULE-ONE"));
    assert!(system(&received[2]).contains("RULE-TWO"));
    assert!(!system(&received[2]).contains("RULE-ONE"));
}

#[tokio::test]
async fn a_fallback_target_on_another_adapter_receives_the_same_prompt() {
    let bed = Bed::start(vec![
        MockResponse::error(401),
        demerzel_mock::responses::text_turn("from the tail"),
    ])
    .await;
    bed.write_all();
    let url = format!("{}/v1", bed.mock.url());
    let user = demerzel::config::Layer::parse(&format!(
        "[providers.local]\nkind = \"openai-compat\"\nbase_url = \"{url}\"\nmodel = \"m1\"\n\n\
         [providers.tail]\nkind = \"openai-responses\"\nbase_url = \"{url}\"\nmodel = \"r1\"\n"
    ))
    .unwrap()
    .0;
    let mut config = bed.config();
    config.tail = vec![Target::new("tail", "r1")];
    config.other_instances = demerzel::config::run_instances(
        &demerzel::config::Flags::default(),
        &|_| None,
        None,
        Some(&user),
    )
    .into_iter()
    .filter(|resolved| resolved.instance.name != "local")
    .collect();
    bed.one_shot(config, "hello").await;

    let received = bed.mock.received();
    assert_eq!(received[1].path, "/v1/responses");
    let head = system(&received[0]);
    assert!(head.contains("PROJECT-RULE"));
    assert_eq!(received[1].body["instructions"], head);
}

#[tokio::test]
async fn a_replaced_base_prompt_goes_out_as_it_is() {
    let bed = Bed::start(vec![text_turn("one"), text_turn("two")]).await;
    let base = "  My own base prompt.\n\n  It keeps its blank lines.  \n";
    let mut config = bed.config();
    config.prompt_base_file = Some(bed.user_file("base.md", base));
    bed.one_shot(config.clone(), "hello").await;
    assert_eq!(system(&bed.mock.received()[0]), base);

    // The other segments follow the user's base prompt, which stays
    // byte for byte.
    bed.project_file("AGENTS.md", "PROJECT-RULE\n");
    bed.one_shot(config, "hello").await;
    let received = bed.mock.received();
    let prompt = system(&received[1]);
    assert!(prompt.starts_with(base));
    assert!(prompt.contains("PROJECT-RULE"));
    assert!(!prompt.contains(DEFAULT_SYSTEM_PROMPT));
}

#[tokio::test]
async fn a_file_that_cannot_join_gives_the_open_a_note() {
    let bed = Bed::start(vec![text_turn("one")]).await;
    let runtime = Runtime::new(bed.config()).unwrap();
    let settings = SessionSettings::from_config(runtime.config());
    let opened = runtime.open(&Resume::Fresh, &settings, "first").unwrap();
    assert!(opened.prompt_notes.is_empty(), "{:?}", opened.prompt_notes);
    drop(opened);

    // Not text: the file stays out, and the open names it.
    std::fs::write(bed.project.path().join("AGENTS.md"), [0xff, 0xfe]).unwrap();
    let mut opened = runtime.open(&Resume::Fresh, &settings, "first").unwrap();
    assert_eq!(opened.prompt_notes.len(), 1, "{:?}", opened.prompt_notes);
    assert!(opened.prompt_notes[0].contains("AGENTS.md"));
    opened
        .engine
        .user_turn("first".into(), &noop, &mut NoPrompt)
        .await
        .unwrap();
    assert_eq!(system(&bed.mock.received()[0]), DEFAULT_SYSTEM_PROMPT);
}

#[tokio::test]
async fn a_base_prompt_file_that_does_not_read_stops_the_run_at_boot() {
    let bed = Bed::start(Vec::new()).await;
    let mut config = bed.config();
    config.prompt_base_file = Some(bed.user.path().join("gone.md"));
    let Err(error) = Runtime::new(config) else {
        panic!("a missing base prompt file must stop the run");
    };
    assert!(matches!(error, RunError::Config(_)), "{error:?}");
    assert!(error.to_string().contains("gone.md"), "{error}");
}

/// One run of the binary in private directories: a one-shot prompt
/// against the Codex mock. Returns the output.
fn binary(dirs: &Path, base_url: &str, log: bool) -> std::process::Output {
    let data = dirs.join("data");
    std::fs::create_dir_all(data.join("demerzel")).unwrap();
    std::fs::write(
        data.join("demerzel/auth.json"),
        json!({
            "version": 1,
            "openai_codex": {"access_token": "test-token", "refresh_token": "test-refresh"},
        })
        .to_string(),
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_demerzel"));
    command
        .arg("hello")
        .args(["--base-url", base_url])
        .current_dir(dirs.join("project"))
        .env("HOME", dirs.join("home"))
        .env("XDG_CONFIG_HOME", dirs.join("config"))
        .env("XDG_DATA_HOME", &data)
        .env_remove("DEMERZEL_LOG")
        .env_remove("DEMERZEL_PROVIDER")
        .env_remove("DEMERZEL_MODEL")
        .env_remove("DEMERZEL_BASE_URL")
        .env_remove("ANTHROPIC_API_KEY");
    if log {
        command.env("DEMERZEL_LOG", "trace");
    }
    command.output().unwrap()
}

/// The private directories of a binary run: the project, and the
/// user's `demerzel` config directory. The paths are resolved, so
/// they compare with the paths the binary prints.
fn binary_dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dirs = tempfile::tempdir().unwrap();
    let root = dirs.path().canonicalize().unwrap();
    let project = root.join("project");
    let user = root.join("config/demerzel");
    for dir in [&project, &user, &root.join("home")] {
        std::fs::create_dir_all(dir).unwrap();
    }
    (dirs, project, user)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_binary_composes_from_the_user_directory_and_the_project() {
    let mock = MockServer::start(vec![demerzel_mock::codex::text_turn("done")], Vec::new()).await;
    let (dirs, project, user) = binary_dirs();
    std::fs::write(user.join("personality.md"), "PROFILE-TEXT\n").unwrap();
    std::fs::write(user.join("AGENTS.md"), "USER-RULE\n").unwrap();
    std::fs::write(project.join("AGENTS.md"), "PROJECT-RULE\n").unwrap();
    // A relative path starts at the user file's directory.
    std::fs::write(
        user.join("config.toml"),
        "[prompt]\nbase_file = \"base.md\"\n",
    )
    .unwrap();
    std::fs::write(user.join("base.md"), "MY-BASE\n").unwrap();

    let output = binary(dirs.path(), &mock.url(), true);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    let received = mock.received();
    let prompt = received[0].body["instructions"].as_str().unwrap();
    assert!(prompt.starts_with("MY-BASE\n"), "{prompt}");
    let order = ["PROFILE-TEXT", "USER-RULE", "PROJECT-RULE"].map(|text| {
        prompt
            .find(text)
            .unwrap_or_else(|| panic!("{text} in {prompt}"))
    });
    assert!(order.is_sorted(), "{prompt}");

    // The trace holds the request body at the `trace` level, so a
    // reader of the log sees the instruction files.
    let logs = dirs.path().join("data/demerzel/logs");
    let log = std::fs::read_dir(&logs).unwrap().next().unwrap().unwrap();
    let log = std::fs::read_to_string(log.path()).unwrap();
    assert!(log.contains("PROJECT-RULE"), "{log}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_project_file_cannot_replace_the_base_prompt() {
    let mock = MockServer::start(vec![demerzel_mock::codex::text_turn("done")], Vec::new()).await;
    let (dirs, project, _user) = binary_dirs();
    std::fs::create_dir_all(project.join(".demerzel")).unwrap();
    std::fs::write(project.join("evil.md"), "EVIL-BASE\n").unwrap();
    std::fs::write(
        project.join(".demerzel/config.toml"),
        format!(
            "[prompt]\nbase_file = \"{}\"\n",
            project.join("evil.md").display()
        ),
    )
    .unwrap();

    // A project link to a file outside the project stays out too,
    // and the run says so.
    #[cfg(unix)]
    std::os::unix::fs::symlink(dirs.path().join("data"), project.join("AGENTS.md")).unwrap();

    let output = binary(dirs.path(), &mock.url(), false);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("prompt.base_file"), "{stderr}");
    #[cfg(unix)]
    assert!(stderr.contains("AGENTS.md"), "{stderr}");
    let received = mock.received();
    assert_eq!(received[0].body["instructions"], DEFAULT_SYSTEM_PROMPT);
}

#[test]
fn doctor_reports_the_prompt_and_a_base_file_that_does_not_read() {
    let (dirs, project, user) = binary_dirs();
    std::fs::write(project.join("AGENTS.md"), "PROJECT-RULE\n").unwrap();
    std::fs::write(
        user.join("config.toml"),
        "[prompt]\nbase_file = \"gone.md\"\n",
    )
    .unwrap();
    let doctor = || {
        let output = Command::new(env!("CARGO_BIN_EXE_demerzel"))
            .arg("doctor")
            .current_dir(&project)
            .env("HOME", dirs.path().join("home"))
            .env("XDG_CONFIG_HOME", dirs.path().join("config"))
            .env("XDG_DATA_HOME", dirs.path().join("data"))
            .env_remove("DEMERZEL_PROVIDER")
            .env_remove("DEMERZEL_MODEL")
            .env_remove("DEMERZEL_BASE_URL")
            .env_remove("ANTHROPIC_API_KEY")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&output.stdout).into_owned(),
            output.status.code().unwrap(),
        )
    };
    let lines_with = |report: &str, fields: &[&str]| {
        report
            .lines()
            .filter(|line| fields.iter().all(|field| line.contains(field)))
            .count()
    };

    // The key shows, the missing file counts as a problem, and the
    // project's instruction file shows with its size.
    let (report, code) = doctor();
    assert_eq!(code, 1, "{report}");
    assert_eq!(
        lines_with(&report, &["prompt.base_file", "gone.md"]),
        2,
        "{report}"
    );
    let agents = project.join("AGENTS.md").display().to_string();
    assert_eq!(lines_with(&report, &[&agents, "12"]), 1, "{report}");

    std::fs::write(user.join("gone.md"), "MY-BASE\n").unwrap();
    let (report, _) = doctor();
    assert_eq!(
        lines_with(&report, &["prompt.base_file", "gone.md"]),
        1,
        "{report}"
    );
}
