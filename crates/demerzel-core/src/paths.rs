//! XDG path resolution, shared by every crate that stores state.

use std::io;
use std::path::{Path, PathBuf};

/// The Demerzel data directory: `$XDG_DATA_HOME/demerzel`, or
/// `~/.local/share/demerzel`. An empty `XDG_DATA_HOME` counts as
/// unset, per the XDG base-directory rules.
pub fn data_dir() -> io::Result<PathBuf> {
    let base = match std::env::var_os("XDG_DATA_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => {
            let home =
                std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME is not set"))?;
            Path::new(&home).join(".local/share")
        }
    };
    Ok(base.join("demerzel"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_ends_with_demerzel() {
        // HOME is set in every test environment we support.
        let dir = data_dir().unwrap();
        assert!(dir.ends_with("demerzel"));
    }
}
