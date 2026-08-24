//! Layer-2 benchmark: render throughput of the full-screen interface.
//!
//! Three benches decompose the number, all on a 120x40 screen over a
//! transcript that already holds more history than one screen:
//! - `frame`: one redraw with no new content. The per-frame cost of
//!   the layout: the visible rows come from the blocks' layout
//!   caches, and the walk stops at the viewport, so the cost does not
//!   move with the history length. The buffer diff finds nothing to
//!   change, so this is the floor of a frame; `stream` carries the
//!   changed frames.
//! - `ingest`: 64 KiB of model text pushed in 16-byte deltas, no
//!   frames. The transcript's cost per streamed byte.
//! - `stream`: the same 64 KiB with a frame every 64 deltas: 65
//!   changed frames. The frames are almost all of the time, so the
//!   throughput is in frames per second; the bytes ride along. A
//!   real stream arrives slower than the renderer draws, so arrival,
//!   not the frame cap, sets the batch in use.
//!
//! Each bench runs twice: on prose, and on a formatted stream with
//! headings, lists, a table, and code fences (`*-formatted`). A
//! formatted block renders again whole on every frame that changes
//! it, so the formatted `stream` carries the cost of the markdown
//! pass, and `frame-formatted` redraws a screen whose rows come from
//! formatted blocks. The prose numbers keep the M2 workloads, so
//! they compare with the M2 receipts.
//!
//! The screen is headless: frames render into a buffer and diff
//! against the last one, the same work the live interface does before
//! the terminal write. Outside this number: the terminal-size query
//! and the escape-sequence encoding of the changed cells on the live
//! backend, and the write itself. The transcript stays under its
//! block cap throughout, so the drop path is outside it. The stream
//! crosses the per-block byte cap once, so one block split is inside
//! `ingest` and `stream`.

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use demerzel_tui::bench::{MAX_BLOCK_BYTES, Screen};

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

/// A history block of one line; lengths cycle from one row to several
/// wrapped rows.
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

/// The formatted stream: the same size, built from a markdown
/// document with one of each element. ASCII, as above.
fn formatted_text() -> String {
    const DOC: &str = "## A heading\n\n\
        Some prose with *emphasis*, **strong** words, and `inline code` that runs long \
        enough to wrap on the screen.\n\n\
        - first item\n- second item with more words\n  - nested item\n- third\n\n\
        | name | value | note |\n|---|---:|---|\n| alpha | 1 | first |\n| beta | 22 | second |\n\n\
        ```rust\nfn main() {\n    println!(\"hello\");\n}\n```\n\n\
        > A quote that also runs long enough to wrap when the pane is narrow.\n\n";
    let mut text = String::with_capacity(STREAM + DOC.len());
    while text.len() < STREAM {
        text.push_str(DOC);
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
    let mut group = c.benchmark_group("render");
    for (suffix, text) in [("", stream_text()), ("-formatted", formatted_text())] {
        let deltas: Vec<&str> = text
            .as_bytes()
            .chunks(DELTA)
            .map(|chunk| std::str::from_utf8(chunk).expect("ascii"))
            .collect();

        // Workload checks, once and outside the timed spans: the
        // stream lands as one block per byte cap, the total stays
        // under the block cap so the drop path is not measured, and
        // the stream draws the frame count the throughput names.
        let streamed_blocks = HISTORY_LINES + STREAM.div_ceil(MAX_BLOCK_BYTES);
        let mut check = screen_with_history();
        assert_eq!(check.blocks(), HISTORY_LINES);
        ingest(&mut check, &deltas);
        assert_eq!(check.blocks(), streamed_blocks);
        let mut check = screen_with_history();
        assert_eq!(stream(&mut check, &deltas), FRAMES);
        assert_eq!(check.blocks(), streamed_blocks);
        drop(check);

        // The formatted `frame` redraws a screen whose tail is the
        // streamed text, so the formatted rows are the ones on
        // screen; the prose `frame` keeps the M2 workload.
        let mut screen = screen_with_history();
        if !suffix.is_empty() {
            ingest(&mut screen, &deltas);
            screen.draw();
        }
        group.throughput(Throughput::Elements(1));
        group.bench_function(format!("frame{suffix}"), |b| b.iter(|| screen.draw()));
        drop(screen);

        group.throughput(Throughput::Bytes(STREAM as u64));
        group.bench_function(format!("ingest{suffix}"), |b| {
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
        group.bench_function(format!("stream{suffix}"), |b| {
            b.iter_batched(
                screen_with_history,
                |mut screen| {
                    stream(&mut screen, &deltas);
                    screen
                },
                BatchSize::LargeInput,
            )
        });
    }
    group.finish();
}

criterion_group!(benches, bench_render);
criterion_main!(benches);
