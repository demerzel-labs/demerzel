#!/usr/bin/env bash
# Layer 2 render benchmark: the full-screen interface's frame cost and
# streamed-text throughput over a headless screen. Runs criterion,
# then collects every estimate and its raw samples into one results
# file.
set -euo pipefail
cd "$(dirname "$0")/../.."

python3 - "${CARGO_TARGET_DIR:-target}" <<'EOF'
import json
import os
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, os.path.join(os.getcwd(), "bench"))
import harness

target = sys.argv[1]
with tempfile.TemporaryDirectory(prefix="demerzel-render-") as temporary:
    diagnostics = os.path.join(temporary, "fixtures.json")
    environment = {**os.environ, "DEMERZEL_RENDER_FIXTURE_REPORT": diagnostics}
    start = time.time()
    subprocess.run(
        ["cargo", "bench", "-p", "demerzel-tui", "--bench", "render"],
        env=environment,
        check=True,
    )
    with open(diagnostics) as source:
        fixtures = json.load(source)

out = harness.result_path("render")
report = harness.base_report("render-throughput-headless", "target/release/demerzel")
report["bin"] = "cargo bench -p demerzel-tui --bench render"
report["screen"] = {"width": 120, "height": 40}
report["legacy_fixture"] = {"history_blocks": 200, "streamed_blocks": 2, "replay_blocks": 301}
report["stream"] = {"bytes": 65536, "delta_bytes": 16, "deltas_per_frame": 64, "frames": 65}
report["m25_fixture"] = fixtures
report["m25_fixture"]["retention_note"] = (
    "Retained text and layout buffer capacities, not process memory. "
    "Layout includes rendered text, row offsets, and faces. Text excludes "
    "card and thought backing stores. Both exclude block objects, carry "
    "strings, indexes, frame buffers, allocator overhead, and temporary allocations. "
    "after_browse follows a draw at the first row, which lays out every block; "
    "after_follow returns to the live end and draws again."
)
report["note"] = (
    "Frames render into a buffer and diff against the last one, the "
    "work the live interface does before the terminal write; the "
    "size query, the escape encoding, and the write stay outside. "
    "frame is one redraw with no new content, the floor of a frame; "
    "frame-selected is the same redraw with a drag selection that "
    "spans every transcript row; "
    "ingest pushes the stream with no frames; stream pushes it with "
    "a frame every 64 deltas and is frame-bound, so its throughput "
    "is frames per second. Legacy names retain their original fixtures. "
    "The -m25 benches use a separate mixed fixture with code, cards, and thinking. "
    "frame-m25 draws its mixed tail; stream-m25 appends code-heavy markdown. "
    "reflow-m25 alternates widths 120 and 80, including buffer resize and one draw. "
    "editor-edit-m25 replaces the empty editor with eight lines and draws once. "
    "frame-filled-editor-m25 redraws that filled editor with cached layout. "
    "The near browse case keeps 400 capped cards and starts 400 collapsed rows "
    "above the end; the cap case keeps 2,000 cards and starts at the first row. "
    "Separate browse-expand and browse-collapse distributions change all cards "
    "and draw once. Each expansion starts with fresh collapsed layouts. "
    "Each collapse starts after an untimed expansion/draw at the same anchor. "
    "Fixture creation, anchor setup, opposite-state preparation, and drop stay "
    "outside timing; the timed draw includes layouts changed by the operation. "
    "frame-browse measures the cached, collapsed state."
)
report["benches"] = harness.criterion_benches(target, group="render", since=start)
required = {
    "frame", "frame-selected", "frame-formatted", "frame-selected-formatted",
    "frame-replayed", "replay", "ingest", "stream", "ingest-formatted", "stream-formatted",
    "frame-m25", "frame-thinking-open-m25", "frame-filled-editor-m25", "editor-edit-m25",
    "reflow-m25", "frame-browse-near-m25", "frame-browse-cap-m25",
    "browse-expand-near-m25", "browse-expand-cap-m25",
    "browse-collapse-near-m25", "browse-collapse-cap-m25", "ingest-m25", "stream-m25",
}
missing = required - report["benches"].keys()
if missing:
    sys.exit(f"missing fresh criterion estimates: {', '.join(sorted(missing))}")
harness.write_report(out, report)
print(f"raw data: {out}")
EOF
