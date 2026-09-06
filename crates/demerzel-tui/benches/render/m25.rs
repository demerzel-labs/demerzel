//! M2.5 fixtures, kept separate from the legacy comparison workloads.

use std::fmt::Write as _;
use std::time::Duration;

use criterion::measurement::WallTime;
use criterion::{BatchSize, BenchmarkGroup, Throughput};
use demerzel_tui::bench::{Buffers, MAX_BLOCK_BYTES, Screen};
use demerzel_tui::{Replay, ReplayEntry, ReplayResult};

use super::{DELTA, DELTAS_PER_FRAME, FRAMES, HEIGHT, STREAM, WIDTH, deltas, ingest, stream};

const TURNS: usize = 50;
const ENTRIES_PER_TURN: usize = 4;
const CAP_BLOCKS: usize = 2_000;
const NARROW_WIDTH: u16 = 80;
const CARD_OUTPUT_BYTES: usize = 16 * 1024;
const FILLED_EDITOR: &str = "Review the change and preserve the public contracts.\n\
    Check the parser with a complete response.\n\
    Check the parser with a partial response.\n\
    Check the parser with an empty response.\n\
    Keep the error path clear.\n\
    Run the narrow tests.\n\
    Read the final diff.\n\
    Report the result and any remaining risk.";

fn document() -> String {
    let mut doc = String::from(
        "## Parse a response\n\n\
         Keep **valid rows** and return an *error* for `missing` values.\n\n\
         - read the input\n- check the result\n\n\
         | case | result |\n|---|---|\n| complete | accept |\n| missing | error |\n\n\
         ```rust\n",
    );
    for index in 0..12 {
        writeln!(
            doc,
            "let value_{index:02} = rows.get({index:02}).map(|row| row.trim()).ok_or(\"missing value\")?; // parse row"
        )
        .unwrap();
    }
    doc.push_str("```\n");
    doc
}

/// Count the code content, excluding the fence delimiter lines.
/// The stream can end within a line or an open fence.
fn code_bytes(text: &str) -> usize {
    let mut in_fence = false;
    let mut bytes = 0;
    for line in text.split_inclusive('\n') {
        if line.starts_with("```") {
            in_fence = !in_fence;
        } else if in_fence {
            bytes += line.len();
        }
    }
    bytes
}

fn repeated(text: &str, bytes: usize) -> String {
    assert!(text.is_ascii());
    let mut output = text.repeat(bytes.div_ceil(text.len()));
    output.truncate(bytes);
    output
}

fn history(doc: &str) -> Replay {
    let mut replay = Replay::default();
    replay.entries.reserve(TURNS * ENTRIES_PER_TURN);
    for turn in 0..TURNS {
        replay
            .entries
            .push(ReplayEntry::Prompt(format!("Check response {turn}.")));
        replay.entries.push(ReplayEntry::Text(doc.into()));
        replay.entries.push(ReplayEntry::Reasoning(
            "Check **each row** before the edit.\nKeep the error path and run the narrow tests."
                .into(),
        ));
        replay.entries.push(ReplayEntry::Tool {
            name: "bash".into(),
            preview: Some("$ cargo test -p response-parser".into()),
            result: Some(ReplayResult {
                output: "running 2 tests\ntest complete ... ok\ntest missing ... ok\n2 passed\n"
                    .into(),
                is_error: false,
                duration: Some(Duration::from_millis(40)),
            }),
        });
    }
    replay
}

fn screen(doc: &str) -> Screen {
    let mut screen = Screen::new(WIDTH, HEIGHT, true);
    screen.replay(history(doc));
    screen.draw();
    screen
}

fn cards(count: usize) -> Screen {
    let output = repeated(
        "a tool output line with a fixed width for wrapping and retained bytes\n",
        CARD_OUTPUT_BYTES,
    );
    let mut replay = Replay::default();
    for _ in 0..count {
        replay.entries.push(ReplayEntry::Tool {
            name: "bash".into(),
            preview: Some("$ check rows".into()),
            result: Some(ReplayResult {
                output: output.clone(),
                is_error: false,
                duration: Some(Duration::from_millis(40)),
            }),
        });
    }
    let mut screen = Screen::new(WIDTH, HEIGHT, true);
    screen.replay(replay);
    screen.draw();
    screen
}

fn buffer_json(buffers: &Buffers) -> String {
    format!(
        "{{\"blocks\":{},\"laid_out_blocks\":{},\"rendered_blocks\":{},\"text_capacity_bytes\":{},\"layout_capacity_bytes\":{}}}",
        buffers.blocks,
        buffers.laid_out_blocks,
        buffers.rendered_blocks,
        buffers.text_capacity_bytes,
        buffers.layout_capacity_bytes
    )
}

/// One untimed capacity snapshot before browsing, after every block
/// has a layout, and after returning to follow mode. These capacities
/// isolate the cost a cache eviction policy could reclaim.
fn retention(mut screen: Screen) -> String {
    screen.draw();
    let before = screen.buffers();
    screen.scroll_up(usize::MAX / 2);
    screen.draw();
    let browsed = screen.buffers();
    assert_eq!(browsed.laid_out_blocks, browsed.blocks);
    screen.follow();
    screen.draw();
    let followed = screen.buffers();
    format!(
        "{{\"before_browse\":{},\"after_browse\":{},\"after_follow\":{}}}",
        buffer_json(&before),
        buffer_json(&browsed),
        buffer_json(&followed)
    )
}

fn diagnostics(doc: &str, text: &str) {
    let Ok(path) = std::env::var("DEMERZEL_RENDER_FIXTURE_REPORT") else {
        return;
    };
    let mixed = retention(screen(doc));
    let mut capped = Screen::new(WIDTH, HEIGHT, true);
    let block = repeated(doc, MAX_BLOCK_BYTES);
    for _ in 0..CAP_BLOCKS {
        let mut replay = Replay::default();
        replay.entries.push(ReplayEntry::Text(block.clone()));
        capped.replay(replay);
    }
    let capped = retention(capped);
    let report = format!(
        r#"{{
  "version": "m25-v1",
  "history_turns": {TURNS},
  "history_blocks": {},
  "document_bytes": {},
  "document_code_bytes": {},
  "stream_bytes": {STREAM},
  "stream_code_bytes": {},
  "delta_bytes": {DELTA},
  "deltas_per_frame": {DELTAS_PER_FRAME},
  "frames": {FRAMES},
  "filled_editor_bytes": {},
  "filled_editor_lines": 8,
  "reflow_widths": [{WIDTH}, {NARROW_WIDTH}],
  "browse_card_output_bytes": {CARD_OUTPUT_BYTES},
  "browse_cases": {{
    "near": {{"card_blocks": 400, "initial_rows_above_end": 400}},
    "cap": {{"card_blocks": {CAP_BLOCKS}, "initial_position": "first row"}}
  }},
  "retention": {{"mixed": {mixed}, "capped_code": {capped}}}
}}"#,
        TURNS * ENTRIES_PER_TURN,
        doc.len(),
        code_bytes(doc),
        code_bytes(text),
        FILLED_EDITOR.len(),
    );
    std::fs::write(path, report).expect("write fixture diagnostics");
}

pub(super) fn bench(group: &mut BenchmarkGroup<'_, WallTime>) {
    let doc = document();
    let text = repeated(&doc, STREAM);
    let chunks = deltas(&text);
    assert!(code_bytes(&text) * 2 > text.len());

    let mut check = screen(&doc);
    assert_eq!(check.blocks(), TURNS * ENTRIES_PER_TURN);
    let history_rendered = check.rendered_blocks();
    assert!(history_rendered > 0);
    let visible = check.visible_text();
    assert!(visible.contains("value_"));
    assert!(visible.contains("cargo test"));
    assert!(visible.contains("each row"));
    assert_eq!(stream(&mut check, &chunks), FRAMES);
    // A split through a fence carries its opener to the next block.
    assert_eq!(
        check.blocks(),
        TURNS * ENTRIES_PER_TURN + STREAM.div_ceil(MAX_BLOCK_BYTES)
    );
    assert_eq!(
        check.rendered_blocks() - history_rendered,
        STREAM.div_ceil(MAX_BLOCK_BYTES)
    );
    diagnostics(&doc, &text);

    let mut frame = screen(&doc);
    group.bench_function("frame-m25", |b| b.iter(|| frame.draw()));

    let mut thinking = screen(&doc);
    thinking.toggle_thinking();
    thinking.draw();
    group.bench_function("frame-thinking-open-m25", |b| b.iter(|| thinking.draw()));

    let mut filled = screen(&doc);
    filled.set_editor(FILLED_EDITOR);
    filled.draw();
    group.bench_function("frame-filled-editor-m25", |b| b.iter(|| filled.draw()));
    group.bench_function("editor-edit-m25", |b| {
        b.iter_batched(
            || screen(&doc),
            |mut screen| {
                screen.set_editor(FILLED_EDITOR);
                screen.draw();
                screen
            },
            BatchSize::LargeInput,
        )
    });

    let mut reflow = screen(&doc);
    let mut width = WIDTH;
    group.bench_function("reflow-m25", |b| {
        b.iter(|| {
            width = if width == WIDTH { NARROW_WIDTH } else { WIDTH };
            reflow.resize(width, HEIGHT);
            reflow.draw();
        })
    });

    // The near case keeps 400 capped cards and starts 400 collapsed
    // rows above the end. The cap case keeps 2,000 cards and starts
    // at the first row. Expand and collapse are separate iterations,
    // alternating, and each includes the toggle and one changed frame.
    for (name, count, rows) in [("near", 400, 400), ("cap", CAP_BLOCKS, usize::MAX / 2)] {
        let mut browse = cards(count);
        browse.scroll_up(rows);
        browse.draw();
        group.bench_function(format!("frame-browse-{name}-m25"), |b| {
            b.iter(|| browse.draw())
        });
        let mut expanded = false;
        group.bench_function(format!("browse-toggle-{name}-m25"), |b| {
            b.iter(|| {
                expanded = !expanded;
                browse.set_cards_expanded(expanded);
                browse.draw();
            })
        });
    }
    group.throughput(Throughput::Bytes(STREAM as u64));
    group.bench_function("ingest-m25", |b| {
        b.iter_batched(
            || screen(&doc),
            |mut screen| {
                ingest(&mut screen, &chunks);
                screen
            },
            BatchSize::LargeInput,
        )
    });
    group.throughput(Throughput::Elements(FRAMES as u64));
    group.bench_function("stream-m25", |b| {
        b.iter_batched(
            || screen(&doc),
            |mut screen| {
                stream(&mut screen, &chunks);
                screen
            },
            BatchSize::LargeInput,
        )
    });
}
