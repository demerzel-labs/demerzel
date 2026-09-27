//! Mock model endpoint for hermetic tests and Layer-2 benchmarks.
//!
//! The server implements `POST /responses`, `POST /v1/responses`,
//! `POST /v1/messages`, `POST /v1/chat/completions`, and
//! `GET /v1/models` for key checks. All five replay one scripted list
//! of responses in request order. It also implements
//! `POST /oauth/token` for the credential-refresh and login-exchange
//! paths. It records every request so tests can assert on
//! serialization and headers. Never part of the shipped binary.

pub mod anthropic;
pub mod codex;
pub mod compat;
pub mod responses;

use std::collections::VecDeque;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::{get, post};
use serde_json::Value;
use serde_json::json;

/// One server-sent event in a scripted response: the text after
/// `data: `.
#[derive(Debug, Clone)]
pub struct SseEvent {
    pub data: String,
}

impl SseEvent {
    /// An event whose payload is one JSON value.
    pub fn new(data: Value) -> Self {
        Self {
            data: data.to_string(),
        }
    }

    /// An event whose payload is not JSON, such as the `[DONE]`
    /// marker of the Chat Completions stream.
    pub fn raw(data: impl Into<String>) -> Self {
        Self { data: data.into() }
    }
}

/// A check on a request body. `Err` makes the mock refuse the request
/// with status 400 and the message, the way a strict server refuses a
/// request that lacks a field it requires.
pub type BodyCheck = fn(&Value) -> Result<(), String>;

/// One scripted reply for the model routes.
#[derive(Debug, Clone)]
pub struct MockResponse {
    pub status: u16,
    pub events: Vec<SseEvent>,
    /// The whole SSE body, sent verbatim in place of `events`. A
    /// fixture file captured from a real server uses it.
    pub body: Option<String>,
    /// Send the events, then hold the connection open forever.
    pub stall: bool,
    /// A `retry-after` header value, in seconds.
    pub retry_after: Option<u64>,
    /// A check the request body must pass before this reply goes out.
    pub check: Option<BodyCheck>,
    /// The `content-type` of a 200 reply.
    pub content_type: &'static str,
}

impl MockResponse {
    /// A streaming reply with the given events.
    pub fn stream(events: Vec<Value>) -> Self {
        Self::events(events.into_iter().map(SseEvent::new).collect())
    }

    /// A streaming reply with the given events, JSON or raw.
    pub fn events(events: Vec<SseEvent>) -> Self {
        Self {
            status: 200,
            events,
            body: None,
            stall: false,
            retry_after: None,
            check: None,
            content_type: "text/event-stream",
        }
    }

    /// A streaming reply whose body is `text`, verbatim.
    pub fn sse_body(text: impl Into<String>) -> Self {
        Self {
            body: Some(text.into()),
            ..Self::events(Vec::new())
        }
    }

    /// This reply with another `content-type`, the way a wrong
    /// endpoint answers a stream request with a page.
    pub fn with_content_type(mut self, content_type: &'static str) -> Self {
        self.content_type = content_type;
        self
    }

    /// This reply, behind a check on the request body.
    pub fn with_check(mut self, check: BodyCheck) -> Self {
        self.check = Some(check);
        self
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
            ..Self::events(Vec::new())
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
    pub content_type: Option<String>,
    pub body: Value,
}

struct ServerState {
    responses: Mutex<VecDeque<MockResponse>>,
    token_responses: Mutex<VecDeque<Value>>,
    received: Mutex<Vec<Received>>,
    /// Cycle the response script forever and record nothing. The
    /// Layer-2 benchmarks run unbounded iteration counts.
    cycle: bool,
    /// Numbers the `x-request-id` header on every model-route reply,
    /// the way the live endpoints tag a response.
    replies: AtomicU64,
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
            replies: AtomicU64::new(0),
        });
        let app = Router::new()
            .route("/responses", post(handle_responses))
            .route("/v1/responses", post(handle_api_responses))
            .route("/v1/messages", post(handle_messages))
            .route("/v1/chat/completions", post(handle_chat))
            .route("/v1/models", get(handle_models))
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
        content_type: header("content-type"),
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

async fn handle_api_responses(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    scripted_reply(&state, "/v1/responses", &headers, body)
}

async fn handle_messages(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    scripted_reply(&state, "/v1/messages", &headers, body)
}

async fn handle_chat(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    scripted_reply(&state, "/v1/chat/completions", &headers, body)
}

async fn handle_models(State(state): State<Arc<ServerState>>, headers: HeaderMap) -> Response {
    scripted_reply(&state, "/v1/models", &headers, String::new())
}

/// Replay the next scripted reply, whichever model route asked.
fn scripted_reply(state: &ServerState, path: &str, headers: &HeaderMap, body: String) -> Response {
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
    // Parse the body only when something reads it: the record or a
    // check. A cycling benchmark does neither, so its floor holds.
    let check = next.as_ref().and_then(|reply| reply.check);
    let parsed = (!state.cycle || check.is_some())
        .then(|| serde_json::from_str::<Value>(&body).unwrap_or(Value::Null));
    let refusal = match (check, &parsed) {
        (Some(check), Some(body)) => check(body).err(),
        _ => None,
    };
    if !state.cycle
        && let Some(body) = parsed
    {
        record(state, path, headers, body);
    }
    let Some(reply) = next else {
        return (StatusCode::GONE, "mock script exhausted").into_response();
    };
    if let Some(message) = refusal {
        return (
            StatusCode::BAD_REQUEST,
            json!({"error": {"message": message}}).to_string(),
        )
            .into_response();
    }
    let request_id = format!("mock-{}", state.replies.fetch_add(1, Ordering::Relaxed) + 1);
    if reply.status != 200 {
        let status =
            StatusCode::from_u16(reply.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        // The body echoes the request's credential header, the way a
        // careless proxy can: an adapter must redact it.
        let echo = headers
            .get("authorization")
            .or_else(|| headers.get("x-api-key"))
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        let mut response = (
            status,
            json!({"error": {"message": format!("scripted error for {echo}")}}).to_string(),
        )
            .into_response();
        if let Some(seconds) = reply.retry_after {
            response.headers_mut().insert(
                "retry-after",
                seconds.to_string().parse().expect("digits are a header"),
            );
        }
        response.headers_mut().insert(
            "x-request-id",
            request_id.parse().expect("a plain id is a header"),
        );
        return response;
    }
    let sse = match reply.body {
        Some(body) => body,
        None => {
            let mut sse = String::new();
            for event in &reply.events {
                sse.push_str("data: ");
                sse.push_str(&event.data);
                sse.push_str("\n\n");
            }
            sse
        }
    };
    let body = if reply.stall {
        Body::from_stream(StallBody(Some(Bytes::from(sse))))
    } else {
        Body::from(sse)
    };
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", reply.content_type)
        .header("x-request-id", request_id)
        .body(body)
        .expect("build sse response")
}

async fn handle_token(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if !state.cycle {
        record(&state, "/oauth/token", &headers, parse_token_body(&body));
    }
    let next = state.token_responses.lock().unwrap().pop_front();
    let Some(reply) = next else {
        return (StatusCode::GONE, "mock token script exhausted").into_response();
    };
    (StatusCode::OK, reply.to_string()).into_response()
}

/// The token route receives two body shapes: the refresh flow posts
/// JSON, the login exchange posts a urlencoded form. Record both as
/// one JSON object so tests assert the same way.
fn parse_token_body(body: &str) -> Value {
    if let Ok(value) = serde_json::from_str(body) {
        return value;
    }
    let mut map = serde_json::Map::new();
    for pair in body.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        map.insert(form_decode(key), Value::String(form_decode(value)));
    }
    Value::Object(map)
}

/// Decode one urlencoded form value. A copy of the provider crate's
/// private decoder: depending on `demerzel-providers` here would make
/// a dev-dependency cycle.
fn form_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
