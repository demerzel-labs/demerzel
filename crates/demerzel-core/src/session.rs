//! JSONL session store: one append-only file per session.
//!
//! The file is the source of truth for resume and replay, and it is
//! the fixture format for the mock server. It is plaintext and
//! self-contained: inspect it with a pager, export it with `cp`,
//! delete it with `rm` and nothing remains. The harness writes no
//! secrets into it; auth tokens live only in the auth store.
//!
//! Layout: `<data root>/projects/<project-id>/sessions/<id>.jsonl`.
//! The first record is metadata. A later meta record marks a resume
//! or a model switch; the last meta record describes the current
//! run. Every other record is one event: a message, an approval
//! decision, a usage report, or a turn end. Readers skip record
//! types they do not know, so old binaries read new files. A torn
//! final line — a crash mid-append — is dropped on replay and
//! truncated away on resume.
//!
//! The file is also the measurement trace for one session: the meta
//! records carry the environment (project directory, provider,
//! model), the messages carry every tool step, the usage records
//! carry the token counts with the cached parts, and the turn records
//! carry how each user turn ended. A user turn opens with a user
//! message that holds a text block; the turn record that follows
//! before the next one closes it. A turn without one was cut off:
//! a dropped turn future (Ctrl-C in the interfaces), a process exit
//! mid-turn, or a session write failure. The file carries no clock
//! beyond the meta record's creation time; wall time per turn is the
//! caller's to measure.

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::approval::{ApprovalDecision, ApprovalMode};
use crate::message::{Message, Usage};
use crate::paths;
use crate::provider::Effort;

/// Version of the session-file schema, written into every meta record.
pub const SCHEMA_VERSION: u32 = 1;

/// The metadata record that opens every session file. A resume, a
/// model switch, an effort switch, and an approval switch append a
/// fresh one, so the last meta record describes the current run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub schema: u32,
    /// Project directory the session ran in.
    pub cwd: String,
    /// Adapter id, e.g. "openai-codex".
    pub provider: String,
    pub model: String,
    /// The reasoning effort the run's requests carry, by its level
    /// name; absent when the provider's default applies. A name, not
    /// the enum, so a reader keeps the file when a later build writes
    /// a level it does not know.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// The approval mode in force, by its name; absent in a file from
    /// before the field, and readers keep such a file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<String>,
    /// RFC 3339 creation time.
    pub created: String,
}

impl SessionMeta {
    /// Metadata for a run starting now. The stored `cwd` is canonical,
    /// so one project has one spelling across resumes.
    pub fn now(
        cwd: &Path,
        provider: &str,
        model: &str,
        effort: Option<Effort>,
        approval: Option<ApprovalMode>,
    ) -> Self {
        let canonical = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
        Self {
            schema: SCHEMA_VERSION,
            cwd: canonical.display().to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            effort: effort.map(|effort| effort.as_str().to_string()),
            approval: approval.map(|mode| mode.as_str().to_string()),
            created: humantime::format_rfc3339_seconds(SystemTime::now()).to_string(),
        }
    }
}

/// One line of a session file. The serialized shape is part of the
/// session-file format; changing it is a schema change. Non-exhaustive:
/// a later record type is additive, and readers skip what they do not
/// know.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SessionRecord {
    Meta(SessionMeta),
    Message(Message),
    /// One approval decision, recorded as a non-prompt event so the
    /// record stays auditable and the prompt prefix stays stable.
    Approval {
        tool: String,
        mode: ApprovalMode,
        decision: ApprovalDecision,
        /// Id of the tool call the decision was for.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
        /// The user's answer when the decision was `AskUser`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        approved: Option<bool>,
        /// The literal answer for the audit trail: `allow`,
        /// `allow_for_session`, `deny`, or `abort`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answer: Option<String>,
    },
    Usage(Usage),
    /// How one user turn ended. `outcome` is one of `completed`,
    /// `max_tokens`, `refused`, `call_limit`, `aborted`, or `error`;
    /// the first five are the engine's turn outcomes by their
    /// `as_str` names, and `error` marks a provider failure, with the
    /// message in `error`, cut at 1 KiB.
    Turn {
        outcome: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// A record type from a newer schema. Replay skips it, so old
    /// binaries read new files. The harness never writes this variant.
    #[serde(other)]
    Unknown,
}

/// Borrowed mirror of [`SessionRecord`], so appends serialize without
/// cloning the payload. The serialized shape is identical.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RecordRef<'a> {
    Meta(&'a SessionMeta),
    Message(&'a Message),
    Approval {
        tool: &'a str,
        mode: ApprovalMode,
        decision: ApprovalDecision,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        approved: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        answer: Option<&'a str>,
    },
    Usage(&'a Usage),
    Turn {
        outcome: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<&'a str>,
    },
}

/// The record type alone. Only the replay error path uses it, to name
/// the record type in the message; the happy path parses each line
/// once.
#[derive(Deserialize)]
struct RecordTag<'a> {
    #[serde(borrow, rename = "type")]
    kind: Option<std::borrow::Cow<'a, str>>,
}

/// History rebuilt from a session file.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Replay {
    pub meta: Option<SessionMeta>,
    pub messages: Vec<Message>,
}

/// The session directory tree under one data root.
#[derive(Debug, Clone)]
pub struct SessionStore {
    root: PathBuf,
}

impl SessionStore {
    /// Open the store at the default data root (see
    /// [`crate::paths::data_dir`]).
    pub fn open_default() -> io::Result<Self> {
        Ok(Self::at(paths::data_dir()?))
    }

    /// Open the store at an explicit data root. Tests use a temp dir.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The sessions directory for one project.
    pub fn sessions_dir(&self, project: &Path) -> PathBuf {
        self.root
            .join("projects")
            .join(project_id(project))
            .join("sessions")
    }

    /// Create a new session file for the project and write its meta
    /// record.
    pub fn create(
        &self,
        project: &Path,
        provider: &str,
        model: &str,
        effort: Option<Effort>,
        approval: Option<ApprovalMode>,
    ) -> io::Result<Session> {
        let dir = self.sessions_dir(project);
        std::fs::create_dir_all(&dir)?;
        let mut attempt = 0;
        let (id, path, file) = loop {
            let id = new_session_id();
            let path = dir.join(format!("{id}.jsonl"));
            match OpenOptions::new().create_new(true).append(true).open(&path) {
                Ok(file) => break (id, path, file),
                // A cross-process id collision: mint a fresh id once.
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists && attempt == 0 => {
                    attempt += 1;
                }
                Err(error) => return Err(error),
            }
        };
        let mut session = Session {
            id,
            path,
            file,
            cwd: project.to_path_buf(),
            buf: Vec::new(),
        };
        session.append_meta(&SessionMeta::now(
            project, provider, model, effort, approval,
        ))?;
        Ok(session)
    }

    /// The newest session file for the project, by id. Session ids
    /// start with a millisecond timestamp, so the name order is the
    /// creation order.
    pub fn latest(&self, project: &Path) -> io::Result<Option<PathBuf>> {
        let dir = self.sessions_dir(project);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut newest: Option<PathBuf> = None;
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_none_or(|ext| ext != "jsonl") {
                continue;
            }
            if newest.as_ref().is_none_or(|best| path > *best) {
                newest = Some(path);
            }
        }
        Ok(newest)
    }
}

/// One open session: an append handle on its file.
#[derive(Debug)]
pub struct Session {
    id: String,
    path: PathBuf,
    file: File,
    /// The project directory the session is scoped to, kept so a
    /// mid-run meta record — a model switch — names the same project
    /// as the one that opened the file.
    cwd: PathBuf,
    /// Reused serialization buffer, so appends allocate nothing steady
    /// state.
    buf: Vec<u8>,
}

impl Session {
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The project directory the session is scoped to.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one record as one line. The line is written with a
    /// single call, so a reader never sees a torn record boundary.
    pub fn append(&mut self, record: &SessionRecord) -> io::Result<()> {
        let record = match record {
            // Through `append_meta`, so the session adopts the
            // meta's cwd on every path a meta record can enter.
            SessionRecord::Meta(meta) => return self.append_meta(meta),
            SessionRecord::Message(message) => RecordRef::Message(message),
            SessionRecord::Approval {
                tool,
                mode,
                decision,
                tool_use_id,
                approved,
                answer,
            } => RecordRef::Approval {
                tool,
                mode: *mode,
                decision: *decision,
                tool_use_id: tool_use_id.as_deref(),
                approved: *approved,
                answer: answer.as_deref(),
            },
            SessionRecord::Usage(usage) => RecordRef::Usage(usage),
            SessionRecord::Turn { outcome, error } => RecordRef::Turn {
                outcome,
                error: error.as_deref(),
            },
            // Replay-only marker; there is nothing to write.
            SessionRecord::Unknown => return Ok(()),
        };
        self.append_ref(&record)
    }

    pub fn append_meta(&mut self, meta: &SessionMeta) -> io::Result<()> {
        self.append_ref(&RecordRef::Meta(meta))?;
        // The session adopts the meta's canonical cwd, so a later
        // meta append — a model switch — names the same project even
        // when the file the session resumed had no meta record.
        self.cwd = PathBuf::from(&meta.cwd);
        Ok(())
    }

    pub fn append_message(&mut self, message: &Message) -> io::Result<()> {
        self.append_ref(&RecordRef::Message(message))
    }

    pub fn append_approval(
        &mut self,
        tool: &str,
        mode: ApprovalMode,
        decision: ApprovalDecision,
        tool_use_id: &str,
        approved: Option<bool>,
        answer: Option<&str>,
    ) -> io::Result<()> {
        self.append_ref(&RecordRef::Approval {
            tool,
            mode,
            decision,
            tool_use_id: Some(tool_use_id),
            approved,
            answer,
        })
    }

    pub fn append_usage(&mut self, usage: &Usage) -> io::Result<()> {
        self.append_ref(&RecordRef::Usage(usage))
    }

    /// Close one user turn with its outcome. See
    /// [`SessionRecord::Turn`] for the outcome vocabulary.
    pub fn append_turn(&mut self, outcome: &str, error: Option<&str>) -> io::Result<()> {
        self.append_ref(&RecordRef::Turn { outcome, error })
    }

    fn append_ref(&mut self, record: &RecordRef<'_>) -> io::Result<()> {
        self.buf.clear();
        serde_json::to_writer(&mut self.buf, record).map_err(io::Error::other)?;
        self.buf.push(b'\n');
        self.file.write_all(&self.buf)
    }

    /// Reopen an existing session file for appending and rebuild its
    /// history. A torn final line is truncated away first, so the next
    /// append starts on a clean record boundary.
    pub fn resume(path: &Path) -> io::Result<(Self, Replay)> {
        truncate_torn_tail(path)?;
        let replay = replay(path)?;
        let id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_string();
        let file = OpenOptions::new().append(true).open(path)?;
        // The stored cwd is already canonical, so a later meta append
        // names the same spelling. A file without a meta record is
        // outside the format; an empty cwd keeps the append total.
        let cwd = replay
            .meta
            .as_ref()
            .map(|meta| PathBuf::from(&meta.cwd))
            .unwrap_or_default();
        Ok((
            Self {
                id,
                path: path.to_path_buf(),
                file,
                cwd,
                buf: Vec::new(),
            },
            replay,
        ))
    }
}

/// Cut an unterminated final line, the leftover of a crash mid-append.
/// Reads backward in blocks from the file end, so a long session never
/// loads whole into memory.
fn truncate_torn_tail(path: &Path) -> io::Result<()> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    const BLOCK: u64 = 8192;
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(());
    }
    let mut byte = [0u8; 1];
    file.seek(SeekFrom::Start(len - 1))?;
    file.read_exact(&mut byte)?;
    if byte[0] == b'\n' {
        return Ok(());
    }
    let mut end = len - 1;
    let mut buf = vec![0u8; BLOCK as usize];
    loop {
        let start = end.saturating_sub(BLOCK);
        let read_len = (end - start) as usize;
        if read_len == 0 {
            return file.set_len(0);
        }
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut buf[..read_len])?;
        if let Some(position) = buf[..read_len].iter().rposition(|b| *b == b'\n') {
            return file.set_len(start + position as u64 + 1);
        }
        end = start;
    }
}

/// Rebuild history from a session file. Unknown record types are
/// skipped; a malformed known record or a record without a type is an
/// error; an unterminated final line is ignored.
pub fn replay(path: &Path) -> io::Result<Replay> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut out = Replay::default();
    let mut line = String::new();
    let mut number = 0usize;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        number += 1;
        if !line.ends_with('\n') {
            // Torn tail from a crash mid-append; resume truncates it.
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let bad = |what: &str, detail: String| {
            io::Error::other(format!("{}:{number}: {what}: {detail}", path.display()))
        };
        let record: SessionRecord = match serde_json::from_str(trimmed) {
            Ok(record) => record,
            Err(error) => {
                // Name the record type in the error when the line can
                // still identify itself.
                let kind = serde_json::from_str::<RecordTag<'_>>(trimmed)
                    .ok()
                    .and_then(|tag| tag.kind);
                return Err(match kind {
                    Some(kind) => bad(&format!("bad {kind} record"), error.to_string()),
                    None => bad("bad session record", error.to_string()),
                });
            }
        };
        match record {
            SessionRecord::Meta(meta) => {
                if meta.schema > SCHEMA_VERSION {
                    return Err(bad(
                        "unsupported session schema",
                        format!(
                            "file has schema {}, this build reads {SCHEMA_VERSION}",
                            meta.schema
                        ),
                    ));
                }
                out.meta = Some(meta);
            }
            SessionRecord::Message(message) => out.messages.push(message),
            SessionRecord::Approval { .. }
            | SessionRecord::Usage(_)
            | SessionRecord::Turn { .. } => {}
            SessionRecord::Unknown => {}
        }
    }
    Ok(out)
}

/// Directory name for one project: the canonical path folded to safe
/// characters, bounded in length, plus a short hash of the unfolded
/// path so distinct projects never share a directory.
fn project_id(project: &Path) -> String {
    const MAX_STEM: usize = 96;
    let canonical = project
        .canonicalize()
        .unwrap_or_else(|_| project.to_path_buf());
    let display = canonical.display().to_string();
    let mut stem: String = display
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    stem.truncate(MAX_STEM);
    let hash = fnv1a64(display.as_bytes());
    format!("{stem}-{:08x}", (hash >> 32) as u32 ^ hash as u32)
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xCBF2_9CE4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

/// Crockford base32, the ULID alphabet.
const ULID_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

const ENTROPY_MASK: u128 = (1 << 80) - 1;

/// The last issued id as `(millis, entropy)`, so ids stay strictly
/// increasing inside one process even when the clock steps backward.
static LAST_ID: Mutex<(u128, u128)> = Mutex::new((0, 0));

/// A ULID-format session id: 48-bit millisecond timestamp plus 80 bits
/// of local entropy, 26 characters of Crockford base32. Ids sort by
/// creation time; inside one millisecond the entropy increments, so
/// the order stays strict. The entropy seeds from the clock and the
/// process id; it is unique per machine, not cryptographic.
fn new_session_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let raw_millis =
        u128::from(u64::try_from(now.as_millis()).unwrap_or(u64::MAX)) & ((1 << 48) - 1);
    let seed = now.as_nanos() as u64 ^ (u64::from(std::process::id()) << 32);
    let (millis, entropy) = {
        let mut last = LAST_ID.lock().unwrap();
        // Clamp a backward clock step, so a new id never sorts before
        // an issued one and `latest` keeps returning the newest.
        let millis = raw_millis.max(last.0);
        let entropy = if last.0 == millis {
            (last.1 + 1) & ENTROPY_MASK
        } else {
            (u128::from(splitmix64(seed)) << 16 | u128::from(splitmix64(seed ^ 0xDEAD) >> 48))
                & ENTROPY_MASK
        };
        *last = (millis, entropy);
        (millis, entropy)
    };
    let value = (millis << 80) | entropy;
    let mut id = String::with_capacity(26);
    for shift in (0..26).rev() {
        let index = ((value >> (shift * 5)) & 0x1F) as usize;
        id.push(ULID_ALPHABET[index] as char);
    }
    id
}

fn splitmix64(seed: u64) -> u64 {
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{ContentBlock, Role};

    fn text_message(role: Role, text: &str) -> Message {
        Message {
            role,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    #[test]
    fn create_append_and_replay_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(dir.path());
        let project = dir.path().join("project");
        std::fs::create_dir(&project).unwrap();

        let mut session = store
            .create(
                &project,
                "openai-codex",
                "gpt-test",
                None,
                Some(ApprovalMode::Ask),
            )
            .unwrap();
        session
            .append_message(&text_message(Role::User, "hi"))
            .unwrap();
        session
            .append_message(&text_message(Role::Assistant, "hello"))
            .unwrap();
        session
            .append_usage(&Usage {
                input_tokens: 10,
                output_tokens: 2,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            })
            .unwrap();

        let replay = replay(session.path()).unwrap();
        let meta = replay.meta.unwrap();
        assert_eq!(meta.schema, SCHEMA_VERSION);
        assert_eq!(meta.provider, "openai-codex");
        assert_eq!(meta.model, "gpt-test");
        assert_eq!(meta.approval.as_deref(), Some("ask"));
        assert_eq!(replay.messages.len(), 2);
        assert_eq!(replay.messages[0], text_message(Role::User, "hi"));
    }

    #[test]
    fn record_shapes_are_stable_and_the_borrowed_mirror_matches() {
        // The session-file format depends on these exact shapes.
        let record = SessionRecord::Message(text_message(Role::User, "hi"));
        let value = serde_json::to_value(&record).unwrap();
        assert_eq!(value["type"], "message");
        assert_eq!(value["role"], "user");
        assert_eq!(value["content"][0]["type"], "text");

        let approval = SessionRecord::Approval {
            tool: "bash".into(),
            mode: ApprovalMode::Ask,
            decision: ApprovalDecision::AskUser,
            tool_use_id: Some("call_9".into()),
            approved: Some(true),
            answer: Some("allow".into()),
        };
        let value = serde_json::to_value(&approval).unwrap();
        assert_eq!(value["type"], "approval");
        assert_eq!(value["mode"], "ask");
        assert_eq!(value["decision"], "ask_user");
        assert_eq!(value["tool_use_id"], "call_9");
        assert_eq!(value["approved"], true);

        let usage = SessionRecord::Usage(Usage::default());
        let value = serde_json::to_value(&usage).unwrap();
        assert_eq!(value["type"], "usage");
        assert_eq!(value["input_tokens"], 0);

        let turn = SessionRecord::Turn {
            outcome: "completed".into(),
            error: None,
        };
        let value = serde_json::to_value(&turn).unwrap();
        assert_eq!(value["type"], "turn");
        assert_eq!(value["outcome"], "completed");
        assert!(value.get("error").is_none());
        let failed = SessionRecord::Turn {
            outcome: "error".into(),
            error: Some("rate limited".into()),
        };
        let value = serde_json::to_value(&failed).unwrap();
        assert_eq!(value["error"], "rate limited");

        // The borrowed append path serializes the identical shape.
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(dir.path());
        let mut session = store.create(dir.path(), "p", "m", None, None).unwrap();
        session
            .append_approval(
                "bash",
                ApprovalMode::Ask,
                ApprovalDecision::AskUser,
                "call_9",
                Some(true),
                Some("allow"),
            )
            .unwrap();
        let text = std::fs::read_to_string(session.path()).unwrap();
        let line = text.lines().last().unwrap();
        assert_eq!(line, serde_json::to_string(&approval).unwrap());
    }

    #[test]
    fn resume_appends_to_the_same_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(dir.path());
        let project = dir.path().to_path_buf();
        let mut session = store.create(&project, "p", "m", None, None).unwrap();
        session
            .append_message(&text_message(Role::User, "one"))
            .unwrap();
        let path = session.path().to_path_buf();
        drop(session);

        let (mut resumed, replayed) = Session::resume(&path).unwrap();
        assert_eq!(replayed.messages.len(), 1);
        resumed
            .append_message(&text_message(Role::Assistant, "two"))
            .unwrap();
        let replayed = replay(&path).unwrap();
        assert_eq!(replayed.messages.len(), 2);
    }

    #[test]
    fn a_torn_final_line_is_dropped_and_truncated_on_resume() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(dir.path());
        let mut session = store.create(dir.path(), "p", "m", None, None).unwrap();
        session
            .append_message(&text_message(Role::User, "kept"))
            .unwrap();
        let path = session.path().to_path_buf();
        drop(session);
        // A crash mid-append leaves a record without its newline.
        use std::io::Write as _;
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        write!(file, "{{\"type\":\"message\",\"TORN_FRAGMENT").unwrap();
        drop(file);

        // Replay ignores the torn tail.
        let replayed = replay(&path).unwrap();
        assert_eq!(replayed.messages.len(), 1);

        // Resume truncates it, so the next append starts clean.
        let (mut resumed, _) = Session::resume(&path).unwrap();
        resumed
            .append_message(&text_message(Role::Assistant, "next"))
            .unwrap();
        let replayed = replay(&path).unwrap();
        assert_eq!(replayed.messages.len(), 2);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("TORN_FRAGMENT"));
    }

    #[test]
    fn a_torn_tail_longer_than_one_scan_block_is_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(dir.path());
        let mut session = store.create(dir.path(), "p", "m", None, None).unwrap();
        session
            .append_message(&text_message(Role::User, "kept"))
            .unwrap();
        let path = session.path().to_path_buf();
        drop(session);
        use std::io::Write as _;
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        // A torn record larger than the 8 KiB backward-scan block.
        write!(
            file,
            "{{\"type\":\"message\",\"x\":\"{}",
            "y".repeat(40_000)
        )
        .unwrap();
        drop(file);

        let (_, replayed) = Session::resume(&path).unwrap();
        assert_eq!(replayed.messages.len(), 1);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with('\n'));
        assert!(!text.contains("yyyy"));
    }

    #[test]
    fn append_matches_the_owned_record_serialization_for_every_variant() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(dir.path());
        let mut session = store.create(dir.path(), "p", "m", None, None).unwrap();
        let records = [
            SessionRecord::Meta(SessionMeta::now(dir.path(), "p2", "m2", None, None)),
            SessionRecord::Message(text_message(Role::Assistant, "hi")),
            SessionRecord::Approval {
                tool: "bash".into(),
                mode: ApprovalMode::Auto,
                decision: ApprovalDecision::Allow,
                tool_use_id: Some("call_3".into()),
                approved: None,
                answer: None,
            },
            SessionRecord::Usage(Usage {
                input_tokens: 1,
                output_tokens: 2,
                cache_read_tokens: 3,
                cache_creation_tokens: 4,
            }),
            SessionRecord::Turn {
                outcome: "completed".into(),
                error: None,
            },
            SessionRecord::Turn {
                outcome: "error".into(),
                error: Some("server error: 503".into()),
            },
        ];
        for record in &records {
            session.append(record).unwrap();
        }
        let text = std::fs::read_to_string(session.path()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        // Line 0 is the create-time meta; the appended records follow.
        for (line, record) in lines[1..].iter().zip(&records) {
            assert_eq!(*line, serde_json::to_string(record).unwrap());
        }
    }

    #[test]
    fn unknown_record_types_are_skipped_on_replay() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(dir.path());
        let project = dir.path().to_path_buf();
        let mut session = store.create(&project, "p", "m", None, None).unwrap();
        session
            .append_message(&text_message(Role::User, "hi"))
            .unwrap();
        // A record type from a future schema version.
        use std::io::Write as _;
        let mut file = OpenOptions::new()
            .append(true)
            .open(session.path())
            .unwrap();
        writeln!(file, r#"{{"type":"hologram","data":42}}"#).unwrap();
        drop(file);

        let replayed = replay(session.path()).unwrap();
        assert_eq!(replayed.messages.len(), 1);
    }

    #[test]
    fn malformed_and_untyped_records_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.jsonl");
        std::fs::write(&path, "{\"type\":\"message\",\"role\":\"nobody\"}\n").unwrap();
        let error = replay(&path).unwrap_err();
        assert!(error.to_string().contains("bad message record"));

        std::fs::write(&path, "{\"role\":\"user\"}\n").unwrap();
        let error = replay(&path).unwrap_err();
        assert!(error.to_string().contains("bad session record"));
    }

    #[test]
    fn a_newer_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future.jsonl");
        std::fs::write(
            &path,
            format!(
                "{{\"type\":\"meta\",\"schema\":{},\"cwd\":\"/x\",\"provider\":\"p\",\"model\":\"m\",\"created\":\"2026-08-07T00:00:00Z\"}}\n",
                SCHEMA_VERSION + 1
            ),
        )
        .unwrap();
        let error = replay(&path).unwrap_err();
        assert!(error.to_string().contains("unsupported session schema"));
    }

    #[test]
    fn latest_returns_the_newest_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(dir.path());
        let project = dir.path().to_path_buf();
        assert!(store.latest(&project).unwrap().is_none());

        let first = store.create(&project, "p", "m", None, None).unwrap();
        let second = store.create(&project, "p", "m", None, None).unwrap();
        let latest = store.latest(&project).unwrap().unwrap();
        assert_eq!(latest, second.path());
        assert_ne!(first.path(), second.path());
    }

    #[test]
    fn delete_means_nothing_remains() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(dir.path());
        let project = dir.path().to_path_buf();
        let session = store.create(&project, "p", "m", None, None).unwrap();
        let path = session.path().to_path_buf();
        drop(session);

        std::fs::remove_file(&path).unwrap();
        assert!(store.latest(&project).unwrap().is_none());
        // The only artifact of a session is its one file.
        let dir_entries: Vec<_> = std::fs::read_dir(store.sessions_dir(&project))
            .unwrap()
            .collect();
        assert!(dir_entries.is_empty());
    }

    #[test]
    fn session_ids_are_unique_and_strictly_increasing() {
        let ids: Vec<String> = (0..64).map(|_| new_session_id()).collect();
        for pair in ids.windows(2) {
            assert!(pair[0] < pair[1], "{} !< {}", pair[0], pair[1]);
        }
        for id in &ids {
            assert_eq!(id.len(), 26);
            assert!(id.bytes().all(|b| ULID_ALPHABET.contains(&b)));
        }
    }

    #[test]
    fn project_ids_are_readable_safe_and_collision_free() {
        let id = project_id(Path::new("/does/not/exist/my project"));
        assert!(id.starts_with("-does-not-exist-my-project-"));
        assert!(!id.contains('/'));

        // Paths that fold to the same stem stay distinct.
        let a = project_id(Path::new("/does/not/exist/a-b/x"));
        let b = project_id(Path::new("/does/not/exist/a/b/x"));
        assert_ne!(a, b);

        // A deep path stays inside one filename component.
        let deep = "/does/not/exist/".to_string() + &"d/".repeat(200);
        let id = project_id(Path::new(&deep));
        assert!(id.len() <= 96 + 1 + 8);
    }
}
