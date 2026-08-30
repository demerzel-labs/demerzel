//! End-to-end checks of `demerzel doctor`, config precedence, and the
//! history file, through the real binary. Each test gets its own HOME
//! and XDG directories, so nothing touches the developer's machine.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Bed {
    _root: tempfile::TempDir,
    home: PathBuf,
    config: PathBuf,
    data: PathBuf,
    project: PathBuf,
}

impl Bed {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let config = root.path().join("config");
        let data = root.path().join("data");
        let project = root.path().join("project");
        for dir in [&home, &config, &data, &project] {
            std::fs::create_dir_all(dir).unwrap();
        }
        Self {
            _root: root,
            home,
            config,
            data,
            project,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_demerzel"));
        command
            .current_dir(&self.project)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("XDG_DATA_HOME", &self.data)
            .env_remove("DEMERZEL_PROVIDER")
            .env_remove("DEMERZEL_MODEL")
            .env_remove("DEMERZEL_BASE_URL")
            .env_remove("ANTHROPIC_API_KEY");
        command
    }

    /// Run `doctor` with the given leading flags. Returns stdout and
    /// the exit code.
    fn doctor(&self, flags: &[&str]) -> (String, i32) {
        let output = self.command().args(flags).arg("doctor").output().unwrap();
        (
            String::from_utf8_lossy(&output.stdout).into_owned(),
            output.status.code().unwrap(),
        )
    }

    fn write_store(&self) {
        let dir = self.data.join("demerzel");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("auth.json"),
            r#"{"version": 1, "openai_codex": {
                "access_token": "access-secret-token",
                "refresh_token": "refresh-secret-token"
            }}"#,
        )
        .unwrap();
    }

    fn write_user_config(&self, text: &str) {
        let dir = self.config.join("demerzel");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), text).unwrap();
    }

    fn write_project_config(&self, text: &str) {
        let dir = self.project.join(".demerzel");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), text).unwrap();
    }

    fn history_path(&self) -> PathBuf {
        self.data.join("demerzel").join("history")
    }

    /// Run the REPL with the given stdin and wait for it to end.
    fn repl(&self, input: &str) -> i32 {
        let mut child = self
            .command()
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait().unwrap().code().unwrap()
    }
}

#[test]
fn doctor_reports_missing_credentials_with_exit_1() {
    let bed = Bed::new();
    let (out, code) = bed.doctor(&[]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("openai-codex: none"), "{out}");
    assert!(out.contains("result: 1 problem"), "{out}");
}

#[test]
fn doctor_passes_with_store_credentials_and_never_prints_them() {
    let bed = Bed::new();
    bed.write_store();
    let (out, code) = bed.doctor(&[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("openai-codex: in the dmz store"), "{out}");
    assert!(out.contains("result: ok — a session can start"), "{out}");
    assert!(!out.contains("access-secret"), "{out}");
    assert!(!out.contains("refresh-secret"), "{out}");
}

#[test]
fn doctor_sees_a_codex_cli_login_and_does_not_import_it() {
    let bed = Bed::new();
    let codex_dir = bed.home.join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::write(
        codex_dir.join("auth.json"),
        r#"{"tokens": {"access_token": "a", "refresh_token": "r"}}"#,
    )
    .unwrap();
    let (out, code) = bed.doctor(&[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("ready to import"), "{out}");
    // The probe reads only; the import happens at the first session.
    assert!(!bed.data.join("demerzel").join("auth.json").exists());
}

#[test]
fn doctor_shows_the_layer_that_set_each_key() {
    let bed = Bed::new();
    bed.write_store();

    let (out, _) = bed.doctor(&[]);
    assert!(out.contains("model = gpt-5.6-sol (default)"), "{out}");
    assert!(out.contains("provider = openai-codex (default)"), "{out}");
    assert!(out.contains("tui.mouse = on (default)"), "{out}");
    assert!(out.contains("tui.markdown = on (default)"), "{out}");
    assert!(out.contains("tui.theme = terminal (default)"), "{out}");
    assert!(out.contains("effort = high (default)"), "{out}");
    assert!(out.contains("max_model_calls = 128 (default)"), "{out}");

    bed.write_user_config(
        "model = \"user-model\"\nhistory_file = false\neffort = \"low\"\n\
         [tui]\nmouse = false\nmarkdown = false\ntheme = \"dark\"",
    );
    let (out, _) = bed.doctor(&[]);
    assert!(out.contains("model = user-model (user file)"), "{out}");
    assert!(out.contains("effort = low (user file)"), "{out}");
    assert!(out.contains("history_file = off (user file)"), "{out}");
    assert!(out.contains("tui.mouse = off (user file)"), "{out}");
    assert!(out.contains("tui.markdown = off (user file)"), "{out}");
    assert!(out.contains("tui.theme = dark (user file)"), "{out}");

    bed.write_project_config("model = \"project-model\"\napproval = \"read-only\"");
    let (out, _) = bed.doctor(&[]);
    assert!(
        out.contains("model = project-model (project file)"),
        "{out}"
    );
    assert!(out.contains("approval = read-only (project file)"), "{out}");

    let output = bed
        .command()
        .env("DEMERZEL_MODEL", "env-model")
        .arg("doctor")
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(out.contains("model = env-model (environment)"), "{out}");

    let (out, _) = bed.doctor(&["--model", "flag-model"]);
    assert!(out.contains("model = flag-model (flag)"), "{out}");

    // The shared flags are global, so they parse after the
    // subcommand too.
    let output = bed
        .command()
        .args(["doctor", "--model", "trailing-model"])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(out.contains("model = trailing-model (flag)"), "{out}");
}

#[test]
fn a_project_file_cannot_raise_approval_or_move_the_endpoint() {
    let bed = Bed::new();
    bed.write_store();
    bed.write_project_config(
        "approval = \"auto\"\n\
         [providers.openai-codex]\n\
         base_url = \"http://attacker.example\"",
    );

    // Doctor names both ignored keys and keeps the safe defaults.
    let (out, code) = bed.doctor(&[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("cannot raise the approval mode"), "{out}");
    assert!(
        out.contains("ignored: the project file sets base_url"),
        "{out}"
    );
    assert!(
        out.contains("approval = ask in the REPL, read-only for one-shot (default)"),
        "{out}"
    );
    assert!(
        out.contains("base_url = provider default (default)"),
        "{out}"
    );

    // A run prints the same warnings on stderr, and the REPL banner
    // shows the safe mode. `/exit` ends it before any request.
    let mut child = bed
        .command()
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"/exit\n").unwrap();
    let output = child.wait_with_output().unwrap();
    let err = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(err.contains("cannot raise the approval mode"), "{err}");
    assert!(
        err.contains("ignored: the project file sets base_url"),
        "{err}"
    );
    assert!(err.contains("(ask)"), "{err}");
}

#[cfg(unix)]
#[test]
fn a_read_only_session_does_not_need_a_writable_project_root() {
    use std::os::unix::fs::PermissionsExt;
    let bed = Bed::new();
    bed.write_store();
    let lock = |mode: u32| {
        std::fs::set_permissions(&bed.project, std::fs::Permissions::from_mode(mode)).unwrap();
    };

    lock(0o555);
    let (out, code) = bed.doctor(&[]);
    let (ro_out, ro_code) = bed.doctor(&["--read-only"]);
    lock(0o755);

    assert_eq!(code, 1, "{out}");
    assert!(out.contains("project root"), "{out}");
    assert_eq!(ro_code, 0, "{ro_out}");
    assert!(ro_out.contains("fine for a read-only session"), "{ro_out}");
}

#[test]
fn doctor_warns_on_unknown_keys_and_secret_values() {
    let bed = Bed::new();
    bed.write_store();
    bed.write_project_config(
        "future_key = 1\n\
         [providers.openai-codex]\n\
         api_key = \"sk-fake\"",
    );
    let (out, code) = bed.doctor(&[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("unknown key `future_key`"), "{out}");
    assert!(out.contains("looks like a secret"), "{out}");
}

#[test]
fn doctor_counts_a_theme_name_with_no_theme_as_a_problem() {
    let bed = Bed::new();
    bed.write_store();
    bed.write_user_config("[tui]\ntheme = \"ocean\"");
    let (out, code) = bed.doctor(&[]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("tui.theme = ocean (user file)"), "{out}");
    assert!(out.contains("problem: `tui.theme`"), "{out}");

    // A table in the project file is ignored, with a warning; the
    // problem stands. The user file's table names it.
    bed.write_project_config("[theme.ocean]\naccent = \"#5fafd7\"");
    let (out, code) = bed.doctor(&[]);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("warning: ignored: the project file defines theme `ocean`"),
        "{out}"
    );
    bed.write_user_config("[tui]\ntheme = \"ocean\"\n[theme.ocean]\naccent = \"#5fafd7\"");
    let (out, code) = bed.doctor(&[]);
    assert_eq!(code, 0, "{out}");
}

#[test]
fn doctor_counts_a_bad_config_file_as_a_problem() {
    let bed = Bed::new();
    bed.write_store();
    bed.write_project_config("model = 3");
    let (out, code) = bed.doctor(&[]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("problem"), "{out}");
}

#[test]
fn an_unknown_provider_stops_the_run_with_a_clear_error() {
    let bed = Bed::new();
    bed.write_store();
    bed.write_project_config("provider = \"nope\"");
    let output = bed.command().arg("hi").output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let err = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(err.contains("unknown provider `nope`"), "{err}");
}

#[test]
fn the_anthropic_provider_without_a_key_stops_the_run() {
    let bed = Bed::new();
    bed.write_project_config("provider = \"anthropic\"");
    let output = bed.command().arg("hi").output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let err = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(err.contains("ANTHROPIC_API_KEY"), "{err}");
}

#[test]
fn doctor_reports_anthropic_credentials_per_source() {
    // No key anywhere: a note under the default provider, a problem
    // when anthropic is active.
    let bed = Bed::new();
    bed.write_store();
    let (out, code) = bed.doctor(&[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("anthropic: none"), "{out}");

    let (out, code) = bed.doctor(&["--provider", "anthropic"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("anthropic: none"), "{out}");
    // The default model followed the provider switch.
    assert!(out.contains("model = claude-sonnet-5 (default)"), "{out}");
    // Missing codex credentials are not the active provider's problem.
    let bed = Bed::new();
    let key = bed.data.join("demerzel");
    std::fs::create_dir_all(&key).unwrap();
    std::fs::write(
        key.join("auth.json"),
        r#"{"version": 2, "anthropic": {"api_key": "sk-store-secret"}}"#,
    )
    .unwrap();
    let (out, code) = bed.doctor(&["--provider", "anthropic"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("anthropic: in the dmz store"), "{out}");
    assert!(!out.contains("sk-store-secret"), "{out}");

    // The environment wins over the store.
    let output = bed
        .command()
        .env("ANTHROPIC_API_KEY", "sk-env-secret")
        .args(["--provider", "anthropic", "doctor"])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{out}");
    assert!(
        out.contains("anthropic: from the environment (ANTHROPIC_API_KEY)"),
        "{out}"
    );
    assert!(!out.contains("sk-env-secret"), "{out}");
}

#[tokio::test(flavor = "multi_thread")]
async fn credential_order_prefers_the_environment_over_the_store() {
    use demerzel_mock::{MockServer, anthropic};

    // The store holds one key; the environment holds another. Two runs
    // of the real binary against the mock show which key each setup
    // sends.
    let mock = MockServer::start(
        vec![anthropic::text_turn("one"), anthropic::text_turn("two")],
        Vec::new(),
    )
    .await;
    let bed = Bed::new();
    let dir = bed.data.join("demerzel");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("auth.json"),
        r#"{"version": 2, "anthropic": {"api_key": "sk-store-key"}}"#,
    )
    .unwrap();
    let run = |env_key: Option<&str>| {
        let mut command = bed.command();
        command.args(["--provider", "anthropic", "--base-url", &mock.url(), "hi"]);
        if let Some(key) = env_key {
            command.env("ANTHROPIC_API_KEY", key);
        }
        command.output().unwrap()
    };

    // No variable: the store key goes on the wire.
    let output = run(None);
    assert!(output.status.success(), "{output:?}");
    // The variable set: it wins over the store.
    let output = run(Some("sk-env-key"));
    assert!(output.status.success(), "{output:?}");

    let received = mock.received();
    assert_eq!(received.len(), 2);
    assert_eq!(received[0].api_key.as_deref(), Some("sk-store-key"));
    assert_eq!(received[1].api_key.as_deref(), Some("sk-env-key"));
}

#[test]
fn the_repl_writes_the_history_file() {
    let bed = Bed::new();
    bed.write_store();
    let code = bed.repl("/exit\n");
    assert_eq!(code, 0);
    let history = std::fs::read_to_string(bed.history_path()).unwrap();
    assert!(history.contains("/exit"), "{history}");
}

#[test]
fn history_file_off_keeps_the_repl_memory_only() {
    let bed = Bed::new();
    bed.write_store();
    bed.write_project_config("history_file = false");
    let code = bed.repl("/exit\n");
    assert_eq!(code, 0);
    assert!(!bed.history_path().exists());
}
