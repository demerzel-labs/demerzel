//! The `bash` tool: a real subprocess with the sandbox posture.
//!
//! The child gets a constructed environment — PATH, HOME, TERM, LANG
//! only, never the ambient credentials of the harness process. Each
//! call has a timeout that kills the whole process group. Output is
//! captured with a bounded buffer per stream.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use demerzel_core::ToolAccess;
use demerzel_providers::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

use crate::{Tool, ToolError, ToolOutput, parse_input};

/// Environment variables the child inherits. Everything else is
/// dropped, so ambient credentials never reach a command.
const INHERITED_VARS: [&str; 4] = ["PATH", "HOME", "TERM", "LANG"];

/// PATH used when the harness itself has none.
const FALLBACK_PATH: &str = "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

const DEFAULT_TIMEOUT_SECONDS: u64 = 120;
const MAX_TIMEOUT_SECONDS: u64 = 600;

/// Per-stream capture budget: the first `HEAD_BYTES` and the last
/// `TAIL_BYTES` survive; the middle is dropped with a marker. The tail
/// window is larger because build and test tools put the verdict last.
const HEAD_BYTES: usize = 8 * 1024;
const TAIL_BYTES: usize = 24 * 1024;

/// Grace period for stream readers after the child dies. A leaked
/// grandchild outside the process group can hold the pipe open.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

pub struct BashTool {
    root: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BashInput {
    command: String,
    timeout_seconds: Option<u64>,
}

impl BashTool {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    async fn bash(&self, input: Value) -> Result<ToolOutput, ToolError> {
        let input: BashInput = parse_input(input)?;
        let timeout = input.timeout_seconds.unwrap_or(DEFAULT_TIMEOUT_SECONDS);
        if timeout == 0 || timeout > MAX_TIMEOUT_SECONDS {
            return Err(ToolError::InvalidInput(format!(
                "timeout_seconds must be between 1 and {MAX_TIMEOUT_SECONDS}"
            )));
        }

        let mut command = tokio::process::Command::new("bash");
        command
            .arg("-c")
            .arg(&input.command)
            .current_dir(&self.root)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for name in INHERITED_VARS {
            if let Ok(value) = std::env::var(name) {
                command.env(name, value);
            }
        }
        if std::env::var("PATH").is_err() {
            command.env("PATH", FALLBACK_PATH);
        }
        #[cfg(unix)]
        command.process_group(0);

        let mut child = command
            .spawn()
            .map_err(|error| ToolError::Failed(format!("cannot start bash: {error}")))?;
        let stdout_task = tokio::spawn(capture(child.stdout.take()));
        let stderr_task = tokio::spawn(capture(child.stderr.take()));

        let status = tokio::select! {
            status = child.wait() => Some(status),
            _ = tokio::time::sleep(Duration::from_secs(timeout)) => None,
        };
        let timed_out = status.is_none();
        if timed_out {
            kill_process_group(&child);
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        let exit = status.and_then(Result::ok);
        let stdout = drain(stdout_task).await;
        let stderr = drain(stderr_task).await;

        let mut content = String::new();
        if timed_out {
            content.push_str(&format!("command timed out after {timeout}s\n"));
        }
        if let Some(exit) = exit {
            match exit.code() {
                Some(code) => content.push_str(&format!("exit code: {code}\n")),
                None => content.push_str("exit code: none (terminated by a signal)\n"),
            }
        }
        let truncated = stdout.truncated || stderr.truncated;
        push_stream(&mut content, "stdout", &stdout);
        push_stream(&mut content, "stderr", &stderr);
        if stdout.text.is_empty() && stderr.text.is_empty() {
            content.push_str("no output\n");
        }
        content.pop();

        let failed = timed_out || !exit.is_some_and(|exit| exit.success());
        Ok(ToolOutput {
            content,
            is_error: failed,
            truncated,
        })
    }
}

struct Captured {
    text: String,
    truncated: bool,
}

/// Read one pipe to EOF with a bounded buffer: the first `HEAD_BYTES`
/// stay, the last `TAIL_BYTES` roll, the rest is counted and dropped.
async fn capture(pipe: Option<impl tokio::io::AsyncRead + Unpin>) -> Captured {
    let mut head: Vec<u8> = Vec::new();
    let mut tail: Vec<u8> = Vec::new();
    let mut omitted = 0usize;
    if let Some(mut pipe) = pipe {
        let mut chunk = [0u8; 8 * 1024];
        loop {
            match pipe.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    let mut bytes = &chunk[..count];
                    if head.len() < HEAD_BYTES {
                        let take = bytes.len().min(HEAD_BYTES - head.len());
                        head.extend_from_slice(&bytes[..take]);
                        bytes = &bytes[take..];
                    }
                    tail.extend_from_slice(bytes);
                    if tail.len() > TAIL_BYTES {
                        let drop = tail.len() - TAIL_BYTES;
                        tail.drain(..drop);
                        omitted += drop;
                    }
                }
            }
        }
    }
    let truncated = omitted > 0;
    let mut text = String::from_utf8_lossy(&head).into_owned();
    if truncated {
        text.push_str(&format!("\n[truncated: omitted {omitted} bytes]\n"));
    }
    text.push_str(&String::from_utf8_lossy(&tail));
    Captured { text, truncated }
}

async fn drain(task: tokio::task::JoinHandle<Captured>) -> Captured {
    match tokio::time::timeout(DRAIN_GRACE, task).await {
        Ok(Ok(captured)) => captured,
        _ => Captured {
            text: "[output unavailable: the stream did not close]".to_string(),
            truncated: true,
        },
    }
}

/// Kill the child's whole process group, so a pipeline or a spawned
/// grandchild dies with the shell.
fn kill_process_group(child: &tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // The child was spawned with process_group(0), so its pid is
        // the process-group id.
        unsafe {
            libc::killpg(pid as i32, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = child;
}

fn push_stream(content: &mut String, name: &str, captured: &Captured) {
    if captured.text.is_empty() {
        return;
    }
    content.push_str(name);
    content.push_str(":\n");
    content.push_str(&captured.text);
    if !content.ends_with('\n') {
        content.push('\n');
    }
}

impl Tool for BashTool {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn description(&self) -> &'static str {
        "Run a bash command in the project root. The result carries the exit code, stdout, and stderr. The command gets a minimal environment and a timeout."
    }

    fn access(&self) -> ToolAccess {
        ToolAccess::Execute
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command to run with `bash -c`."
                },
                "timeout_seconds": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 600,
                    "description": "Kill the command after this many seconds. Default: 120."
                }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    fn run(&self, input: Value) -> BoxFuture<'_, Result<ToolOutput, ToolError>> {
        Box::pin(self.bash(input))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn bash(dir: &tempfile::TempDir, input: Value) -> Result<ToolOutput, ToolError> {
        BashTool::new(dir.path()).run(input).await
    }

    #[tokio::test]
    async fn captures_stdout_stderr_and_the_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let output = bash(&dir, json!({"command": "echo out; echo err >&2"}))
            .await
            .unwrap();
        assert!(!output.is_error);
        assert!(output.content.starts_with("exit code: 0\n"));
        assert!(output.content.contains("stdout:\nout\n"));
        assert!(output.content.contains("stderr:\nerr"));
    }

    #[tokio::test]
    async fn a_nonzero_exit_is_an_error_result_with_the_code() {
        let dir = tempfile::tempdir().unwrap();
        let output = bash(&dir, json!({"command": "exit 3"})).await.unwrap();
        assert!(output.is_error);
        assert!(output.content.starts_with("exit code: 3\n"));
        assert!(output.content.contains("no output"));
    }

    #[tokio::test]
    async fn runs_in_the_project_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("marker.txt"), "here\n").unwrap();
        let output = bash(&dir, json!({"command": "cat marker.txt"}))
            .await
            .unwrap();
        assert!(output.content.contains("here"));
    }

    #[tokio::test]
    async fn the_environment_is_constructed_not_inherited() {
        let dir = tempfile::tempdir().unwrap();
        // A credential-shaped variable in the harness process must not
        // reach the child; the allowlisted variables must.
        unsafe { std::env::set_var("DMZ_TEST_SECRET", "leaked") };
        let output = bash(&dir, json!({"command": "env"})).await.unwrap();
        unsafe { std::env::remove_var("DMZ_TEST_SECRET") };
        assert!(!output.content.contains("DMZ_TEST_SECRET"));
        assert!(output.content.contains("PATH="));
        assert!(output.content.contains("HOME="));
    }

    #[tokio::test]
    async fn a_timeout_kills_the_command_and_reports_it() {
        let dir = tempfile::tempdir().unwrap();
        let start = std::time::Instant::now();
        let output = bash(
            &dir,
            json!({"command": "echo FIRST_MARK; sleep 30; echo SECOND_MARK", "timeout_seconds": 1}),
        )
        .await
        .unwrap();
        assert!(start.elapsed() < Duration::from_secs(10));
        assert!(output.is_error);
        assert!(output.content.contains("command timed out after 1s"));
        assert!(output.content.contains("FIRST_MARK"));
        assert!(!output.content.contains("SECOND_MARK"));
    }

    #[tokio::test]
    async fn oversized_output_keeps_the_head_and_the_tail() {
        let dir = tempfile::tempdir().unwrap();
        let output = bash(&dir, json!({"command": "seq 1 20000"})).await.unwrap();
        assert!(output.truncated);
        assert!(output.content.contains("1\n2\n"));
        assert!(output.content.contains("[truncated: omitted"));
        assert!(output.content.contains("20000"));
    }

    #[tokio::test]
    async fn an_invalid_timeout_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let error = bash(&dir, json!({"command": "true", "timeout_seconds": 0}))
            .await
            .unwrap_err();
        assert!(matches!(error, ToolError::InvalidInput(_)));
        let error = bash(&dir, json!({"command": "true", "timeout_seconds": 601}))
            .await
            .unwrap_err();
        assert!(matches!(error, ToolError::InvalidInput(_)));
    }
}
