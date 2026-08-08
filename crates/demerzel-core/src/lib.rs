//! Domain types and the agent loop for the Demerzel harness.
//!
//! The harness owns its own message model and provider contract.
//! Provider adapters translate between this model and each provider's
//! wire format. No adapter is privileged.

mod approval;
mod engine;
mod message;
pub mod paths;
mod provider;
mod session;
mod tool;

pub use approval::{ApprovalDecision, ApprovalMode, ApprovalPolicy, ApprovalRequest, ToolAccess};
pub use engine::{
    Approver, DEFAULT_SYSTEM_PROMPT, Engine, EngineConfig, EngineError, EngineEvent, EventSink,
    NoPrompt, TurnOutcome,
};
pub use message::{ContentBlock, Message, Role, SealedPayload, StopReason, Usage};
pub use provider::{Capabilities, EventStream, Provider, ProviderError, Request, StreamEvent};
pub use session::{
    Replay, SCHEMA_VERSION, Session, SessionMeta, SessionRecord, SessionStore, replay,
};
pub use tool::{BoxFuture, ToolDefinition, ToolHost, ToolOutput};
