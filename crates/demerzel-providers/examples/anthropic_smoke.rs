//! Live smoke call against the real Messages API with an API key.
//! The call sends the full tool set, so it also confirms that the
//! endpoint accepts every tool input schema.
//!
//! Run: `ANTHROPIC_API_KEY=... cargo run -p demerzel-providers \
//!   --example anthropic_smoke -- [model] [prompt...]`

use std::future::poll_fn;

use demerzel_core::{ContentBlock, Message, Role};
use demerzel_providers::anthropic::{
    Anthropic, AnthropicConfig, DEFAULT_MODEL, ENV_API_KEY, NO_CREDENTIALS_HELP,
};
use demerzel_providers::auth::AuthStore;
use demerzel_providers::{Provider, Request, StreamEvent};
use demerzel_tools::Registry;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let model = args.next().unwrap_or_else(|| DEFAULT_MODEL.to_string());
    let rest: Vec<String> = args.collect();
    let prompt = if rest.is_empty() {
        "Reply with exactly one word: pong".to_string()
    } else {
        rest.join(" ")
    };
    // The runtime's credential order: the environment, then the store.
    let api_key = std::env::var(ENV_API_KEY)
        .ok()
        .filter(|key| !key.is_empty())
        .or_else(|| {
            AuthStore::open_default()
                .ok()
                .and_then(|store| store.anthropic_api_key().map(str::to_string))
        })
        .ok_or(NO_CREDENTIALS_HELP)?;
    let provider = Anthropic::new(AnthropicConfig::default(), api_key);
    let request = {
        let mut request = Request::new(model);
        request.system_prompt = "You are demerzel, a terminal coding agent.".to_string();
        request.messages = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: prompt }],
        }];
        request.tools = Registry::full(std::env::current_dir()?).definitions();
        request
    };

    let mut stream = provider.stream(request).await?;
    while let Some(event) = poll_fn(|cx| stream.as_mut().poll_next(cx)).await {
        match event? {
            StreamEvent::TextDelta(text) => print!("{text}"),
            StreamEvent::ReasoningDelta(_) => {}
            StreamEvent::ReasoningDone { summary, sealed } => {
                eprintln!(
                    "[reasoning] summary={:?}... sealed={}",
                    summary.chars().take(60).collect::<String>(),
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
