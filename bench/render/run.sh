#!/usr/bin/env bash
# Layer 2 render benchmark: the full-screen interface's frame cost and
# streamed-text throughput over a headless screen. Runs criterion,
# then collects every estimate and its raw samples into one results
# file.
set -euo pipefail
cd "$(dirname "$0")/../.."

START=$(date +%s)
cargo bench -p demerzel-tui --bench render

OUT="bench/results/render-$(date +%F)-$(git rev-parse --short HEAD).json"
python3 - "$OUT" "${CARGO_TARGET_DIR:-target}" "$START" <<'EOF'
import os
import sys

sys.path.insert(0, os.path.join(os.getcwd(), "bench"))
import harness

out, target, start = sys.argv[1], sys.argv[2], float(sys.argv[3])
report = harness.base_report("render-throughput-headless", "target/release/demerzel")
report["bin"] = "cargo bench -p demerzel-tui --bench render"
report["screen"] = {"width": 120, "height": 40, "history_lines": 200}
report["stream"] = {"bytes": 65536, "delta_bytes": 16, "deltas_per_frame": 64, "frames": 65}
report["note"] = (
    "Frames render into a buffer and diff against the last one, the "
    "work the live interface does before the terminal write; the "
    "size query, the escape encoding, and the write stay outside. "
    "frame is one redraw with no new content, the floor of a frame; "
    "ingest pushes the stream with no frames; stream pushes it with "
    "a frame every 64 deltas and is frame-bound, so its throughput "
    "is frames per second."
)
report["benches"] = harness.criterion_benches(target, group="render", since=start)
if not report["benches"]:
    sys.exit(f"no criterion estimates under {target}/criterion/render")
harness.write_report(out, report)
EOF
echo "raw data: $OUT"
