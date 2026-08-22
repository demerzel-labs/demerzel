//! Live prompt-cache trace. Runs a fixed list of user turns through the
//! harness against a real provider, one fresh session per run, and
//! prints the per-call usage as one JSON document on stdout.
//! `bench/cache/replay.py` drives it and turns the trace into the
//! cache-hit report; this program only records.
//!
//! Run: `cargo run --release -p demerzel --example cache_replay -- \
//!   --provider anthropic --runs 2 -- "first prompt" "second prompt"`
//!
//! The run is read-only: the engine advertises only the read tools,
//! and the project directory is the working directory. Sessions land
//! in a private temporary data root. Credentials resolve the way a
//! session resolves them: the environment, then the default auth
//! store. A turn that fails ends the program with a non-zero exit;
//! a partial trace is not a receipt.

use std::sync::Mutex;
use std::time::Instant;

use clap::Parser;
use demerzel::{RunConfig, Runtime};
use demerzel_core::{ApprovalMode, EngineEvent, EventSink, NoPrompt, TurnOutcome, Usage};
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

/// Collects what one turn reports: usage per model call and the
/// tools the turn ran.
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

fn outcome_name(outcome: TurnOutcome) -> &'static str {
    match outcome {
        TurnOutcome::Completed => "completed",
        TurnOutcome::MaxTokens => "max_tokens",
        TurnOutcome::Refused => "refused",
        TurnOutcome::CallLimit => "call_limit",
        TurnOutcome::Aborted => "aborted",
        _ => "other",
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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
    config.approval = ApprovalMode::ReadOnly;
    config.data_root = Some(data.path().to_path_buf());
    let runtime = Runtime::new(config)?;

    let mut sessions = Vec::with_capacity(args.runs as usize);
    for run in 1..=args.runs {
        let (session, history) = runtime.open_session(false, &model)?;
        let session_id = session.id().to_string();
        let mut engine = runtime.engine(session, history, &model);
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
                "outcome": outcome_name(outcome),
                "wall_ms": (wall_ms * 10.0).round() / 10.0,
                "tools": tools,
                "calls": calls.iter().map(|u| json!({
                    "input_tokens": u.input_tokens,
                    "output_tokens": u.output_tokens,
                    "cache_read_tokens": u.cache_read_tokens,
                })).collect::<Vec<_>>(),
            }));
        }
        sessions.push(json!({"session": session_id, "turns": turns}));
    }

    let report = json!({
        "provider": args.provider,
        "model": model,
        "runs": args.runs,
        "sessions": sessions,
    });
    println!("{report}");
    Ok(())
}
