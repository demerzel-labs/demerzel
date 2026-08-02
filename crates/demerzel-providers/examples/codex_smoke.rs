//! Live smoke call against the real Codex endpoint with the imported
//! Codex CLI credentials. Part of the M1 step-2 verify line.
//!
//! Run: `cargo run -p demerzel-providers --example codex_smoke -- [model] [prompt...]`

use std::future::poll_fn;

use demerzel_core::{ContentBlock, Message, Role};
use demerzel_providers::openai_codex::{AuthStore, CodexConfig, OpenAiCodex};
use demerzel_providers::{Provider, Request, StreamEvent};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let model = args.next().unwrap_or_else(|| "gpt-5.6-sol".to_string());
    let rest: Vec<String> = args.collect();
    let prompt = if rest.is_empty() {
        "Reply with exactly one word: pong".to_string()
    } else {
        rest.join(" ")
    };
    let auth = AuthStore::open_default()?;
    let provider = OpenAiCodex::new(CodexConfig::default(), auth);
    let request = Request {
        model,
        system_prompt: "You are demerzel, a terminal coding agent.".to_string(),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: prompt }],
        }],
        tools: Vec::new(),
        max_output_tokens: None,
    };

    let mut stream = provider.stream(request).await?;
    while let Some(event) = poll_fn(|cx| stream.as_mut().poll_next(cx)).await {
        match event? {
            StreamEvent::TextDelta(text) => print!("{text}"),
            StreamEvent::ReasoningDelta(_) => {}
            StreamEvent::ReasoningDone { summary, sealed } => {
                eprintln!(
                    "[reasoning] summary={summary:?} sealed={}",
                    sealed.is_some()
                );
            }
            StreamEvent::Usage(usage) => eprintln!(
                "\n[usage] input={} output={} cache_read={}",
                usage.input_tokens, usage.output_tokens, usage.cache_read_tokens
            ),
            StreamEvent::Done(reason) => eprintln!("[done] {reason:?}"),
            other => eprintln!("[event] {other:?}"),
        }
    }
    Ok(())
}
