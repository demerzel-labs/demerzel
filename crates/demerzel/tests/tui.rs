//! Scripted PTY sessions through the real binary: the full-screen
//! interface against the mock server. No network, no model.
//!
//! Each test spawns the binary on a pseudo-terminal, drives it with
//! key bytes, and asserts on the merged output stream. Frame diffs
//! interleave escape sequences with text, so assertions run against a
//! stream with the escape sequences stripped.

use std::io::Read as _;
use std::os::fd::{FromRawFd as _, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::{Duration, Instant};

use demerzel_core::{ApprovalMode, ContentBlock, Message, Role, SessionStore};
use demerzel_mock::{MockResponse, MockServer};
use demerzel_providers::auth::NO_CREDENTIALS_HELP;
use serde_json::{Value, json};

const READY: &str = "demerzel ready";
// No trailing space: the frame diff skips cells that stay blank.
const PROMPT: &str = "❯";
const ENTER_ALT_SCREEN: &str = "\x1b[?1049h";
const LEAVE_ALT_SCREEN: &str = "\x1b[?1049l";
/// The status after one Ctrl-C on an empty input box.
const QUIT_HINT: &str = "press Ctrl-C again to quit";
const CTRL_C: &[u8] = b"\x03";
const CTRL_D: &[u8] = b"\x04";
const END: &[u8] = b"\x1b[F";
const CTRL_O: &[u8] = b"\x0f";
const CTRL_T: &[u8] = b"\x14";
const PAGE_UP: &[u8] = b"\x1b[5~";
const SHIFT_TAB: &[u8] = b"\x1b[Z";
/// Alt-Enter as a legacy terminal reports it.
const ALT_ENTER: &[u8] = b"\x1b\r";
/// Shift-Enter as a terminal with the keyboard-enhancement flags
/// reports it.
const SHIFT_ENTER: &[u8] = b"\x1b[13;2u";
/// The keyboard-enhancement push the interface sends on entry, with
/// the disambiguate flag. A PTY can observe the request alone.
const KEYBOARD_PUSH: &str = "\x1b[>1u";
/// One wheel notch up, as a terminal reports it with SGR mouse mode.
const WHEEL_UP: &[u8] = b"\x1b[<64;10;10M";
/// One wheel notch down. At the bottom of a short transcript it
/// changes nothing on screen, but it wakes a draw, and it is not a
/// key: the quit question stands through it.
const WHEEL_DOWN: &[u8] = b"\x1b[<65;10;10M";
/// Mouse reporting mode 1002 — buttons, the wheel, and the pointer
/// while a button is down — the first sequence capture turns on.
/// The mode is the only sign of capture a PTY can observe, so the
/// test pins it; the interface owns the sequence, not a dependency.
const MOUSE_CAPTURE: &str = "\x1b[?1002h";
/// A left-button press, drag, and release at 1-based `(col, row)`,
/// as a terminal reports them with SGR mouse mode.
fn mouse_press(col: u16, row: u16) -> Vec<u8> {
    format!("\x1b[<0;{col};{row}M").into_bytes()
}
fn mouse_drag(col: u16, row: u16) -> Vec<u8> {
    format!("\x1b[<32;{col};{row}M").into_bytes()
}
fn mouse_release(col: u16, row: u16) -> Vec<u8> {
    format!("\x1b[<0;{col};{row}m").into_bytes()
}

/// The standard base64 form of `text`, for the OSC 52 payload.
fn base64(text: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in text.as_bytes().chunks(3) {
        let bits = (u32::from(chunk[0]) << 16)
            | (u32::from(chunk.get(1).copied().unwrap_or(0)) << 8)
            | u32::from(chunk.get(2).copied().unwrap_or(0));
        out.push(ALPHABET[(bits >> 18 & 63) as usize] as char);
        out.push(ALPHABET[(bits >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(bits >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(bits & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn text_delta(text: &str) -> Value {
    json!({"type": "response.output_text.delta", "delta": text})
}

fn completed() -> Value {
    json!({
        "type": "response.completed",
        "response": {"usage": {
            "input_tokens": 10,
            "output_tokens": 5,
            "input_tokens_details": {"cached_tokens": 2},
        }},
    })
}

fn completed_with_input(input_tokens: u64) -> Value {
    json!({
        "type": "response.completed",
        "response": {"usage": {"input_tokens": input_tokens, "output_tokens": 5}},
    })
}

fn incomplete_with_input(input_tokens: u64) -> Value {
    json!({
        "type": "response.incomplete",
        "response": {"usage": {"input_tokens": input_tokens, "output_tokens": 5}},
    })
}

fn text_turn(text: &str) -> MockResponse {
    MockResponse::stream(vec![text_delta(text), completed()])
}

fn tool_turn(name: &str, input: Value) -> MockResponse {
    tool_call("call_1", name, input)
}

fn tool_call(id: &str, name: &str, input: Value) -> MockResponse {
    MockResponse::stream(vec![
        json!({
            "type": "response.output_item.added",
            "item": {"type": "function_call", "id": "item_1", "call_id": id, "name": name},
        }),
        json!({
            "type": "response.output_item.done",
            "item": {"type": "function_call", "call_id": id, "arguments": input.to_string()},
        }),
        completed_for_tool(),
    ])
}

/// One reasoning segment with `summary`, then a text reply, the way
/// the Responses endpoint streams them.
fn reasoning_turn(summary: &str, reply: &str) -> MockResponse {
    MockResponse::stream(vec![
        json!({
            "type": "response.output_item.added",
            "item": {"type": "reasoning", "id": "rs_1"},
        }),
        json!({"type": "response.reasoning_summary_part.added", "summary_index": 0}),
        json!({"type": "response.reasoning_summary_text.delta", "delta": summary}),
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "reasoning",
                "id": "rs_1",
                "summary": [{"type": "summary_text", "text": summary}],
            },
        }),
        text_delta(reply),
        completed(),
    ])
}

fn completed_for_tool() -> Value {
    json!({"type": "response.completed", "response": {"usage": {
        "input_tokens": 10, "output_tokens": 5,
    }}})
}

/// A spawned session on a PTY. The child dies with the test.
struct Pty {
    child: Child,
    master: OwnedFd,
    seen: Vec<u8>,
    /// Search cursor into the stripped, space-free view of `seen`.
    cursor: usize,
    /// The project directory the session runs in.
    project: PathBuf,
    /// The home, data, config, and project directories of this run.
    /// Shared with a relaunch, which runs in the same ones.
    dirs: Arc<tempfile::TempDir>,
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Pty {
    /// Spawn the binary on a fresh PTY with a private home, a filled
    /// auth store, and the mock endpoint.
    fn spawn(base_url: &str, extra_args: &[&str]) -> Self {
        Self::spawn_with_project_config(base_url, extra_args, None)
    }

    /// [`Pty::spawn`] with an empty data directory: no auth store,
    /// so no credential resolves.
    fn spawn_without_login(base_url: &str, extra_args: &[&str]) -> Self {
        Self::spawn_full(
            base_url,
            extra_args,
            None,
            None,
            &[],
            "project",
            None,
            false,
        )
    }

    /// [`Pty::spawn`] with a `.demerzel/config.toml` in the project.
    fn spawn_with_project_config(
        base_url: &str,
        extra_args: &[&str],
        project_config: Option<&str>,
    ) -> Self {
        Self::spawn_with_configs(base_url, extra_args, project_config, None)
    }

    /// [`Pty::spawn`] with a project file and a user file.
    fn spawn_with_configs(
        base_url: &str,
        extra_args: &[&str],
        project_config: Option<&str>,
        user_config: Option<&str>,
    ) -> Self {
        Self::spawn_with_env(base_url, extra_args, project_config, user_config, &[])
    }

    /// [`Pty::spawn_with_configs`] with extra environment variables
    /// on the child: the color depth the terminal declares.
    fn spawn_with_env(
        base_url: &str,
        extra_args: &[&str],
        project_config: Option<&str>,
        user_config: Option<&str>,
        env: &[(&str, &str)],
    ) -> Self {
        Self::spawn_full(
            base_url,
            extra_args,
            project_config,
            user_config,
            env,
            "project",
            None,
            true,
        )
    }

    /// [`Pty::spawn`] in a project directory named `name` that holds
    /// a `.git/HEAD` with `head` as its text.
    fn spawn_in_repo(base_url: &str, name: &str, head: &str) -> Self {
        Self::spawn_full(base_url, &[], None, None, &[], name, Some(head), true)
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_full(
        base_url: &str,
        extra_args: &[&str],
        project_config: Option<&str>,
        user_config: Option<&str>,
        env: &[(&str, &str)],
        project_name: &str,
        head: Option<&str>,
        login: bool,
    ) -> Self {
        let dirs = tempfile::tempdir().unwrap();
        let home = dirs.path().join("home");
        let data = dirs.path().join("data");
        let project = dirs.path().join(project_name);
        for dir in [&home, &data, &project] {
            std::fs::create_dir_all(dir).unwrap();
        }
        if login {
            write_auth_store(&data);
        }
        if let Some(head) = head {
            let git = project.join(".git");
            std::fs::create_dir(&git).unwrap();
            std::fs::write(git.join("HEAD"), head).unwrap();
        }
        if let Some(config) = project_config {
            let dir = project.join(".demerzel");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("config.toml"), config).unwrap();
        }
        if let Some(config) = user_config {
            let dir = dirs.path().join("config").join("demerzel");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("config.toml"), config).unwrap();
        }
        Self::launch(Arc::new(dirs), project, base_url, extra_args, env)
    }

    /// Spawn the binary on a fresh PTY in prepared directories.
    fn launch(
        dirs: Arc<tempfile::TempDir>,
        project: PathBuf,
        base_url: &str,
        extra_args: &[&str],
        env: &[(&str, &str)],
    ) -> Self {
        let home = dirs.path().join("home");
        let data = dirs.path().join("data");
        let (master, slave) = openpty();
        let slave_fd = std::os::fd::AsRawFd::as_raw_fd(&slave);
        let mut command = Command::new(env!("CARGO_BIN_EXE_demerzel"));
        command
            .args(extra_args)
            .args(["--base-url", base_url])
            .current_dir(&project)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", dirs.path().join("config"))
            .env("XDG_DATA_HOME", &data)
            .env("TERM", "xterm-256color")
            .env_remove("COLORTERM")
            .env_remove("NO_COLOR")
            .env_remove("DEMERZEL_PROVIDER")
            .env_remove("DEMERZEL_MODEL")
            .env_remove("DEMERZEL_BASE_URL")
            .env_remove("ANTHROPIC_API_KEY");
        for (name, value) in env {
            command.env(name, value);
        }
        unsafe {
            use std::os::unix::process::CommandExt as _;
            command.pre_exec(move || {
                libc::setsid();
                for fd in 0..3 {
                    if libc::dup2(slave_fd, fd) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        drop(slave);
        set_nonblocking(&master);
        Self {
            child,
            master,
            seen: Vec::new(),
            cursor: 0,
            project,
            dirs,
        }
    }

    /// Wait for a clean exit, then spawn the binary again in the same
    /// directories: the same project, data, and config. `-c` then
    /// opens the session the first run left.
    fn relaunch(self, base_url: &str, extra_args: &[&str]) -> Self {
        let dirs = self.dirs.clone();
        let project = self.project.clone();
        let (status, _) = self.wait_end();
        assert!(status.success(), "exit status: {status:?}");
        Self::launch(dirs, project, base_url, extra_args, &[])
    }

    /// Read the merged stream until `marker` shows up. The comparison
    /// ignores escape sequences and spaces: the frame diff never
    /// writes a cell that stays blank, so spaces over untouched
    /// ground do not reach the stream.
    /// Waits are ordered: each match advances a cursor, so the next
    /// wait finds only later output.
    fn wait_for(&mut self, marker: &str) {
        let needle: String = marker.chars().filter(|ch| *ch != ' ').collect();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let stripped = strip_escapes(&self.seen);
            let haystack: Vec<u8> = stripped
                .iter()
                .copied()
                .filter(|byte| *byte != b' ')
                .collect();
            if let Some(end) = find_after(&haystack, self.cursor, needle.as_bytes()) {
                self.cursor = end;
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {marker:?}; stripped output: {:?}",
                String::from_utf8_lossy(&stripped),
            );
            self.poll_read();
        }
    }

    /// Wait for `until`, then assert `marker` showed nowhere between
    /// the last match and it. A repaint before this makes the span
    /// the whole screen, so the check covers what is on it.
    fn absent_until(&mut self, marker: &str, until: &str) {
        self.all_absent_until(&[marker], until);
    }

    /// [`Pty::absent_until`] for several markers over one span.
    fn all_absent_until(&mut self, markers: &[&str], until: &str) {
        let from = self.cursor;
        self.wait_for(until);
        let stripped = strip_escapes(&self.seen);
        let haystack: Vec<u8> = stripped
            .iter()
            .copied()
            .filter(|byte| *byte != b' ')
            .collect();
        for marker in markers {
            let needle: String = marker.chars().filter(|ch| *ch != ' ').collect();
            assert!(
                find_after(&haystack[..self.cursor], from, needle.as_bytes()).is_none(),
                "{marker:?} showed before {until:?}; stripped output: {:?}",
                String::from_utf8_lossy(&stripped),
            );
        }
    }

    fn poll_read(&mut self) {
        let fd = std::os::fd::AsRawFd::as_raw_fd(&self.master);
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut poll, 1, 100) };
        if ready <= 0 {
            return;
        }
        let mut chunk = [0u8; 4096];
        let mut file = std::fs::File::from(self.master.try_clone().unwrap());
        if let Ok(count) = file.read(&mut chunk) {
            self.seen.extend_from_slice(&chunk[..count]);
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        let fd = std::os::fd::AsRawFd::as_raw_fd(&self.master);
        let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        assert_eq!(written, bytes.len() as isize, "PTY write failed");
    }

    /// Change the window size. The frame diff writes only changed
    /// cells, so an assertion on an in-place update — the status bar —
    /// needs the full repaint the next draw runs after a resize. The
    /// child has no controlling terminal, so no signal announces the
    /// change: the next draw sees it, and a key or a wheel notch must
    /// wake one.
    fn resize(&self, cols: u16, rows: u16) {
        let size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let fd = std::os::fd::AsRawFd::as_raw_fd(&self.master);
        let result = unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &size) };
        assert_eq!(result, 0, "PTY resize failed");
    }

    /// The project directory the session runs in.
    fn project(&self) -> PathBuf {
        self.project.clone()
    }

    /// The session store's root for this run: the data directory the
    /// binary opens by default under `XDG_DATA_HOME`.
    fn data_dir(&self) -> PathBuf {
        self.dirs.path().join("data/demerzel")
    }

    /// A login lands: the auth store appears in this run's data
    /// directory, the way `login` writes it.
    fn log_in(&self) {
        write_auth_store(&self.dirs.path().join("data"));
    }

    /// The session-file stems this run created, across all projects.
    fn session_files(&self) -> Vec<String> {
        let projects = self.dirs.path().join("data/demerzel/projects");
        let mut stems = Vec::new();
        let Ok(entries) = std::fs::read_dir(&projects) else {
            return stems;
        };
        for project in entries.flatten() {
            let sessions = project.path().join("sessions");
            let Ok(files) = std::fs::read_dir(&sessions) else {
                continue;
            };
            for file in files.flatten() {
                if let Some(stem) = file.path().file_stem() {
                    stems.push(stem.to_string_lossy().into_owned());
                }
            }
        }
        stems
    }

    /// The debug-trace files this run wrote, under the data
    /// directory.
    fn log_files(&self) -> Vec<PathBuf> {
        let logs = self.dirs.path().join("data/demerzel/logs");
        let Ok(entries) = std::fs::read_dir(&logs) else {
            return Vec::new();
        };
        entries.flatten().map(|entry| entry.path()).collect()
    }

    /// Wait for a clean exit and return the raw output.
    fn wait_exit(self) -> Vec<u8> {
        let (status, raw) = self.wait_end();
        assert!(status.success(), "exit status: {status:?}");
        raw
    }

    /// Wait for the process to end and return its status and the raw
    /// output.
    fn wait_end(mut self) -> (std::process::ExitStatus, Vec<u8>) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.poll_read();
            if let Some(status) = self.child.try_wait().unwrap() {
                // Collect what the shutdown path still wrote.
                self.poll_read();
                return (status, std::mem::take(&mut self.seen));
            }
            assert!(Instant::now() < deadline, "the session did not exit");
        }
    }
}

fn write_auth_store(data: &Path) {
    let dir = data.join("demerzel");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("auth.json"),
        json!({
            "version": 1,
            "openai_codex": {"access_token": "test-token", "refresh_token": "test-refresh"},
        })
        .to_string(),
    )
    .unwrap();
}

fn openpty() -> (OwnedFd, OwnedFd) {
    let mut master: libc::c_int = 0;
    let mut slave: libc::c_int = 0;
    // A fresh PTY reports zero rows and columns; give it a real
    // window or the interface has nothing to draw into.
    let mut size = libc::winsize {
        ws_row: 30,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // The window parameter is `*const` on Linux and `*mut` on macOS;
    // a raw mutable pointer coerces to both without tripping the
    // Linux-side unnecessary-mut lint.
    let result = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut size,
        )
    };
    assert_eq!(result, 0, "openpty failed");
    unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) }
}

fn set_nonblocking(fd: &OwnedFd) {
    let raw = std::os::fd::AsRawFd::as_raw_fd(fd);
    unsafe {
        let flags = libc::fcntl(raw, libc::F_GETFL);
        libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
}

/// Drop ANSI escape sequences (CSI, OSC, and two-byte escapes), so
/// text assertions survive frame diffs and cursor movement.
fn strip_escapes(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut bytes = raw.iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        if byte != 0x1b {
            out.push(byte);
            continue;
        }
        match bytes.next() {
            // CSI: parameters, then one final byte in 0x40..=0x7e.
            Some(b'[') => {
                for follow in bytes.by_ref() {
                    if (0x40..=0x7e).contains(&follow) {
                        break;
                    }
                }
            }
            // OSC: terminated by BEL or ESC \.
            Some(b']') => {
                while let Some(follow) = bytes.next() {
                    if follow == 0x07 {
                        break;
                    }
                    if follow == 0x1b && bytes.peek() == Some(&b'\\') {
                        bytes.next();
                        break;
                    }
                }
            }
            // Two-byte escape: the byte after ESC is consumed.
            Some(_) | None => {}
        }
    }
    out
}

/// Whether the stripped, space-free view of `raw` contains `marker`,
/// the same comparison [`Pty::wait_for`] runs with a cursor.
/// The parameters of every `ESC [ … m` sequence in `raw` that sets
/// a color: `38;5;n` for a palette entry, `38;2;r;g;b` for truecolor,
/// `48;…` for a background, in one list per sequence. The theme
/// scenarios read the depth off them.
fn color_params(raw: &[u8]) -> Vec<Vec<u16>> {
    let mut found = Vec::new();
    let mut i = 0;
    while i + 1 < raw.len() {
        if raw[i] != 0x1b || raw[i + 1] != b'[' {
            i += 1;
            continue;
        }
        let start = i + 2;
        let mut end = start;
        while end < raw.len() && (raw[end].is_ascii_digit() || raw[end] == b';') {
            end += 1;
        }
        if end < raw.len() && raw[end] == b'm' {
            let params: Vec<u16> = String::from_utf8_lossy(&raw[start..end])
                .split(';')
                .filter_map(|p| p.parse().ok())
                .collect();
            if params
                .iter()
                .any(|p| matches!(p, 30..=38 | 40..=48 | 90..=97 | 100..=107))
            {
                found.push(params);
            }
        }
        i = end.max(i + 1);
    }
    found
}

/// Whether one sequence carries `needle` as a run of parameters.
fn has_params(params: &[Vec<u16>], needle: &[u16]) -> bool {
    params
        .iter()
        .any(|sequence| sequence.windows(needle.len()).any(|w| w == needle))
}

/// Whether any sequence sets a truecolor foreground or background.
fn has_truecolor(params: &[Vec<u16>]) -> bool {
    has_params(params, &[38, 2]) || has_params(params, &[48, 2])
}

fn contains_marker(raw: &[u8], marker: &str) -> bool {
    let needle: Vec<u8> = marker.bytes().filter(|byte| *byte != b' ').collect();
    let haystack: Vec<u8> = strip_escapes(raw)
        .into_iter()
        .filter(|byte| *byte != b' ')
        .collect();
    find_after(&haystack, 0, &needle).is_some()
}

/// The output after the interface left the alternate screen, with
/// the escape sequences stripped: where the exit line lands.
fn after_restore(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw).into_owned();
    let restore = text.rfind(LEAVE_ALT_SCREEN).expect("leave-alt-screen");
    String::from_utf8_lossy(&strip_escapes(&text.as_bytes()[restore..])).into_owned()
}

/// The offset after the first match at or past `from`.
fn find_after(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || from >= haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|position| from + position + needle.len())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tui_session_boots_streams_and_exits_with_the_turn_time_on_the_usage_line() {
    let mock = MockServer::start(vec![text_turn("hi there from the model")], Vec::new()).await;
    let mut pty = Pty::spawn(&mock.url(), &[]);

    pty.wait_for(READY);
    pty.wait_for(PROMPT);
    pty.send(b"hello\r");
    pty.wait_for("hi there from the model");
    // The usage line ends with the turn's time.
    pty.wait_for("2 cached ·");
    pty.send(CTRL_D);
    let raw = pty.wait_exit();

    // The marker reached stderr before the alternate screen switch.
    let text = String::from_utf8_lossy(&raw).into_owned();
    let marker = text.find(READY).expect("ready marker");
    let alt = text.find(ENTER_ALT_SCREEN).expect("alternate screen");
    assert!(marker < alt, "the marker must precede the alternate screen");
    // The exit path restored the main screen.
    assert!(text.contains("\x1b[?1049l"), "missing leave-alt-screen");
    assert_eq!(mock.received().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_debug_trace_names_its_file_after_the_marker_and_logs_the_turn() {
    let mock = MockServer::start(vec![text_turn("traced reply")], Vec::new()).await;
    let mut pty = Pty::spawn_with_env(&mock.url(), &[], None, None, &[("DEMERZEL_LOG", "info")]);

    pty.wait_for(READY);
    pty.wait_for("log file:");
    pty.wait_for(PROMPT);
    pty.send(b"hello\r");
    pty.wait_for("traced reply");
    pty.wait_for("2 cached ·");
    // The turn line lands with the turn's end and flushes at once;
    // wait for it, and read the file before the exit takes the run's
    // directories with it.
    let logs = pty.log_files();
    assert_eq!(logs.len(), 1, "{logs:?}");
    let deadline = Instant::now() + Duration::from_secs(5);
    let log = loop {
        let log = std::fs::read_to_string(&logs[0]).unwrap();
        if log.lines().any(|line| line.contains("turn completed")) {
            break log;
        }
        assert!(Instant::now() < deadline, "no turn line in {log}");
        std::thread::sleep(Duration::from_millis(20));
    };
    pty.send(CTRL_D);
    let raw = pty.wait_exit();

    // The path line sits between the marker and the alternate
    // screen: the probe still finds the marker first, and the user
    // sees the path on the main screen.
    let text = String::from_utf8_lossy(&raw).into_owned();
    let marker = text.find(READY).expect("ready marker");
    let path_line = text.find("log file:").expect("path line");
    let alt = text.find(ENTER_ALT_SCREEN).expect("alternate screen");
    assert!(marker < path_line && path_line < alt, "{text:?}");

    // One file for the run, under the data directory, with the call
    // line, its duration, and the turn line.
    let call = log
        .lines()
        .find(|line| line.contains("model call") && line.contains("provider="))
        .unwrap_or_else(|| panic!("no call line in {log}"));
    assert!(call.contains("duration_ms="), "{call}");
    assert!(call.contains("stop=end_turn"), "{call}");
    let turn = log
        .lines()
        .find(|line| line.contains("turn completed"))
        .unwrap();
    assert!(turn.contains("duration_ms="), "{turn}");
    assert!(log.contains("session created"), "{log}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_plain_interface_names_the_log_file_after_the_marker() {
    let mock = MockServer::start(Vec::new(), Vec::new()).await;
    let mut pty = Pty::spawn_with_env(
        &mock.url(),
        &["--plain"],
        None,
        None,
        &[("DEMERZEL_LOG", "info")],
    );

    pty.wait_for(READY);
    pty.wait_for("log file:");
    // Ctrl-D reaches the line editor only once it reads: sent before
    // the prompt, the EOF is lost on Linux and the session stays.
    pty.wait_for(PROMPT);
    pty.send(CTRL_D);
    let raw = pty.wait_exit();

    let text = String::from_utf8_lossy(&raw).into_owned();
    let marker = text.find(READY).expect("ready marker");
    let path_line = text.find("log file:").expect("path line");
    assert!(marker < path_line, "{text:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_mode_previews_the_command_and_a_key_approves() {
    let mock = MockServer::start(
        vec![
            tool_turn("bash", json!({"command": "echo approved-ran"})),
            text_turn("finished"),
        ],
        Vec::new(),
    )
    .await;
    let mut pty = Pty::spawn(&mock.url(), &[]);

    pty.wait_for(PROMPT);
    pty.send(b"run it\r");
    // The dispatch preview shows the command; the question follows.
    pty.wait_for("$ echo approved-ran");
    pty.wait_for("approve bash? [y]es / [a]lways / [N]o");
    pty.send(b"y");
    pty.wait_for("approve bash? yes");
    pty.wait_for("finished");
    pty.send(CTRL_D);
    pty.wait_exit();

    // The tool ran: its output went back to the model.
    let second = mock.received()[1].body.to_string();
    assert!(second.contains("approved-ran"), "{second}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_edit_call_previews_as_a_diff_before_the_question() {
    let mock = MockServer::start(
        vec![
            tool_turn(
                "edit",
                json!({
                    "path": "demo.txt",
                    "tag": "t0",
                    "edits": [{"op": "replace", "start": 1, "end": 2, "content": "one\ntwo\n"}],
                }),
            ),
            text_turn("understood"),
        ],
        Vec::new(),
    )
    .await;
    let mut pty = Pty::spawn(&mock.url(), &[]);

    pty.wait_for(PROMPT);
    pty.send(b"change it\r");
    // The diff preview reaches the screen before the question; its
    // exact vocabulary is pinned by the preview unit tests.
    pty.wait_for("edit demo.txt");
    pty.wait_for("approve edit? [y]es / [a]lways / [N]o");
    pty.send(b"n");
    pty.wait_for("approve edit? no");
    pty.wait_for("understood");
    pty.send(CTRL_D);
    let raw = pty.wait_exit();

    // The question adds no second copy of the call: the "wants to
    // run" header appears only when no preview rendered.
    assert!(
        !contains_marker(&raw, "edit wants to run:"),
        "a covered call must not render twice"
    );
    // The declined call went back to the model as an error result.
    let second = mock.received()[1].body.to_string();
    assert!(second.contains("declined"), "{second}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_without_a_dispatch_preview_asks_with_its_input() {
    // An input shape the preview does not know: the question falls
    // back to the JSON input, so nothing is approved blind.
    let mock = MockServer::start(
        vec![
            tool_turn("bash", json!({"cmd": "mystery"})),
            text_turn("done"),
        ],
        Vec::new(),
    )
    .await;
    let mut pty = Pty::spawn(&mock.url(), &[]);

    pty.wait_for(PROMPT);
    pty.send(b"go\r");
    // The card shows the question's preview as its body.
    pty.wait_for("mystery");
    pty.wait_for("approve bash? [y]es / [a]lways / [N]o");
    pty.send(b"n");
    pty.wait_for("done");
    pty.send(CTRL_D);
    pty.wait_exit();
}

/// A file taller than a collapsed card's output tail.
fn tall_file() -> String {
    (1..=8).map(|i| format!("row {i} of the file\n")).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_cards_show_each_state_and_the_output_tail() {
    let mock = MockServer::start(
        vec![
            // One turn: a read, then a bash call, then text.
            tool_call("call_r", "read", json!({"path": "tall.txt"})),
            tool_call("call_b", "bash", json!({"command": "echo card-out"})),
            text_turn("first done"),
            // A declined call.
            tool_call("call_d", "bash", json!({"command": "echo never"})),
            text_turn("second done"),
            // A call the user interrupts.
            tool_call("call_s", "bash", json!({"command": "sleep 20"})),
            text_turn("recovered"),
        ],
        Vec::new(),
    )
    .await;
    let mut pty = Pty::spawn(&mock.url(), &[]);
    std::fs::write(pty.project().join("tall.txt"), tall_file()).unwrap();

    pty.wait_for(PROMPT);
    pty.send(b"look\r");
    // A card changes in place, and the frame diff writes only the
    // cells that change: a state glyph alone, a row that appears.
    // The waits below name rows that appear. The read runs without
    // a question; its card ends with the last five output lines and
    // a footer with the summary and the hint for the rest.
    pty.wait_for("row 8 of the file");
    pty.wait_for("4 more lines · Ctrl-O");
    // The bash call's preview joins its header; the card waits on
    // the question. A resize makes the next draw repaint the card
    // whole, so the pending glyph reaches the stream with its row;
    // the spinner rests at a question, so a key that answers
    // nothing triggers the draw.
    pty.wait_for("$ echo card-out");
    pty.wait_for("approve bash? [y]es / [a]lways / [N]o");
    pty.resize(100, 29);
    pty.send(b" ");
    pty.wait_for("? bash $ echo card-out");
    pty.send(b"y");
    pty.wait_for("card-out");
    pty.wait_for("approve bash? yes");
    pty.wait_for("first done");

    // The declined call keeps its card, with the state as its footer.
    pty.send(b"again\r");
    pty.wait_for("$ echo never");
    pty.wait_for("approve bash? [y]es / [a]lways / [N]o");
    pty.send(b"n");
    pty.wait_for("declined");
    pty.wait_for("approve bash? no");
    pty.wait_for("second done");

    // The interrupted call: approved, running, then Ctrl-C. A resize
    // makes the next draw repaint the whole screen, so the progress
    // line reaches the stream in full: the elapsed time and the
    // tool's name, with the prompt row and the bar under it.
    pty.send(b"wait\r");
    pty.wait_for("$ sleep 20");
    pty.wait_for("approve bash? [y]es / [a]lways / [N]o");
    pty.send(b"y");
    pty.wait_for("approve bash? yes");
    pty.resize(100, 30);
    pty.wait_for("· bash $ sleep 20");
    pty.wait_for("s · bash");
    pty.wait_for(PROMPT);
    pty.wait_for("─ ask ─");
    pty.send(CTRL_C);
    pty.wait_for("interrupted");
    pty.wait_for("[turn aborted]");
    pty.send(b"after\r");
    pty.wait_for("recovered");

    // The expand key shows the read's hidden lines; the card sits
    // above the viewport now, so a page up brings it back, and a
    // resize repaints the rows whole.
    pty.send(CTRL_O);
    pty.send(PAGE_UP);
    pty.resize(100, 29);
    pty.wait_for("row 1 of the file");
    pty.wait_for("row 3 of the file");
    pty.send(CTRL_D);
    let raw = pty.wait_exit();

    // The spinner drew: some frame of it reached the stream. The
    // state glyphs of the ended calls reached it too; only cards
    // write them.
    let text = String::from_utf8_lossy(&raw);
    assert!(
        text.chars()
            .any(|ch| ('\u{2800}'..='\u{28ff}').contains(&ch)),
        "no spinner frame in the output"
    );
    for glyph in ["✓", "⊘", "✗"] {
        assert!(
            contains_marker(&raw, glyph),
            "no {glyph} glyph in the output"
        );
    }
    // The declined call went back to the model as an error result;
    // the interrupted one went back as the repair for a call with
    // no result.
    let requests = mock.received();
    assert_eq!(requests.len(), 7);
    assert!(requests[4].body.to_string().contains("declined"));
    assert!(requests[6].body.to_string().contains("interrupted"));
}

#[tokio::test(flavor = "multi_thread")]
async fn slash_commands_switch_the_model_and_start_a_fresh_session() {
    let mock = MockServer::start(
        vec![text_turn("first reply"), text_turn("second reply")],
        Vec::new(),
    )
    .await;
    let mut pty = Pty::spawn(&mock.url(), &[]);

    pty.wait_for(PROMPT);
    pty.send(b"/help\r");
    pty.wait_for("/model [name]");
    pty.send(b"/model gpt-test\r");
    pty.wait_for("model: gpt-test");
    // A resize makes a later draw repaint the whole screen, so the
    // updated status bar reaches the stream in full. Where it lands
    // depends on frame timing, so the check runs on the whole output.
    pty.resize(90, 30);
    pty.send(b"hello\r");
    pty.wait_for("first reply");
    pty.send(b"/new\r");
    pty.wait_for("fresh session");
    pty.send(b"again\r");
    pty.wait_for("second reply");
    pty.send(CTRL_D);
    let raw = pty.wait_exit();
    assert!(
        contains_marker(&raw, "gpt-test ─ ask"),
        "the bar never showed the switched model"
    );

    // The switch reached the requests; the fresh session dropped the
    // first exchange from the second one.
    let requests = mock.received();
    assert_eq!(requests.len(), 2);
    let first = requests[0].body.to_string();
    let second = requests[1].body.to_string();
    assert!(first.contains("gpt-test"), "{first}");
    assert!(second.contains("gpt-test"), "{second}");
    assert!(second.contains("again"), "{second}");
    assert!(!second.contains("hello"), "{second}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_session_command_names_the_open_session_and_new_restarts_the_totals() {
    let mock = MockServer::start(vec![text_turn("named reply")], Vec::new()).await;
    let mut pty = Pty::spawn(&mock.url(), &[]);

    pty.wait_for(PROMPT);
    // Before the first prompt there is no session to name.
    pty.send(b"/session\r");
    pty.wait_for("no open session");
    pty.send(b"hello\r");
    pty.wait_for("named reply");
    // The session file exists now; /session names it and its file.
    let sessions = pty.session_files();
    assert_eq!(sessions.len(), 1);
    let id = sessions[0].clone();
    pty.send(b"/session\r");
    pty.wait_for(&format!("session: {id}"));
    pty.wait_for(&format!("{id}.jsonl"));
    // A fresh session restarts the totals: after the turn above the
    // status shows non-zero counts, so a later zero row is the
    // reset. A resize repaints the box's edge in full.
    pty.send(b"/new\r");
    pty.wait_for("fresh session");
    pty.resize(80, 30);
    pty.send(b"y");
    pty.wait_for("0 in · 0 out · 0 cached");
    pty.send(CTRL_C);
    pty.send(CTRL_D);
    pty.wait_exit();
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_c_aborts_a_turn_and_the_session_continues() {
    let mock = MockServer::start(
        vec![
            MockResponse::stream_then_stall(vec![text_delta("partial answer")]),
            text_turn("recovered"),
        ],
        Vec::new(),
    )
    .await;
    let mut pty = Pty::spawn(&mock.url(), &[]);

    pty.wait_for(PROMPT);
    pty.send(b"first\r");
    pty.wait_for("partial answer");
    pty.send(CTRL_C);
    pty.wait_for("[turn aborted]");
    pty.send(b"second\r");
    pty.wait_for("recovered");
    pty.send(CTRL_D);
    pty.wait_exit();
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_c_twice_on_an_empty_box_ends_the_session_with_the_exit_line() {
    let mock = MockServer::start(vec![text_turn("kept reply")], Vec::new()).await;
    let mut pty = Pty::spawn(&mock.url(), &[]);

    pty.wait_for(PROMPT);
    pty.send(b"hello\r");
    pty.wait_for("kept reply");
    pty.wait_for("2 cached ·");
    // A press on a box with text clears the box and asks nothing:
    // the repaint after the resize shows an empty box and the
    // segments, not the hint.
    pty.send(b"draft");
    pty.wait_for("draft");
    pty.send(CTRL_C);
    pty.resize(100, 29);
    pty.send(WHEEL_DOWN);
    pty.all_absent_until(&[QUIT_HINT, "draft"], "─ ask ─ effort");
    // On the empty box the first press asks, on the status; the
    // second ends the session.
    pty.send(CTRL_C);
    pty.wait_for(QUIT_HINT);
    let sessions = pty.session_files();
    pty.send(CTRL_C);
    let raw = pty.wait_exit();

    // The exit line follows the screen restore and names the open
    // session, which `-c` resumes.
    assert_eq!(sessions.len(), 1);
    let after = after_restore(&raw);
    assert!(
        after.contains(&format!(
            "session {} · resume with demerzel -c",
            sessions[0]
        )),
        "{after:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lone_ctrl_c_times_out_and_the_session_stays() {
    let mock = MockServer::start(vec![text_turn("still here")], Vec::new()).await;
    let mut pty = Pty::spawn(&mock.url(), &[]);

    pty.wait_for(PROMPT);
    pty.send(CTRL_C);
    pty.wait_for(QUIT_HINT);
    // Past the span the hint goes, and a press asks again instead of
    // ending the session.
    tokio::time::sleep(Duration::from_millis(3500)).await;
    pty.resize(100, 29);
    pty.send(WHEEL_DOWN);
    pty.absent_until(QUIT_HINT, "─ ask ─ effort");
    pty.send(CTRL_C);
    pty.wait_for(QUIT_HINT);
    // A typed key takes the question back, and the session goes on.
    pty.send(b"h");
    pty.resize(100, 30);
    pty.send(WHEEL_DOWN);
    pty.absent_until(QUIT_HINT, "─ ask ─ effort");
    pty.send(b"ello\r");
    pty.wait_for("still here");
    pty.wait_for("2 cached ·");
    pty.send(CTRL_D);
    let raw = pty.wait_exit();

    // Ctrl-D prints the exit line too.
    let after = after_restore(&raw);
    assert!(after.contains("resume with demerzel -c"), "{after:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pasted_line_takes_the_quit_question_back() {
    let mock = MockServer::start(Vec::new(), Vec::new()).await;
    let mut pty = Pty::spawn(&mock.url(), &[]);

    pty.wait_for(PROMPT);
    pty.send(CTRL_C);
    pty.wait_for(QUIT_HINT);
    // Text arrives by paste: the question goes with it. The press
    // after it clears the box, and the next press asks again — with
    // the question still standing, it would end the session.
    pty.send(b"\x1b[200~pasted\x1b[201~");
    pty.wait_for("pasted");
    pty.send(CTRL_C);
    pty.send(CTRL_C);
    pty.wait_for(QUIT_HINT);
    pty.send(CTRL_C);
    let raw = pty.wait_exit();
    assert!(!after_restore(&raw).contains("resume with"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_session_command_keeps_the_exit_line_for_the_opened_session() {
    let mock = MockServer::start(vec![text_turn("first reply")], Vec::new()).await;
    let mut pty = Pty::spawn(&mock.url(), &[]);

    pty.wait_for(PROMPT);
    pty.send(b"hello\r");
    pty.wait_for("first reply");
    pty.wait_for("2 cached ·");
    pty.send(b"/new\r");
    pty.wait_for("fresh session");
    let sessions = pty.session_files();
    pty.send(CTRL_C);
    pty.wait_for(QUIT_HINT);
    pty.send(CTRL_C);
    let raw = pty.wait_exit();

    // The run opened one session and dropped it; the line still
    // names it, and `-c` opens it.
    assert_eq!(sessions.len(), 1);
    let after = after_restore(&raw);
    assert!(
        after.contains(&format!(
            "session {} · resume with demerzel -c",
            sessions[0]
        )),
        "{after:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_session_id_stops_the_run_before_the_screen_opens() {
    let mock = MockServer::start(Vec::new(), Vec::new()).await;
    let id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let pty = Pty::spawn(&mock.url(), &["--session", id]);
    let (status, raw) = pty.wait_end();

    assert!(!status.success(), "exit status: {status:?}");
    let text = String::from_utf8_lossy(&raw);
    assert!(text.contains(id), "{text:?}");
    assert!(!text.contains(READY), "{text:?}");
    assert!(!text.contains(ENTER_ALT_SCREEN), "{text:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_with_no_session_quits_on_two_presses_and_prints_no_exit_line() {
    let mock = MockServer::start(Vec::new(), Vec::new()).await;
    let mut pty = Pty::spawn(&mock.url(), &[]);

    pty.wait_for(PROMPT);
    pty.send(CTRL_C);
    pty.wait_for(QUIT_HINT);
    pty.send(CTRL_C);
    let sessions = pty.session_files();
    let raw = pty.wait_exit();

    assert!(sessions.is_empty());
    let after = after_restore(&raw);
    assert!(!after.contains("resume with"), "{after:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_plain_flag_keeps_the_line_interface() {
    let mock = MockServer::start(vec![text_turn("plain reply")], Vec::new()).await;
    let mut pty = Pty::spawn(&mock.url(), &["--plain"]);

    pty.wait_for(READY);
    pty.wait_for(PROMPT);
    pty.send(b"hello\r");
    pty.wait_for("plain reply");
    // Wait for the next prompt: the line editor drops an EOF that
    // arrives before it re-enters raw mode.
    pty.wait_for(PROMPT);
    pty.send(CTRL_D);
    let raw = pty.wait_exit();

    let text = String::from_utf8_lossy(&raw);
    assert!(
        !text.contains(ENTER_ALT_SCREEN),
        "--plain must not switch screens"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_plain_interface_quits_on_two_presses_with_the_exit_line() {
    let mock = MockServer::start(vec![text_turn("plain reply")], Vec::new()).await;
    let mut pty = Pty::spawn(&mock.url(), &["--plain"]);

    pty.wait_for(PROMPT);
    pty.send(b"hello\r");
    pty.wait_for("plain reply");
    pty.wait_for(PROMPT);
    // The first press discards the typed line and asks, on stderr;
    // the second ends the session.
    pty.send(b"draft");
    pty.wait_for("draft");
    pty.send(CTRL_C);
    pty.wait_for(QUIT_HINT);
    pty.wait_for(PROMPT);
    let sessions = pty.session_files();
    pty.send(CTRL_C);
    let raw = pty.wait_exit();

    assert_eq!(sessions.len(), 1);
    let text = String::from_utf8_lossy(&strip_escapes(&raw)).into_owned();
    assert!(
        text.contains(&format!(
            "session {} · resume with demerzel -c",
            sessions[0]
        )),
        "{text:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_plain_interface_shares_the_command_registry() {
    let mock = MockServer::start(
        vec![text_turn("plain reply"), text_turn("fresh reply")],
        Vec::new(),
    )
    .await;
    let mut pty = Pty::spawn(&mock.url(), &["--plain"]);

    pty.wait_for(PROMPT);
    pty.send(b"/help\r");
    pty.wait_for("/model [name]");
    pty.wait_for(PROMPT);
    pty.send(b"/model gpt-test\r");
    pty.wait_for("model: gpt-test");
    pty.wait_for(PROMPT);
    pty.send(b"/effort low\r");
    pty.wait_for("effort: low");
    pty.wait_for(PROMPT);
    pty.send(b"/approval read-only\r");
    pty.wait_for("approval: read-only");
    pty.wait_for(PROMPT);
    pty.send(b"hello\r");
    pty.wait_for("plain reply");
    pty.wait_for(PROMPT);
    // With the engine open, the switches reach it in place.
    pty.send(b"/effort high\r");
    pty.wait_for("effort: high");
    pty.wait_for(PROMPT);
    pty.send(b"/approval ask\r");
    pty.wait_for("approval: ask");
    pty.wait_for(PROMPT);
    pty.send(b"/new\r");
    pty.wait_for("fresh session");
    pty.wait_for(PROMPT);
    pty.send(b"again\r");
    pty.wait_for("fresh reply");
    pty.wait_for(PROMPT);
    pty.send(CTRL_D);
    pty.wait_exit();

    // The switches happened before the session opened and while it
    // ran, and the fresh session dropped the first exchange.
    let requests = mock.received();
    let first = requests[0].body.to_string();
    let second = requests[1].body.to_string();
    assert!(first.contains("gpt-test"), "{first}");
    assert_eq!(requests[0].body["reasoning"]["effort"], "low");
    assert_eq!(tool_names(&requests[0].body), ["read", "grep", "glob"]);
    assert!(tool_names(&requests[1].body).contains(&"bash".to_string()));
    assert_eq!(requests[1].body["reasoning"]["effort"], "high");
    assert!(second.contains("again"), "{second}");
    assert!(!second.contains("hello"), "{second}");
}

/// A reply taller than the 30-row screen.
fn long_reply() -> String {
    (0..60)
        .map(|i| format!("row {i:02} of the long reply\n"))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_wheel_browses_the_transcript_and_end_follows_again() {
    let mock = MockServer::start(vec![text_turn(&long_reply())], Vec::new()).await;
    // Capture is on by default.
    let mut pty = Pty::spawn(&mock.url(), &[]);

    pty.wait_for(PROMPT);
    pty.send(b"long\r");
    pty.wait_for("row 59 of the long reply");
    // The usage notice is the last thing the turn draws.
    pty.wait_for("2 cached");
    // A resize makes the next draw repaint the whole screen, so the
    // browsed window and the indicator reach the stream in full. The
    // height changes, not the width: a width change returns the view
    // to follow mode.
    pty.resize(100, 29);
    pty.send(WHEEL_UP);
    pty.send(WHEEL_UP);
    // Two notches of three rows: the window moved up six rows past
    // the usage notice and the last five reply rows. The resize
    // lands in the same frame: the pane loses one row and the top
    // row holds, so the scroll deepens to seven. A 25-row pane —
    // 29 rows less the box and its frame — opens on row 29; the
    // indicator counts the seven below.
    pty.wait_for("row 29 of the long reply");
    pty.wait_for("7 below");
    // A click on the indicator, at the right of the pane's last
    // row, returns to the bottom. The frame diff rewrites only the
    // digits that changed, so a resize forces the full repaint the
    // assertion reads.
    pty.send(&mouse_press(95, 25));
    pty.send(&mouse_release(95, 25));
    pty.resize(100, 30);
    pty.wait_for("row 59 of the long reply");
    // Up again; End returns to the bottom of the transcript.
    pty.send(WHEEL_UP);
    pty.wait_for("3 below");
    pty.send(END);
    pty.resize(100, 29);
    pty.send(b"x");
    pty.wait_for("row 59 of the long reply");
    pty.send(CTRL_C);
    pty.send(CTRL_D);
    let raw = pty.wait_exit();

    let text = String::from_utf8_lossy(&raw);
    assert!(text.contains(MOUSE_CAPTURE), "capture is on by default");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_mouse_key_turns_capture_off_and_a_project_file_cannot_turn_it_on() {
    let mock = MockServer::start(vec![text_turn("no mouse")], Vec::new()).await;
    let mut pty = Pty::spawn_with_configs(
        &mock.url(),
        &[],
        Some("[tui]\nmouse = true\n"),
        Some("[tui]\nmouse = false\n"),
    );

    pty.wait_for(PROMPT);
    pty.send(b"hello\r");
    pty.wait_for("no mouse");
    pty.send(CTRL_D);
    let raw = pty.wait_exit();

    let text = String::from_utf8_lossy(&raw);
    assert!(!text.contains(MOUSE_CAPTURE), "capture must stay off");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_drag_selects_transcript_text_and_the_release_copies_it() {
    let mock = MockServer::start(
        vec![text_turn("Done.\n\n```rust\nfn main() {}\n```\n")],
        Vec::new(),
    )
    .await;
    let mut pty = Pty::spawn(&mock.url(), &[]);

    pty.wait_for(PROMPT);
    pty.send(b"hello\r");
    pty.wait_for("fn main() {}");
    // The usage notice is the last thing the turn draws.
    pty.wait_for("2 cached");
    // The screen, 1-based rows: the banner, the echo, then the reply
    // on rows 3 to 7 — "Done.", a gap, the fence frame, the code, the
    // frame's end. The press lands on "Done." and the drag runs to
    // the frame's end. The frame repaints the selected cells, so
    // the selected text reaches the stream again.
    pty.send(&mouse_press(1, 3));
    pty.send(&mouse_drag(6, 7));
    pty.wait_for("fn main() {}");
    pty.send(&mouse_release(6, 7));
    // The copy: the rows between the points, without the fence
    // frame, as one OSC 52 write.
    let expected = format!("\x1b]52;c;{}\x07", base64("Done.\n\nfn main() {}"));
    let deadline = Instant::now() + Duration::from_secs(15);
    while !String::from_utf8_lossy(&pty.seen).contains(&expected) {
        assert!(
            Instant::now() < deadline,
            "no OSC 52 write; raw: {:?}",
            String::from_utf8_lossy(&pty.seen)
        );
        pty.poll_read();
    }
    pty.send(CTRL_D);
    pty.wait_exit();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_long_code_fence_can_be_browsed_and_its_diagram_copied() {
    let diagram = concat!(
        "+------------+      +--------------+\n",
        "|  terminal  | ---> |    engine    |\n",
        "+------------+      +--------------+\n",
        "                           |\n",
        "                           v\n",
        "                    +--------------+\n",
        "                    |    tools     |\n",
        "                    +--------------+",
    );
    // The diagram starts a fence taller than the screen. It must be
    // reached by browsing before a drag can copy it.
    let details: String = (0..60)
        .map(|row| format!("detail {row:02}: messages travel through the engine\n"))
        .collect();
    let reply = format!("```text\n{diagram}\n{details}```\n");
    let mock = MockServer::start(vec![text_turn(&reply)], Vec::new()).await;
    let mut pty = Pty::spawn(&mock.url(), &[]);

    pty.wait_for(PROMPT);
    pty.send(b"diagram\r");
    pty.wait_for("detail 59");
    pty.wait_for("2 cached");
    // Browse to the start. A height change forces a repaint, so the
    // wait reads the diagram from the browsed view, not an old frame.
    pty.send(&PAGE_UP.repeat(4));
    pty.resize(100, 29);
    pty.send(WHEEL_UP);
    pty.wait_for("╭─ text");
    pty.wait_for("|  terminal  | ---> |    engine    |");
    pty.wait_for("|    tools     |");

    // The banner and prompt occupy two rows; the fence frame is row
    // three. Select from that frame through the diagram's eighth row.
    // The OSC 52 payload must retain the diagram's spaces and lines
    // while dropping the fence frame and its per-row prefix.
    pty.send(&mouse_press(1, 3));
    pty.send(&mouse_drag(100, 11));
    pty.send(&mouse_release(100, 11));
    let expected = format!("\x1b]52;c;{}\x07", base64(diagram));
    let deadline = Instant::now() + Duration::from_secs(15);
    while !String::from_utf8_lossy(&pty.seen).contains(&expected) {
        assert!(
            Instant::now() < deadline,
            "no diagram OSC 52 write; raw: {:?}",
            String::from_utf8_lossy(&pty.seen)
        );
        pty.poll_read();
    }
    pty.send(END);
    pty.resize(100, 30);
    pty.send(b"x");
    pty.wait_for("detail 59");
    pty.send(CTRL_C);
    pty.send(CTRL_D);
    pty.wait_exit();
}

/// A reply with one of each markdown element the renderer formats.
const FORMATTED_REPLY: &str = "# Title\n\nSome prose.\n\n- first item\n- second item\n\n\
                               | name | n |\n|---|---|\n| ab | 1 |\n\n\
                               ```rust\nfn main() {}\n```\n";

#[tokio::test(flavor = "multi_thread")]
async fn a_reply_renders_as_markdown() {
    let mock = MockServer::start(vec![text_turn(FORMATTED_REPLY)], Vec::new()).await;
    let mut pty = Pty::spawn(&mock.url(), &[]);

    pty.wait_for(PROMPT);
    pty.send(b"show\r");
    // The usage notice is the last thing the turn draws. A resize
    // makes the next draw repaint the whole screen, so every row
    // reaches the stream in order; the key is that draw.
    pty.wait_for("2 cached");
    pty.resize(100, 29);
    pty.send(b"x");
    pty.wait_for("# Title");
    pty.wait_for("• first item");
    pty.wait_for("• second item");
    pty.wait_for("name │ n");
    pty.wait_for("ab │ 1");
    pty.wait_for("╭─ rust");
    pty.wait_for("│ fn main() {}");
    pty.wait_for("╰─");
    pty.send(CTRL_C);
    pty.send(CTRL_D);
    let raw = pty.wait_exit();
    // The fence is highlighted: the keyword `fn` ends where a style
    // change starts. Which style is the theme's business.
    assert!(
        raw.windows(3).any(|bytes| bytes == b"fn\x1b"),
        "no style change after the keyword"
    );
}

const SUMMARY: &str = "**Plan** the fix\nthen check the second line";

#[tokio::test(flavor = "multi_thread")]
async fn a_thinking_block_closes_to_one_line_and_ctrl_t_opens_it() {
    let mock = MockServer::start(vec![reasoning_turn(SUMMARY, "done")], Vec::new()).await;
    let mut pty = Pty::spawn(&mock.url(), &[]);

    pty.wait_for(PROMPT);
    pty.send(b"think\r");
    pty.wait_for("2 cached");
    pty.resize(100, 29);
    pty.send(b"x");
    // Closed: the state, then the first line with its marks rendered
    // and the rest behind the mark.
    pty.wait_for("▸ thought for");
    pty.wait_for("· Plan the fix…");
    pty.wait_for("done");
    // Open: the rows the block gains appear; the header changes in
    // place, so only its glyph reaches the stream.
    pty.send(CTRL_T);
    pty.wait_for("▾");
    pty.wait_for("then check the second line");
    // Closed again: after a repaint the screen holds one line of it.
    pty.send(CTRL_T);
    pty.resize(100, 28);
    pty.send(b"y");
    pty.absent_until("second line", "❯ xy");
    pty.send(CTRL_C);
    pty.send(CTRL_D);
    pty.wait_exit();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_thinking_config_key_opens_the_blocks_or_hides_them() {
    let mock = MockServer::start(vec![reasoning_turn(SUMMARY, "done")], Vec::new()).await;
    let mut pty =
        Pty::spawn_with_project_config(&mock.url(), &[], Some("[tui]\nthinking = \"open\"\n"));
    pty.wait_for(PROMPT);
    pty.send(b"think\r");
    pty.wait_for("2 cached");
    pty.resize(100, 29);
    pty.send(b"x");
    pty.wait_for("▾ thought for");
    pty.wait_for("Plan the fix");
    pty.wait_for("then check the second line");
    pty.wait_for("done");
    pty.send(CTRL_C);
    pty.send(CTRL_D);
    pty.wait_exit();

    let mock = MockServer::start(vec![reasoning_turn(SUMMARY, "done")], Vec::new()).await;
    let mut pty =
        Pty::spawn_with_project_config(&mock.url(), &[], Some("[tui]\nthinking = \"hidden\"\n"));
    pty.wait_for(PROMPT);
    pty.send(b"think\r");
    pty.wait_for("2 cached");
    pty.resize(100, 29);
    pty.send(b"x");
    pty.absent_until("thought", "❯ x");
    pty.send(CTRL_C);
    pty.send(CTRL_D);
    pty.wait_exit();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_markdown_config_key_shows_the_raw_text() {
    let mock = MockServer::start(vec![text_turn(FORMATTED_REPLY)], Vec::new()).await;
    let mut pty =
        Pty::spawn_with_project_config(&mock.url(), &[], Some("[tui]\nmarkdown = false\n"));

    pty.wait_for(PROMPT);
    pty.send(b"show\r");
    pty.wait_for("2 cached");
    pty.resize(100, 29);
    pty.send(b"x");
    pty.wait_for("# Title");
    pty.wait_for("- first item");
    pty.wait_for("| name | n |");
    pty.wait_for("```rust");
    pty.send(CTRL_C);
    pty.send(CTRL_D);
    pty.wait_exit();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_effort_flag_and_command_reach_the_requests() {
    let mock = MockServer::start(
        vec![text_turn("low reply"), text_turn("high reply")],
        Vec::new(),
    )
    .await;
    let mut pty = Pty::spawn(&mock.url(), &["--effort", "low"]);

    pty.wait_for(PROMPT);
    pty.send(b"/effort\r");
    pty.wait_for("effort: low");
    pty.send(b"one\r");
    pty.wait_for("low reply");
    pty.send(b"/effort high\r");
    pty.wait_for("effort: high");
    // The status names the new level; a resize repaints the edge.
    pty.resize(100, 29);
    pty.send(b"x");
    pty.wait_for("─ effort high ─");
    pty.send(CTRL_C);
    pty.send(b"two\r");
    pty.wait_for("high reply");
    pty.send(CTRL_D);
    pty.wait_exit();

    let requests = mock.received();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body["reasoning"]["effort"], "low");
    assert_eq!(requests[1].body["reasoning"]["effort"], "high");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_status_names_the_project_and_follows_the_branch_between_turns() {
    let mock = MockServer::start(
        vec![text_turn("one reply"), text_turn("two reply")],
        Vec::new(),
    )
    .await;
    let mut pty = Pty::spawn_in_repo(
        &mock.url(),
        "status-repo",
        "ref: refs/heads/feature/status\n",
    );

    // The boot read: the project directory's name and the branch,
    // between the effort and the totals.
    pty.wait_for(PROMPT);
    pty.wait_for("─ status-repo ─ feature/status ─ 0 in");
    // A detached head between turns shows its short hash at the next
    // turn's start; a resize repaints the edge in full.
    let git = pty.project().join(".git");
    std::fs::write(
        git.join("HEAD"),
        "0123456789abcdef0123456789abcdef01234567\n",
    )
    .unwrap();
    pty.send(b"one\r");
    pty.wait_for("one reply");
    pty.wait_for("2 cached");
    pty.resize(100, 29);
    pty.send(b"x");
    pty.wait_for("─ status-repo ─ 0123456 ─");
    // No repository: the branch segment goes, the project stays.
    std::fs::remove_dir_all(&git).unwrap();
    pty.send(CTRL_C);
    pty.send(b"two\r");
    pty.wait_for("two reply");
    pty.wait_for("2 cached");
    pty.resize(100, 30);
    pty.send(b"x");
    pty.wait_for("─ status-repo ─");
    pty.absent_until("0123456", "cached");
    pty.send(CTRL_C);
    pty.send(CTRL_D);
    pty.wait_exit();
}

/// The context on the status: the last call's input tokens and the
/// share of the model's window from the provider catalog. The next
/// call replaces the count, a token-limited call included; a new
/// session clears it.
#[tokio::test(flavor = "multi_thread")]
async fn the_status_counts_the_context_and_its_share_of_the_window() {
    let mock = MockServer::start(
        vec![
            MockResponse::stream(vec![text_delta("first"), completed_with_input(46_240)]),
            MockResponse::stream(vec![text_delta("second"), incomplete_with_input(101_003)]),
        ],
        Vec::new(),
    )
    .await;
    let mut pty = Pty::spawn(&mock.url(), &[]);
    pty.wait_for(PROMPT);
    // The default model's window is in the catalog: 46,240 of
    // 272,000 reads 17%.
    pty.send(b"one\r");
    pty.wait_for("first");
    pty.wait_for("context 46.2k · 17% ─");
    // The context is one prompt, not a sum: the next call replaces
    // the count — a call the endpoint cut at a token limit included,
    // where a stale reading misleads most.
    pty.send(b"two\r");
    pty.wait_for("second");
    pty.wait_for("101003 in");
    pty.wait_for("hit a token limit");
    // A resize repaints the edge in full: the incremental redraw
    // sends changed cells alone.
    pty.resize(100, 29);
    pty.send(b"x");
    pty.wait_for("context 101k · 37% ─");
    pty.send(CTRL_C);
    // A fresh session has no context yet: no segment before the
    // zeroed totals.
    pty.send(b"/new\r");
    pty.wait_for("fresh session");
    pty.resize(100, 30);
    pty.send(b"x");
    // `─ context` is the segment; the token-limit notice on the
    // repainted transcript says "context window" and must not match.
    pty.absent_until("─ context", "0 in · 0 out · 0 cached ─");
    pty.send(CTRL_C);
    pty.send(CTRL_D);
    pty.wait_exit();
}

/// A model in no catalog shows the count alone; a switch to a
/// cataloged model brings the share with no new call, a switch back
/// takes it away, and a set `context_window` key wins over the
/// catalog.
#[tokio::test(flavor = "multi_thread")]
async fn a_window_comes_from_the_catalog_or_the_context_window_key() {
    let mock = MockServer::start(
        vec![MockResponse::stream(vec![
            text_delta("first"),
            completed_with_input(46_240),
        ])],
        Vec::new(),
    )
    .await;
    let mut pty = Pty::spawn(&mock.url(), &["--model", "plain-model"]);
    pty.wait_for(PROMPT);
    pty.send(b"one\r");
    pty.wait_for("first");
    pty.wait_for("context 46.2k ─");
    // The window follows the model: the catalog names the new one.
    pty.send(b"/model gpt-5.6-sol\r");
    pty.wait_for("model: gpt-5.6-sol");
    pty.resize(100, 29);
    pty.send(b"x");
    pty.wait_for("context 46.2k · 17% ─");
    pty.send(CTRL_C);
    // Back to the unlisted model: the share goes, the count stays.
    pty.send(b"/model plain-model\r");
    pty.wait_for("model: plain-model");
    pty.resize(100, 30);
    pty.send(b"x");
    pty.wait_for("context 46.2k ─");
    pty.send(CTRL_C);
    pty.send(CTRL_D);
    pty.wait_exit();

    let mock = MockServer::start(
        vec![MockResponse::stream(vec![
            text_delta("first"),
            completed_with_input(46_240),
        ])],
        Vec::new(),
    )
    .await;
    // The key wins over the catalog: the default model's row says
    // 272,000, the key says 100,000, and 46,240 reads 46%.
    let mut pty = Pty::spawn_with_configs(
        &mock.url(),
        &[],
        None,
        Some("[providers.openai-codex]\ncontext_window = 100000\n"),
    );
    pty.wait_for(PROMPT);
    pty.send(b"one\r");
    pty.wait_for("first");
    pty.wait_for("context 46.2k · 46% ─");
    pty.send(CTRL_D);
    pty.wait_exit();
}

/// The tool names one request advertised.
fn tool_names(request: &serde_json::Value) -> Vec<String> {
    request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_approval_command_and_shift_tab_set_the_mode_for_the_turns_that_follow() {
    let mock = MockServer::start(
        vec![
            tool_turn("bash", json!({"command": "echo auto-ran"})),
            text_turn("auto done"),
            text_turn("read-only done"),
            tool_turn("bash", json!({"command": "echo asked"})),
            text_turn("ask done"),
        ],
        Vec::new(),
    )
    .await;
    let mut pty = Pty::spawn(&mock.url(), &[]);

    pty.wait_for(PROMPT);
    pty.send(b"/approval\r");
    pty.wait_for("approval: ask");
    // Auto: the bash call runs with no question.
    pty.send(b"/approval auto\r");
    pty.wait_for("approval: auto");
    pty.send(b"run it\r");
    pty.absent_until("approve bash?", "auto done");
    pty.wait_for("2 cached");
    // Read-only: the next request carries the read tools alone.
    pty.send(b"/approval read-only\r");
    pty.wait_for("approval: read-only");
    pty.resize(100, 29);
    pty.send(b"x");
    pty.wait_for("─ read-only ─ effort");
    pty.send(CTRL_C);
    pty.send(b"look\r");
    pty.wait_for("read-only done");
    pty.wait_for("2 cached");
    // Ask again: a question shows, and the status names the mode.
    pty.send(b"/approval ask\r");
    pty.wait_for("approval: ask");
    pty.resize(100, 30);
    pty.send(b"x");
    pty.wait_for("─ ask ─ effort");
    pty.send(CTRL_C);
    pty.send(b"run again\r");
    pty.wait_for("approve bash? [y]es / [a]lways / [N]o");
    pty.send(b"y");
    pty.wait_for("ask done");
    pty.wait_for("2 cached");
    // Shift-Tab cycles from ask to auto, with no prompt echo: the
    // note shows and no `/approval auto` line precedes it.
    pty.send(SHIFT_TAB);
    pty.absent_until("❯ /approval auto", "approval: auto");
    pty.resize(100, 29);
    pty.send(b"x");
    pty.wait_for("─ auto ─ effort");
    pty.send(CTRL_C);
    let sessions = pty.session_files();
    pty.send(CTRL_D);
    pty.wait_exit();

    // One session took every switch in place.
    assert_eq!(sessions.len(), 1);
    let requests = mock.received();
    assert_eq!(requests.len(), 5);
    assert!(tool_names(&requests[0].body).contains(&"bash".to_string()));
    assert_eq!(tool_names(&requests[2].body), ["read", "grep", "glob"]);
    assert!(tool_names(&requests[3].body).contains(&"bash".to_string()));
    // The tool ran under auto and under ask. The engine test covers
    // the meta records a switch writes.
    assert!(requests[1].body.to_string().contains("auto-ran"));
    assert!(requests[4].body.to_string().contains("asked"));
    // The tools block is the same bytes once ask is back: the
    // provider's cached prefix holds.
    assert_eq!(requests[0].body["tools"], requests[3].body["tools"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_editor_takes_several_lines_and_completes_a_command() {
    let mock = MockServer::start(vec![text_turn("four rows seen")], Vec::new()).await;
    let mut pty = Pty::spawn(&mock.url(), &[]);

    pty.wait_for(PROMPT);
    // Alt-Enter and Shift-Enter each add a row; a paste keeps its
    // newlines. Each row reaches the screen as it grows the box.
    pty.send(b"first-row");
    pty.send(ALT_ENTER);
    pty.send(b"second-row");
    pty.send(SHIFT_ENTER);
    pty.send(b"\x1b[200~third-row\nfourth-row\x1b[201~");
    pty.wait_for("first-row");
    pty.wait_for("second-row");
    pty.wait_for("third-row");
    pty.wait_for("fourth-row");
    pty.send(b"\r");
    pty.wait_for("four rows seen");

    // A command completes from the popup: the prefix lists it with
    // its usage and help, Enter puts it on the line, Enter again
    // submits it. The prefix alone would answer "unknown command".
    pty.send(b"/ef");
    pty.wait_for("/effort [level]");
    pty.wait_for("show or set the reasoning effort");
    pty.send(b"\r");
    pty.send(b"\r");
    pty.wait_for("effort: high");
    // Up recalls the submitted /effort without the popup, so the
    // next Up keeps browsing: the four-row prompt comes back. The
    // resize lands a repaint between the two keys, so the check
    // covers the whole screen.
    pty.send(b"\x1b[A");
    pty.resize(100, 29);
    pty.absent_until("show or set the reasoning effort", "─ ask ─");
    pty.send(b"\x1b[A");
    pty.wait_for("fourth-row");
    pty.send(CTRL_C);
    pty.resize(100, 30);
    // Shift-Enter with the popup open stays a newline: the line
    // submits as typed, not as the completed /effort.
    pty.send(b"/e");
    pty.wait_for("/exit");
    pty.send(SHIFT_ENTER);
    pty.send(b"row-two");
    pty.wait_for("row-two");
    pty.send(b"\r");
    pty.wait_for("unknown command: /e");
    // Esc closes the popup: a repaint shows no popup row, and Enter
    // then submits the line as typed, which no command answers. The
    // resize between Esc and Enter keeps the two keys out of one
    // read, where an Esc before a byte reads as Alt.
    pty.send(b"/ne");
    pty.wait_for("start a fresh session at the next prompt");
    pty.send(b"\x1b");
    pty.resize(100, 29);
    pty.absent_until("start a fresh session", "─ ask ─");
    pty.send(b"\r");
    pty.wait_for("unknown command: /ne");
    // Tab moves the selection: `/e` lists /effort first, then /exit.
    // The completed /exit ends the session; /effort would not.
    pty.send(b"/e");
    pty.wait_for("/exit");
    pty.send(b"\t");
    pty.send(b"\r");
    pty.send(b"\r");
    let raw = pty.wait_exit();

    let text = String::from_utf8_lossy(&raw);
    assert!(
        text.contains(KEYBOARD_PUSH),
        "the keyboard-enhancement flags were not pushed"
    );
    // The prompt reached the model with its four rows as lines.
    let body = mock.received()[0].body.to_string();
    assert!(
        body.contains("first-row\\nsecond-row\\nthird-row\\nfourth-row"),
        "{body}"
    );
}

/// A reply with a code fence, so the terminal theme's palette
/// entries — the fence's cyan — reach the stream.
const THEMED_REPLY: &str = "Done.\n\n```rust\nfn main() {}\n```\n";

/// One session over `configs` and `env`; returns the raw stream.
async fn themed_session(
    project_config: Option<&str>,
    user_config: Option<&str>,
    env: &[(&str, &str)],
) -> Vec<u8> {
    let mock = MockServer::start(vec![text_turn(THEMED_REPLY)], Vec::new()).await;
    let mut pty = Pty::spawn_with_env(&mock.url(), &[], project_config, user_config, env);
    pty.wait_for(PROMPT);
    pty.send(b"paint\r");
    pty.wait_for("2 cached");
    pty.resize(100, 29);
    pty.send(b"x");
    pty.wait_for("fn main");
    pty.send(CTRL_C);
    pty.send(CTRL_D);
    pty.wait_exit()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_terminal_theme_names_palette_entries_only() {
    // The backend writes a palette entry as `38;5;n`, n under 16.
    let raw = themed_session(None, None, &[("COLORTERM", "truecolor")]).await;
    let params = color_params(&raw);
    assert!(
        has_params(&params, &[38, 5, 6]),
        "the fence takes the palette's cyan: {params:?}"
    );
    assert!(!has_truecolor(&params), "no truecolor: {params:?}");
    let own = params
        .iter()
        .flat_map(|p| p.windows(3))
        .filter(|w| w[0] == 38 && w[1] == 5)
        .all(|w| w[2] < 16);
    assert!(own, "no entry past the terminal's own: {params:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_dark_theme_paints_truecolor_when_the_terminal_declares_it() {
    let raw = themed_session(
        None,
        Some("[tui]\ntheme = \"dark\"\n"),
        &[("COLORTERM", "truecolor")],
    )
    .await;
    let params = color_params(&raw);
    assert!(
        has_params(&params, &[38, 2, 0xd0, 0xd8, 0xe4]),
        "the text role in truecolor: {params:?}"
    );
    // The box's edge, with the status on it, takes the dim role; a
    // framed screen draws no bar, so the bar's ground never paints.
    assert!(
        has_params(&params, &[38, 2, 0x7b, 0x84, 0x94]),
        "the dim role in truecolor: {params:?}"
    );
    assert!(
        !has_params(&params, &[48, 2, 0x36, 0x3e, 0x4e]),
        "no bar ground on a framed screen: {params:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_light_theme_fits_a_256_color_terminal() {
    // TERM=xterm-256color and no COLORTERM: every RGB value lands on
    // a palette index.
    let raw = themed_session(Some("[tui]\ntheme = \"light\"\n"), None, &[]).await;
    let params = color_params(&raw);
    let past_sixteen = params
        .iter()
        .flat_map(|p| p.windows(3))
        .any(|w| (w[0] == 38 || w[0] == 48) && w[1] == 5 && w[2] >= 16);
    assert!(past_sixteen, "palette entries past sixteen: {params:?}");
    assert!(!has_truecolor(&params), "no truecolor: {params:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_config_defined_theme_reaches_the_screen() {
    // The user file defines the theme; the project file picks it.
    // The project file's own table is ignored: a repository cannot
    // paint the approval question away.
    let raw = themed_session(
        Some("[tui]\ntheme = \"ocean\"\n[theme.ocean]\ncode_block = \"#010203\"\n"),
        Some("[theme.ocean]\ncode_block = \"#5fafd7\"\nkeyword = \"#040506\"\n"),
        &[("COLORTERM", "truecolor")],
    )
    .await;
    let params = color_params(&raw);
    assert!(has_params(&params, &[38, 2, 95, 175, 215]), "{params:?}");
    assert!(has_params(&params, &[38, 2, 4, 5, 6]), "{params:?}");
    assert!(!has_params(&params, &[38, 2, 1, 2, 3]), "{params:?}");
    assert!(
        String::from_utf8_lossy(&raw).contains("ignored: the project file defines theme `ocean`"),
        "the warning reaches stderr"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn no_color_keeps_every_color_off_the_screen() {
    let raw = themed_session(
        None,
        Some("[tui]\ntheme = \"dark\"\n"),
        &[("COLORTERM", "truecolor"), ("NO_COLOR", "1")],
    )
    .await;
    let params = color_params(&raw);
    assert!(params.is_empty(), "{params:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_theme_name_with_no_theme_stops_the_run() {
    let mock = MockServer::start(Vec::new(), Vec::new()).await;
    let mut pty =
        Pty::spawn_with_project_config(&mock.url(), &[], Some("[tui]\ntheme = \"ocean\"\n"));
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        pty.poll_read();
        if let Some(status) = pty.child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "the run did not stop");
    };
    // Collect what the exit path wrote after the last poll; the
    // status can land before the tail crosses the PTY.
    pty.poll_read();
    assert!(!status.success());
    let text = String::from_utf8_lossy(&pty.seen);
    assert!(
        text.contains("`tui.theme`") && text.contains("ocean"),
        "{text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resumed_session_replays_its_turns_as_blocks_before_the_notice() {
    let mock = MockServer::start(
        vec![
            // The first run: a reasoning turn, then a turn with a
            // read call and a reply. The second run: one reply.
            reasoning_turn("Plan the fix", "first reply"),
            tool_call("call_r", "read", json!({"path": "tall.txt"})),
            text_turn("read it"),
            text_turn("third reply"),
        ],
        Vec::new(),
    )
    .await;
    let mut pty = Pty::spawn(&mock.url(), &[]);
    std::fs::write(pty.project().join("tall.txt"), tall_file()).unwrap();
    pty.wait_for(PROMPT);
    pty.send(b"hello\r");
    pty.wait_for("first reply");
    pty.wait_for("2 cached ·");
    pty.send(b"look\r");
    pty.wait_for("row 8 of the file");
    pty.wait_for("read it");
    pty.wait_for("2 cached ·");
    let sessions = pty.session_files();
    assert_eq!(sessions.len(), 1);
    pty.send(CTRL_D);

    let mut pty = pty.relaunch(&mock.url(), &["-c"]);
    pty.wait_for(PROMPT);
    // A taller window, so the whole replay fits above the box.
    pty.resize(120, 40);
    pty.send(b"again\r");
    // The past turns land as blocks, in order — the prompt line,
    // the closed thought, the reply, its usage line, the second
    // prompt, the finished card with its output tail, the reply —
    // then the notice, then the new turn's reply.
    pty.wait_for("❯ hello");
    pty.wait_for("▸ thought · Plan the fix");
    pty.wait_for("first reply");
    pty.wait_for("2 cached ·");
    pty.wait_for("❯ look");
    pty.wait_for("✓ read");
    pty.wait_for("row 8 of the file");
    pty.wait_for("read it");
    pty.wait_for(&format!("continuing session {} (6 messages)", sessions[0]));
    pty.wait_for("❯ again");
    pty.wait_for("third reply");
    pty.send(CTRL_D);
    pty.wait_exit();

    // The resumed run sent the history back to the model.
    let requests = mock.received();
    assert_eq!(requests.len(), 4);
    let last = requests[3].body.to_string();
    assert!(
        last.contains("first reply") && last.contains("again"),
        "{last}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resumed_file_over_the_cap_names_the_messages_it_leaves_out() {
    let mock = MockServer::start(vec![text_turn("after the cap")], Vec::new()).await;
    let mut pty = Pty::spawn(&mock.url(), &["-c"]);
    // A session file with two messages more than the replay shows,
    // written through the store the binary opens at the first
    // prompt. The boot reads no session file, so the file can land
    // after the screen opens.
    let store = SessionStore::at(pty.data_dir());
    let mut session = store
        .create(
            &pty.project(),
            "openai-codex",
            "gpt-test",
            None,
            ApprovalMode::Ask,
        )
        .unwrap();
    for index in 0..202 {
        let role = if index % 2 == 0 {
            Role::User
        } else {
            Role::Assistant
        };
        session
            .append_message(&Message {
                role,
                content: vec![ContentBlock::Text {
                    text: format!("message {index}"),
                }],
            })
            .unwrap();
    }
    drop(session);

    pty.wait_for(PROMPT);
    pty.resize(120, 40);
    pty.send(b"more\r");
    pty.wait_for("message 201");
    pty.wait_for("continuing session");
    pty.wait_for("(202 messages)");
    pty.wait_for("after the cap");
    // The dim line sits at the top of the transcript: page up to it.
    // A height change repaints the rows whole and holds the top row,
    // so the first replayed messages reach the stream in full.
    for _ in 0..8 {
        pty.send(PAGE_UP);
    }
    pty.wait_for("2 earlier messages left out");
    pty.resize(120, 39);
    pty.send(b"x");
    pty.wait_for("2 earlier messages left out");
    pty.wait_for("❯ message 2");
    pty.wait_for("message 3");
    pty.send(END);
    // The wake key sits in the box: a press clears it, then Ctrl-D
    // on the empty box ends the session.
    pty.send(CTRL_C);
    pty.send(CTRL_D);
    pty.wait_exit();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_plain_interface_prints_the_resume_notice_alone() {
    let mock = MockServer::start(
        vec![text_turn("plain reply"), text_turn("plain again")],
        Vec::new(),
    )
    .await;
    let mut pty = Pty::spawn(&mock.url(), &["--plain"]);
    pty.wait_for(PROMPT);
    pty.send(b"hello\r");
    pty.wait_for("plain reply");
    pty.wait_for(PROMPT);
    pty.send(CTRL_D);

    let mut pty = pty.relaunch(&mock.url(), &["--plain", "-c"]);
    pty.wait_for(PROMPT);
    pty.send(b"more\r");
    // The notice alone: no replayed prompt, no replayed reply.
    pty.all_absent_until(&["hello", "plain reply"], "continuing session");
    pty.wait_for("(2 messages)");
    pty.wait_for("plain again");
    pty.wait_for(PROMPT);
    pty.send(CTRL_D);
    pty.wait_exit();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_without_a_login_shows_the_first_run_screen_until_a_login_lands() {
    let mock = MockServer::start(vec![text_turn("logged in now")], Vec::new()).await;
    let mut pty = Pty::spawn_without_login(&mock.url(), &[]);

    // The screen before the prompt: the wordmark's first row, the
    // state per provider in the doctor's words, and the login
    // commands, the session's provider first.
    pty.wait_for(READY);
    pty.wait_for("┌┬┐┌─┐┌┬┐");
    pty.wait_for("openai-codex, the session's provider");
    pty.wait_for("openai-codex: none");
    pty.wait_for("anthropic: none");
    pty.wait_for("dmz login the ChatGPT subscription");
    pty.wait_for("dmz login anthropic an API key");
    // The Ctrl-C rule holds on the screen: one press asks, and Enter
    // takes the question back and the screen down.
    pty.send(CTRL_C);
    pty.wait_for(QUIT_HINT);
    pty.send(b"\r");
    pty.wait_for(PROMPT);
    // A turn fails with the credential error, and no session opens.
    pty.send(b"hello\r");
    pty.wait_for(NO_CREDENTIALS_HELP);
    assert!(pty.session_files().is_empty());
    assert!(mock.received().is_empty());
    // The login lands: the next turn runs.
    pty.log_in();
    pty.send(b"hello\r");
    pty.wait_for("logged in now");
    let sessions = pty.session_files();
    pty.send(CTRL_D);
    pty.wait_exit();
    assert_eq!(sessions.len(), 1);
    assert_eq!(mock.received().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_without_a_login_quits_from_the_first_run_screen() {
    let mock = MockServer::start(Vec::new(), Vec::new()).await;
    let mut pty = Pty::spawn_without_login(&mock.url(), &[]);
    pty.wait_for("openai-codex: none");
    pty.send(CTRL_C);
    pty.wait_for(QUIT_HINT);
    pty.send(CTRL_C);
    let raw = pty.wait_exit();
    // No session opened, so no exit line.
    assert!(!after_restore(&raw).contains("resume with"));

    // A provider-specific session names its provider and leads with
    // its command; Ctrl-D ends the run from the screen.
    let mut pty = Pty::spawn_without_login(&mock.url(), &["--provider", "anthropic"]);
    pty.wait_for("anthropic, the session's provider");
    pty.wait_for("dmz login anthropic an API key");
    pty.wait_for("dmz login the ChatGPT subscription");
    pty.send(CTRL_D);
    pty.wait_exit();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_plain_interface_without_a_login_stops_at_boot() {
    let mock = MockServer::start(Vec::new(), Vec::new()).await;
    let pty = Pty::spawn_without_login(&mock.url(), &["--plain"]);
    let (status, raw) = pty.wait_end();
    assert!(!status.success(), "exit status: {status:?}");
    let text = String::from_utf8_lossy(&raw);
    assert!(text.contains(NO_CREDENTIALS_HELP), "{text:?}");
    assert!(!text.contains(ENTER_ALT_SCREEN), "{text:?}");
}
