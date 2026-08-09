//! Mock model endpoint for hermetic tests and Layer-2 benchmarks.
//!
//! The server implements `POST /responses` (Codex) and
//! `POST /v1/messages` (Anthropic); both replay one scripted list of
//! responses in request order. It also implements `POST /oauth/token`
//! for the credential-refresh path. It records every request so tests
//! can assert on serialization and headers. Never part of the shipped
//! binary.

pub mod anthropic;
pub mod codex;

use std::collections::VecDeque;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::task::{Context, Poll};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::post;
use serde_json::Value;
use serde_json::json;

/// One server-sent event in a scripted response.
#[derive(Debug, Clone)]
pub struct SseEvent {
    pub data: Value,
}

impl SseEvent {
    pub fn new(data: Value) -> Self {
        Self { data }
    }
}

/// One scripted reply for the model routes.
#[derive(Debug, Clone)]
pub struct MockResponse {
    pub status: u16,
    pub events: Vec<SseEvent>,
    /// Send the events, then hold the connection open forever.
    pub stall: bool,
    /// A `retry-after` header value, in seconds.
    pub retry_after: Option<u64>,
}

impl MockResponse {
    /// A streaming reply with the given events.
    pub fn stream(events: Vec<Value>) -> Self {
        Self {
            status: 200,
            events: events.into_iter().map(SseEvent::new).collect(),
            stall: false,
            retry_after: None,
        }
    }

    /// A streaming reply that sends its events and never completes.
    /// Abort tests use it to cut a turn mid-stream.
    pub fn stream_then_stall(events: Vec<Value>) -> Self {
        Self {
            stall: true,
            ..Self::stream(events)
        }
    }

    /// An error reply with the given HTTP status and no stream.
    pub fn error(status: u16) -> Self {
        Self {
            status,
            events: Vec::new(),
            stall: false,
            retry_after: None,
        }
    }

    /// An error reply that carries a `retry-after` header.
    pub fn error_with_retry_after(status: u16, seconds: u64) -> Self {
        Self {
            retry_after: Some(seconds),
            ..Self::error(status)
        }
    }
}

/// Body that yields one chunk and then stays pending forever.
struct StallBody(Option<Bytes>);

impl futures_core::Stream for StallBody {
    type Item = Result<Bytes, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.0.take() {
            Some(bytes) => Poll::Ready(Some(Ok(bytes))),
            None => Poll::Pending,
        }
    }
}

/// One recorded request.
#[derive(Debug, Clone)]
pub struct Received {
    pub path: String,
    pub authorization: Option<String>,
    pub account_id: Option<String>,
    pub api_key: Option<String>,
    pub anthropic_version: Option<String>,
    pub body: Value,
}

struct ServerState {
    responses: Mutex<VecDeque<MockResponse>>,
    token_responses: Mutex<VecDeque<Value>>,
    received: Mutex<Vec<Received>>,
    /// Cycle the response script forever and record nothing. The
    /// Layer-2 benchmarks run unbounded iteration counts.
    cycle: bool,
}

/// A running mock server. Dropping the handle stops the server task.
pub struct MockServer {
    addr: SocketAddr,
    state: Arc<ServerState>,
    task: tokio::task::JoinHandle<()>,
}

impl MockServer {
    /// Start the server on an ephemeral local port.
    pub async fn start(responses: Vec<MockResponse>, token_responses: Vec<Value>) -> Self {
        Self::start_inner(responses, token_responses, false).await
    }

    /// Start the server with a `/responses` script that cycles
    /// forever. This mode records no requests on any route, so an
    /// unbounded benchmark run holds no growing state and
    /// [`MockServer::received`] returns an empty list. The token
    /// route has no script, so a refresh attempt gets 410.
    pub async fn start_looping(responses: Vec<MockResponse>) -> Self {
        Self::start_inner(responses, Vec::new(), true).await
    }

    async fn start_inner(
        responses: Vec<MockResponse>,
        token_responses: Vec<Value>,
        cycle: bool,
    ) -> Self {
        let state = Arc::new(ServerState {
            responses: Mutex::new(responses.into()),
            token_responses: Mutex::new(token_responses.into()),
            received: Mutex::new(Vec::new()),
            cycle,
        });
        let app = Router::new()
            .route("/responses", post(handle_responses))
            .route("/v1/messages", post(handle_messages))
            .route("/oauth/token", post(handle_token))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock server");
        let addr = listener.local_addr().expect("mock server addr");
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("mock server run");
        });
        Self { addr, state, task }
    }

    /// Base URL of the running server, without a trailing slash.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// All requests received so far, in order.
    pub fn received(&self) -> Vec<Received> {
        self.state.received.lock().unwrap().clone()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn record(state: &ServerState, path: &str, headers: &HeaderMap, body: Value) {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    state.received.lock().unwrap().push(Received {
        path: path.to_string(),
        authorization: header("authorization"),
        account_id: header("chatgpt-account-id"),
        api_key: header("x-api-key"),
        anthropic_version: header("anthropic-version"),
        body,
    });
}

async fn handle_responses(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    scripted_reply(&state, "/responses", &headers, body)
}

async fn handle_messages(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    scripted_reply(&state, "/v1/messages", &headers, body)
}

/// Replay the next scripted reply, whichever model route asked.
fn scripted_reply(state: &ServerState, path: &str, headers: &HeaderMap, body: String) -> Response {
    if !state.cycle {
        let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        record(state, path, headers, body);
    }
    let next = {
        let mut responses = state.responses.lock().unwrap();
        let reply = responses.pop_front();
        if state.cycle
            && let Some(reply) = &reply
        {
            responses.push_back(reply.clone());
        }
        reply
    };
    let Some(reply) = next else {
        return (StatusCode::GONE, "mock script exhausted").into_response();
    };
    if reply.status != 200 {
        let status =
            StatusCode::from_u16(reply.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut response = (
            status,
            json!({"error": {"message": "scripted error"}}).to_string(),
        )
            .into_response();
        if let Some(seconds) = reply.retry_after {
            response.headers_mut().insert(
                "retry-after",
                seconds.to_string().parse().expect("digits are a header"),
            );
        }
        return response;
    }
    let mut sse = String::new();
    for event in &reply.events {
        sse.push_str("data: ");
        sse.push_str(&event.data.to_string());
        sse.push_str("\n\n");
    }
    let body = if reply.stall {
        Body::from_stream(StallBody(Some(Bytes::from(sse))))
    } else {
        Body::from(sse)
    };
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(body)
        .expect("build sse response")
}

async fn handle_token(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if !state.cycle {
        let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        record(&state, "/oauth/token", &headers, body);
    }
    let next = state.token_responses.lock().unwrap().pop_front();
    let Some(reply) = next else {
        return (StatusCode::GONE, "mock token script exhausted").into_response();
    };
    (StatusCode::OK, reply.to_string()).into_response()
}
