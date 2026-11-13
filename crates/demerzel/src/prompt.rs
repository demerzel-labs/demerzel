//! Prompt composition: the system prompt of one session.
//!
//! [`compose`] runs once per session, at the first prompt, and the
//! engine sends its text unchanged on every model call. The text is
//! a function of the file contents alone: no time, no path, no
//! target. Equal inputs give equal bytes, so a provider's prefix
//! cache serves the next turn and the next session.
//!
//! One prompt serves every target of the session's route. A fallback
//! attempt sends the same text to another adapter, so the composer
//! takes no route and no segment names a provider or a model.
//!
//! The segments, in their contract order:
//!
//! 1. The base prompt: the harness default, or the user's own file.
//! 2. The personality profile.
//! 3. The instruction files of the user and the project.
//! 4. The skill index.
//! 5. The memory recall of the first prompt.
//!
//! The order and the caps are contract. The heading words and the
//! marker words are not.

use std::fs::File;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use demerzel_core::DEFAULT_SYSTEM_PROMPT;

/// The bytes of the personality profile that join the prompt.
pub(crate) const PROFILE_CAP: usize = 8 * 1024;
/// The bytes of one instruction file that join the prompt.
pub(crate) const INSTRUCTION_FILE_CAP: usize = 32 * 1024;
/// The bytes of instruction content that join the prompt, over every
/// file. The caps count file content; the headings and the markers
/// are a fixed overhead outside the count.
pub(crate) const INSTRUCTIONS_CAP: usize = 64 * 1024;

/// The user's profile file, in the user's config directory.
const PROFILE_FILE: &str = "personality.md";
/// The instruction file, in the user's config directory and in the
/// project root.
const INSTRUCTION_FILE: &str = "AGENTS.md";
/// The project's instruction file when the project has no
/// [`INSTRUCTION_FILE`].
const INSTRUCTION_FILE_FALLBACK: &str = "CLAUDE.md";

const PROFILE_HEADING: &str = "# User profile";
const INSTRUCTIONS_HEADING: &str = "# Instructions";
const SKILLS_HEADING: &str = "# Skills";
const RECALL_HEADING: &str = "# Project memory";
const USER_INSTRUCTIONS: &str = "## User instructions";
const PROJECT_INSTRUCTIONS: &str = "## Project instructions";

/// The line after a file cut at its cap.
const FILE_MARKER: &str = "[The harness cut this file at its size limit. The rest is left out.]";
/// Why a file that a cap cut adds nothing: blank lines fill the part
/// inside the cap.
const NO_TEXT: &str = "no text inside the size limit";
/// Why a file after full instructions adds nothing.
const NO_ROOM: &str = "the instructions are at their size limit";

/// The line after instructions cut at the segment cap.
const INSTRUCTIONS_MARKER: &str =
    "[The harness cut the instructions at their size limit. The rest is left out.]";

/// What one composition reads.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Sources<'a> {
    /// The user's base prompt, the content of `prompt.base_file`.
    /// `None` keeps the harness default.
    pub base: Option<&'a str>,
    /// The user's config directory, which holds the profile and the
    /// user's instruction file. `None` reads no user file.
    pub user_dir: Option<&'a Path>,
    /// The project root, which holds the project's instruction file.
    pub project_dir: &'a Path,
}

/// The kind of a file that joined the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Profile,
    Instructions,
}

/// One file that joined the prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Loaded {
    pub kind: Kind,
    pub path: PathBuf,
    /// The bytes of the file's content in the prompt.
    pub bytes: usize,
    /// A cap cut the content.
    pub cut: bool,
}

/// A composed system prompt and what went into it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Composed {
    pub text: String,
    /// The files in the prompt, in prompt order.
    pub files: Vec<Loaded>,
    /// The files that exist and did not join, each with its reason.
    pub skipped: Vec<(PathBuf, String)>,
}

impl Composed {
    /// One line for the user per file that a cap cut and per file
    /// that did not join. An empty list for a prompt that holds
    /// every file whole.
    pub(crate) fn notes(&self) -> Vec<String> {
        let cut = self.files.iter().filter(|file| file.cut).map(|file| {
            format!(
                "the prompt holds the first {} bytes of {}: the rest is over the size limit",
                file.bytes,
                file.path.display()
            )
        });
        let skipped = self
            .skipped
            .iter()
            .map(|(path, reason)| format!("the prompt leaves out {}: {reason}", path.display()));
        cut.chain(skipped).collect()
    }
}

/// The five segments of a system prompt. A segment without content
/// leaves no heading.
#[derive(Debug, Default)]
struct Segments<'a> {
    base: &'a str,
    profile: Option<String>,
    instructions: Option<String>,
    skills: Option<String>,
    recall: Option<String>,
}

impl Segments<'_> {
    /// The prompt: the base as it is, then each segment under its
    /// heading, in the contract order.
    fn join(self) -> String {
        let rest = [
            (PROFILE_HEADING, self.profile),
            (INSTRUCTIONS_HEADING, self.instructions),
            (SKILLS_HEADING, self.skills),
            (RECALL_HEADING, self.recall),
        ];
        // One allocation for the whole prompt: each segment adds its
        // heading, its body, and two blank lines.
        let rest_len: usize = rest
            .iter()
            .filter_map(|(heading, body)| Some(heading.len() + body.as_ref()?.len() + 4))
            .sum();
        let mut text = String::with_capacity(self.base.len() + rest_len);
        text.push_str(self.base);
        for (heading, body) in rest {
            let Some(body) = body else { continue };
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(heading);
            text.push_str("\n\n");
            text.push_str(&body);
        }
        text
    }
}

/// Compose the system prompt of a session that starts with
/// `first_prompt`. A file that is absent adds nothing; a file that
/// cannot join lands in [`Composed::skipped`] and the session goes
/// on without it.
pub(crate) fn compose(sources: Sources<'_>, first_prompt: &str) -> Composed {
    let mut composed = Composed::default();
    let profile = sources
        .user_dir
        .and_then(|dir| profile(&dir.join(PROFILE_FILE), &mut composed));
    let instructions = instructions(&instruction_files(sources), &mut composed);
    // Segment 5 retrieves for the first prompt. Until the memory
    // store exists, the prompt has nothing to retrieve from.
    let _ = first_prompt;
    composed.text = Segments {
        base: sources.base.unwrap_or(DEFAULT_SYSTEM_PROMPT),
        profile,
        instructions,
        skills: None,
        recall: None,
    }
    .join();
    composed
}

/// Read the file `prompt.base_file` names. The content goes out as
/// it is: no trim, no cap, no repair.
pub(crate) fn read_base(path: &Path) -> Result<String, String> {
    let fail = |reason: &dyn std::fmt::Display| {
        format!(
            "cannot read the base prompt `{}` that `prompt.base_file` names: {reason}",
            path.display()
        )
    };
    let metadata = std::fs::metadata(path).map_err(|error| fail(&error))?;
    if !metadata.is_file() {
        return Err(fail(&"not a regular file"));
    }
    std::fs::read_to_string(path).map_err(|error| fail(&error))
}

/// The files of `composed` for a log line or a report: each path
/// with the bytes of its content in the prompt.
pub(crate) fn file_list(composed: &Composed) -> String {
    if composed.files.is_empty() {
        return "no files".to_string();
    }
    let files: Vec<String> = composed
        .files
        .iter()
        .map(|file| {
            let cut = if file.cut { ", cut" } else { "" };
            format!("{} ({} bytes{cut})", file.path.display(), file.bytes)
        })
        .collect();
    format!("files: {}", files.join(", "))
}

/// One instruction file of segment 3.
struct Instruction<'a> {
    /// The line above the file's content.
    label: &'static str,
    path: PathBuf,
    /// The directory the file must resolve inside. A project file
    /// travels with the repository, so a link in it must not pull a
    /// file from outside the project into the prompt. `None` for the
    /// user's own file.
    root: Option<&'a Path>,
}

/// The instruction files of segment 3, in prompt order: the user's
/// file, then the project's.
fn instruction_files(sources: Sources<'_>) -> Vec<Instruction<'_>> {
    let mut files = Vec::with_capacity(2);
    if let Some(dir) = sources.user_dir {
        files.push(Instruction {
            label: USER_INSTRUCTIONS,
            path: dir.join(INSTRUCTION_FILE),
            root: None,
        });
    }
    // The fallback serves only a project with no entry of the first
    // name. An entry that cannot join, such as a directory, still is
    // the project's file, and the session says so.
    let project = sources.project_dir.join(INSTRUCTION_FILE);
    let absent = std::fs::symlink_metadata(&project)
        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound);
    files.push(Instruction {
        label: PROJECT_INSTRUCTIONS,
        path: if absent {
            sources.project_dir.join(INSTRUCTION_FILE_FALLBACK)
        } else {
            project
        },
        root: Some(sources.project_dir),
    });
    files
}

/// The text of a file for the prompt: no byte-order mark, and no
/// white space at its start and its end.
fn content(text: &str) -> &str {
    text.trim_start_matches('\u{feff}').trim()
}

/// Segment 2: the profile, cut at [`PROFILE_CAP`].
fn profile(path: &Path, composed: &mut Composed) -> Option<String> {
    let (text, cut) = read_capped(path, None, PROFILE_CAP, composed)?;
    let content = content(&text);
    if content.is_empty() {
        if cut {
            skip(path, NO_TEXT.to_string(), composed);
        }
        return None;
    }
    composed.files.push(Loaded {
        kind: Kind::Profile,
        path: path.to_path_buf(),
        bytes: content.len(),
        cut,
    });
    let mut body = content.to_string();
    if cut {
        body.push_str("\n\n");
        body.push_str(FILE_MARKER);
    }
    Some(body)
}

/// Segment 3: each file under its label, in order. Each file is cut
/// at [`INSTRUCTION_FILE_CAP`] and the content of all files at
/// [`INSTRUCTIONS_CAP`]. Two paths that resolve to one file count
/// once, at the first.
fn instructions(files: &[Instruction<'_>], composed: &mut Composed) -> Option<String> {
    let mut body = String::new();
    let mut left = INSTRUCTIONS_CAP;
    let mut seen: Vec<PathBuf> = Vec::with_capacity(files.len());
    let mut files = files.iter();
    for file in files.by_ref() {
        let path = &file.path;
        let Some(resolved) = resolve(path, composed) else {
            continue;
        };
        // The file is in the prompt already: nothing to check or say.
        if seen.contains(&resolved) {
            continue;
        }
        let read = regular(path, &resolved, file.root, composed)
            .and_then(|len| read_resolved(path, &resolved, len, INSTRUCTION_FILE_CAP, composed));
        seen.push(resolved);
        let Some((text, cut)) = read else {
            continue;
        };
        let mut content = content(&text);
        if content.is_empty() {
            if cut {
                skip(path, NO_TEXT.to_string(), composed);
            }
            continue;
        }
        let over = content.len() > left;
        if over {
            content = content[..content.floor_char_boundary(left)].trim_end();
        }
        if !content.is_empty() {
            if !body.is_empty() {
                body.push_str("\n\n");
            }
            body.push_str(file.label);
            body.push_str("\n\n");
            body.push_str(content);
            if cut && !over {
                body.push_str("\n\n");
                body.push_str(FILE_MARKER);
            }
            left -= content.len();
            composed.files.push(Loaded {
                kind: Kind::Instructions,
                path: path.clone(),
                bytes: content.len(),
                cut: cut || over,
            });
        } else {
            skip(path, NO_ROOM.to_string(), composed);
        }
        if over {
            // The marker ends the segment: the later files stay out.
            if !body.is_empty() {
                body.push_str("\n\n");
            }
            body.push_str(INSTRUCTIONS_MARKER);
            break;
        }
    }
    // After the cut, each later entry stays out unread, and the
    // session names it.
    for file in files {
        if file.path.symlink_metadata().is_ok() {
            skip(&file.path, NO_ROOM.to_string(), composed);
        }
    }
    (!body.is_empty()).then_some(body)
}

/// Leave `path` out of the prompt, with the reason.
fn skip(path: &Path, reason: String, composed: &mut Composed) {
    log::warn!("the prompt leaves out {}: {reason}", path.display());
    composed.skipped.push((path.to_path_buf(), reason));
}

/// The path that `path` names, with its symlinks resolved. `None`
/// for no entry at the path. A link to no file and a path that does
/// not resolve join [`Composed::skipped`].
fn resolve(path: &Path, composed: &mut Composed) -> Option<PathBuf> {
    match path.canonicalize() {
        Ok(resolved) => Some(resolved),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // An entry that resolves to nothing is a dangling link.
            if path.symlink_metadata().is_ok() {
                skip(path, "a link to no file".to_string(), composed);
            }
            None
        }
        Err(error) => {
            skip(path, error.to_string(), composed);
            None
        }
    }
}

/// The size of the regular file at `resolved`, which `path` names.
/// These paths join [`Composed::skipped`]:
///
/// - A path that resolves outside `root`, when the file has one.
/// - A path that is not a regular file. A pipe at the path would
///   hold the open.
fn regular(
    path: &Path,
    resolved: &Path,
    root: Option<&Path>,
    composed: &mut Composed,
) -> Option<u64> {
    // Both sides compare resolved, so a project behind a symlink
    // still holds its own files.
    if let Some(root) = root {
        match root.canonicalize() {
            Ok(root) if resolved.starts_with(&root) => {}
            Ok(_) => {
                skip(path, "a link out of the project".to_string(), composed);
                return None;
            }
            Err(error) => {
                let reason = format!("the project directory does not resolve: {error}");
                skip(path, reason, composed);
                return None;
            }
        }
    }
    match std::fs::metadata(resolved) {
        Ok(metadata) if metadata.is_file() => Some(metadata.len()),
        Ok(_) => {
            skip(path, "not a regular file".to_string(), composed);
            None
        }
        Err(error) => {
            skip(path, error.to_string(), composed);
            None
        }
    }
}

/// [`resolve`], [`regular`], and [`read_resolved`] for one file.
fn read_capped(
    path: &Path,
    root: Option<&Path>,
    cap: usize,
    composed: &mut Composed,
) -> Option<(String, bool)> {
    let resolved = resolve(path, composed)?;
    let len = regular(path, &resolved, root, composed)?;
    read_resolved(path, &resolved, len, cap, composed)
}

/// Read at most `cap` bytes of the text file at `resolved`, which
/// `path` names and which holds `len` bytes. Returns the text and
/// whether the file holds more. A file that does not read, or whose
/// read part is not UTF-8 text, joins [`Composed::skipped`]. The read
/// stops one byte after the cap, so a large file costs its cap and
/// not its size.
fn read_resolved(
    path: &Path,
    resolved: &Path,
    len: u64,
    cap: usize,
    composed: &mut Composed,
) -> Option<(String, bool)> {
    let limit = cap as u64 + 1;
    let mut bytes = Vec::with_capacity(len.min(limit) as usize);
    let read = File::open(resolved).and_then(|file| file.take(limit).read_to_end(&mut bytes));
    if let Err(error) = read {
        skip(path, error.to_string(), composed);
        return None;
    }
    let cut = bytes.len() > cap;
    // The byte after the cap takes part in the check: the cut can
    // split one character, and only a true first part of a character
    // passes. Any other fault means the file is not text.
    let valid = match std::str::from_utf8(&bytes) {
        Ok(text) => text,
        Err(fault) if cut && fault.error_len().is_none() => {
            std::str::from_utf8(&bytes[..fault.valid_up_to()]).expect("valid up to the fault")
        }
        Err(_) => {
            skip(path, "not UTF-8 text".to_string(), composed);
            return None;
        }
    };
    let end = valid.floor_char_boundary(cap);
    bytes.truncate(end);
    let text = String::from_utf8(bytes).expect("cut at a character boundary");
    Some((text, cut))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A user config directory and a project root.
    struct Bed {
        user: tempfile::TempDir,
        project: tempfile::TempDir,
    }

    impl Bed {
        fn new() -> Self {
            Self {
                user: tempfile::tempdir().unwrap(),
                project: tempfile::tempdir().unwrap(),
            }
        }

        fn sources(&self) -> Sources<'_> {
            Sources {
                base: None,
                user_dir: Some(self.user.path()),
                project_dir: self.project.path(),
            }
        }

        fn compose(&self) -> Composed {
            compose(self.sources(), "first prompt")
        }

        fn user(&self, name: &str, content: &str) -> PathBuf {
            let path = self.user.path().join(name);
            std::fs::write(&path, content).unwrap();
            path
        }

        fn project(&self, name: &str, content: &str) -> PathBuf {
            let path = self.project.path().join(name);
            std::fs::write(&path, content).unwrap();
            path
        }

        /// One project file per name, each `len` bytes of its name's
        /// digit, as the instruction files of a segment.
        fn files<const N: usize>(&self, names: [&str; N], len: usize) -> Vec<Instruction<'_>> {
            names
                .into_iter()
                .map(|name| Instruction {
                    label: "## File",
                    path: self.project(name, &name.repeat(len)),
                    root: Some(self.project.path()),
                })
                .collect()
        }

        /// The paths that did not join the prompt.
        fn skipped(&self) -> Vec<PathBuf> {
            let composed = self.compose();
            composed.skipped.into_iter().map(|(path, _)| path).collect()
        }
    }

    /// The offset of `needle` in `text`; the test fails without it.
    fn at(text: &str, needle: &str) -> usize {
        text.find(needle)
            .unwrap_or_else(|| panic!("`{needle}` is not in the prompt"))
    }

    #[test]
    fn the_five_segments_join_in_the_contract_order() {
        let text = Segments {
            base: "BASE",
            profile: Some("PROFILE".into()),
            instructions: Some("INSTRUCTIONS".into()),
            skills: Some("SKILLS".into()),
            recall: Some("RECALL".into()),
        }
        .join();
        let order = ["BASE", "PROFILE", "INSTRUCTIONS", "SKILLS", "RECALL"].map(|s| at(&text, s));
        assert!(order.is_sorted(), "{text}");
        assert!(text.starts_with("BASE"));
    }

    #[test]
    fn a_project_without_files_sends_the_base_prompt_alone() {
        let bed = Bed::new();
        let composed = bed.compose();
        assert_eq!(composed.text, DEFAULT_SYSTEM_PROMPT);
        assert!(composed.files.is_empty());
        assert!(composed.skipped.is_empty());
    }

    #[test]
    fn the_profile_comes_before_the_user_file_and_the_project_file_last() {
        let bed = Bed::new();
        bed.user(PROFILE_FILE, "terse replies\n");
        bed.user(INSTRUCTION_FILE, "user rule\n");
        bed.project(INSTRUCTION_FILE, "project rule\n");
        let composed = bed.compose();
        let text = &composed.text;
        assert!(text.starts_with(DEFAULT_SYSTEM_PROMPT));
        let order = ["terse replies", "user rule", "project rule"].map(|s| at(text, s));
        assert!(order.is_sorted(), "{text}");
        let kinds: Vec<Kind> = composed.files.iter().map(|file| file.kind).collect();
        assert_eq!(
            kinds,
            [Kind::Profile, Kind::Instructions, Kind::Instructions]
        );
    }

    #[test]
    fn equal_inputs_compose_equal_bytes() {
        let bed = Bed::new();
        bed.user(PROFILE_FILE, "terse replies\n");
        bed.project(INSTRUCTION_FILE, "project rule\n");
        let first = bed.compose();
        // The first prompt and the run do not enter segments 1 to 3.
        let second = compose(bed.sources(), "another first prompt");
        assert_eq!(first.text, second.text);
    }

    #[test]
    fn a_replaced_base_prompt_goes_out_as_it_is() {
        let bed = Bed::new();
        let base = "  My own base.\n\nKeep the blank line.  \n";
        let mut sources = bed.sources();
        sources.base = Some(base);
        assert_eq!(compose(sources, "p").text, base);

        // With instructions, the base still leads, byte for byte.
        bed.project(INSTRUCTION_FILE, "project rule\n");
        let text = compose(sources, "p").text;
        assert!(text.starts_with(base));
        assert!(text.ends_with("project rule"));

        // An empty base leaves the segments alone.
        sources.base = Some("");
        assert!(compose(sources, "p").text.starts_with(INSTRUCTIONS_HEADING));
    }

    #[test]
    fn the_fallback_file_serves_only_a_project_without_the_instruction_file() {
        let bed = Bed::new();
        bed.project(INSTRUCTION_FILE_FALLBACK, "fallback rule\n");
        assert!(bed.compose().text.contains("fallback rule"));

        bed.project(INSTRUCTION_FILE, "project rule\n");
        let text = bed.compose().text;
        assert!(text.contains("project rule"));
        assert!(!text.contains("fallback rule"));
    }

    #[test]
    fn a_file_over_its_cap_is_cut_with_a_marker() {
        let bed = Bed::new();
        let path = bed.project(INSTRUCTION_FILE, &"7".repeat(INSTRUCTION_FILE_CAP + 100));
        let composed = bed.compose();
        assert_eq!(
            composed.files,
            [Loaded {
                kind: Kind::Instructions,
                path,
                bytes: INSTRUCTION_FILE_CAP,
                cut: true,
            }]
        );
        let text = &composed.text;
        assert_eq!(text.matches('7').count(), INSTRUCTION_FILE_CAP);
        assert!(at(text, FILE_MARKER) > text.rfind('7').unwrap());
    }

    #[test]
    fn a_file_at_its_cap_joins_whole_without_a_marker() {
        let bed = Bed::new();
        bed.project(INSTRUCTION_FILE, &"7".repeat(INSTRUCTION_FILE_CAP));
        let composed = bed.compose();
        assert!(!composed.files[0].cut);
        assert_eq!(composed.files[0].bytes, INSTRUCTION_FILE_CAP);
        assert!(!composed.text.contains(FILE_MARKER));
    }

    #[test]
    fn a_cut_keeps_whole_characters() {
        let bed = Bed::new();
        // Two bytes per letter, one byte of offset: the cap falls
        // inside a letter.
        let content = format!("a{}", "é".repeat(INSTRUCTION_FILE_CAP));
        bed.project(INSTRUCTION_FILE, &content);
        let composed = bed.compose();
        assert!(composed.skipped.is_empty(), "{:?}", composed.skipped);
        assert_eq!(composed.files[0].bytes, INSTRUCTION_FILE_CAP - 1);
        assert!(composed.files[0].cut);
    }

    #[test]
    fn the_profile_is_cut_at_its_own_cap_with_a_marker() {
        let bed = Bed::new();
        bed.user(PROFILE_FILE, &"p".repeat(PROFILE_CAP * 2));
        let composed = bed.compose();
        assert_eq!(composed.files[0].bytes, PROFILE_CAP);
        assert!(composed.files[0].cut);
        assert!(composed.text.ends_with(FILE_MARKER));
    }

    #[test]
    fn the_instructions_are_cut_at_the_segment_cap_with_a_marker() {
        let bed = Bed::new();
        let files = bed.files(["1", "2", "3", "4"], INSTRUCTION_FILE_CAP - 1024);
        let mut composed = Composed::default();
        let body = instructions(&files, &mut composed).unwrap();

        // Two files join whole, the third up to the cap, the fourth
        // not at all.
        let bytes: Vec<usize> = composed.files.iter().map(|file| file.bytes).collect();
        assert_eq!(
            bytes,
            [
                INSTRUCTION_FILE_CAP - 1024,
                INSTRUCTION_FILE_CAP - 1024,
                2048
            ]
        );
        // The file after the cut is named, so the session can say so.
        let skipped: Vec<&Path> = composed.skipped.iter().map(|(path, _)| &**path).collect();
        assert_eq!(skipped, [&*files[3].path]);
        assert_eq!(bytes.iter().sum::<usize>(), INSTRUCTIONS_CAP);
        assert!(composed.files[2].cut);
        assert_eq!(body.matches('3').count(), 2048);
        assert!(!body.contains('4'));
        assert!(body.ends_with(INSTRUCTIONS_MARKER));
        assert!(!body.contains(FILE_MARKER));
    }

    #[test]
    fn instructions_that_fill_the_cap_leave_the_next_file_out_with_a_marker() {
        let bed = Bed::new();
        let files = bed.files(["1", "2", "3"], INSTRUCTION_FILE_CAP);
        let mut composed = Composed::default();
        let body = instructions(&files, &mut composed).unwrap();
        assert_eq!(composed.files.len(), 2);
        assert!(composed.files.iter().all(|file| !file.cut));
        assert!(!body.contains('3'));
        assert!(body.ends_with(INSTRUCTIONS_MARKER));
        // The file with no room is named, so the session can say so.
        let skipped: Vec<&Path> = composed.skipped.iter().map(|(path, _)| &**path).collect();
        assert_eq!(skipped, [&*files[2].path]);
    }

    #[cfg(unix)]
    #[test]
    fn two_paths_to_one_file_count_once() {
        let bed = Bed::new();
        let project = bed.project(INSTRUCTION_FILE, "one shared rule\n");
        std::os::unix::fs::symlink(&project, bed.user.path().join(INSTRUCTION_FILE)).unwrap();
        let composed = bed.compose();
        assert_eq!(composed.text.matches("one shared rule").count(), 1);
        assert_eq!(composed.files.len(), 1);
        assert!(composed.skipped.is_empty());
    }

    #[test]
    fn a_path_that_is_no_text_file_stays_out_with_a_reason() {
        let bed = Bed::new();
        std::fs::create_dir(bed.user.path().join(INSTRUCTION_FILE)).unwrap();
        let binary = bed.project(INSTRUCTION_FILE, "");
        std::fs::write(&binary, [0xff, 0xfe, 0x00, 0x41]).unwrap();
        let composed = bed.compose();
        assert_eq!(composed.text, DEFAULT_SYSTEM_PROMPT);
        assert_eq!(
            bed.skipped(),
            [bed.user.path().join(INSTRUCTION_FILE), binary]
        );
        // Each file that stays out gives the user one line.
        assert_eq!(composed.notes().len(), 2);
    }

    #[test]
    fn a_project_entry_that_cannot_join_still_shadows_the_fallback_file() {
        let bed = Bed::new();
        std::fs::create_dir(bed.project.path().join(INSTRUCTION_FILE)).unwrap();
        bed.project(INSTRUCTION_FILE_FALLBACK, "fallback rule\n");
        assert_eq!(bed.compose().text, DEFAULT_SYSTEM_PROMPT);
        assert_eq!(bed.skipped(), [bed.project.path().join(INSTRUCTION_FILE)]);
    }

    #[cfg(unix)]
    #[test]
    fn a_project_link_out_of_the_project_stays_out() {
        let bed = Bed::new();
        let outside = bed.user("secret.txt", "OUTSIDE-TEXT\n");
        let link = bed.project.path().join(INSTRUCTION_FILE);
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let composed = bed.compose();
        assert_eq!(composed.text, DEFAULT_SYSTEM_PROMPT);
        assert_eq!(bed.skipped(), std::slice::from_ref(&link));

        // A link inside the project joins, and so does the user's own
        // link to a file anywhere.
        std::fs::remove_file(&link).unwrap();
        bed.project("rules.md", "INSIDE-TEXT\n");
        std::os::unix::fs::symlink("rules.md", &link).unwrap();
        std::os::unix::fs::symlink(&outside, bed.user.path().join(INSTRUCTION_FILE)).unwrap();
        let composed = bed.compose();
        assert!(composed.skipped.is_empty(), "{:?}", composed.skipped);
        assert!(at(&composed.text, "OUTSIDE-TEXT") < at(&composed.text, "INSIDE-TEXT"));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_to_no_file_stays_out_with_a_reason_and_shadows_the_fallback_file() {
        let bed = Bed::new();
        let link = bed.project.path().join(INSTRUCTION_FILE);
        std::os::unix::fs::symlink("gone.md", &link).unwrap();
        bed.project(INSTRUCTION_FILE_FALLBACK, "fallback rule\n");
        assert_eq!(bed.compose().text, DEFAULT_SYSTEM_PROMPT);
        assert_eq!(bed.skipped(), [link]);
    }

    #[cfg(unix)]
    #[test]
    fn a_project_link_to_the_users_own_file_counts_once_without_a_note() {
        let bed = Bed::new();
        let user = bed.user(INSTRUCTION_FILE, "one shared rule\n");
        std::os::unix::fs::symlink(&user, bed.project.path().join(INSTRUCTION_FILE)).unwrap();
        let composed = bed.compose();
        assert_eq!(composed.text.matches("one shared rule").count(), 1);
        assert!(composed.notes().is_empty(), "{:?}", composed.notes());
    }

    #[test]
    fn a_cut_file_with_no_text_inside_its_cap_stays_out_with_a_reason() {
        let bed = Bed::new();
        let content = format!("{}RULE", "\n".repeat(INSTRUCTION_FILE_CAP + 1));
        let path = bed.project(INSTRUCTION_FILE, &content);
        assert_eq!(bed.compose().text, DEFAULT_SYSTEM_PROMPT);
        assert_eq!(bed.skipped(), [path]);
    }

    #[test]
    fn only_a_true_split_character_passes_the_cut() {
        let bed = Bed::new();
        // The first byte of a two-byte character sits at the last
        // place inside the cap, and no second byte follows it.
        let mut content = "7".repeat(INSTRUCTION_FILE_CAP - 1).into_bytes();
        content.extend([0xc3, b'A', b'A']);
        let path = bed.project(INSTRUCTION_FILE, "");
        std::fs::write(&path, content).unwrap();
        assert_eq!(bed.skipped(), [path]);
    }

    #[test]
    fn a_byte_order_mark_stays_out_of_the_prompt() {
        let bed = Bed::new();
        bed.project(INSTRUCTION_FILE, "\u{feff}project rule\n");
        let composed = bed.compose();
        assert!(!composed.text.contains('\u{feff}'));
        assert_eq!(composed.files[0].bytes, "project rule".len());
    }

    #[test]
    fn a_cut_file_gives_the_user_a_note_and_a_whole_file_gives_none() {
        let bed = Bed::new();
        bed.project(INSTRUCTION_FILE, "project rule\n");
        assert!(bed.compose().notes().is_empty());
        bed.project(INSTRUCTION_FILE, &"7".repeat(INSTRUCTION_FILE_CAP + 1));
        assert_eq!(bed.compose().notes().len(), 1);
    }

    #[test]
    fn an_empty_file_adds_no_heading() {
        let bed = Bed::new();
        bed.user(PROFILE_FILE, "\n\n");
        bed.project(INSTRUCTION_FILE, "  \n");
        assert_eq!(bed.compose().text, DEFAULT_SYSTEM_PROMPT);
    }

    #[test]
    fn a_missing_base_file_names_the_key_and_the_path() {
        let bed = Bed::new();
        let path = bed.user.path().join("base.md");
        let error = read_base(&path).unwrap_err();
        assert!(error.contains("prompt.base_file"), "{error}");
        assert!(error.contains("base.md"), "{error}");
        std::fs::write(&path, "mine\n").unwrap();
        assert_eq!(read_base(&path).unwrap(), "mine\n");
    }
}
