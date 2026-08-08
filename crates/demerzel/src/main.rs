use std::io::Write as _;

use demerzel::{RunConfig, run_one_shot};
use demerzel_core::{ApprovalMode, EngineEvent, TurnOutcome};

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "usage: demerzel [--auto | --read-only] [-c] \"prompt\"
       demerzel --version

One-shot mode runs one prompt and exits. Writes are off by default;
--auto turns them on. -c continues the latest session in this project.
The interactive REPL arrives with M1 step 6.";

fn main() {
    let mut approval = ApprovalMode::ReadOnly;
    let mut continue_last = false;
    let mut words: Vec<String> = Vec::new();
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("demerzel {VERSION}");
                return;
            }
            "--auto" => approval = ApprovalMode::Auto,
            "--read-only" => approval = ApprovalMode::ReadOnly,
            "-c" => continue_last = true,
            "--help" | "-h" => {
                println!("{USAGE}");
                return;
            }
            other if other.starts_with('-') => {
                eprintln!("unknown flag: {other}\n{USAGE}");
                std::process::exit(2);
            }
            word => words.push(word.to_string()),
        }
    }
    let prompt = words.join(" ");
    if prompt.is_empty() {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }

    let cwd = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("demerzel: cannot read the working directory: {error}");
            std::process::exit(1);
        }
    };
    let mut config = RunConfig::new(cwd);
    config.approval = approval;
    config.continue_last = continue_last;
    if let Ok(model) = std::env::var("DEMERZEL_MODEL") {
        config.model = model;
    }
    if let Ok(base_url) = std::env::var("DEMERZEL_BASE_URL") {
        config.base_url = Some(base_url);
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build the tokio runtime");
    let outcome = runtime.block_on(async {
        let mut pending_newline = false;
        let mut sink = |event: EngineEvent<'_>| match event {
            EngineEvent::TextDelta(delta) => {
                print!("{delta}");
                let _ = std::io::stdout().flush();
                pending_newline = !delta.ends_with('\n');
            }
            EngineEvent::ToolStart { name, .. } => {
                eprintln!("· {name} …");
            }
            EngineEvent::ToolEnd {
                name,
                summary,
                is_error,
            } => {
                let mark = if is_error { "✗" } else { "·" };
                eprintln!("{mark} {}", summary.unwrap_or(name));
            }
            EngineEvent::ReasoningDelta(_) | EngineEvent::ModelUsage(_) => {}
        };
        let result = run_one_shot(&config, prompt, &mut sink).await;
        if pending_newline {
            println!();
        }
        result
    });

    match outcome {
        Ok(TurnOutcome::Completed) => {}
        Ok(TurnOutcome::MaxTokens) => {
            eprintln!("demerzel: the model hit the output-token limit");
            std::process::exit(1);
        }
        Ok(TurnOutcome::CallLimit) => {
            eprintln!("demerzel: the turn hit the model-call limit");
            std::process::exit(1);
        }
        Err(error) => {
            eprintln!("demerzel: {error}");
            std::process::exit(1);
        }
    }
}
