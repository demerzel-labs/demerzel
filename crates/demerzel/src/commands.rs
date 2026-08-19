//! The slash-command registry shared by the REPL and the TUI driver.
//! One table names the commands; both interfaces dispatch through
//! [`dispatch`] and apply the returned effect, so the wording and the
//! behavior stay identical. M3 file-based custom commands need a
//! runtime-built table beside this constant one; the dispatch seam
//! stays.

/// One command in the registry: its argument shape and the help
/// line. The usage line starts with the command name; the `dispatch`
/// match is the name-to-handler map, and a test pins every table row
/// to a match arm.
struct Command {
    usage: &'static str,
    help: &'static str,
}

/// The M2 command set. `/quit` stays as the M1 alias of `/exit`.
const COMMANDS: &[Command] = &[
    Command {
        usage: "/help",
        help: "list the commands",
    },
    Command {
        usage: "/model [name]",
        help: "show or switch the session model",
    },
    Command {
        usage: "/new",
        help: "start a fresh session at the next prompt",
    },
    Command {
        usage: "/exit",
        help: "end the session (alias: /quit)",
    },
];

/// The note both interfaces show after `/new`.
pub(crate) const NEW_SESSION_NOTE: &str = "the next prompt starts a fresh session";

/// The note both interfaces show for the current or switched model.
pub(crate) fn model_note(model: &str) -> String {
    format!("model: {model}")
}

/// What one command asks the interface to do.
pub(crate) enum Effect {
    /// End the session.
    Exit,
    /// Show a message; nothing else changes.
    Note(String),
    /// Drop the open session. The next prompt opens a fresh one.
    NewSession,
    /// Switch the session model.
    SetModel(String),
}

/// A line is a command only when its first token after `/` is a bare
/// word, so a prompt that starts with an absolute path reaches the
/// model.
pub(crate) fn slash_command(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix('/')?;
    let mut parts = rest.splitn(2, char::is_whitespace);
    let first = parts.next().unwrap_or("");
    let bare_word = !first.is_empty() && first.chars().all(|c| c.is_ascii_lowercase() || c == '-');
    bare_word.then(|| (first, parts.next().unwrap_or("").trim()))
}

/// Turn one command line into its effect. `None` means the line is
/// not a command and goes to the model. `current_model` fills the
/// bare `/model` reply.
pub(crate) fn dispatch(line: &str, current_model: &str) -> Option<Effect> {
    let (name, args) = slash_command(line)?;
    let effect = match name {
        "exit" | "quit" => Effect::Exit,
        "help" => Effect::Note(help_text()),
        "new" => Effect::NewSession,
        "model" => match args.split_whitespace().collect::<Vec<_>>().as_slice() {
            [] => Effect::Note(model_note(current_model)),
            [model] => Effect::SetModel((*model).to_string()),
            _ => Effect::Note("usage: /model [name]".to_string()),
        },
        other => Effect::Note(unknown_command(other)),
    };
    Some(effect)
}

/// The `/help` reply. Both interfaces show the same words.
fn help_text() -> String {
    let width = COMMANDS
        .iter()
        .map(|command| command.usage.len())
        .max()
        .unwrap_or(0);
    let mut out = String::from("commands:");
    for command in COMMANDS {
        out.push_str(&format!("\n  {:width$}  {}", command.usage, command.help));
    }
    out.push_str(
        "\napproval keys: y runs the call, a grants it for the session \
         (a command grant covers only that exact call), n declines",
    );
    out
}

/// The reply for a command no interface knows.
fn unknown_command(name: &str) -> String {
    format!("unknown command: /{name} — /help lists the commands")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_commands_require_a_bare_word() {
        assert_eq!(slash_command("/exit"), Some(("exit", "")));
        assert_eq!(slash_command("/quit  "), Some(("quit", "")));
        assert_eq!(slash_command("/model  gpt-x "), Some(("model", "gpt-x")));
        assert_eq!(slash_command("/home/user/x.rs:12 panics, fix it"), None);
        assert_eq!(slash_command("/Weird"), None);
        assert_eq!(slash_command("plain prompt"), None);
    }

    #[test]
    fn the_registry_dispatches_every_command() {
        assert!(matches!(dispatch("/exit", "m"), Some(Effect::Exit)));
        assert!(matches!(dispatch("/quit", "m"), Some(Effect::Exit)));
        assert!(matches!(dispatch("/new", "m"), Some(Effect::NewSession)));
        assert!(dispatch("not a command", "m").is_none());

        let Some(Effect::SetModel(model)) = dispatch("/model gpt-x", "m") else {
            panic!("expected a model switch");
        };
        assert_eq!(model, "gpt-x");

        let Some(Effect::Note(note)) = dispatch("/model", "current-model") else {
            panic!("expected the current model");
        };
        assert_eq!(note, "model: current-model");

        let Some(Effect::Note(note)) = dispatch("/model a b", "m") else {
            panic!("expected the usage line");
        };
        assert!(note.starts_with("usage:"));
    }

    #[test]
    fn help_names_every_command_and_unknown_points_to_it() {
        let Some(Effect::Note(help)) = dispatch("/help", "m") else {
            panic!("expected the help text");
        };
        for command in COMMANDS {
            assert!(help.contains(command.usage), "missing {}", command.usage);
        }

        let Some(Effect::Note(note)) = dispatch("/nope", "m") else {
            panic!("expected the unknown reply");
        };
        assert!(note.contains("/help"));
    }

    #[test]
    fn every_table_row_has_a_dispatch_arm() {
        // The table feeds /help; the match feeds behavior. A row
        // without an arm would advertise a command that answers
        // "unknown command".
        for command in COMMANDS {
            let line = command.usage.split_whitespace().next().unwrap();
            let effect = dispatch(line, "m").expect("a table row must parse");
            if let Effect::Note(note) = &effect {
                assert!(
                    !note.contains("unknown command"),
                    "{line} is in the table but not in the dispatch match"
                );
            }
        }
    }
}
