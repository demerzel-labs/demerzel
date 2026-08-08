//! Provider adapters for the Demerzel harness.
//!
//! The provider contract — the trait, capabilities, request shape, and
//! stream events — lives in `demerzel-core` next to the loop that
//! consumes it. This crate holds the adapters and re-exports the
//! contract, so adapter code and adapter users import one crate.

pub mod openai_codex;

pub use demerzel_core::{
    BoxFuture, Capabilities, EventStream, Provider, ProviderError, Request, StreamEvent,
    ToolDefinition,
};
