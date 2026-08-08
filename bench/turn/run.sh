#!/usr/bin/env bash
# Layer 2 turn benchmark: per-turn latency against a local mock.
# Runs criterion, then collects every bench estimate and its raw
# samples into one results file.
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo bench -p demerzel --bench turn

OUT="bench/results/turn-$(date +%F)-$(git rev-parse --short HEAD).json"
python3 - "$OUT" "${CARGO_TARGET_DIR:-target}" <<'EOF'
import glob
import json
import os
import subprocess
import sys

sys.path.insert(0, os.path.join(os.getcwd(), "bench"))
import harness

out, target = sys.argv[1], sys.argv[2]
fs = subprocess.run(
    ["findmnt", "-no", "FSTYPE", "--target", target], capture_output=True, text=True
).stdout.strip()
report = harness.base_report("turn-latency-vs-local-mock", "target/release/demerzel")
report["bin"] = "cargo bench --bench turn"
report["data_filesystem"] = fs
report["note"] = (
    "Every number includes the localhost round-trip to the mock; "
    "transport_floor isolates it. Harness-added latency per turn is "
    "turn_text minus transport_floor."
)
report["benches"] = {}
for est_path in sorted(glob.glob(f"{target}/criterion/*/new/estimates.json")):
    name = est_path.split(os.sep)[-3]
    if name == "report":
        continue
    with open(est_path) as f:
        est = json.load(f)
    entry = {
        "mean_ns": round(est["mean"]["point_estimate"]),
        "median_ns": round(est["median"]["point_estimate"]),
        "stddev_ns": round(est["std_dev"]["point_estimate"]),
        "mean_ci95_ns": [
            round(est["mean"]["confidence_interval"]["lower_bound"]),
            round(est["mean"]["confidence_interval"]["upper_bound"]),
        ],
    }
    if est.get("slope"):
        entry["slope_ns"] = round(est["slope"]["point_estimate"])
    sample_path = est_path.replace("estimates.json", "sample.json")
    if os.path.exists(sample_path):
        with open(sample_path) as f:
            entry["samples"] = json.load(f)
    report["benches"][name] = entry
harness.write_report(out, report)
EOF
echo "raw data: $OUT"
