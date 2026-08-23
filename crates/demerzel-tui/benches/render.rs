//! Layer-2 benchmark: render throughput of the full-screen interface.
//!
//! Three benches decompose the number, all on a 120x40 screen over a
//! transcript that already holds more history than one screen:
//! - `frame`: one redraw with no new content. The per-frame cost of
//!   the layout: the visible tail wraps, the rest of the history does
//!   not, so the cost does not move with the history length. The
//!   buffer diff finds nothing to change, so this is the floor of a
//!   frame; `stream` carries the changed frames.
//! - `ingest`: 64 KiB of model text pushed in 16-byte deltas, no
//!   frames. The transcript's cost per streamed byte.
//! - `stream`: the same 64 KiB with a frame every 64 deltas: 65
//!   changed frames. The frames are almost all of the time, so the
//!   throughput is in frames per second; the bytes ride along. A
//!   real stream arrives slower than the renderer draws, so arrival,
//!   not the frame cap, sets the batch in use.
//!
//! The screen is headless: frames render into a buffer and diff
//! against the last one, the same work the live interface does before
//! the terminal write. Outside this number: the terminal-size query
//! and the escape-sequence encoding of the changed cells on the live
//! backend, and the write itself. The transcript stays under its line
//! cap and its entry cap throughout, so the eviction paths are outside
//! it too.

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use demerzel_tui::bench::Screen;

const WIDTH: u16 = 120;
const HEIGHT: u16 = 40;
/// More than one 40-row screen, so the viewport never shows the start.
const HISTORY_LINES: usize = 200;
/// Bytes per streamed delta: a few tokens, the size adapters deliver.
const DELTA: usize = 16;
const STREAM: usize = 64 * 1024;
const DELTAS_PER_FRAME: usize = 64;
/// Frames in one `stream` iteration: one per batch, one at the end.
const FRAMES: usize = STREAM / DELTA / DELTAS_PER_FRAME + 1;

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

/// The `ingest` workload: every delta, no frames.
fn ingest(screen: &mut Screen, deltas: &[&str]) {
    for delta in deltas {
        screen.push_text(delta);
    }
}

/// The `stream` workload: every delta, a frame per batch and one at
/// the end. Returns the frames drawn.
fn stream(screen: &mut Screen, deltas: &[&str]) -> usize {
    let mut frames = 0;
    for (index, delta) in deltas.iter().enumerate() {
        screen.push_text(delta);
        if index % DELTAS_PER_FRAME == DELTAS_PER_FRAME - 1 {
            screen.draw();
            frames += 1;
        }
    }
    screen.draw();
    frames + 1
}

fn bench_render(c: &mut Criterion) {
    let text = stream_text();
    let deltas: Vec<&str> = text
        .as_bytes()
        .chunks(DELTA)
        .map(|chunk| std::str::from_utf8(chunk).expect("ascii"))
        .collect();
    let mut group = c.benchmark_group("render");

    // Workload checks, once and outside the timed spans: every
    // streamed line lands as one transcript line, the total stays
    // under the cap so the drop path is not measured, and the stream
    // draws the frame count the throughput names.
    let streamed_lines = HISTORY_LINES + text.matches('\n').count() + 1;
    let mut check = screen_with_history();
    assert_eq!(check.lines(), HISTORY_LINES);
    ingest(&mut check, &deltas);
    assert_eq!(check.lines(), streamed_lines);
    let mut check = screen_with_history();
    assert_eq!(stream(&mut check, &deltas), FRAMES);
    assert_eq!(check.lines(), streamed_lines);
    drop(check);

    let mut screen = screen_with_history();
    group.bench_function("frame", |b| b.iter(|| screen.draw()));
    drop(screen);

    group.throughput(Throughput::Bytes(STREAM as u64));
    group.bench_function("ingest", |b| {
        b.iter_batched(
            screen_with_history,
            |mut screen| {
                ingest(&mut screen, &deltas);
                screen
            },
            BatchSize::LargeInput,
        )
    });

    group.throughput(Throughput::Elements(FRAMES as u64));
    group.bench_function("stream", |b| {
        b.iter_batched(
            screen_with_history,
            |mut screen| {
                stream(&mut screen, &deltas);
                screen
            },
            BatchSize::LargeInput,
        )
    });
    group.finish();
}

criterion_group!(benches, bench_render);
criterion_main!(benches);
