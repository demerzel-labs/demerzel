//! The debug trace through the real binary: `DEMERZEL_LOG` on a
//! one-shot run against the mock server, in a private data
//! directory. No network, no model.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use demerzel_mock::{MockResponse, MockServer, codex};
use serde_json::json;

/// The fake credentials in the auth store. The trace must never
/// hold them.
const ACCESS_TOKEN: &str = "test-token";
const REFRESH_TOKEN: &str = "test-refresh";

/// One finished run and its private directories.
struct Run {
    dirs: tempfile::TempDir,
    output: Output,
}

impl Run {
    fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.output.stderr).into_owned()
    }

    /// The log files the run wrote, under the data directory.
    fn log_files(&self) -> Vec<PathBuf> {
        let logs = self.dirs.path().join("data/demerzel/logs");
        let Ok(entries) = std::fs::read_dir(&logs) else {
            return Vec::new();
        };
        entries.flatten().map(|entry| entry.path()).collect()
    }

    /// The one log file's text.
    fn log(&self) -> String {
        let files = self.log_files();
        assert_eq!(files.len(), 1, "{files:?}");
        std::fs::read_to_string(&files[0]).unwrap()
    }
}

/// Run one prompt through the binary with the variable set to
/// `level`, or unset for `None`.
fn one_shot(base_url: &str, level: Option<&str>, prompt: &str) -> Run {
    let dirs = tempfile::tempdir().unwrap();
    let home = dirs.path().join("home");
    let data = dirs.path().join("data");
    let project = dirs.path().join("project");
    for dir in [&home, &data, &project] {
        std::fs::create_dir_all(dir).unwrap();
    }
    write_auth_store(&data);
    let mut command = Command::new(env!("CARGO_BIN_EXE_demerzel"));
    command
        .arg(prompt)
        .args(["--base-url", base_url])
        .current_dir(&project)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", dirs.path().join("config"))
        .env("XDG_DATA_HOME", &data)
        .env_remove("DEMERZEL_LOG")
        .env_remove("DEMERZEL_PROVIDER")
        .env_remove("DEMERZEL_MODEL")
        .env_remove("DEMERZEL_BASE_URL")
        .env_remove("ANTHROPIC_API_KEY");
    if let Some(level) = level {
        command.env("DEMERZEL_LOG", level);
    }
    let output = command.output().unwrap();
    Run { dirs, output }
}

fn write_auth_store(data: &Path) {
    let dir = data.join("demerzel");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("auth.json"),
        json!({
            "version": 1,
            "openai_codex": {"access_token": ACCESS_TOKEN, "refresh_token": REFRESH_TOKEN},
        })
        .to_string(),
    )
    .unwrap();
}

/// The lines of the log that hold `needle`.
fn lines_with<'a>(log: &'a str, needle: &str) -> Vec<&'a str> {
    log.lines().filter(|line| line.contains(needle)).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_trace_run_holds_no_credential_and_one_event_per_line() {
    let mock = MockServer::start(vec![codex::text_turn("traced")], Vec::new()).await;
    let run = one_shot(&mock.url(), Some("trace"), "hello\nthere");
    assert!(run.output.status.success(), "{}", run.stderr());

    // The stderr line names the file, and the file is where it says.
    let stderr = run.stderr();
    let path_line = stderr
        .lines()
        .find(|line| line.starts_with("log file: "))
        .expect("the path line");
    let path = Path::new(&path_line["log file: ".len()..]);
    assert!(path.is_file(), "{path:?}");
    assert_eq!(run.log_files(), [path.to_path_buf()]);

    // The credentials never reach the file, at the fullest level.
    let log = run.log();
    for secret in [
        ACCESS_TOKEN,
        REFRESH_TOKEN,
        "Bearer",
        "bearer",
        "uthorization",
    ] {
        assert!(!log.contains(secret), "{secret} in the trace");
    }
    // The fullest level holds the request body, the wire events, and
    // the conversation.
    assert!(!lines_with(&log, "request body ").is_empty(), "{log}");
    assert!(log.contains("\"instructions\""), "{log}");
    assert!(!lines_with(&log, "sse payload ").is_empty(), "{log}");
    assert!(!lines_with(&log, "message {").is_empty(), "{log}");
    // The request went out with the bearer header the mock saw; the
    // trace's promise is about the file, not the wire.
    assert_eq!(
        mock.received()[0].authorization.as_deref(),
        Some(&*format!("Bearer {ACCESS_TOKEN}"))
    );

    // One event per line: a time, a level, a target, a message. The
    // prompt's line break lands escaped, not as a second line.
    for line in log.lines() {
        let mut parts = line.splitn(4, ' ');
        let time = parts.next().unwrap();
        humantime::parse_rfc3339(time).unwrap_or_else(|_| panic!("time on {line:?}"));
        let level = parts.next().unwrap();
        assert!(
            ["ERROR", "WARN", "INFO", "DEBUG", "TRACE"].contains(&level),
            "{line:?}"
        );
        assert!(!parts.next().unwrap().is_empty(), "{line:?}");
        assert!(parts.next().is_some(), "{line:?}");
    }
    assert!(log.contains("hello\\nthere"), "{log}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_info_level_holds_the_attempts_the_retry_the_call_and_the_turn() {
    let mock = MockServer::start(
        vec![MockResponse::error(503), codex::text_turn("second try")],
        Vec::new(),
    )
    .await;
    let run = one_shot(&mock.url(), Some("info"), "go");
    assert!(run.output.status.success(), "{}", run.stderr());
    let log = run.log();

    // Each attempt names its status and the endpoint's request id.
    let attempts = lines_with(&log, "attempt 1 of 4: status 503 request-id=mock-1");
    assert_eq!(attempts.len(), 1, "{log}");
    let attempts = lines_with(&log, "attempt 2 of 4: status 200 request-id=mock-2");
    assert_eq!(attempts.len(), 1, "{log}");
    // The retry names its delay and its cause.
    let retries = lines_with(&log, "retry in ");
    assert_eq!(retries.len(), 1, "{log}");
    assert!(
        retries[0].contains("ms after status 503 (attempt 1 of 4)"),
        "{}",
        retries[0]
    );
    // The stream's terminal event, the call, and the turn.
    assert!(!lines_with(&log, "stream ended: response.completed").is_empty());
    let calls = lines_with(&log, "model call 1:");
    assert_eq!(calls.len(), 1, "{log}");
    for field in [
        "provider=openai-codex",
        "stop=end_turn",
        "duration_ms=",
        "input_tokens=10",
        "cache_read_tokens=2",
    ] {
        assert!(calls[0].contains(field), "{field} missing on {}", calls[0]);
    }
    assert_eq!(lines_with(&log, "turn completed duration_ms=").len(), 1);
    assert_eq!(lines_with(&log, "session created id=").len(), 1);
    // Nothing from the levels below `info`.
    assert!(lines_with(&log, " DEBUG ").is_empty(), "{log}");
    assert!(lines_with(&log, " TRACE ").is_empty(), "{log}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unset_variable_writes_nothing() {
    let mock = MockServer::start(vec![codex::text_turn("quiet")], Vec::new()).await;
    let run = one_shot(&mock.url(), None, "go");
    assert!(run.output.status.success(), "{}", run.stderr());
    assert!(run.log_files().is_empty());
    assert!(!run.stderr().contains("log file:"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_value_that_is_not_a_level_stops_the_run() {
    let mock = MockServer::start(Vec::new(), Vec::new()).await;
    let run = one_shot(&mock.url(), Some("loud"), "go");
    assert_eq!(run.output.status.code(), Some(1));
    let stderr = run.stderr();
    assert!(stderr.contains("DEMERZEL_LOG"), "{stderr}");
    assert!(stderr.contains("loud"), "{stderr}");
    assert!(run.log_files().is_empty());
    assert!(mock.received().is_empty());
}
