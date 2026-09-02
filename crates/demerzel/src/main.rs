use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use demerzel::config::{self, Flags};
use demerzel::repl::{self, Renderer};
use demerzel::trace;
use demerzel::{Resume, RunConfig, Runtime, doctor, login};
use demerzel_core::{ApprovalMode, Effort, TurnOutcome};

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
    #[arg(short = 'c', long = "continue", conflicts_with = "session")]
    continue_last: bool,

    /// Continue the session with this id in this project. /session
    /// shows the id, and so does the line a session prints on exit.
    #[arg(long, value_name = "ID")]
    session: Option<String>,

    /// Use the plain line interface instead of the full screen.
    #[arg(long)]
    plain: bool,

    /// Provider id. DEMERZEL_PROVIDER sets it too.
    #[arg(long, global = true)]
    provider: Option<String>,

    /// Model name to request. DEMERZEL_MODEL sets it too.
    #[arg(long, global = true)]
    model: Option<String>,

    /// Endpoint base URL. DEMERZEL_BASE_URL sets it too.
    #[arg(long, global = true)]
    base_url: Option<String>,

    /// Reasoning effort: none, minimal, low, medium, high, xhigh, max,
    /// or ultra. DEMERZEL_EFFORT sets it too.
    #[arg(long, global = true)]
    effort: Option<Effort>,

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
    /// Log in to a provider.
    ///
    /// With no argument the flow is the ChatGPT subscription, through
    /// the browser.
    Login {
        /// anthropic (an API key, prompted and verified) or
        /// openai-codex (the browser flow, the default).
        #[arg(value_name = "PROVIDER")]
        provider: Option<String>,
    },
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
            effort: self.effort,
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
            exit(1);
        }
    };
    match &cli.command {
        Some(Command::Doctor) => exit(doctor::run(&cwd, &cli.flags())),
        Some(Command::Login { provider }) => exit(run_login(&cli, provider.as_deref(), &cwd)),
        None => {}
    }
    // The debug trace covers a session run; `doctor` and `login`
    // write none. It opens before the config resolves, so every
    // later step can emit.
    if let Err(message) = trace::init() {
        eprintln!("demerzel: {message}");
        exit(1);
    }
    let config = match run_config(&cli, cwd) {
        Ok(config) => config,
        Err(message) => {
            eprintln!("demerzel: {message}");
            exit(1);
        }
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build the tokio runtime");

    if cli.prompt.is_none() {
        let result = if use_tui(&cli) {
            runtime.block_on(demerzel::tui::run(&config))
        } else {
            runtime.block_on(repl::run(&config))
        };
        if let Err(error) = result {
            eprintln!("demerzel: {error}");
            exit(1);
        }
        trace::flush();
        return;
    }

    // A one-shot run has no ready marker: the path line comes first.
    trace::announce();
    let prompt = cli.prompt.expect("checked above");
    let max_model_calls = config.max_model_calls;
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
            exit(130);
        }
        Some(Ok(TurnOutcome::Completed)) => {}
        Some(Ok(TurnOutcome::MaxTokens)) => {
            eprintln!(
                "demerzel: the model hit a token limit — the output cap or the context window"
            );
            exit(1);
        }
        Some(Ok(TurnOutcome::CallLimit)) => {
            eprintln!(
                "demerzel: the turn hit the model-call limit ({max_model_calls} model calls)"
            );
            exit(1);
        }
        Some(Ok(TurnOutcome::Refused)) => {
            eprintln!("demerzel: the model refused the request");
            exit(1);
        }
        Some(Ok(TurnOutcome::Aborted)) => {
            eprintln!("demerzel: stopped at an approval prompt");
            exit(1);
        }
        Some(Ok(_)) => {}
        Some(Err(error)) => {
            eprintln!("demerzel: {error}");
            exit(1);
        }
    }
    trace::flush();
}

/// End the process after the debug trace wrote out its buffer. A
/// failed boot names the file too, so the trace of a run that never
/// reached its interface is not a secret.
fn exit(code: i32) -> ! {
    if code != 0 {
        trace::announce();
    }
    trace::flush();
    std::process::exit(code)
}

/// The full screen needs a real terminal on both ends and a terminal
/// type that can drive it. Everything else falls back to the REPL.
fn use_tui(cli: &Cli) -> bool {
    use std::io::IsTerminal as _;
    if cli.plain {
        return false;
    }
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return false;
    }
    std::env::var_os("TERM").is_some_and(|term| !term.is_empty() && term != "dumb")
}

/// Run `dmz login` and return the process exit code. Ctrl-C stops
/// the flow and exits 130, like an aborted one-shot.
fn run_login(cli: &Cli, provider: Option<&str>, cwd: &Path) -> i32 {
    // The positional wins; the global `--provider` flag is the same
    // request in flag form.
    let provider = provider.or(cli.provider.as_deref());
    let options = match login_options(cli, provider, cwd) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("demerzel: {message}");
            return 1;
        }
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build the tokio runtime");
    let outcome = runtime.block_on(async {
        // Biased, signal first: the Ctrl-C handler installs before
        // the login future can occupy the first poll.
        tokio::select! {
            biased;
            signal = tokio::signal::ctrl_c() => {
                // A registration failure resolves here too; name it
                // instead of posing as an interrupt.
                if let Err(error) = signal {
                    eprintln!("demerzel: the Ctrl-C watch failed: {error}");
                }
                None
            }
            result = login::run(provider, &options) => Some(result),
        }
    });
    match outcome {
        None => {
            eprintln!("demerzel: login aborted");
            // Exit while the runtime lives. Dropping it would wait
            // for a parked stdin read that only Enter can end.
            exit(130);
        }
        Some(Ok(())) => 0,
        Some(Err(error)) => {
            eprintln!("demerzel: {error}");
            1
        }
    }
}

/// Build the login options: the store path the provider crate owns,
/// and for the key check the endpoint the config layers resolve for
/// the anthropic provider — flags beat environment beat user file,
/// the same order a session uses.
fn login_options(cli: &Cli, provider: Option<&str>, cwd: &Path) -> Result<login::Options, String> {
    let store_path =
        demerzel_providers::auth::AuthStore::default_path().map_err(|e| e.to_string())?;
    let mut options = login::Options::new(store_path);
    if provider == Some(demerzel_providers::anthropic::PROVIDER_ID) {
        let locations = config::Locations::locate(cwd);
        let user = load_layer(locations.user.as_deref())?;
        let project = load_layer(Some(&locations.project))?;
        let mut flags = cli.flags();
        flags.provider = Some(demerzel_providers::anthropic::PROVIDER_ID.to_string());
        let (settings, ignored) = config::resolve(
            &flags,
            &|name| std::env::var(name).ok(),
            project.as_ref(),
            user.as_ref(),
        );
        for warning in &ignored {
            eprintln!("demerzel: {warning}");
        }
        if let Some(base_url) = settings.base_url.value {
            options.anthropic_base_url = base_url;
        }
    }
    Ok(options)
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
    config.effort = settings.effort.value;
    config.max_model_calls = settings.max_model_calls.value;
    config.context_window = settings.context_window.value;
    config.resume = match &cli.session {
        Some(id) => Resume::Id(id.clone()),
        None if cli.continue_last => Resume::Latest,
        None => Resume::Fresh,
    };
    config.mouse = settings.tui.mouse.value;
    config.markdown = settings.tui.markdown.value;
    config.thinking = settings.tui.thinking.value;
    config.theme = config::theme(&settings.tui.theme.value, user.as_ref())?;
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
        assert!(!cli.plain);
        assert_eq!(cli.flags().approval, None);
    }

    #[test]
    fn the_plain_flag_parses() {
        assert!(parse(&["--plain"]).plain);
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
    fn the_session_flag_names_an_id_and_excludes_continue() {
        let cli = parse(&["--session", "01ARZ3NDEKTSV4RRFFQ69G5FAV"]);
        assert_eq!(cli.session.as_deref(), Some("01ARZ3NDEKTSV4RRFFQ69G5FAV"));
        assert!(!cli.continue_last);
        let error = Cli::try_parse_from(["demerzel", "-c", "--session", "X"]).unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
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
        assert!(matches!(
            cli.command,
            Some(Command::Login { provider: None })
        ));
        let cli = parse(&["login", "anthropic"]);
        assert!(
            matches!(cli.command, Some(Command::Login { provider: Some(p) }) if p == "anthropic")
        );
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
