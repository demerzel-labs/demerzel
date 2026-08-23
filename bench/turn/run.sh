#!/usr/bin/env bash
# Layer 2 turn benchmark: per-turn latency against a local mock.
# Runs criterion, then collects every bench estimate and its raw
# samples into one results file.
set -euo pipefail
cd "$(dirname "$0")/../.."

START=$(date +%s)
cargo bench -p demerzel --bench turn

python3 - "${CARGO_TARGET_DIR:-target}" "$START" <<'EOF'
import os
import sys

sys.path.insert(0, os.path.join(os.getcwd(), "bench"))
import harness

target, start = sys.argv[1], float(sys.argv[2])
out = harness.result_path("turn")
fs = harness.filesystem(os.path.abspath(target))
report = harness.base_report("turn-latency-vs-local-mock", "target/release/demerzel")
report["bin"] = "cargo bench --bench turn"
report["data_filesystem"] = fs
report["note"] = (
    "Every number includes the localhost round-trip to the mock; "
    "transport_floor isolates it. Harness-added latency per turn is "
    "turn_text minus transport_floor."
)
report["benches"] = harness.criterion_benches(target, since=start)
if not report["benches"]:
    sys.exit(f"no criterion estimates under {target}/criterion")
harness.write_report(out, report)
print(f"raw data: {out}")
EOF
