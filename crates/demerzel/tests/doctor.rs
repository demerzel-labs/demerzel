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
            .env_remove("DEMERZEL_BASE_URL");
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

    bed.write_user_config("model = \"user-model\"\nhistory_file = false");
    let (out, _) = bed.doctor(&[]);
    assert!(out.contains("model = user-model (user file)"), "{out}");
    assert!(out.contains("history_file = off (user file)"), "{out}");

    bed.write_project_config("model = \"project-model\"\napproval = \"auto\"");
    let (out, _) = bed.doctor(&[]);
    assert!(
        out.contains("model = project-model (project file)"),
        "{out}"
    );
    assert!(out.contains("approval = auto (project file)"), "{out}");

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
    bed.write_project_config("provider = \"anthropic\"");
    let output = bed.command().arg("hi").output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let err = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(err.contains("unknown provider `anthropic`"), "{err}");
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
