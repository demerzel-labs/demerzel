#!/usr/bin/env bash
# Layer 2 turn benchmark: harness-added latency per turn against the
# mock server. Runs criterion, then collects the estimates into one
# results file.
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo bench -p demerzel --bench turn

OUT="bench/results/turn-$(date +%F)-$(git rev-parse --short HEAD).json"
python3 - "$OUT" "$(git rev-parse --short HEAD)" <<'EOF'
import json
import sys
import time

out, rev = sys.argv[1], sys.argv[2]
report = {
    "benchmark": "turn-latency-vs-mock",
    "date": time.strftime("%Y-%m-%d"),
    "git_rev": rev,
    "benches": {},
}
for name in ("turn_text", "turn_read_tool"):
    with open(f"target/criterion/{name}/new/estimates.json") as f:
        est = json.load(f)
    report["benches"][name] = {
        "mean_ns": round(est["mean"]["point_estimate"]),
        "median_ns": round(est["median"]["point_estimate"]),
        "stddev_ns": round(est["std_dev"]["point_estimate"]),
        "mean_ci95_ns": [
            round(est["mean"]["confidence_interval"]["lower_bound"]),
            round(est["mean"]["confidence_interval"]["upper_bound"]),
        ],
    }
with open(out, "w") as f:
    json.dump(report, f, indent=2)
    f.write("\n")
EOF
echo "raw data: $OUT"
