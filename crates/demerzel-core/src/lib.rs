//! Domain types for the Demerzel harness.
//!
//! The harness owns its own message model. Provider adapters translate
//! between this model and each provider's wire format. No adapter is
//! privileged.

mod message;

pub use message::{ContentBlock, Message, Role, SealedPayload, StopReason, Usage};
