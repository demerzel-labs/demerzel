#!/usr/bin/env bash
# Layer 2 turn benchmark: per-turn latency against a local mock.
# Runs criterion, then collects every bench estimate and its raw
# samples into one results file.
set -euo pipefail
cd "$(dirname "$0")/../.."

START=$(date +%s)
cargo bench -p demerzel --bench turn

SLUG="$(python3 -c 'import sys; sys.path.insert(0, "bench"); import harness; print(harness.os_slug())')"
OUT="bench/results/turn-$(date +%F)-$(git rev-parse --short HEAD)-$SLUG.json"
python3 - "$OUT" "${CARGO_TARGET_DIR:-target}" "$START" <<'EOF'
import os
import sys

sys.path.insert(0, os.path.join(os.getcwd(), "bench"))
import harness

out, target, start = sys.argv[1], sys.argv[2], float(sys.argv[3])
fs = harness.native.filesystem(os.path.abspath(target))
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
EOF
echo "raw data: $OUT"
