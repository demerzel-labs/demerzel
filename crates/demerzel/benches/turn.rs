//! Layer-2 benchmark: harness-added latency per turn against the mock
//! server. The number includes the localhost round-trip to the mock
//! and the session-file appends; it excludes engine construction and
//! session creation, which happen outside the timed span.
//!
//! The mock runs on its own single-worker runtime, so server work
//! never shares the bench thread. The bench runtime is
//! `current_thread`, the flavor the shipped binary uses.

use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use demerzel_core::{
    ApprovalMode, ApprovalPolicy, Engine, EngineConfig, EngineEvent, NoPrompt, SessionStore,
};
use demerzel_mock::{MockResponse, MockServer};
use demerzel_providers::openai_codex::{AuthStore, CodexConfig, OpenAiCodex};
use demerzel_tools::Registry;
use serde_json::{Value, json};

fn text_delta(text: &str) -> Value {
    json!({"type": "response.output_text.delta", "delta": text})
}

fn completed() -> Value {
    json!({
        "type": "response.completed",
        "response": {"usage": {
            "input_tokens": 10,
            "output_tokens": 5,
            "input_tokens_details": {"cached_tokens": 2},
        }},
    })
}

fn text_turn(text: &str) -> MockResponse {
    MockResponse::stream(vec![text_delta(text), completed()])
}

fn tool_turn(name: &str, input: Value) -> MockResponse {
    MockResponse::stream(vec![
        json!({
            "type": "response.output_item.added",
            "item": {"type": "function_call", "id": "item_1", "call_id": "call_1", "name": name},
        }),
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "call_id": "call_1",
                "arguments": input.to_string(),
            },
        }),
        completed(),
    ])
}

fn noop(_: EngineEvent<'_>) {}

/// The long-lived pieces one bench shares across iterations: the mock,
/// the adapter with its connection pool, the registry, and the store.
struct Bed {
    _server_rt: tokio::runtime::Runtime,
    _mock: MockServer,
    project: tempfile::TempDir,
    _data: tempfile::TempDir,
    provider: OpenAiCodex,
    registry: Registry,
    store: SessionStore,
}

impl Bed {
    fn start(script: Vec<MockResponse>) -> Self {
        let server_rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("build the mock-server runtime");
        let mock = server_rt.block_on(MockServer::start_looping(script));
        let project = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let auth_path = data.path().join("auth.json");
        std::fs::write(
            &auth_path,
            json!({
                "version": 1,
                "openai_codex": {"access_token": "bench-token", "refresh_token": "bench-refresh"},
            })
            .to_string(),
        )
        .unwrap();
        let provider = OpenAiCodex::new(
            CodexConfig {
                base_url: mock.url(),
                ..CodexConfig::default()
            },
            AuthStore::open(auth_path, None).unwrap(),
        );
        let registry = Registry::full(project.path());
        let store = SessionStore::at(data.path().join("sessions"));
        Self {
            _server_rt: server_rt,
            _mock: mock,
            project,
            _data: data,
            provider,
            registry,
            store,
        }
    }

    fn engine(&self) -> Engine<'_> {
        let session = self
            .store
            .create(self.project.path(), "openai-codex", "bench-model")
            .unwrap();
        Engine::new(
            &self.provider,
            &self.registry,
            ApprovalPolicy::new(ApprovalMode::Auto),
            EngineConfig::new("bench-model"),
            session,
        )
    }
}

fn bench_turns(c: &mut Criterion) {
    let bench_rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build the bench runtime");

    // One model call, text only: the floor of the per-turn overhead.
    let bed = Bed::start(vec![text_turn("done")]);
    c.bench_function("turn_text", |b| {
        b.to_async(&bench_rt).iter_custom(|iters| {
            let bed = &bed;
            async move {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let mut engine = bed.engine();
                    let start = Instant::now();
                    engine
                        .user_turn("bench".into(), &noop, &mut NoPrompt)
                        .await
                        .unwrap();
                    total += start.elapsed();
                }
                total
            }
        })
    });
    drop(bed);

    // Two model calls with one read dispatch between them: the tool
    // round-trip cost on top of the text floor.
    let bed = Bed::start(vec![
        tool_turn("read", json!({"path": "f.txt"})),
        text_turn("done"),
    ]);
    std::fs::write(bed.project.path().join("f.txt"), "alpha\nbeta\n").unwrap();
    c.bench_function("turn_read_tool", |b| {
        b.to_async(&bench_rt).iter_custom(|iters| {
            let bed = &bed;
            async move {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let mut engine = bed.engine();
                    let start = Instant::now();
                    engine
                        .user_turn("bench".into(), &noop, &mut NoPrompt)
                        .await
                        .unwrap();
                    total += start.elapsed();
                }
                total
            }
        })
    });
}

criterion_group!(benches, bench_turns);
criterion_main!(benches);
