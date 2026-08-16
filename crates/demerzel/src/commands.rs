//! Slash-command parsing shared by the REPL and the TUI driver. The
//! M3 registry of custom commands grows here.

/// A line is a command only when its first token after `/` is a bare
/// word, so a prompt that starts with an absolute path reaches the
/// model.
pub(crate) fn slash_command(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('/')?;
    let first = rest.split_whitespace().next().unwrap_or("");
    let bare_word = !first.is_empty() && first.chars().all(|c| c.is_ascii_lowercase() || c == '-');
    bare_word.then_some(first)
}

/// The reply for a command no interface knows. Both interfaces show
/// the same words.
pub(crate) fn unknown_command(name: &str) -> String {
    format!(
        "unknown command: /{name} — /exit and /quit work today; \
         more commands arrive in M2"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_commands_require_a_bare_word() {
        assert_eq!(slash_command("/exit"), Some("exit"));
        assert_eq!(slash_command("/quit  "), Some("quit"));
        assert_eq!(slash_command("/help me"), Some("help"));
        assert_eq!(slash_command("/home/user/x.rs:12 panics, fix it"), None);
        assert_eq!(slash_command("/Weird"), None);
        assert_eq!(slash_command("plain prompt"), None);
    }
}
