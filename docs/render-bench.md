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
The `browse-expand-near-m25` and `browse-expand-cap-m25` benchmarks expand every card and draw once.
The `browse-collapse-near-m25` and `browse-collapse-cap-m25` benchmarks collapse every card and draw once.
Separate distributions show the cost of each operation.

Each expansion starts with fresh collapsed layouts at the specified anchor.
Each collapse starts after an untimed expansion and draw at that same anchor.
The transcript keeps the browse anchor through each change.
The measured work includes the state change across all cards and the changed layouts below the anchor.
Fixture creation, anchor setup, and preparation of the opposite form stay outside timing.
The fixture drops outside timing.

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

## Historical ingest instruction counts

The standalone [driver](../bench/render/ingest_instructions.rs) measures the step-7 plain-ingest routine on Linux x86_64.
It uses a per-thread `instructions:user` hardware counter through `perf_event_open`.
The counter excludes kernel and hypervisor execution.
A zero count or incomplete counter coverage fails the run.
The driver requires access to user-space performance counters.

The [receipt](../bench/results/ingest-instructions-2026-09-06-b0fc879-linux-x86_64.json) uses Rust 1.97.0 for both release builds.
Check that `stable` resolves to that compiler before reproducing this receipt.
Each historical checkout receives the same example overlay; its tracked sources stay unchanged.
Run these commands from the repository root:

```bash
rustc +stable --version
git worktree add --detach target/m25-ingest-before b84e253095a79a41a99c26d651fffc7e43e08b3b
git worktree add --detach target/m25-ingest-after 2e7e0a1177555a2cf9efcfc2094974baa2289fc4
mkdir -p target/m25-ingest-before/crates/demerzel-tui/examples target/m25-ingest-after/crates/demerzel-tui/examples
git show b0fc879:bench/render/ingest_instructions.rs > target/m25-ingest-driver.rs
cp target/m25-ingest-driver.rs target/m25-ingest-before/crates/demerzel-tui/examples/ingest_instructions.rs
cp target/m25-ingest-driver.rs target/m25-ingest-after/crates/demerzel-tui/examples/ingest_instructions.rs
(cd target/m25-ingest-before && cargo +stable build --release --locked -p demerzel-tui --example ingest_instructions --target-dir target/ingest-count)
(cd target/m25-ingest-after && cargo +stable build --release --locked -p demerzel-tui --example ingest_instructions --target-dir target/ingest-count)
target/m25-ingest-before/target/ingest-count/release/examples/ingest_instructions > target/m25-ingest-before/target/ingest-count/result.json
target/m25-ingest-after/target/ingest-count/release/examples/ingest_instructions > target/m25-ingest-after/target/ingest-count/result.json
```

Each sample includes the owned screen transfer and the counter control overhead.
Fixture setup, its initial draw, assertions, and destruction stay outside the measured span.
The output retains 31 raw samples after 10 warmups, plus empty-span counts without subtraction.
Compare the medians and preserve the raw counts, source revision, driver hash, and compiler with the result.
This routine does not measure Criterion's full batching machinery or explain a wall-time change by itself.

## Workload checks

Run `cargo bench -p demerzel-tui --bench render -- --test` to execute each workload once without timing samples.
Set `DEMERZEL_RENDER_FIXTURE_REPORT` to a file path to include the retention snapshots in that check.
The wrapper sets this variable automatically for a complete receipt.
