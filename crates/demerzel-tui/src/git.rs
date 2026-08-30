//! The git branch for the status, read from the repository's own
//! files. The read runs no process and adds no dependency: it walks
//! up from a directory to the first `.git`, follows a worktree's
//! `.git` file to its git directory, and parses `HEAD`.

use std::fs;
use std::path::{Path, PathBuf};

/// Characters of a detached head's hash the status shows.
const SHORT_HASH: usize = 7;

/// The branch checked out in the repository that holds `dir`: the
/// name after `ref: refs/heads/` in `HEAD`, or the first seven
/// characters of a detached head's hash. `None` when no directory at
/// or above `dir` holds a `.git`, or when the first one's `HEAD`
/// reads as neither. The name is the file's text as is; the status
/// cuts and sanitizes it.
pub fn git_branch(dir: &Path) -> Option<String> {
    let head = fs::read_to_string(git_dir(dir)?.join("HEAD")).ok()?;
    parse_head(&head).map(str::to_owned)
}

/// The git directory of the first `.git` at or above `dir`: the
/// `.git` directory itself, or for a worktree the directory its
/// `.git` file names on the `gitdir:` line, taken relative to the
/// file's directory. The first `.git` decides: a file that names no
/// directory ends the walk.
fn git_dir(dir: &Path) -> Option<PathBuf> {
    for ancestor in dir.ancestors() {
        let dot_git = ancestor.join(".git");
        let Ok(meta) = fs::metadata(&dot_git) else {
            continue;
        };
        if meta.is_dir() {
            return Some(dot_git);
        }
        let file = fs::read_to_string(&dot_git).ok()?;
        let target = file.strip_prefix("gitdir:")?.trim();
        return Some(ancestor.join(target));
    }
    None
}

/// What `HEAD` names: a branch, or a detached head's short hash.
fn parse_head(head: &str) -> Option<&str> {
    let head = head.trim();
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
        // A .git file that names no directory ends the walk at it.
        fs::write(sibling.join(".git"), "not a worktree\n").unwrap();
        assert_eq!(git_branch(&sibling), None);
    }
}
