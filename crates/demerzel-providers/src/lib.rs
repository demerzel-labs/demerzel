//! Provider adapters for the Demerzel harness.
//!
//! The provider contract — the trait, capabilities, request shape, and
//! stream events — lives in `demerzel-core` next to the loop that
//! consumes it. This crate holds the adapters and re-exports the
//! contract, so adapter code and adapter users import one crate.

pub mod anthropic;
pub mod auth;
pub mod oauth;
pub mod openai_codex;
mod retry;
mod sse;

pub use demerzel_core::{
    BoxFuture, Capabilities, EventStream, Provider, ProviderError, Request, StreamEvent,
    ToolDefinition,
};

/// The provider ids this build ships. A slice, so a new adapter never
/// changes the item's type.
pub const PROVIDER_IDS: &[&str] = &[openai_codex::PROVIDER_ID, anthropic::PROVIDER_ID];

/// The default model of a shipped provider; `None` for an unknown id.
pub fn default_model(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        openai_codex::PROVIDER_ID => Some(openai_codex::DEFAULT_MODEL),
        anthropic::PROVIDER_ID => Some(anthropic::DEFAULT_MODEL),
        _ => None,
    }
}
