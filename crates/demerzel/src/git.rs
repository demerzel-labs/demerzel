//! The git branch for the status, read from the repository's own
//! files. The read runs no process and adds no dependency: it walks
//! up from a directory to the first `.git`, follows a worktree's
//! `.git` file to its git directory, and parses `HEAD`.

use std::fs::{self, OpenOptions};
use std::io::Read as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

/// Characters of a detached head's hash the status shows.
const SHORT_HASH: usize = 7;

/// Bytes a read of `HEAD` or of a worktree's `.git` file takes at
/// most. A real one holds one short line; the cap bounds a stray
/// file.
const READ_BYTES: u64 = 4096;

/// The branch checked out in the repository that holds `dir`: the
/// name after `ref: refs/heads/` in `HEAD`, or the first seven
/// characters of a detached head's hash. `None` when no directory at
/// or above `dir` holds a `.git`, or when the first one's `HEAD`
/// reads as neither. The name is the file's text as is; the status
/// cuts and sanitizes it. `dir` must be absolute: the walk cannot
/// leave a relative root. The call blocks on the file reads.
pub fn git_branch(dir: &Path) -> Option<String> {
    let head = read_line(&git_dir(dir)?.join("HEAD"))?;
    parse_head(&head).map(str::to_owned)
}

/// The git directory of the first `.git` at or above `dir`: the
/// `.git` directory itself, or for a worktree the directory its
/// `.git` file names after `gitdir: `, taken relative to the file's
/// directory. The path is the line's text as is, the way git reads
/// it: a trailing space is part of it. The first `.git` decides: a
/// file that names no directory ends the walk.
fn git_dir(dir: &Path) -> Option<PathBuf> {
    for ancestor in dir.ancestors() {
        let dot_git = ancestor.join(".git");
        let Ok(meta) = fs::metadata(&dot_git) else {
            continue;
        };
        if meta.is_dir() {
            return Some(dot_git);
        }
        let line = read_line(&dot_git)?;
        let target = line.strip_prefix("gitdir: ")?;
        if target.is_empty() {
            return None;
        }
        return Some(ancestor.join(target));
    }
    None
}

/// The first line of the regular file at `path`, out of its first
/// [`READ_BYTES`]. A FIFO or a device is no file: a read of one
/// could block for good. The open takes `O_NONBLOCK`, so it returns
/// on a FIFO with no writer, and the type check runs on the open
/// handle, so no swap between a check and the open can pass it.
fn read_line(path: &Path) -> Option<String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(READ_BYTES).read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    Some(text.lines().next().unwrap_or_default().to_owned())
}

/// What `HEAD` names: a branch, or a detached head's short hash.
/// The name is the line's text as is: git forbids a space in a ref
/// but not every Unicode blank, so nothing trims it.
fn parse_head(head: &str) -> Option<&str> {
    if let Some(name) = head.strip_prefix("ref: refs/heads/") {
        return (!name.is_empty()).then_some(name);
    }
    let detached = head.len() >= SHORT_HASH && head.bytes().all(|byte| byte.is_ascii_hexdigit());
    detached.then(|| &head[..SHORT_HASH])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(head: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".git/HEAD"), head).unwrap();
        dir
    }

    #[test]
    fn a_branch_reads_from_head_in_the_directory_or_above_it() {
        let dir = repo("ref: refs/heads/feature/status\n");
        assert_eq!(git_branch(dir.path()).as_deref(), Some("feature/status"));
        let nested = dir.path().join("crates/deep");
        fs::create_dir_all(&nested).unwrap();
        assert_eq!(git_branch(&nested).as_deref(), Some("feature/status"));
    }

    #[test]
    fn a_detached_head_shows_seven_hash_characters() {
        let dir = repo("0123456789abcdef0123456789abcdef01234567\n");
        assert_eq!(git_branch(dir.path()).as_deref(), Some("0123456"));
    }

    #[test]
    fn no_repository_and_a_head_that_names_neither_read_as_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(git_branch(dir.path()), None);
        let dir = repo("ref: refs/remotes/origin/main\n");
        assert_eq!(git_branch(dir.path()), None);
        let dir = repo("");
        assert_eq!(git_branch(dir.path()), None);
    }

    #[test]
    fn a_worktree_follows_its_git_file_to_the_git_directory() {
        let main = repo("ref: refs/heads/main\n");
        let linked = main.path().join(".git/worktrees/wt");
        fs::create_dir_all(&linked).unwrap();
        fs::write(linked.join("HEAD"), "ref: refs/heads/wt-branch\n").unwrap();
        // An absolute gitdir, and a relative one against the file's
        // directory.
        let worktree = tempfile::tempdir().unwrap();
        fs::write(
            worktree.path().join(".git"),
            format!("gitdir: {}\n", linked.display()),
        )
        .unwrap();
        assert_eq!(git_branch(worktree.path()).as_deref(), Some("wt-branch"));
        let sibling = main.path().join("sibling");
        fs::create_dir(&sibling).unwrap();
        fs::write(sibling.join(".git"), "gitdir: ../.git/worktrees/wt\n").unwrap();
        assert_eq!(git_branch(&sibling).as_deref(), Some("wt-branch"));
        // A .git file that names no directory ends the walk at it,
        // and an empty gitdir names none: the project's own HEAD
        // file is no branch.
        fs::write(sibling.join("HEAD"), "ref: refs/heads/decoy\n").unwrap();
        for text in [
            "not a worktree\n",
            "gitdir:\n",
            "gitdir: \n",
            "gitdir:../.git\n",
        ] {
            fs::write(sibling.join(".git"), text).unwrap();
            assert_eq!(git_branch(&sibling), None, "{text:?}");
        }
    }

    #[test]
    fn git_paths_and_refs_keep_their_blanks() {
        // A git directory whose name ends in a space, and a branch
        // whose name ends in a no-break space: both valid to git.
        let main = repo("ref: refs/heads/main\n");
        let linked = main.path().join(".git/worktrees/wt ");
        fs::create_dir_all(&linked).unwrap();
        fs::write(linked.join("HEAD"), "ref: refs/heads/wt\u{a0}\n").unwrap();
        let sibling = main.path().join("sibling");
        fs::create_dir(&sibling).unwrap();
        fs::write(sibling.join(".git"), "gitdir: ../.git/worktrees/wt \n").unwrap();
        assert_eq!(git_branch(&sibling).as_deref(), Some("wt\u{a0}"));
    }

    #[test]
    fn the_reads_take_one_line_and_stop_at_the_cap() {
        // A second line in a .git file stays out of the path.
        let main = repo("ref: refs/heads/main\n");
        let linked = main.path().join(".git/worktrees/wt");
        fs::create_dir_all(&linked).unwrap();
        fs::write(linked.join("HEAD"), "ref: refs/heads/wt-branch\r\n").unwrap();
        let sibling = main.path().join("sibling");
        fs::create_dir(&sibling).unwrap();
        fs::write(
            sibling.join(".git"),
            "gitdir: ../.git/worktrees/wt\nsecond line\n",
        )
        .unwrap();
        assert_eq!(git_branch(&sibling).as_deref(), Some("wt-branch"));
        // A stray file of any size costs the cap at most.
        let long = format!("ref: refs/heads/{}", "a".repeat(100_000));
        let dir = repo(&long);
        let name = git_branch(dir.path()).unwrap();
        assert!(name.len() <= READ_BYTES as usize, "{}", name.len());
        assert!(name.bytes().all(|byte| byte == b'a'));
    }

    #[test]
    fn a_git_entry_that_is_no_file_and_no_directory_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = std::ffi::CString::new(dir.path().join(".git").to_str().unwrap()).unwrap();
        // A FIFO with no writer would block a read for good.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert_eq!(git_branch(dir.path()), None);
    }
}
