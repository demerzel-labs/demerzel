//! Layer-2 benchmark: render throughput of the full-screen interface.
//!
//! Three benches decompose the number, all on a 120x40 screen over a
//! transcript that already holds 2,000 lines of history:
//! - `frame`: one redraw with no new content. The per-frame cost of
//!   the layout: the visible tail wraps, the rest of the history does
//!   not.
//! - `ingest`: 64 KiB of model text pushed in 16-byte deltas, no
//!   frames. The transcript's cost per streamed byte.
//! - `stream`: the same 64 KiB with a frame every 64 deltas, the
//!   batch a 16 ms frame cap makes of a fast stream. The throughput
//!   the renderer sustains, in bytes per second.
//!
//! The screen is headless: frames render into a buffer and diff
//! against the last one, the same work the live interface does before
//! the terminal write. The write itself is the terminal's cost, not
//! the harness's, and it stays outside this number.

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use demerzel_tui::bench::Screen;

const WIDTH: u16 = 120;
const HEIGHT: u16 = 40;
const HISTORY_LINES: usize = 2_000;
/// Bytes per streamed delta: a few tokens, the size adapters deliver.
const DELTA: usize = 16;
const STREAM: usize = 64 * 1024;
const DELTAS_PER_FRAME: usize = 64;

/// A history line; lengths cycle from one row to several wrapped rows.
fn history_line(index: usize) -> String {
    format!(
        "{index:05} {}",
        "lorem ipsum dolor sit ".repeat(1 + index % 7)
    )
}

/// The streamed text: ASCII prose with a newline about every 80
/// bytes, so lines form the way model output does. ASCII keeps every
/// 16-byte cut on a char boundary.
fn stream_text() -> String {
    const WORDS: &str = "the quick brown fox jumps over the lazy dog ";
    let mut text = String::with_capacity(STREAM + WORDS.len());
    let mut column = 0;
    while text.len() < STREAM {
        text.push_str(WORDS);
        column += WORDS.len();
        if column >= 80 {
            text.push('\n');
            column = 0;
        }
    }
    text.truncate(STREAM);
    assert!(text.is_ascii());
    text
}

fn screen_with_history() -> Screen {
    let mut screen = Screen::new(WIDTH, HEIGHT);
    for index in 0..HISTORY_LINES {
        screen.push_line(&history_line(index));
    }
    screen.draw();
    screen
}

fn bench_render(c: &mut Criterion) {
    let text = stream_text();
    let deltas: Vec<&str> = text
        .as_bytes()
        .chunks(DELTA)
        .map(|chunk| std::str::from_utf8(chunk).expect("ascii"))
        .collect();
    let mut group = c.benchmark_group("render");

    let mut screen = screen_with_history();
    group.bench_function("frame", |b| b.iter(|| screen.draw()));
    drop(screen);

    group.throughput(Throughput::Bytes(STREAM as u64));
    group.bench_function("ingest", |b| {
        b.iter_batched(
            screen_with_history,
            |mut screen| {
                for delta in &deltas {
                    screen.push_text(delta);
                }
                assert!(screen.lines() > HISTORY_LINES);
                screen
            },
            BatchSize::LargeInput,
        )
    });

    group.bench_function("stream", |b| {
        b.iter_batched(
            screen_with_history,
            |mut screen| {
                for (index, delta) in deltas.iter().enumerate() {
                    screen.push_text(delta);
                    if index % DELTAS_PER_FRAME == DELTAS_PER_FRAME - 1 {
                        screen.draw();
                    }
                }
                screen.draw();
                assert!(screen.lines() > HISTORY_LINES);
                screen
            },
            BatchSize::LargeInput,
        )
    });
    group.finish();
}

criterion_group!(benches, bench_render);
criterion_main!(benches);
