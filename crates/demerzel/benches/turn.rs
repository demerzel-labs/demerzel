//! Layer-2 benchmark: per-turn latency against a local mock endpoint.
//!
//! Three benches decompose the number:
//! - `transport_floor`: one bare HTTP POST to the mock, full body
//!   read. Everything below the harness: localhost round-trip, mock
//!   parse, SSE serialize.
//! - `turn_text`: one full engine turn with a text-only reply. The
//!   harness-added latency is `turn_text - transport_floor`.
//! - `turn_read_tool`: a turn with one read dispatch, two model calls.
//!
//! The bench drives `demerzel::Runtime`, the same wiring the
//! interfaces use. Session and engine construction, and session-file
//! cleanup, run outside the timed span. Session files land under the
//! cargo target directory, not tmpfs.
//!
//! The mock runs on its own single-worker runtime, so server work
//! never shares the bench thread. The bench runtime is
//! `current_thread`, the flavor the shipped binary uses. The auth
//! store keeps its default token URL; the script never answers 401,
//! so no refresh fires.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use demerzel::{AuthPaths, RunConfig, Runtime};
use demerzel_core::{ApprovalMode, EngineEvent, NoPrompt, TurnOutcome};
use demerzel_mock::codex::{text_turn, tool_turn};
use demerzel_mock::{MockResponse, MockServer};
use serde_json::json;

fn noop(_: EngineEvent<'_>) {}

/// The workspace target directory: a real filesystem, unlike `/tmp`.
fn bench_root() -> PathBuf {
    let target = match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"),
    };
    let root = target.join("bench-data");
    std::fs::create_dir_all(&root).expect("create the bench data root");
    root
}

/// The long-lived pieces one bench shares across iterations. Field
/// order matters: the mock drops before its runtime.
struct Bed {
    mock: MockServer,
    _server_rt: tokio::runtime::Runtime,
    project: tempfile::TempDir,
    _data: tempfile::TempDir,
    runtime: Runtime,
}

impl Bed {
    fn start(script: Vec<MockResponse>) -> Self {
        let root = bench_root();
        let server_rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("build the mock-server runtime");
        let mock = server_rt.block_on(MockServer::start_looping(script));
        let project = tempfile::tempdir_in(&root).unwrap();
        let data = tempfile::tempdir_in(&root).unwrap();
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

        let mut config = RunConfig::new(project.path().to_path_buf());
        config.model = "bench-model".into();
        config.approval = ApprovalMode::Auto;
        config.data_root = Some(data.path().join("sessions"));
        config.base_url = Some(mock.url());
        config.auth = Some(AuthPaths {
            store: auth_path,
            codex_cli: None,
        });
        let runtime = Runtime::new(config).expect("build the harness runtime");

        Self {
            mock,
            _server_rt: server_rt,
            project,
            _data: data,
            runtime,
        }
    }

    /// One measured turn: build the engine and remove the session file
    /// outside the timed span; assert the workload really ran.
    async fn timed_turn(&self, expected_history: usize) -> Duration {
        let config = self.runtime.config();
        let settings = demerzel::SessionSettings::from_config(config);
        let (session, replay) = self
            .runtime
            .open_session(&config.resume, &settings)
            .unwrap();
        let mut engine = self
            .runtime
            .engine(session, replay.messages, &settings)
            .unwrap();
        let start = Instant::now();
        let outcome = engine
            .user_turn("bench".into(), &noop, &mut NoPrompt)
            .await
            .unwrap();
        let elapsed = start.elapsed();
        assert_eq!(outcome, TurnOutcome::Completed);
        assert_eq!(engine.history().len(), expected_history);
        let path = engine.session().path().to_path_buf();
        drop(engine);
        std::fs::remove_file(path).unwrap();
        elapsed
    }
}

fn bench_turns(c: &mut Criterion) {
    let bench_rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build the bench runtime");

    // The transport-and-mock floor under every turn.
    let bed = Bed::start(vec![text_turn("done")]);
    let client = reqwest::Client::new();
    let url = format!("{}/responses", bed.mock.url());
    c.bench_function("transport_floor", |b| {
        b.to_async(&bench_rt).iter_custom(|iters| {
            let client = &client;
            let url = &url;
            async move {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let start = Instant::now();
                    let body = client
                        .post(url)
                        .json(&json!({"model": "bench-model"}))
                        .send()
                        .await
                        .unwrap()
                        .text()
                        .await
                        .unwrap();
                    total += start.elapsed();
                    assert!(body.contains("response.completed"));
                }
                total
            }
        })
    });
    drop(bed);

    // One model call, text only: user and assistant in the history.
    let bed = Bed::start(vec![text_turn("done")]);
    c.bench_function("turn_text", |b| {
        b.to_async(&bench_rt).iter_custom(|iters| {
            let bed = &bed;
            async move {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    total += bed.timed_turn(2).await;
                }
                total
            }
        })
    });
    drop(bed);

    // Two model calls with one read dispatch between them: user,
    // tool-call, tool-result, and final assistant in the history.
    let bed = Bed::start(vec![
        tool_turn("call_1", "read", json!({"path": "f.txt"})),
        text_turn("done"),
    ]);
    std::fs::write(bed.project.path().join("f.txt"), "alpha\nbeta\n").unwrap();
    c.bench_function("turn_read_tool", |b| {
        b.to_async(&bench_rt).iter_custom(|iters| {
            let bed = &bed;
            async move {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    total += bed.timed_turn(4).await;
                }
                total
            }
        })
    });
}

criterion_group!(benches, bench_turns);
criterion_main!(benches);
