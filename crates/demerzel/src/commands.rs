//! The slash-command registry shared by the REPL and the TUI driver.
//! One table names the commands; both interfaces dispatch through
//! [`dispatch`] and apply the returned effect, so the wording and the
//! behavior stay identical. Runtime-built commands can add another
//! table beside this constant one; dispatch stays the join point.

use demerzel_core::{ApprovalMode, Effort};

use crate::SessionSettings;

/// One command in the registry: its argument shape and the help
/// line. The usage line starts with the command name; the `dispatch`
/// match is the name-to-handler map, and a test pins every table row
/// to a match arm.
struct Command {
    usage: &'static str,
    help: &'static str,
}

/// The built-in command set. `/quit` stays as an alias of `/exit`.
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
        usage: "/effort [level]",
        help: "show or set the reasoning effort: none, minimal, low, medium, high, xhigh, max, ultra",
    },
    Command {
        usage: "/approval [mode]",
        help: "show or set the approval mode: read-only, ask, auto",
    },
    Command {
        usage: "/new",
        help: "start a fresh session at the next prompt",
    },
    Command {
        usage: "/session",
        help: "show the open session's id and file",
    },
    Command {
        usage: "/exit",
        help: "end the session (alias: /quit)",
    },
];

/// The registry as the full-screen interface's completion list: the
/// same usage and help lines `/help` shows.
pub(crate) fn specs() -> Vec<demerzel_tui::CommandSpec> {
    COMMANDS
        .iter()
        .map(|command| demerzel_tui::CommandSpec::new(command.usage, command.help))
        .collect()
}

/// The note both interfaces show after `/new`.
pub(crate) const NEW_SESSION_NOTE: &str = "the next prompt starts a fresh session";

/// The note both interfaces show for the current or switched model.
pub(crate) fn model_note(model: &str) -> String {
    format!("model: {model}")
}

/// The note both interfaces show for the current or new effort.
pub(crate) fn effort_note(effort: Effort) -> String {
    format!("effort: {effort}")
}

/// The note both interfaces show for the current or new approval
/// mode.
pub(crate) fn approval_note(mode: ApprovalMode) -> String {
    format!("approval: {mode}")
}

/// The `/session` reply: the open session's id and file, or the
/// word that none is open yet.
pub(crate) fn session_note(session: Option<(&str, &std::path::Path)>) -> String {
    match session {
        Some((id, path)) => format!("session: {id}\n  {}", path.display()),
        None => "no open session; the next prompt opens one".to_string(),
    }
}

/// The line both interfaces print on stderr when a run that opened a
/// session ends: the session's id and the command that resumes it.
/// `-c` when the session is the project's latest, the common case;
/// `--session <id>` when a newer one exists, so the line never names
/// a command that opens another session.
pub(crate) fn exit_line(id: &str, latest: bool) -> String {
    if latest {
        format!("session {id} · resume with demerzel -c")
    } else {
        format!("session {id} · resume with demerzel --session {id}")
    }
}

/// What one command asks the interface to do.
pub(crate) enum Effect {
    /// End the session.
    Exit,
    /// Show a message; nothing else changes.
    Note(String),
    /// Drop the open session. The next prompt opens a fresh one.
    NewSession,
    /// Show the open session's id and file.
    Session,
    /// Switch the session model.
    SetModel(String),
    /// Set the reasoning effort for the turns that follow.
    SetEffort(Effort),
    /// Set the approval mode for the turns that follow.
    SetApproval(ApprovalMode),
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
/// not a command and goes to the model. `current` fills the bare
/// `/model`, `/effort`, and `/approval` replies.
pub(crate) fn dispatch(line: &str, current: &SessionSettings) -> Option<Effect> {
    let (name, args) = slash_command(line)?;
    let effect = match name {
        "exit" | "quit" => Effect::Exit,
        "help" => Effect::Note(help_text()),
        "new" => Effect::NewSession,
        "session" => Effect::Session,
        "model" => {
            // Two probes decide the shape; a pasted flood of tokens
            // must not build a vector of itself.
            let mut tokens = args.split_whitespace();
            match (tokens.next(), tokens.next()) {
                (None, _) => Effect::Note(model_note(&current.model)),
                (Some(model), None) => Effect::SetModel(model.to_string()),
                _ => Effect::Note("usage: /model [name]".to_string()),
            }
        }
        "effort" => {
            let mut tokens = args.split_whitespace();
            match (tokens.next(), tokens.next()) {
                (None, _) => Effect::Note(effort_note(current.effort)),
                (Some(level), None) => match level.parse() {
                    Ok(effort) => Effect::SetEffort(effort),
                    Err(_) => Effect::Note(format!("usage: /effort [{}]", Effort::NAMES)),
                },
                _ => Effect::Note(format!("usage: /effort [{}]", Effort::NAMES)),
            }
        }
        "approval" => {
            let mut tokens = args.split_whitespace();
            match (tokens.next(), tokens.next()) {
                (None, _) => Effect::Note(approval_note(current.approval)),
                (Some(mode), None) => match mode.parse() {
                    Ok(mode) => Effect::SetApproval(mode),
                    Err(_) => Effect::Note(format!("usage: /approval [{}]", ApprovalMode::NAMES)),
                },
                _ => Effect::Note(format!("usage: /approval [{}]", ApprovalMode::NAMES)),
            }
        }
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
         (a command grant covers only that exact call), n declines\
         \nShift-Tab cycles the approval mode in the full-screen interface",
    );
    out
}

/// The reply for a command no interface knows.
fn unknown_command(name: &str) -> String {
    format!("unknown command: /{name} — /help lists the commands")
}

#[cfg(test)]
mod tests {
    fn settings(model: &str, effort: Effort, approval: ApprovalMode) -> SessionSettings {
        SessionSettings {
            model: model.to_string(),
            effort,
            approval,
        }
    }

    use super::*;

    #[test]
    fn slash_commands_require_a_bare_word() {
        assert_eq!(slash_command("/exit"), Some(("exit", "")));
        assert_eq!(slash_command("/quit  "), Some(("quit", "")));
        assert_eq!(slash_command("/model  gpt-x "), Some(("model", "gpt-x")));
        assert_eq!(
            slash_command("/workspace/example.rs:12 panics, fix it"),
            None
        );
        assert_eq!(slash_command("/Weird"), None);
        assert_eq!(slash_command("plain prompt"), None);
    }

    #[test]
    fn the_registry_dispatches_every_command() {
        assert!(matches!(
            dispatch("/exit", &settings("m", Effort::Medium, ApprovalMode::Ask)),
            Some(Effect::Exit)
        ));
        assert!(matches!(
            dispatch("/quit", &settings("m", Effort::Medium, ApprovalMode::Ask)),
            Some(Effect::Exit)
        ));
        assert!(matches!(
            dispatch("/new", &settings("m", Effort::Medium, ApprovalMode::Ask)),
            Some(Effect::NewSession)
        ));
        assert!(matches!(
            dispatch(
                "/session",
                &settings("m", Effort::Medium, ApprovalMode::Ask)
            ),
            Some(Effect::Session)
        ));
        assert!(
            dispatch(
                "not a command",
                &settings("m", Effort::Medium, ApprovalMode::Ask)
            )
            .is_none()
        );

        let Some(Effect::SetModel(model)) = dispatch(
            "/model gpt-x",
            &settings("m", Effort::Medium, ApprovalMode::Ask),
        ) else {
            panic!("expected a model switch");
        };
        assert_eq!(model, "gpt-x");

        let Some(Effect::SetEffort(effort)) = dispatch(
            "/effort high",
            &settings("m", Effort::Medium, ApprovalMode::Ask),
        ) else {
            panic!("expected an effort change");
        };
        assert_eq!(effort, Effort::High);
        let Some(Effect::Note(note)) =
            dispatch("/effort", &settings("m", Effort::Low, ApprovalMode::Ask))
        else {
            panic!("expected the current effort");
        };
        assert_eq!(note, "effort: low");
        let Some(Effect::SetEffort(effort)) = dispatch(
            "/effort xhigh",
            &settings("m", Effort::Low, ApprovalMode::Ask),
        ) else {
            panic!("expected an effort change");
        };
        assert_eq!(effort, Effort::XHigh);
        let Some(Effect::Note(note)) = dispatch(
            "/effort extreme",
            &settings("m", Effort::Low, ApprovalMode::Ask),
        ) else {
            panic!("expected usage");
        };
        assert!(note.starts_with("usage: /effort"));
        let Some(Effect::Note(note)) = dispatch(
            "/model",
            &settings("current-model", Effort::Medium, ApprovalMode::Ask),
        ) else {
            panic!("expected the current model");
        };
        assert_eq!(note, "model: current-model");

        let Some(Effect::Note(note)) = dispatch(
            "/model a b",
            &settings("m", Effort::Medium, ApprovalMode::Ask),
        ) else {
            panic!("expected the usage line");
        };
        assert!(note.starts_with("usage:"));
    }

    #[test]
    fn approval_shows_sets_and_rejects_a_stranger() {
        let Some(Effect::Note(note)) =
            dispatch("/approval", &settings("m", Effort::Low, ApprovalMode::Auto))
        else {
            panic!("bare /approval is a note");
        };
        assert_eq!(note, "approval: auto");
        let Some(Effect::SetApproval(mode)) = dispatch(
            "/approval read-only",
            &settings("m", Effort::Low, ApprovalMode::Ask),
        ) else {
            panic!("/approval read-only sets the mode");
        };
        assert_eq!(mode, ApprovalMode::ReadOnly);
        for line in ["/approval Auto", "/approval ask now"] {
            let Some(Effect::Note(note)) =
                dispatch(line, &settings("m", Effort::Low, ApprovalMode::Ask))
            else {
                panic!("{line} is a usage note");
            };
            assert!(
                note.starts_with("usage: /approval") && note.contains(ApprovalMode::NAMES),
                "{note}"
            );
        }
    }

    #[test]
    fn help_names_every_command_and_unknown_points_to_it() {
        let Some(Effect::Note(help)) =
            dispatch("/help", &settings("m", Effort::Medium, ApprovalMode::Ask))
        else {
            panic!("expected the help text");
        };
        for command in COMMANDS {
            assert!(help.contains(command.usage), "missing {}", command.usage);
        }

        let Some(Effect::Note(note)) =
            dispatch("/nope", &settings("m", Effort::Medium, ApprovalMode::Ask))
        else {
            panic!("expected the unknown reply");
        };
        assert!(note.contains("/help"));
    }

    #[test]
    fn the_exit_line_names_the_command_that_opens_the_session() {
        let id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
        assert_eq!(
            exit_line(id, true),
            "session 01ARZ3NDEKTSV4RRFFQ69G5FAV · resume with demerzel -c"
        );
        let older = exit_line(id, false);
        assert!(older.starts_with("session 01ARZ3NDEKTSV4RRFFQ69G5FAV · "));
        assert!(older.ends_with(&format!("--session {id}")), "{older}");
    }

    #[test]
    fn every_table_row_has_a_dispatch_arm() {
        // The table feeds /help; the match feeds behavior. A row
        // without an arm would advertise a command that answers
        // "unknown command".
        for command in COMMANDS {
            let line = command.usage.split_whitespace().next().unwrap();
            let effect = dispatch(line, &settings("m", Effort::Medium, ApprovalMode::Ask))
                .expect("a table row must parse");
            if let Effect::Note(note) = &effect {
                assert_ne!(
                    note,
                    &unknown_command(line.trim_start_matches('/')),
                    "{line} is in the table but not in the dispatch match"
                );
            }
        }
    }
}
