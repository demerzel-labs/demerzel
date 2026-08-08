//! JSONL session store: one append-only file per session.
//!
//! The file is the source of truth for resume and replay, and it is
//! the fixture format for the mock server. It is plaintext and
//! self-contained: inspect it with a pager, export it with `cp`,
//! delete it with `rm` and nothing remains. The harness writes no
//! secrets into it; auth tokens live only in the auth store.
//!
//! Layout: `<data root>/projects/<project-id>/sessions/<id>.jsonl`.
//! The first record is metadata. Every later record is one event:
//! a message, an approval decision, or a usage report. Readers skip
//! record types they do not know, so old binaries read new files.

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::approval::{ApprovalDecision, ApprovalMode};
use crate::message::{Message, Usage};

/// Version of the session-file schema, written into every meta record.
pub const SCHEMA_VERSION: u32 = 1;

/// The metadata record that opens every session file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub schema: u32,
    /// Project directory the session ran in.
    pub cwd: String,
    /// Adapter id, e.g. "openai-codex".
    pub provider: String,
    pub model: String,
    /// RFC 3339 creation time.
    pub created: String,
}

/// One line of a session file. The serialized shape is part of the
/// session-file format; changing it is a schema change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionRecord {
    Meta(SessionMeta),
    Message(Message),
    /// One approval decision, recorded as a non-prompt event so the
    /// record stays auditable and the prompt prefix stays stable.
    Approval {
        tool: String,
        mode: ApprovalMode,
        decision: ApprovalDecision,
        /// The user's answer when the decision was `AskUser`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        approved: Option<bool>,
    },
    Usage(Usage),
}

/// Record types this version knows. Replay skips unknown types instead
/// of failing, so a newer file still resumes.
const KNOWN_TYPES: [&str; 4] = ["meta", "message", "approval", "usage"];

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
    /// Open the store at the default data root:
    /// `$XDG_DATA_HOME/demerzel`, or `~/.local/share/demerzel`.
    pub fn open_default() -> io::Result<Self> {
        let root = match std::env::var_os("XDG_DATA_HOME") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => {
                let home =
                    std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME is not set"))?;
                Path::new(&home).join(".local/share")
            }
        };
        Ok(Self::at(root.join("demerzel")))
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
    pub fn create(&self, project: &Path, provider: &str, model: &str) -> io::Result<Session> {
        let dir = self.sessions_dir(project);
        std::fs::create_dir_all(&dir)?;
        let id = new_session_id();
        let path = dir.join(format!("{id}.jsonl"));
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)?;
        let mut session = Session { id, path, file };
        session.append(&SessionRecord::Meta(SessionMeta {
            schema: SCHEMA_VERSION,
            cwd: project.display().to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            created: humantime::format_rfc3339_seconds(SystemTime::now()).to_string(),
        }))?;
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
}

impl Session {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one record as one line. The line is written with a
    /// single call, so a reader never sees a torn record boundary.
    pub fn append(&mut self, record: &SessionRecord) -> io::Result<()> {
        let mut line = serde_json::to_string(record).map_err(io::Error::other)?;
        line.push('\n');
        self.file.write_all(line.as_bytes())
    }

    /// Reopen an existing session file for appending and rebuild its
    /// history.
    pub fn resume(path: &Path) -> io::Result<(Self, Replay)> {
        let replay = replay(path)?;
        let id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_string();
        let file = OpenOptions::new().append(true).open(path)?;
        Ok((
            Self {
                id,
                path: path.to_path_buf(),
                file,
            },
            replay,
        ))
    }
}

/// Rebuild history from a session file. Unknown record types are
/// skipped; a malformed known record is an error.
pub fn replay(path: &Path) -> io::Result<Replay> {
    let file = File::open(path)?;
    let mut out = Replay::default();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(&line).map_err(|error| {
            io::Error::other(format!(
                "{}:{}: bad session record: {error}",
                path.display(),
                index + 1
            ))
        })?;
        let kind = value["type"].as_str().unwrap_or_default().to_string();
        if !KNOWN_TYPES.contains(&kind.as_str()) {
            continue;
        }
        let record: SessionRecord = serde_json::from_value(value).map_err(|error| {
            io::Error::other(format!(
                "{}:{}: bad {kind} record: {error}",
                path.display(),
                index + 1
            ))
        })?;
        match record {
            SessionRecord::Meta(meta) => out.meta = Some(meta),
            SessionRecord::Message(message) => out.messages.push(message),
            SessionRecord::Approval { .. } | SessionRecord::Usage(_) => {}
        }
    }
    Ok(out)
}

/// Directory name for one project: the canonical path with every
/// non-portable character folded to `-`. Readable, stable, and safe as
/// a single path component.
fn project_id(project: &Path) -> String {
    let canonical = project
        .canonicalize()
        .unwrap_or_else(|_| project.to_path_buf());
    let mut id: String = canonical
        .display()
        .to_string()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if id.is_empty() {
        id.push('-');
    }
    id
}

/// Crockford base32, the ULID alphabet.
const ULID_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

const ENTROPY_MASK: u128 = (1 << 80) - 1;

/// The last issued id as `(millis, entropy)`, so ids stay strictly
/// increasing inside one process.
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
    let millis = u128::from(u64::try_from(now.as_millis()).unwrap_or(u64::MAX)) & ((1 << 48) - 1);
    let seed = now.as_nanos() as u64 ^ (u64::from(std::process::id()) << 32);
    let entropy = {
        let mut last = LAST_ID.lock().unwrap();
        let entropy = if last.0 == millis {
            (last.1 + 1) & ENTROPY_MASK
        } else {
            (u128::from(splitmix64(seed)) << 16 | u128::from(splitmix64(seed ^ 0xDEAD) >> 48))
                & ENTROPY_MASK
        };
        *last = (millis, entropy);
        entropy
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

        let mut session = store.create(&project, "openai-codex", "gpt-test").unwrap();
        session
            .append(&SessionRecord::Message(text_message(Role::User, "hi")))
            .unwrap();
        session
            .append(&SessionRecord::Message(text_message(
                Role::Assistant,
                "hello",
            )))
            .unwrap();
        session
            .append(&SessionRecord::Usage(Usage {
                input_tokens: 10,
                output_tokens: 2,
                cache_read_tokens: 0,
            }))
            .unwrap();

        let replay = replay(session.path()).unwrap();
        let meta = replay.meta.unwrap();
        assert_eq!(meta.schema, SCHEMA_VERSION);
        assert_eq!(meta.provider, "openai-codex");
        assert_eq!(meta.model, "gpt-test");
        assert_eq!(replay.messages.len(), 2);
        assert_eq!(replay.messages[0], text_message(Role::User, "hi"));
    }

    #[test]
    fn record_shapes_are_stable() {
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
            approved: Some(true),
        };
        let value = serde_json::to_value(&approval).unwrap();
        assert_eq!(value["type"], "approval");
        assert_eq!(value["mode"], "ask");
        assert_eq!(value["decision"], "ask_user");
        assert_eq!(value["approved"], true);

        let usage = SessionRecord::Usage(Usage::default());
        let value = serde_json::to_value(&usage).unwrap();
        assert_eq!(value["type"], "usage");
        assert_eq!(value["input_tokens"], 0);
    }

    #[test]
    fn resume_appends_to_the_same_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(dir.path());
        let project = dir.path().to_path_buf();
        let mut session = store.create(&project, "p", "m").unwrap();
        session
            .append(&SessionRecord::Message(text_message(Role::User, "one")))
            .unwrap();
        let path = session.path().to_path_buf();
        drop(session);

        let (mut resumed, replayed) = Session::resume(&path).unwrap();
        assert_eq!(replayed.messages.len(), 1);
        resumed
            .append(&SessionRecord::Message(text_message(
                Role::Assistant,
                "two",
            )))
            .unwrap();
        let replayed = replay(&path).unwrap();
        assert_eq!(replayed.messages.len(), 2);
    }

    #[test]
    fn unknown_record_types_are_skipped_on_replay() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(dir.path());
        let project = dir.path().to_path_buf();
        let mut session = store.create(&project, "p", "m").unwrap();
        session
            .append(&SessionRecord::Message(text_message(Role::User, "hi")))
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
    fn a_malformed_known_record_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.jsonl");
        std::fs::write(&path, "{\"type\":\"message\",\"role\":\"nobody\"}\n").unwrap();
        let error = replay(&path).unwrap_err();
        assert!(error.to_string().contains("bad message record"));
    }

    #[test]
    fn latest_returns_the_newest_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(dir.path());
        let project = dir.path().to_path_buf();
        assert!(store.latest(&project).unwrap().is_none());

        let first = store.create(&project, "p", "m").unwrap();
        let second = store.create(&project, "p", "m").unwrap();
        let latest = store.latest(&project).unwrap().unwrap();
        assert_eq!(latest, second.path());
        assert_ne!(first.path(), second.path());
    }

    #[test]
    fn delete_means_nothing_remains() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(dir.path());
        let project = dir.path().to_path_buf();
        let session = store.create(&project, "p", "m").unwrap();
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
    fn project_ids_are_readable_and_safe() {
        let id = project_id(Path::new("/does/not/exist/my project"));
        assert_eq!(id, "-does-not-exist-my-project");
        assert!(!id.contains('/'));
    }
}
