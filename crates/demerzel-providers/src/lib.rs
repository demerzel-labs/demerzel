//! The provider contract: the trait, capabilities, request shape, and
//! stream events that every adapter implements.
//!
//! The harness reads [`Capabilities`] flags and degrades features. It
//! never assumes a capability. The trait is object-safe so the harness
//! can hold `Box<dyn Provider>` and route per model role.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use demerzel_core::{Message, StopReason, Usage};
use futures_core::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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

/// A tool the model may call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool input.
    pub input_schema: Value,
}

/// One model request. The system prompt stays separate from history so
/// prompt assembly can keep the prefix stable.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub model: String,
    pub system_prompt: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub max_output_tokens: Option<u64>,
}

/// Neutral stream events. Adapters map wire events to these; unknown
/// wire events are dropped, not errors.
#[derive(Debug, Clone, PartialEq)]
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
    ToolCallEnd {
        id: String,
    },
    Usage(Usage),
    Done(StopReason),
}

/// Provider failure classes. The loop retries `RateLimited`, `Server`,
/// and `Network` with backoff. On `Auth` it refreshes credentials once,
/// retries once, then fails.
#[derive(Debug, thiserror::Error)]
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

/// Boxed future, used to keep the trait object-safe.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

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
    use std::collections::VecDeque;
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

        let request = Request {
            model: "test".into(),
            system_prompt: String::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            max_output_tokens: None,
        };
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

    #[test]
    fn tool_definition_serializes_with_schema() {
        let tool = ToolDefinition {
            name: "read".into(),
            description: "Read a file".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
            }),
        };
        let value = serde_json::to_value(&tool).unwrap();
        assert_eq!(value["name"], "read");
        assert_eq!(value["input_schema"]["type"], "object");
    }
}
