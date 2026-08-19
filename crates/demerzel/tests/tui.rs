//! Scripted PTY sessions through the real binary: the full-screen
//! interface against the mock server. No network, no model.
//!
//! Each test spawns the binary on a pseudo-terminal, drives it with
//! key bytes, and asserts on the merged output stream. Frame diffs
//! interleave escape sequences with text, so assertions run against a
//! stream with the escape sequences stripped.

use std::io::Read as _;
use std::os::fd::{FromRawFd as _, OwnedFd};
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use demerzel_mock::{MockResponse, MockServer};
use serde_json::{Value, json};

const READY: &str = "demerzel ready";
// No trailing space: the frame diff skips cells that stay blank.
const PROMPT: &str = "dmz>";
const ENTER_ALT_SCREEN: &str = "\x1b[?1049h";
const CTRL_C: &[u8] = b"\x03";
const CTRL_D: &[u8] = b"\x04";

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

fn text_turn(text: &str) -> MockResponse {
    MockResponse::stream(vec![text_delta(text), completed()])
}

fn tool_turn(name: &str, input: Value) -> MockResponse {
    MockResponse::stream(vec![
        json!({
            "type": "response.output_item.added",
            "item": {"type": "function_call", "id": "item_1", "call_id": "call_1", "name": name},
        }),
        json!({
            "type": "response.output_item.done",
            "item": {"type": "function_call", "call_id": "call_1", "arguments": input.to_string()},
        }),
        completed_for_tool(),
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
    _dirs: tempfile::TempDir,
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
        let dirs = tempfile::tempdir().unwrap();
        let home = dirs.path().join("home");
        let data = dirs.path().join("data");
        let project = dirs.path().join("project");
        for dir in [&home, &data, &project] {
            std::fs::create_dir_all(dir).unwrap();
        }
        write_auth_store(&data);

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
            .env_remove("DEMERZEL_PROVIDER")
            .env_remove("DEMERZEL_MODEL")
            .env_remove("DEMERZEL_BASE_URL")
            .env_remove("ANTHROPIC_API_KEY");
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
            _dirs: dirs,
        }
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
    /// needs the full repaint the next draw runs after a resize.
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

    /// The session-file stems this run created, across all projects.
    fn session_files(&self) -> Vec<String> {
        let projects = self._dirs.path().join("data/demerzel/projects");
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

    /// Wait for a clean exit and return the raw output.
    fn wait_exit(mut self) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.poll_read();
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "exit status: {status:?}");
                // Collect what the shutdown path still wrote.
                self.poll_read();
                return std::mem::take(&mut self.seen);
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
    let size = libc::winsize {
        ws_row: 30,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let result = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &size,
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
fn contains_marker(raw: &[u8], marker: &str) -> bool {
    let needle: Vec<u8> = marker.bytes().filter(|byte| *byte != b' ').collect();
    let haystack: Vec<u8> = strip_escapes(raw)
        .into_iter()
        .filter(|byte| *byte != b' ')
        .collect();
    find_after(&haystack, 0, &needle).is_some()
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
async fn a_tui_session_boots_streams_and_exits() {
    let mock = MockServer::start(vec![text_turn("hi there from the model")], Vec::new()).await;
    let mut pty = Pty::spawn(&mock.url(), &[]);

    pty.wait_for(READY);
    pty.wait_for(PROMPT);
    pty.send(b"hello\r");
    pty.wait_for("hi there from the model");
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
    pty.wait_for("edit demo.txt");
    pty.wait_for("@ replace lines 1-2");
    pty.wait_for("+ one");
    pty.wait_for("+ two");
    pty.wait_for("approve edit? [y]es / [a]lways / [N]o");
    pty.send(b"n");
    pty.wait_for("approve edit? no");
    pty.wait_for("understood");
    pty.send(CTRL_D);
    pty.wait_exit();

    // The declined call went back to the model as an error result.
    let second = mock.received()[1].body.to_string();
    assert!(second.contains("declined"), "{second}");
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
        contains_marker(&raw, "gpt-test · ask"),
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
async fn the_status_bar_names_the_session_from_the_first_prompt_on() {
    let mock = MockServer::start(vec![text_turn("named reply")], Vec::new()).await;
    let mut pty = Pty::spawn(&mock.url(), &[]);

    pty.wait_for(PROMPT);
    pty.send(b"hello\r");
    pty.wait_for("named reply");
    // The session file exists now; its id shows only on the bar. A
    // resize makes the next draw repaint the bar in full.
    let sessions = pty.session_files();
    assert_eq!(sessions.len(), 1);
    let id = sessions[0].clone();
    pty.resize(90, 30);
    pty.send(b"x");
    pty.wait_for(&id);
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
async fn the_plain_interface_shares_the_command_registry() {
    let mock = MockServer::start(vec![text_turn("plain reply")], Vec::new()).await;
    let mut pty = Pty::spawn(&mock.url(), &["--plain"]);

    pty.wait_for(PROMPT);
    pty.send(b"/help\r");
    pty.wait_for("/model [name]");
    pty.wait_for(PROMPT);
    pty.send(b"/model gpt-test\r");
    pty.wait_for("model: gpt-test");
    pty.wait_for(PROMPT);
    pty.send(b"hello\r");
    pty.wait_for("plain reply");
    pty.wait_for(PROMPT);
    pty.send(CTRL_D);
    pty.wait_exit();

    // The switch happened before the session opened.
    let first = mock.received()[0].body.to_string();
    assert!(first.contains("gpt-test"), "{first}");
}
