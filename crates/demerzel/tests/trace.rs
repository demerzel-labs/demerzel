//! The debug trace through the real binary: `DEMERZEL_LOG` on a
//! one-shot run against the mock server, in a private data
//! directory. No network, no model.
//!
//! The contract these tests hold to: the line shape, the fields each
//! level carries (`docs/config.md`, "The debug trace"), and the
//! credential promise. The sentences around the fields are not
//! contract, so the assertions look for fields on one line and never
//! for a phrase.

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
/// `level`, or unset for `None`. The project holds `f.txt`, so a
/// scripted `read` call has something to read.
fn one_shot(base_url: &str, level: Option<&str>, prompt: &str) -> Run {
    let dirs = tempfile::tempdir().unwrap();
    let home = dirs.path().join("home");
    let data = dirs.path().join("data");
    let project = dirs.path().join("project");
    for dir in [&home, &data, &project] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(project.join("f.txt"), "alpha\n").unwrap();
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

/// The lines of the log that hold every one of `fields`.
fn lines_with<'a>(log: &'a str, fields: &[&str]) -> Vec<&'a str> {
    log.lines()
        .filter(|line| fields.iter().all(|field| line.contains(field)))
        .collect()
}

/// Exactly one line holds every one of `fields`; returns it.
fn one_line_with<'a>(log: &'a str, fields: &[&str]) -> &'a str {
    let lines = lines_with(log, fields);
    assert_eq!(lines.len(), 1, "{fields:?} in\n{log}");
    lines[0]
}

/// The level of one log line: the second field.
fn level_of(line: &str) -> &str {
    line.split(' ').nth(1).unwrap_or_default()
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
    // Private to the user: the file holds the conversation.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode {mode:o}");
    }

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
    assert!(log.contains("\"instructions\""), "{log}");
    assert!(log.contains("response.output_text.delta"), "{log}");
    assert!(log.contains("\"role\":\"assistant\""), "{log}");
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
    // Every target is the harness's own.
    for line in log.lines() {
        let target = line.split(' ').nth(2).unwrap();
        assert!(target.starts_with("demerzel"), "{line:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_info_level_holds_the_session_the_attempts_the_calls_and_the_turn() {
    let mock = MockServer::start(
        vec![
            MockResponse::error(503),
            codex::tool_turn("call_1", "read", json!({"path": "f.txt"})),
            codex::text_turn("second try"),
        ],
        Vec::new(),
    )
    .await;
    let run = one_shot(&mock.url(), Some("info"), "go");
    assert!(run.output.status.success(), "{}", run.stderr());
    let log = run.log();

    // The session, by id and path.
    one_line_with(&log, &["session created", "id=", "path="]);
    // Each answered attempt names its status and the endpoint's
    // request id; the retry names its delay and its cause.
    one_line_with(&log, &["attempt 1 of 4", "status 503", "request-id=mock-1"]);
    one_line_with(&log, &["attempt 2 of 4", "status 200", "request-id=mock-2"]);
    one_line_with(&log, &["retry in ", "ms", "status 503", "attempt 1 of 4"]);
    // Two streams ended, one per model call.
    assert_eq!(
        lines_with(&log, &["stream ended", "response.completed"]).len(),
        2
    );
    // Each model call: the provider, the model, the effort, the stop,
    // the duration, and the usage.
    let calls = lines_with(&log, &["model call", "provider=openai-codex"]);
    assert_eq!(calls.len(), 2, "{log}");
    for (call, stop) in calls.iter().zip(["stop=tool_use", "stop=end_turn"]) {
        for field in [
            "model=",
            "effort=",
            stop,
            "duration_ms=",
            "input_tokens=10",
            "output_tokens=5",
            "cache_read_tokens=2",
        ] {
            assert!(call.contains(field), "{field} missing on {call}");
        }
    }
    // The approval decision and the tool call, with its clock, its
    // size, and its summary line.
    one_line_with(&log, &["approval read", "call_id=call_1", "decision=allow"]);
    let tool = one_line_with(&log, &["tool read", "call_id=call_1"]);
    for field in [
        "duration_ms=",
        "is_error=false",
        "output_bytes=",
        "truncated=false",
        "summary=",
    ] {
        assert!(tool.contains(field), "{field} missing on {tool}");
    }
    // The turn outcome, with the turn's clock.
    one_line_with(&log, &["turn completed", "duration_ms="]);
    // Nothing from the levels below `info`.
    for line in log.lines() {
        assert!(
            ["INFO", "WARN", "ERROR"].contains(&level_of(line)),
            "{line}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_debug_level_adds_the_body_size_and_the_event_counts() {
    let mock = MockServer::start(vec![codex::text_turn("sized")], Vec::new()).await;
    let run = one_shot(&mock.url(), Some("debug"), "go");
    assert!(run.output.status.success(), "{}", run.stderr());
    let log = run.log();

    let request = one_line_with(&log, &["request POST", "body_bytes="]);
    assert_eq!(level_of(request), "DEBUG");
    let counts = one_line_with(&log, &["sse events", "response.completed=1"]);
    assert!(counts.contains("response.output_text.delta=1"), "{counts}");
    // Nothing from `trace`.
    assert!(lines_with(&log, &[" TRACE "]).is_empty(), "{log}");
    assert!(!log.contains("\"instructions\""), "{log}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_run_names_its_file_and_keeps_the_token_out_at_error_level() {
    // A 400 ends the turn; the mock's error body echoes the bearer
    // header, the way a careless proxy can.
    let mock = MockServer::start(vec![MockResponse::error(400)], Vec::new()).await;
    let run = one_shot(&mock.url(), Some("error"), "go");
    assert_eq!(run.output.status.code(), Some(1));
    // The failed run still names its file.
    assert!(run.stderr().contains("log file: "), "{}", run.stderr());
    let log = run.log();
    // `error` alone holds the failure: the turn's error line with the
    // status, and nothing from the levels below.
    let turn = one_line_with(&log, &["turn error", "status 400"]);
    assert_eq!(level_of(turn), "ERROR");
    for line in log.lines() {
        assert_eq!(level_of(line), "ERROR", "{line}");
    }
    // The echo lands redacted.
    assert!(log.contains("[redacted]"), "{log}");
    assert!(!log.contains(ACCESS_TOKEN), "{log}");
    // A boot that fails before the interface — here, no credentials
    // in an empty data directory — names the file too.
    let mock = MockServer::start(Vec::new(), Vec::new()).await;
    let dirs = tempfile::tempdir().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_demerzel"));
    command
        .arg("go")
        .args(["--base-url", &mock.url()])
        .current_dir(dirs.path())
        .env("HOME", dirs.path())
        .env("XDG_CONFIG_HOME", dirs.path().join("config"))
        .env("XDG_DATA_HOME", dirs.path().join("data"))
        .env("DEMERZEL_LOG", "info")
        .env_remove("ANTHROPIC_API_KEY");
    let output = command.output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("log file: "), "{stderr}");
    let logs: Vec<_> = std::fs::read_dir(dirs.path().join("data/demerzel/logs"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(logs.len(), 1);
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
