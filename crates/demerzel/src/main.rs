use clap::Parser;
use demerzel::repl::{self, Renderer};
use demerzel::{RunConfig, run_one_shot};
use demerzel_core::{ApprovalMode, TurnOutcome};

/// Demerzel: a terminal coding agent.
#[derive(Debug, Parser)]
#[command(name = "demerzel", version)]
struct Cli {
    /// One-shot prompt. Give no prompt to start the interactive REPL.
    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        value_name = "PROMPT"
    )]
    prompt: Vec<String>,

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
    let cwd = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("demerzel: cannot read the working directory: {error}");
            std::process::exit(1);
        }
    };
    let interactive = cli.prompt.is_empty();
    let mut config = RunConfig::new(cwd);
    config.approval = cli.approval(interactive);
    config.continue_last = cli.continue_last;
    if let Some(model) = cli.model.filter(|value| !value.is_empty()) {
        config.model = model;
    }
    config.base_url = cli.base_url.filter(|value| !value.is_empty());

    if interactive {
        if let Err(error) = repl::run(&config) {
            eprintln!("demerzel: {error}");
            std::process::exit(1);
        }
        return;
    }

    let prompt = cli.prompt.join(" ");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build the tokio runtime");
    let outcome = runtime.block_on(async {
        let renderer = Renderer::new(false);
        let result = {
            let run = run_one_shot(&config, prompt, &renderer);
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
        assert!(cli.prompt.is_empty());
        assert_eq!(cli.approval(true), ApprovalMode::Ask);
    }

    #[test]
    fn a_prompt_is_one_shot_and_read_only_by_default() {
        let cli = parse(&["explain", "this", "error"]);
        assert_eq!(cli.prompt.join(" "), "explain this error");
        assert_eq!(cli.approval(false), ApprovalMode::ReadOnly);
    }

    #[test]
    fn flags_before_the_prompt_parse_and_flags_after_join_it() {
        let cli = parse(&["--auto", "-c", "fix", "--the", "bug"]);
        assert!(cli.auto);
        assert!(cli.continue_last);
        assert_eq!(cli.prompt.join(" "), "fix --the bug");
        assert_eq!(cli.approval(false), ApprovalMode::Auto);
    }

    #[test]
    fn auto_and_read_only_conflict() {
        let error = Cli::try_parse_from(["demerzel", "--auto", "--read-only", "x"]).unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn model_and_base_url_flags_parse() {
        let cli = parse(&["--model", "gpt-x", "--base-url", "http://localhost:1", "hi"]);
        assert_eq!(cli.model.as_deref(), Some("gpt-x"));
        assert_eq!(cli.base_url.as_deref(), Some("http://localhost:1"));
    }
}
