use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use demerzel::config::{self, Flags};
use demerzel::repl::{self, Renderer};
use demerzel::{RunConfig, Runtime, doctor};
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

    /// Provider id. DEMERZEL_PROVIDER sets it too.
    #[arg(long, global = true)]
    provider: Option<String>,

    /// Model name to request. DEMERZEL_MODEL sets it too.
    #[arg(long, global = true)]
    model: Option<String>,

    /// Endpoint base URL. DEMERZEL_BASE_URL sets it too.
    #[arg(long, global = true)]
    base_url: Option<String>,

    /// Run the write and execute tools without approval prompts.
    #[arg(long, global = true, conflicts_with = "read_only")]
    auto: bool,

    /// Advertise only the read tools.
    #[arg(long, global = true)]
    read_only: bool,
}

/// Reserved subcommand names, so their meaning cannot change later.
#[derive(Debug, Subcommand)]
enum Command {
    /// Check config, credentials, and tools.
    Doctor,
    /// Log in with the ChatGPT subscription. Arrives later in M2.
    Login,
}

impl Cli {
    /// The flag layer of the config resolution. Read-only wins over
    /// auto: clap rejects the pair at one level, but a global flag on
    /// each side of a subcommand slips past that check.
    fn flags(&self) -> Flags {
        Flags {
            provider: self.provider.clone(),
            model: self.model.clone(),
            base_url: self.base_url.clone(),
            approval: if self.read_only {
                Some(ApprovalMode::ReadOnly)
            } else if self.auto {
                Some(ApprovalMode::Auto)
            } else {
                None
            },
        }
    }
}

fn main() {
    let cli = Cli::parse();
    let cwd = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("demerzel: cannot read the working directory: {error}");
            std::process::exit(1);
        }
    };
    match &cli.command {
        Some(Command::Doctor) => std::process::exit(doctor::run(&cwd, &cli.flags())),
        Some(Command::Login) => {
            eprintln!("demerzel: `login` arrives later in M2");
            std::process::exit(1);
        }
        None => {}
    }
    let config = match run_config(&cli, cwd) {
        Ok(config) => config,
        Err(message) => {
            eprintln!("demerzel: {message}");
            std::process::exit(1);
        }
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build the tokio runtime");

    if cli.prompt.is_none() {
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
        Some(Ok(TurnOutcome::Refused)) => {
            eprintln!("demerzel: the model refused the request");
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

/// Resolve flags, environment, and config files into the run config.
fn run_config(cli: &Cli, cwd: PathBuf) -> Result<RunConfig, String> {
    let locations = config::Locations::locate(&cwd);
    let user = load_layer(locations.user.as_deref())?;
    let project = load_layer(Some(&locations.project))?;
    let (settings, ignored) = config::resolve(
        &cli.flags(),
        &|name| std::env::var(name).ok(),
        project.as_ref(),
        user.as_ref(),
    );
    // The trust rule dropped these project-file keys; say so.
    for warning in &ignored {
        eprintln!("demerzel: {warning}");
    }

    let interactive = cli.prompt.is_none();
    let mut config = RunConfig::new(cwd);
    config.approval = settings.approval_mode(interactive);
    config.provider = settings.provider.value;
    config.model = settings.model.value;
    config.base_url = settings.base_url.value;
    config.continue_last = cli.continue_last;
    if settings.history_file.value {
        config.history_path = demerzel_core::paths::data_dir()
            .ok()
            .map(|dir| dir.join("history"));
    }
    Ok(config)
}

/// Load one config layer, printing its unknown-key warnings.
fn load_layer(path: Option<&Path>) -> Result<Option<config::Layer>, String> {
    let Some(path) = path else {
        return Ok(None);
    };
    match config::load(path) {
        Ok(None) => Ok(None),
        Ok(Some((layer, warnings))) => {
            for warning in warnings {
                eprintln!("demerzel: {}: {warning}", path.display());
            }
            Ok(Some(layer))
        }
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("demerzel").chain(args.iter().copied())).unwrap()
    }

    #[test]
    fn a_bare_invocation_is_the_repl_with_no_approval_flag() {
        let cli = parse(&[]);
        assert!(cli.prompt.is_none());
        assert_eq!(cli.flags().approval, None);
    }

    #[test]
    fn approval_flags_map_to_modes() {
        assert_eq!(
            parse(&["--auto"]).flags().approval,
            Some(ApprovalMode::Auto)
        );
        assert_eq!(
            parse(&["--read-only"]).flags().approval,
            Some(ApprovalMode::ReadOnly)
        );
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
    fn read_only_wins_when_global_flags_bypass_the_conflict_check() {
        // One flag before the subcommand and one after slip past
        // clap's same-level conflict check.
        let cli = Cli::try_parse_from(["demerzel", "--read-only", "doctor", "--auto"]).unwrap();
        assert_eq!(cli.flags().approval, Some(ApprovalMode::ReadOnly));
        let cli = Cli::try_parse_from(["demerzel", "--auto", "doctor", "--read-only"]).unwrap();
        assert_eq!(cli.flags().approval, Some(ApprovalMode::ReadOnly));
    }

    #[test]
    fn doctor_and_login_are_reserved_subcommands() {
        let cli = parse(&["doctor"]);
        assert!(matches!(cli.command, Some(Command::Doctor)));
        let cli = parse(&["login"]);
        assert!(matches!(cli.command, Some(Command::Login)));
    }

    #[test]
    fn provider_model_and_base_url_flags_parse() {
        let cli = parse(&[
            "--provider",
            "openai-codex",
            "--model",
            "gpt-x",
            "--base-url",
            "http://localhost:1",
            "hi",
        ]);
        let flags = cli.flags();
        assert_eq!(flags.provider.as_deref(), Some("openai-codex"));
        assert_eq!(flags.model.as_deref(), Some("gpt-x"));
        assert_eq!(flags.base_url.as_deref(), Some("http://localhost:1"));
    }
}
