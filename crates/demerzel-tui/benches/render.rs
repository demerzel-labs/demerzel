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
//! - `frame-selected`: `frame` with a drag selection from the first
//!   transcript row to the last, so every row is in the span. The
//!   two end rows pay the column split and the extra spans; the rows
//!   between take the whole-row path. The number pins that a
//!   selection left on the screen costs a frame about nothing.
//! - `replay`: the first prompt of a resumed session: 50 turns of a
//!   session file — 200 messages' worth of prompts, thoughts, cards,
//!   usage lines, and markdown replies — pushed as blocks, then the
//!   first frame. The push lays nothing out; the frame lays out the
//!   rows it shows. The cost of the replay on resume, past the file
//!   read.
//! - `frame-replayed`: one redraw over that replayed transcript. The
//!   rows on screen come from cards, a thought, and formatted
//!   blocks; the walk stops at the viewport as it does over prose.
//!
//! Each bench runs twice. The prose run keeps a plain-text transcript.
//! The formatted run (`*-formatted`) streams markdown with headings,
//! lists, tables, and code fences through a transcript that renders it.
//! A formatted block renders again on each changed frame, so the
//! formatted `stream` carries the markdown pass cost.
//!
//! The screen is headless: frames render into a buffer and diff
//! against the last one, the same work the live interface does before
//! the terminal write. Outside this number: the terminal-size query
//! and the escape-sequence encoding of the changed cells on the live
//! backend, and the write itself. The transcript stays under its
//! block cap throughout, so the drop path is outside it. The stream
//! crosses the per-block byte cap once, so one block split is inside
//! `ingest` and `stream`.
//!
//! The `*-m25` benches add a separate mixed fixture, with a substantial
//! code-fence share, cards, thinking, reflow, and a filled editor.
//! The legacy fixtures above keep their names and contents.

#[path = "render/m25.rs"]
mod m25;

use std::time::Duration;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use demerzel_core::Usage;
use demerzel_tui::bench::{MAX_BLOCK_BYTES, Screen};
use demerzel_tui::{Replay, ReplayEntry, ReplayResult};

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
/// Turns in the replayed history: four messages each, the replay's
/// message cap.
const REPLAY_TURNS: usize = 50;
/// Entries per replayed turn: the prompt, a thought, a card, its
/// usage line, the reply, its usage line.
const REPLAY_ENTRIES: usize = 6;

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
/// document with one of each element, a table and a list long
/// enough that their per-row work shows. ASCII, as above.
fn formatted_text() -> String {
    let mut doc = String::from(
        "## A heading\n\n\
         Some prose with *emphasis*, **strong** words, and `inline code` that runs long \
         enough to wrap on the screen.\n\n\
         - first item\n- second item with more words\n  - nested item\n- third\n\n\
         ```rust\nfn main() {\n    println!(\"hello\");\n}\n```\n\n\
         > A quote that also runs long enough to wrap when the pane is narrow.\n\n\
         | name | value | note |\n|---|---:|---|\n",
    );
    for row in 0..24 {
        doc.push_str(&format!(
            "| item {row} | {} | note for row {row} |\n",
            row * 7
        ));
    }
    doc.push('\n');
    for item in 0..24 {
        doc.push_str(&format!(
            "{}. step {item} of the [plan](https://example.test/p/{item})\n",
            item + 1
        ));
    }
    doc.push('\n');
    let mut text = String::with_capacity(STREAM + doc.len());
    while text.len() < STREAM {
        text.push_str(&doc);
    }
    text.truncate(STREAM);
    assert!(text.is_ascii());
    text
}

/// The replayed history: `REPLAY_TURNS` turns as the first prompt of
/// a resumed session shows them, and a count of messages left out.
fn replay_history() -> Replay {
    let usage = Usage {
        input_tokens: 34_200,
        output_tokens: 180,
        cache_read_tokens: 30_000,
        cache_creation_tokens: 0,
    };
    let mut entries = Vec::with_capacity(REPLAY_TURNS * REPLAY_ENTRIES);
    for turn in 0..REPLAY_TURNS {
        entries.push(ReplayEntry::Prompt(history_line(turn)));
        entries.push(ReplayEntry::Reasoning(
            "**Plan the change.** Read the file, then edit the one line that matters.".into(),
        ));
        entries.push(ReplayEntry::Tool {
            name: "bash".into(),
            preview: Some(format!("$ echo turn {turn}")),
            result: Some(ReplayResult {
                output: format!("turn {turn}\n"),
                is_error: false,
                duration: Some(Duration::from_millis(40)),
            }),
        });
        entries.push(ReplayEntry::Usage {
            usage,
            elapsed: Some(Duration::from_millis(900)),
        });
        entries.push(ReplayEntry::Text(format!(
            "## Turn {turn}\n\nThe change is in. Next:\n\n- run the tests\n- read the `diff`\n"
        )));
        entries.push(ReplayEntry::Usage {
            usage,
            elapsed: Some(Duration::from_millis(2_100)),
        });
    }
    let mut replay = Replay::default();
    replay.left_out = 12;
    replay.entries = entries;
    replay
}

/// The stream as 16-byte deltas.
fn deltas(text: &str) -> Vec<&str> {
    text.as_bytes()
        .chunks(DELTA)
        .map(|chunk| std::str::from_utf8(chunk).expect("ascii"))
        .collect()
}

fn screen_with_history(markdown: bool) -> Screen {
    let mut screen = Screen::new(WIDTH, HEIGHT, markdown);
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
    let prose = stream_text();
    let formatted = formatted_text();
    let workloads = [("", false, &prose), ("-formatted", true, &formatted)];
    let mut group = c.benchmark_group("render");

    // Workload checks, once and outside the timed spans: the stream
    // lands as one block per byte cap, the total stays under the
    // block cap so the drop path is not measured, and the stream
    // draws the frame count the throughput names.
    let streamed_blocks = HISTORY_LINES + STREAM.div_ceil(MAX_BLOCK_BYTES);
    for (_, markdown, text) in workloads {
        let deltas = deltas(text);
        let mut check = screen_with_history(markdown);
        assert_eq!(check.blocks(), HISTORY_LINES);
        ingest(&mut check, &deltas);
        assert_eq!(check.blocks(), streamed_blocks);
        let mut check = screen_with_history(markdown);
        assert_eq!(stream(&mut check, &deltas), FRAMES);
        assert_eq!(check.blocks(), streamed_blocks);
        // The formatted run measures the markdown path: every
        // streamed block rendered, none fell back to plain text.
        let rendered = if markdown {
            streamed_blocks - HISTORY_LINES
        } else {
            0
        };
        assert_eq!(check.rendered_blocks(), rendered);
    }
    // The selected frame's span must reach the pane's last row: a
    // layout change that moves the row would leave a one-point
    // selection and a bench that measures nothing.
    let mut check = screen_with_history(false);
    assert!(check.select((4, 0), (60, HEIGHT - 5)));
    // The replay lands one block per entry, plus the left-out line.
    let mut check = Screen::new(WIDTH, HEIGHT, true);
    check.replay(replay_history());
    assert_eq!(check.blocks(), REPLAY_TURNS * REPLAY_ENTRIES + 1);

    // The frame benches run before any throughput is set on the group.
    // The formatted one redraws a screen whose tail is the streamed
    // text, so formatted rows are on screen.
    for (suffix, markdown, text) in workloads {
        let mut screen = screen_with_history(markdown);
        if markdown {
            ingest(&mut screen, &deltas(text));
            screen.draw();
        }
        group.bench_function(format!("frame{suffix}"), |b| b.iter(|| screen.draw()));
        // The selection spans the whole transcript pane: the rows
        // above the input box and its frame.
        assert!(screen.select((4, 0), (60, HEIGHT - 5)));
        screen.draw();
        group.bench_function(format!("frame-selected{suffix}"), |b| {
            b.iter(|| screen.draw())
        });
    }

    let mut screen = Screen::new(WIDTH, HEIGHT, true);
    screen.replay(replay_history());
    screen.draw();
    group.bench_function("frame-replayed", |b| b.iter(|| screen.draw()));
    group.bench_function("replay", |b| {
        b.iter_batched(
            || (Screen::new(WIDTH, HEIGHT, true), replay_history()),
            |(mut screen, replay)| {
                screen.replay(replay);
                screen.draw();
                screen
            },
            BatchSize::LargeInput,
        )
    });

    m25::bench(&mut group);

    for (suffix, markdown, text) in workloads {
        let deltas = deltas(text);
        group.throughput(Throughput::Bytes(STREAM as u64));
        group.bench_function(format!("ingest{suffix}"), |b| {
            b.iter_batched(
                || screen_with_history(markdown),
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
                || screen_with_history(markdown),
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
