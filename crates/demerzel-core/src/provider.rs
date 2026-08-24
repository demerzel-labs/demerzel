//! The provider contract: the trait, capabilities, request shape, and
//! stream events that every adapter implements.
//!
//! The contract lives in this crate because it is the harness's own
//! neutral model of a provider — the loop consumes it and the adapters
//! in `demerzel-providers` implement it. The harness reads
//! [`Capabilities`] flags and degrades features. It never assumes a
//! capability. The trait is object-safe so the harness can hold
//! `Box<dyn Provider>` and route per model role.

use std::pin::Pin;
use std::time::Duration;

use futures_core::Stream;
use serde_json::Value;

use serde::{Deserialize, Serialize};

use crate::message::{Message, SealedPayload, StopReason, Usage};
use crate::tool::{BoxFuture, ToolDefinition};

/// What an adapter can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Capabilities {
    pub streaming: bool,
    pub tool_calls: bool,
    pub parallel_tool_calls: bool,
    pub usage_in_stream: bool,
    pub cache_control: bool,
    pub sealed_reasoning: bool,
}

/// One model request. The system prompt stays separate from history so
/// prompt assembly can keep the prefix stable. Build one with
/// [`Request::new`]; the struct is non-exhaustive so later fields
/// (cache breakpoints, per-request reasoning effort) stay additive.
///
/// `messages` can hold two same-role messages in a row — a repaired or
/// limit-cut turn produces that shape. An adapter whose wire format
/// requires strict role alternation merges consecutive same-role
/// messages when it builds the request body.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Request {
    pub model: String,
    pub system_prompt: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub max_output_tokens: Option<u64>,
    /// How hard the model reasons on this request. `None` leaves the
    /// provider's own default.
    pub effort: Option<Effort>,
}

impl Request {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            system_prompt: String::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            max_output_tokens: None,
            effort: None,
        }
    }
}

/// How hard the model reasons on a request: three levels every
/// provider maps to its own knob. Ordered, so a config rule can say
/// "not above".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
}

impl Effort {
    /// The level's wire and config name.
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
        }
    }
}

impl std::str::FromStr for Effort {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "low" => Ok(Effort::Low),
            "medium" => Ok(Effort::Medium),
            "high" => Ok(Effort::High),
            other => Err(format!(
                "`{other}` is not an effort; use low, medium, or high"
            )),
        }
    }
}

impl std::fmt::Display for Effort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Neutral stream events. Adapters map wire events to these; unknown
/// wire events are dropped, not errors. An adapter emits at most one
/// `Usage` event per stream; when it emits more, the last one wins.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum StreamEvent {
    TextDelta(String),
    /// Readable reasoning-summary text.
    ReasoningDelta(String),
    ToolCallStart {
        id: String,
        name: String,
    },
    ToolCallDelta {
        id: String,
        input_fragment: String,
    },
    /// End of one tool call, with the complete parsed input.
    ToolCallEnd {
        id: String,
        input: Value,
    },
    /// End of one reasoning block. `sealed` carries the provider-tagged
    /// opaque payload for same-provider round-trip.
    ReasoningDone {
        summary: String,
        sealed: Option<SealedPayload>,
    },
    Usage(Usage),
    Done(StopReason),
}

/// Provider failure classes. The adapter retries `RateLimited`,
/// `Server`, and `Network` with backoff. On `Auth` it refreshes
/// credentials once, retries once, then fails.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProviderError {
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("rate limited")]
    RateLimited { retry_after: Option<Duration> },
    #[error("server error: {0}")]
    Server(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("protocol error: {0}")]
    Protocol(String),
}

/// The response event stream for one request.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>;

/// The contract every adapter implements.
pub trait Provider: Send + Sync {
    /// Stable adapter id, e.g. "openai-codex". Sealed payloads carry
    /// this id so they never go to a different provider.
    fn id(&self) -> &'static str;

    fn capabilities(&self) -> Capabilities;

    /// Send one request and return the response event stream.
    fn stream(&self, request: Request) -> BoxFuture<'_, Result<EventStream, ProviderError>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Role;
    use std::collections::VecDeque;
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    /// Minimal in-memory provider proving the contract is implementable
    /// and object-safe without an async runtime.
    struct NullProvider;

    struct VecStream(VecDeque<Result<StreamEvent, ProviderError>>);

    impl Stream for VecStream {
        type Item = Result<StreamEvent, ProviderError>;

        fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            Poll::Ready(self.0.pop_front())
        }
    }

    impl Provider for NullProvider {
        fn id(&self) -> &'static str {
            "null"
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities {
                streaming: true,
                ..Capabilities::default()
            }
        }

        fn stream(&self, _request: Request) -> BoxFuture<'_, Result<EventStream, ProviderError>> {
            Box::pin(async {
                let events = VecDeque::from([
                    Ok(StreamEvent::TextDelta("hi".into())),
                    Ok(StreamEvent::Done(StopReason::EndTurn)),
                ]);
                Ok(Box::pin(VecStream(events)) as EventStream)
            })
        }
    }

    fn drain(mut stream: EventStream) -> Vec<StreamEvent> {
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut out = Vec::new();
        while let Poll::Ready(item) = stream.as_mut().poll_next(&mut cx) {
            match item {
                Some(event) => out.push(event.unwrap()),
                None => break,
            }
        }
        out
    }

    #[test]
    fn provider_is_object_safe_and_streams_events() {
        let provider: Box<dyn Provider> = Box::new(NullProvider);
        assert_eq!(provider.id(), "null");
        assert!(provider.capabilities().streaming);
        assert!(!provider.capabilities().tool_calls);

        let mut request = Request::new("test");
        request.messages = vec![Message {
            role: Role::User,
            content: Vec::new(),
        }];
        let future = provider.stream(request);
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        let stream = match std::pin::pin!(future).poll(&mut cx) {
            Poll::Ready(result) => result.unwrap(),
            Poll::Pending => panic!("NullProvider must resolve immediately"),
        };
        let events = drain(stream);
        assert_eq!(
            events,
            vec![
                StreamEvent::TextDelta("hi".into()),
                StreamEvent::Done(StopReason::EndTurn),
            ]
        );
    }
}
