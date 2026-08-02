//! Mock Codex endpoint for hermetic tests and Layer-2 benchmarks.
//!
//! The server implements `POST /responses` and replays a scripted list of
//! responses in request order. It also implements `POST /oauth/token` for
//! the credential-refresh path. It records every request so tests can
//! assert on serialization and headers. Never part of the shipped binary.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;

use axum::Router;
use axum::body::Body;
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

/// One scripted reply for `POST /responses`.
#[derive(Debug, Clone)]
pub struct MockResponse {
    pub status: u16,
    pub events: Vec<SseEvent>,
}

impl MockResponse {
    /// A streaming reply with the given events.
    pub fn stream(events: Vec<Value>) -> Self {
        Self {
            status: 200,
            events: events.into_iter().map(SseEvent::new).collect(),
        }
    }

    /// An error reply with the given HTTP status and no stream.
    pub fn error(status: u16) -> Self {
        Self {
            status,
            events: Vec::new(),
        }
    }
}

/// One recorded request.
#[derive(Debug, Clone)]
pub struct Received {
    pub path: String,
    pub authorization: Option<String>,
    pub account_id: Option<String>,
    pub body: Value,
}

struct ServerState {
    responses: Mutex<VecDeque<MockResponse>>,
    token_responses: Mutex<VecDeque<Value>>,
    received: Mutex<Vec<Received>>,
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
        let state = Arc::new(ServerState {
            responses: Mutex::new(responses.into()),
            token_responses: Mutex::new(token_responses.into()),
            received: Mutex::new(Vec::new()),
        });
        let app = Router::new()
            .route("/responses", post(handle_responses))
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
        body,
    });
}

async fn handle_responses(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    record(&state, "/responses", &headers, body);
    let next = state.responses.lock().unwrap().pop_front();
    let Some(reply) = next else {
        return (StatusCode::GONE, "mock script exhausted").into_response();
    };
    if reply.status != 200 {
        let status =
            StatusCode::from_u16(reply.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        return (
            status,
            json!({"error": {"message": "scripted error"}}).to_string(),
        )
            .into_response();
    }
    let mut sse = String::new();
    for event in &reply.events {
        sse.push_str("data: ");
        sse.push_str(&event.data.to_string());
        sse.push_str("\n\n");
    }
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Body::from(sse))
        .expect("build sse response")
}

async fn handle_token(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    record(&state, "/oauth/token", &headers, body);
    let next = state.token_responses.lock().unwrap().pop_front();
    let Some(reply) = next else {
        return (StatusCode::GONE, "mock token script exhausted").into_response();
    };
    (StatusCode::OK, reply.to_string()).into_response()
}
