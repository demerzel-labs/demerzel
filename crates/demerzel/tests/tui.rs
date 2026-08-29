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
const END: &[u8] = b"\x1b[F";
const CTRL_O: &[u8] = b"\x0f";
const CTRL_T: &[u8] = b"\x14";
const PAGE_UP: &[u8] = b"\x1b[5~";
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
/// Mouse reporting mode 1000, the first sequence capture turns on.
/// The mode is the only sign of capture a PTY can observe, so the
/// test pins it; the interface owns the sequence, not a dependency.
const MOUSE_CAPTURE: &str = "\x1b[?1000h";

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
/// the Codex endpoint streams them.
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
        Self::spawn_with_project_config(base_url, extra_args, None)
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
        let dirs = tempfile::tempdir().unwrap();
        let home = dirs.path().join("home");
        let data = dirs.path().join("data");
        let project = dirs.path().join("project");
        for dir in [&home, &data, &project] {
            std::fs::create_dir_all(dir).unwrap();
        }
        write_auth_store(&data);
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

    /// Wait for `until`, then assert `marker` showed nowhere between
    /// the last match and it. A repaint before this makes the span
    /// the whole screen, so the check covers what is on it.
    fn absent_until(&mut self, marker: &str, until: &str) {
        let from = self.cursor;
        self.wait_for(until);
        let needle: String = marker.chars().filter(|ch| *ch != ' ').collect();
        let stripped = strip_escapes(&self.seen);
        let haystack: Vec<u8> = stripped
            .iter()
            .copied()
            .filter(|byte| *byte != b' ')
            .collect();
        assert!(
            find_after(&haystack[..self.cursor], from, needle.as_bytes()).is_none(),
            "{marker:?} showed before {until:?}; stripped output: {:?}",
            String::from_utf8_lossy(&stripped),
        );
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

    /// The project directory the session runs in.
    fn project(&self) -> std::path::PathBuf {
        self._dirs.path().join("project")
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
    pty.wait_for("dmz>");
    pty.wait_for("· ask ·");
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
    // A fresh session restarts the totals: after the turn above the
    // bar shows non-zero counts, so a later zero row is the reset.
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
    pty.send(b"hello\r");
    pty.wait_for("plain reply");
    pty.wait_for(PROMPT);
    // With the engine open, the switch reaches it in place.
    pty.send(b"/effort high\r");
    pty.wait_for("effort: high");
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
    // Capture is off by default; the user file turns it on.
    let mut pty = Pty::spawn_with_configs(&mock.url(), &[], None, Some("[tui]\nmouse = true\n"));

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
    // row holds, so the scroll deepens to seven. A 27-row pane opens
    // on row 27; the indicator counts the seven below.
    pty.wait_for("row 27 of the long reply");
    pty.wait_for("7 below");
    // End returns to the bottom of the transcript.
    pty.send(END);
    pty.resize(100, 30);
    pty.send(b"x");
    pty.wait_for("row 59 of the long reply");
    pty.send(CTRL_C);
    pty.send(CTRL_D);
    let raw = pty.wait_exit();

    let text = String::from_utf8_lossy(&raw);
    assert!(
        text.contains(MOUSE_CAPTURE),
        "the mouse key turns capture on"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_project_file_cannot_turn_mouse_capture_on() {
    let mock = MockServer::start(vec![text_turn("no mouse")], Vec::new()).await;
    let mut pty = Pty::spawn_with_project_config(&mock.url(), &[], Some("[tui]\nmouse = true\n"));

    pty.wait_for(PROMPT);
    pty.send(b"hello\r");
    pty.wait_for("no mouse");
    pty.send(CTRL_D);
    let raw = pty.wait_exit();

    let text = String::from_utf8_lossy(&raw);
    assert!(!text.contains(MOUSE_CAPTURE), "capture must stay off");
}

#[tokio::test(flavor = "multi_thread")]
async fn mouse_capture_is_off_by_default() {
    let mock = MockServer::start(vec![text_turn("no mouse")], Vec::new()).await;
    let mut pty = Pty::spawn(&mock.url(), &[]);

    pty.wait_for(PROMPT);
    pty.send(b"hello\r");
    pty.wait_for("no mouse");
    pty.send(CTRL_D);
    let raw = pty.wait_exit();

    let text = String::from_utf8_lossy(&raw);
    assert!(!text.contains(MOUSE_CAPTURE), "capture must stay off");
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
    pty.absent_until("second line", "dmz> xy");
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
    pty.absent_until("thought", "dmz> x");
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
    pty.absent_until("start a fresh session", "· ask ·");
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
    // The status bar's ground is the theme's.
    assert!(
        has_params(&params, &[48, 2, 0x36, 0x3e, 0x4e]),
        "the bar's ground in truecolor: {params:?}"
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
    assert!(!status.success());
    let text = String::from_utf8_lossy(&pty.seen);
    assert!(
        text.contains("`tui.theme`") && text.contains("ocean"),
        "{text}"
    );
}
