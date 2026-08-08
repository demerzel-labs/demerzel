use clap::{Parser, Subcommand};
use demerzel::repl::{self, Renderer};
use demerzel::{RunConfig, Runtime};
use demerzel_core::{ApprovalMode, TurnOutcome};

/// Demerzel: a terminal coding agent.
#[derive(Debug, Parser)]
#[command(name = "demerzel", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// One-shot prompt, quoted. Give no prompt to start the
    /// interactive REPL.
    #[arg(value_name = "PROMPT")]
    prompt: Option<String>,

    /// Continue the latest session in this project.
    #[arg(short = 'c', long = "continue")]
    continue_last: bool,

    /// Model name to request.
    #[arg(long, env = "DEMERZEL_MODEL")]
    model: Option<String>,

    /// Endpoint base URL.
    #[arg(long, env = "DEMERZEL_BASE_URL")]
    base_url: Option<String>,

    /// Run the write and execute tools without approval prompts.
    #[arg(long, conflicts_with = "read_only")]
    auto: bool,

    /// Advertise only the read tools.
    #[arg(long)]
    read_only: bool,
}

/// Reserved subcommand names, so their meaning cannot change later.
#[derive(Debug, Subcommand)]
enum Command {
    /// Check providers, credentials, and tools. Arrives in M2.
    Doctor,
    /// Log in with the ChatGPT subscription. Arrives in M2.
    Login,
}

impl Cli {
    /// The approval mode: flags win; else the REPL asks and one-shot
    /// stays read-only.
    fn approval(&self, interactive: bool) -> ApprovalMode {
        if self.auto {
            ApprovalMode::Auto
        } else if self.read_only {
            ApprovalMode::ReadOnly
        } else if interactive {
            ApprovalMode::Ask
        } else {
            ApprovalMode::ReadOnly
        }
    }
}

fn main() {
    let cli = Cli::parse();
    if let Some(command) = &cli.command {
        let name = match command {
            Command::Doctor => "doctor",
            Command::Login => "login",
        };
        eprintln!("demerzel: `{name}` arrives in M2");
        std::process::exit(1);
    }
    let cwd = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("demerzel: cannot read the working directory: {error}");
            std::process::exit(1);
        }
    };
    let interactive = cli.prompt.is_none();
    let mut config = RunConfig::new(cwd);
    config.approval = cli.approval(interactive);
    config.continue_last = cli.continue_last;
    if let Some(model) = cli.model.filter(|value| !value.is_empty()) {
        config.model = model;
    }
    config.base_url = cli.base_url.filter(|value| !value.is_empty());

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build the tokio runtime");

    if interactive {
        if let Err(error) = runtime.block_on(repl::run(&config)) {
            eprintln!("demerzel: {error}");
            std::process::exit(1);
        }
        return;
    }

    let prompt = cli.prompt.expect("checked above");
    let outcome = runtime.block_on(async {
        let harness = match Runtime::new(config) {
            Ok(harness) => harness,
            Err(error) => return Some(Err(error)),
        };
        let renderer = Renderer::new(false);
        let result = {
            let run = harness.one_shot(prompt, &renderer);
            tokio::pin!(run);
            tokio::select! {
                result = &mut run => Some(result),
                _ = tokio::signal::ctrl_c() => None,
            }
        };
        renderer.finish();
        result
    });

    match outcome {
        None => {
            eprintln!("demerzel: aborted");
            std::process::exit(130);
        }
        Some(Ok(TurnOutcome::Completed)) => {}
        Some(Ok(TurnOutcome::MaxTokens)) => {
            eprintln!("demerzel: the model hit the output-token limit");
            std::process::exit(1);
        }
        Some(Ok(TurnOutcome::CallLimit)) => {
            eprintln!("demerzel: the turn hit the model-call limit");
            std::process::exit(1);
        }
        Some(Ok(TurnOutcome::Aborted)) => {
            eprintln!("demerzel: stopped at an approval prompt");
            std::process::exit(1);
        }
        Some(Ok(_)) => {}
        Some(Err(error)) => {
            eprintln!("demerzel: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("demerzel").chain(args.iter().copied())).unwrap()
    }

    #[test]
    fn a_bare_invocation_is_the_repl_in_ask_mode() {
        let cli = parse(&[]);
        assert!(cli.prompt.is_none());
        assert_eq!(cli.approval(true), ApprovalMode::Ask);
    }

    #[test]
    fn a_quoted_prompt_is_one_shot_and_read_only_by_default() {
        let cli = parse(&["explain this error"]);
        assert_eq!(cli.prompt.as_deref(), Some("explain this error"));
        assert_eq!(cli.approval(false), ApprovalMode::ReadOnly);
    }

    #[test]
    fn flags_parse_before_or_after_the_prompt() {
        let cli = parse(&["--auto", "-c", "fix the bug"]);
        assert!(cli.auto);
        assert!(cli.continue_last);
        assert_eq!(cli.prompt.as_deref(), Some("fix the bug"));

        let cli = parse(&["fix the bug", "--auto"]);
        assert!(cli.auto);
        assert_eq!(cli.prompt.as_deref(), Some("fix the bug"));
    }

    #[test]
    fn unquoted_multi_word_prompts_and_unknown_flags_are_errors() {
        assert!(Cli::try_parse_from(["demerzel", "fix", "the", "bug"]).is_err());
        assert!(Cli::try_parse_from(["demerzel", "--auto-aprove", "x"]).is_err());
    }

    #[test]
    fn a_double_dash_escapes_a_dashed_prompt() {
        let cli = parse(&["--", "--auto"]);
        assert_eq!(cli.prompt.as_deref(), Some("--auto"));
        assert!(!cli.auto);
    }

    #[test]
    fn auto_and_read_only_conflict() {
        let error = Cli::try_parse_from(["demerzel", "--auto", "--read-only", "x"]).unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn doctor_and_login_are_reserved_subcommands() {
        let cli = parse(&["doctor"]);
        assert!(matches!(cli.command, Some(Command::Doctor)));
        let cli = parse(&["login"]);
        assert!(matches!(cli.command, Some(Command::Login)));
    }

    #[test]
    fn model_and_base_url_flags_parse() {
        let cli = parse(&["--model", "gpt-x", "--base-url", "http://localhost:1", "hi"]);
        assert_eq!(cli.model.as_deref(), Some("gpt-x"));
        assert_eq!(cli.base_url.as_deref(), Some("http://localhost:1"));
    }
}
