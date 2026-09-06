# Render benchmark fixtures

Run the complete suite with `bench/render/run.sh`.
The command records Criterion samples and fixture details in `bench/results/`.
The [M2.5 plan](m2.5-plan.md#measurement) defines the measurement requirements.

The headless screen includes layout, cell drawing, and the buffer diff.
It excludes the terminal size query, escape encoding, and terminal writes.
Each screen is 120 columns by 40 rows, except during the resize benchmark.

## Comparison rules

The existing benchmark names keep their existing fixtures.
These include the prose fixture, the `-formatted` fixture, and the replay fixture.
The M2.5 fixture uses the `-m25` suffix.
Do not compare its results directly with results from the older fixtures.

The screen height stays at 40 rows.
The current empty editor and its frame use four rows.
The transcript therefore has 36 rows.
The filled editor uses eight input rows and three frame rows.
Its transcript has 29 rows.
Compare the filled editor with the empty editor on the same revision.
An interface change that takes transcript rows can affect comparisons with older revisions.

## M2.5 fixture

The history has 50 turns and 200 blocks.
Each turn contains these blocks:

1. A prompt.
2. Markdown with a heading, emphasis, a list, a table, and a Rust code fence.
3. A completed thinking block, closed by default.
4. A completed tool card with its preview, output, and duration.

The initial viewport shows code, thinking, and a tool card.
The stream repeats the same markdown document to produce 65,536 bytes.
Code content exceeds half of the streamed bytes.
The receipt records the exact document and stream byte counts.
The code count excludes fence delimiters.

Each streamed delta contains 16 bytes.
The stream draws after every 64 deltas and once at the end: 65 frames total.
The streamed reply eventually moves the historical cards and thinking blocks outside the viewport.
Fixture checks confirm the frame count, block split count, and markdown rendering path.

| Benchmark | Timed work |
| --- | --- |
| `frame-m25` | Draw the unchanged mixed viewport with cached layouts. |
| `ingest-m25` | Append the complete stream without drawing. |
| `stream-m25` | Append the stream and draw 65 frames. |
| `reflow-m25` | Change the width and draw one frame, including frame buffer resizing. |
| `frame-thinking-open-m25` | Draw the mixed viewport with its newest thinking block open. |
| `frame-filled-editor-m25` | Draw the mixed viewport with eight input lines and cached editor layout. |
| `editor-edit-m25` | Replace the empty editor with eight lines and draw once. |

The resize benchmark alternates between widths of 120 and 80 columns.
Each iteration measures one direction.
The narrower width wraps the code rows.
A resize returns the transcript to follow mode.
It lays out the visible blocks at the new width.
It does not lay out the full history.

## Browse costs

The browse fixtures use completed cards with 16 KiB of kept output each.
The near case has 400 cards and starts 400 collapsed rows above the end.
The cap case has 2,000 cards and starts at the first row.

The `frame-browse-near-m25` and `frame-browse-cap-m25` benchmarks draw the unchanged, collapsed state.
The `browse-toggle-near-m25` and `browse-toggle-cap-m25` benchmarks toggle every card and draw once.
Successive iterations alternate between expansion and collapse.
The transcript keeps the browse anchor through each change.
The measured work includes the toggle across all cards and layout below the anchor.

## Layout buffer retention

The wrapper also records untimed buffer capacities for two histories:

1. The mixed fixture with 200 blocks.
2. A stress fixture with 2,000 markdown blocks of 32 KiB each.

Each snapshot sequence draws the tail, browses to the first row, and returns to follow mode.
A check confirms that browsing lays out every block.
The receipt records capacities after each draw.
The `rendered_blocks` field shows how many blocks use rendered markdown rows.

The text total counts each block's text capacity.
The layout total counts rendered text, row offsets, and face changes at their allocated capacities.
The layout total measures buffers that a layout eviction policy could release.
They are not process memory measurements.
They exclude these allocations:

- Card and thought backing stores.
- Block objects and carried fence strings.
- Indexes and frame buffers.
- Allocator overhead and temporary allocations.

The Linux instruction-count comparison remains separate from wall-clock timing.
It must use matched legacy fixtures and revisions to assess the historical `ingest` timing change.

## Workload checks

Run `cargo bench -p demerzel-tui --bench render -- --test` to execute each workload once without timing samples.
Set `DEMERZEL_RENDER_FIXTURE_REPORT` to a file path to include the retention snapshots in that check.
The wrapper sets this variable automatically for a complete receipt.
