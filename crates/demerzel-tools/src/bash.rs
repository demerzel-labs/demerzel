//! The `bash` tool: a subprocess with environment and process-group limits.
//!
//! The child gets a constructed environment — an allowlist of names,
//! never the full ambient environment — so credential variables of the
//! harness process never reach a command. The allowlist is not a full
//! credential boundary: HOME passes through, so credential files stay
//! readable, and the approval policy is the gate for that. Each call
//! has a timeout, and the whole process group dies on every exit path:
//! timeout, normal exit, and cancellation.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use demerzel_core::{BoxFuture, ToolAccess};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

use crate::{Tool, ToolError, ToolOutput, parse_input};

/// Environment variables the child inherits by default. Extend the
/// list per tool instance with [`BashTool::inherit_env`].
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

/// Grace period for stream readers after the group is killed. Only a
/// process that escaped the group (setsid) can still hold the pipe.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

pub struct BashTool {
    root: PathBuf,
    inherited: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BashInput {
    command: String,
    timeout_seconds: Option<u64>,
}

impl BashTool {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            inherited: INHERITED_VARS.iter().map(|name| name.to_string()).collect(),
        }
    }

    /// Add environment variables to the inherited allowlist, e.g. from
    /// project config: proxies, JAVA_HOME, CARGO_TARGET_DIR.
    pub fn inherit_env(mut self, extra: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.inherited.extend(extra.into_iter().map(Into::into));
        self
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
        for name in &self.inherited {
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
        // Sweeps the process group on drop, so cancelling the tool
        // future kills the command and everything it spawned.
        let mut guard = GroupKill::new(child.id());
        let stdout_buf = Arc::new(Mutex::new(StreamBuf::default()));
        let stderr_buf = Arc::new(Mutex::new(StreamBuf::default()));
        let stdout_task = tokio::spawn(pump(child.stdout.take(), Arc::clone(&stdout_buf)));
        let stderr_task = tokio::spawn(pump(child.stderr.take(), Arc::clone(&stderr_buf)));

        let status = tokio::select! {
            status = child.wait() => Some(status),
            _ = tokio::time::sleep(Duration::from_secs(timeout)) => None,
        };
        let timed_out = status.is_none();
        // Sweep the group on both paths: the timeout kills the command,
        // and after a normal exit the sweep takes grandchildren that
        // would otherwise hold the pipes open forever. The direct kill
        // covers platforms where the group sweep is a no-op.
        guard.kill_now();
        if timed_out {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        let exit = status.and_then(Result::ok);

        // The readers see EOF once the group is dead; the grace covers
        // a process that escaped the group. Abort on expiry — the
        // shared buffers keep whatever was captured either way.
        let stdout_abort = stdout_task.abort_handle();
        let stderr_abort = stderr_task.abort_handle();
        let (stdout_done, stderr_done) = tokio::join!(
            tokio::time::timeout(DRAIN_GRACE, stdout_task),
            tokio::time::timeout(DRAIN_GRACE, stderr_task),
        );
        if stdout_done.is_err() {
            stdout_abort.abort();
        }
        if stderr_done.is_err() {
            stderr_abort.abort();
        }
        let stdout = assemble(&stdout_buf, stdout_done.is_err());
        let stderr = assemble(&stderr_buf, stderr_done.is_err());

        let mut content = String::new();
        if timed_out {
            content.push_str(&format!("command timed out after {timeout}s\n"));
        }
        if let Some(exit) = exit {
            content.push_str(&exit_line(exit));
        }
        let truncated = stdout.truncated || stderr.truncated;
        push_stream(&mut content, "stdout", &stdout);
        push_stream(&mut content, "stderr", &stderr);
        if stdout.text.is_empty() && stderr.text.is_empty() {
            content.push_str("no output\n");
        }
        content.pop();

        let failed = timed_out || !exit.is_some_and(|exit| exit.success());
        let mut preview: String = input.command.chars().take(48).collect();
        if preview.len() < input.command.len() {
            preview.push('…');
        }
        let summary = if timed_out {
            format!("$ {preview} (timeout {timeout}s)")
        } else {
            match exit.and_then(|exit| exit.code()) {
                Some(code) => format!("$ {preview} (exit {code})"),
                None => format!("$ {preview} (killed)"),
            }
        };
        Ok(ToolOutput {
            content,
            is_error: failed,
            truncated,
            summary: Some(summary),
        })
    }
}

fn exit_line(status: std::process::ExitStatus) -> String {
    if let Some(code) = status.code() {
        return format!("exit code: {code}\n");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("exit code: none (killed by signal {signal})\n");
        }
    }
    "exit code: none\n".to_string()
}

/// Bounded capture of one stream: the first `HEAD_BYTES`, the last
/// `TAIL_BYTES`, and a count of what fell between.
#[derive(Default)]
struct StreamBuf {
    head: Vec<u8>,
    tail: Vec<u8>,
    omitted: usize,
}

impl StreamBuf {
    fn push(&mut self, mut bytes: &[u8]) {
        if self.head.len() < HEAD_BYTES {
            let take = bytes.len().min(HEAD_BYTES - self.head.len());
            self.head.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
        }
        self.tail.extend_from_slice(bytes);
        if self.tail.len() > TAIL_BYTES {
            let drop = self.tail.len() - TAIL_BYTES;
            self.tail.drain(..drop);
            self.omitted += drop;
        }
    }
}

/// Read one pipe into the shared buffer until EOF. The buffer lives in
/// the caller, so a cancelled or aborted pump loses nothing captured.
async fn pump(pipe: Option<impl tokio::io::AsyncRead + Unpin>, buf: Arc<Mutex<StreamBuf>>) {
    let Some(mut pipe) = pipe else { return };
    let mut chunk = [0u8; 8 * 1024];
    loop {
        match pipe.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(count) => buf.lock().unwrap().push(&chunk[..count]),
        }
    }
}

struct Captured {
    text: String,
    truncated: bool,
}

/// Assemble the final text with one lossy decode over the joined
/// bytes, so a multibyte character on the head/tail seam survives
/// when nothing was dropped between them.
fn assemble(buf: &Arc<Mutex<StreamBuf>>, stream_open: bool) -> Captured {
    let buf = buf.lock().unwrap();
    let mut bytes = buf.head.clone();
    if buf.omitted > 0 {
        bytes.extend_from_slice(
            format!("\n[truncated: omitted {} bytes]\n", buf.omitted).as_bytes(),
        );
    }
    bytes.extend_from_slice(&buf.tail);
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if stream_open && !text.is_empty() {
        text.push_str("\n[the stream did not close; output may be incomplete]");
    }
    Captured {
        text,
        truncated: buf.omitted > 0 || stream_open,
    }
}

/// Kills the child's whole process group: on the timeout, after a
/// normal exit (to sweep lingering grandchildren), and on drop when
/// the tool call is cancelled.
struct GroupKill {
    pgid: Option<i32>,
}

impl GroupKill {
    fn new(pid: Option<u32>) -> Self {
        Self {
            pgid: pid.map(|pid| pid as i32),
        }
    }

    fn kill_now(&mut self) {
        let Some(pgid) = self.pgid.take() else {
            return;
        };
        #[cfg(unix)]
        // The child was spawned with process_group(0), so its pid is
        // the process-group id.
        unsafe {
            libc::killpg(pgid, libc::SIGKILL);
        }
        #[cfg(not(unix))]
        let _ = pgid;
    }
}

impl Drop for GroupKill {
    fn drop(&mut self) {
        self.kill_now();
    }
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
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Run a bash command in the project root. The result carries the exit code, stdout, and stderr. The command gets a minimal environment and a timeout, and its process group ends with the call."
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
        assert_eq!(
            output.summary.as_deref(),
            Some("$ echo out; echo err >&2 (exit 0)")
        );
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
    async fn the_env_allowlist_extends_per_instance() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("DMZ_TEST_EXTRA", "visible") };
        let tool = BashTool::new(dir.path()).inherit_env(["DMZ_TEST_EXTRA"]);
        let output = tool.run(json!({"command": "env"})).await.unwrap();
        unsafe { std::env::remove_var("DMZ_TEST_EXTRA") };
        assert!(output.content.contains("DMZ_TEST_EXTRA=visible"));
    }

    #[tokio::test]
    async fn a_timeout_kills_the_command_and_keeps_partial_output() {
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
    async fn a_backgrounded_grandchild_does_not_stall_or_eat_the_output() {
        let dir = tempfile::tempdir().unwrap();
        let start = std::time::Instant::now();
        let output = bash(&dir, json!({"command": "sleep 30 & echo STARTED"}))
            .await
            .unwrap();
        // The group sweep kills the backgrounded sleep, so the pipes
        // close at once: no drain grace, no lost output.
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(!output.is_error);
        assert!(output.content.contains("STARTED"));
        assert!(!output.content.contains("stream did not close"));
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
    async fn a_multibyte_character_on_the_head_seam_survives() {
        let dir = tempfile::tempdir().unwrap();
        // One ASCII byte shifts the 4-byte units off the 8 KiB head
        // boundary, so a character straddles the seam; the stream ends
        // on a unit boundary and stays under the cap, so one decode
        // must heal the seam.
        let output = bash(&dir, json!({"command": "printf x; yes あ | head -c 18000"}))
            .await
            .unwrap();
        assert!(!output.truncated);
        assert!(!output.content.contains('\u{FFFD}'));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_signal_death_names_the_signal() {
        let dir = tempfile::tempdir().unwrap();
        let output = bash(&dir, json!({"command": "kill -SEGV $$"}))
            .await
            .unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("killed by signal 11"));
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
