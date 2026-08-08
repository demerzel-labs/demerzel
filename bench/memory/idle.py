#!/usr/bin/env python3
"""Idle-session memory scenario: Layer 1 measurement.

The driver launches the REPL in a pseudo-terminal N times. For each
launch it waits for the ready marker, lets the process settle, and
samples `/proc/<pid>/smaps_rollup`.

Anonymous memory is the headline: it is the harness's own heap and
stacks, and it is stable across runs. PSS and USS also count clean
mapped binary text, which moves with page-cache state; the report
keeps them because cross-agent comparisons use PSS.
"""

import argparse
import os
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import harness
import pss

METRICS = ("anonymous_kb", "private_dirty_kb", "pss_kb", "uss_kb", "rss_kb")


def sample(bin_path: str, env: dict[str, str], settle_s: float) -> dict[str, int]:
    pid, master, _, seen = harness.spawn_ready(bin_path, env)
    try:
        time.sleep(settle_s)
        fields = pss.rollup(pid)
    finally:
        harness.shutdown(pid, master, seen)
    return {
        "anonymous_kb": fields.get("Anonymous", 0),
        "private_dirty_kb": fields.get("Private_Dirty", 0),
        "pss_kb": fields.get("Pss", 0),
        "uss_kb": fields.get("Private_Clean", 0) + fields.get("Private_Dirty", 0),
        "rss_kb": fields.get("Rss", 0),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=(__doc__ or "").strip())
    parser.add_argument(
        "--bin",
        default=os.path.join(harness.REPO_ROOT, "target/release/demerzel"),
        help="path to the binary (default: target/release/demerzel)",
    )
    parser.add_argument("--runs", type=int, default=10, help="launches (default: 10)")
    parser.add_argument(
        "--settle", type=float, default=1.0, help="seconds to wait before each sample (default: 1)"
    )
    parser.add_argument("--out", help="output JSON path (default: under bench/results)")
    args = parser.parse_args()

    if not os.access(args.bin, os.X_OK):
        print(f"no binary at {args.bin} — run: cargo build --release", file=sys.stderr)
        return 1

    with tempfile.TemporaryDirectory(prefix="demerzel-bench-") as data_home:
        harness.write_auth_fixture(data_home)
        env = harness.bench_env(data_home)
        runs = [sample(args.bin, env, args.settle) for _ in range(args.runs)]

    samples = {metric: [run[metric] for run in runs] for metric in METRICS}
    report = harness.base_report("memory-idle-repl", args.bin)
    report.update(
        {
            "runs": args.runs,
            "settle_s": args.settle,
            "samples_kb": samples,
            "stats_kb": {metric: harness.stats(values) for metric, values in samples.items()},
        }
    )
    out_path = args.out or os.path.join(
        harness.REPO_ROOT,
        "bench/results",
        f"memory-idle-{report['date']}-{report['git_rev']}.json",
    )
    harness.write_report(out_path, report)
    anon = report["stats_kb"]["anonymous_kb"]["median"]
    pss_median = report["stats_kb"]["pss_kb"]["median"]
    print(
        f"idle REPL over {args.runs} launches: anonymous {anon} KiB median, "
        f"PSS {pss_median} KiB median"
    )
    print(f"raw data: {out_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
