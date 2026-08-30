//! Live prompt-cache trace. Runs a fixed list of user turns through the
//! harness against a real provider, one fresh session per run, and
//! prints the per-call usage as one JSON document on stdout.
//! `bench/cache/replay.py` drives it and turns the trace into the
//! cache-hit report; this program only records.
//!
//! Run: `cargo run --release -p demerzel --example cache_replay -- \
//!   --provider anthropic --runs 2 -- "first prompt" "second prompt"`
//!
//! The run advertises the full tool set in `ask` mode with no
//! approver, the prefix a real interactive session sends, and it
//! executes only the read tools: every other call is declined
//! without a prompt, and the turn goes on. The project directory is
//! the working directory. Sessions land in a private temporary data
//! root that goes away with the run. Credentials resolve the way a
//! session resolves them: the environment, then the default auth
//! store. A turn that fails ends the program with a non-zero exit;
//! a partial trace is not a receipt.
//!
//! One `calls` entry per usage report; both shipped adapters report
//! usage once per model call.

use std::sync::Mutex;
use std::time::Instant;

use clap::Parser;
use demerzel::{RunConfig, Runtime};
use demerzel_core::{ApprovalMode, EngineEvent, EventSink, NoPrompt, Usage};
use serde_json::{Value, json};

#[derive(Debug, Parser)]
#[command(name = "cache_replay")]
struct Args {
    /// Provider id: openai-codex or anthropic.
    #[arg(long)]
    provider: String,
    /// Model name; the provider's default when absent.
    #[arg(long)]
    model: Option<String>,
    /// Fresh sessions to run, one after the other.
    #[arg(long, default_value_t = 2)]
    runs: u32,
    /// The user turns, in order. Every session runs them all.
    #[arg(value_name = "PROMPT", required = true, num_args = 1..)]
    prompts: Vec<String>,
}

/// Collects what one turn reports: usage per model call and every
/// tool call the turn made, declined ones included.
#[derive(Default)]
struct Trace {
    calls: Mutex<Vec<Usage>>,
    tools: Mutex<Vec<String>>,
}

impl EventSink for Trace {
    fn event(&self, event: EngineEvent<'_>) {
        match event {
            EngineEvent::ModelUsage(usage) => self.calls.lock().unwrap().push(usage),
            EngineEvent::ToolEnd { name, .. } => self.tools.lock().unwrap().push(name.to_string()),
            _ => {}
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("cache_replay: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let model = match args.model {
        Some(model) => model,
        None => demerzel_providers::default_model(&args.provider)
            .ok_or_else(|| format!("unknown provider `{}`", args.provider))?
            .to_string(),
    };
    let data = tempfile::tempdir()?;
    let mut config = RunConfig::new(std::env::current_dir()?);
    config.provider = args.provider.clone();
    config.model = model.clone();
    config.approval = ApprovalMode::Ask;
    config.data_root = Some(data.path().to_path_buf());
    let runtime = Runtime::new(config)?;

    let mut sessions = Vec::new();
    for run in 1..=args.runs {
        let settings = demerzel::SessionSettings::from_config(runtime.config());
        let (session, history) = runtime.open_session(false, &settings)?;
        let session_id = session.id().to_string();
        let mut engine = runtime.engine(session, history, &settings);
        let mut turns: Vec<Value> = Vec::with_capacity(args.prompts.len());
        for (index, prompt) in args.prompts.iter().enumerate() {
            let trace = Trace::default();
            let start = Instant::now();
            let result = engine
                .user_turn(prompt.clone(), &trace, &mut NoPrompt)
                .await;
            let wall_ms = start.elapsed().as_secs_f64() * 1e3;
            let calls = trace.calls.into_inner().unwrap();
            let tools = trace.tools.into_inner().unwrap();
            let outcome =
                result.map_err(|error| format!("run {run}, turn {}: {error}", index + 1))?;
            let input: u64 = calls.iter().map(|u| u.input_tokens).sum();
            let cached: u64 = calls.iter().map(|u| u.cache_read_tokens).sum();
            eprintln!(
                "run {run}/{} turn {}/{}: {} calls, {input} in, {cached} cached, {} tools, {wall_ms:.0} ms",
                args.runs,
                index + 1,
                args.prompts.len(),
                calls.len(),
                tools.len()
            );
            turns.push(json!({
                "prompt": prompt,
                "outcome": outcome.as_str(),
                "wall_ms": (wall_ms * 10.0).round() / 10.0,
                "tools": tools,
                "calls": calls.iter().map(|u| json!({
                    "input_tokens": u.input_tokens,
                    "output_tokens": u.output_tokens,
                    "cache_read_tokens": u.cache_read_tokens,
                    "cache_creation_tokens": u.cache_creation_tokens,
                })).collect::<Vec<_>>(),
            }));
        }
        sessions.push(json!({"session": session_id, "turns": turns}));
    }

    let report = json!({
        "provider": args.provider,
        "model": model,
        "effort": runtime.config().effort.as_str(),
        "runs": args.runs,
        "sessions": sessions,
    });
    println!("{report}");
    Ok(())
}
