#!/usr/bin/env python3
"""Idle-session memory scenario: Layer 1 measurement.

The driver launches an interactive session in a pseudo-terminal N
times. For each launch it waits for the first prompt, lets the
process settle, and samples the native memory counters. The report
names the interface the run measured.

On Linux, anonymous memory is the headline: it is the harness's own heap and
stacks, and it is stable across runs. PSS and USS also count clean
mapped binary text, which moves with page-cache state; the report
keeps them for comparison with external tools. macOS reports current
physical footprint and lifetime peak footprint. These are not PSS.
"""

import argparse
import math
import os
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import harness


def sample(bin_path: str, env: dict[str, str], settle_s: float) -> dict[str, int]:
    pid, master, _, seen = harness.spawn_ready(bin_path, env)
    # Wait for the first frame before settling. wait_prompt owns cleanup
    # on failure; after it succeeds this function owns the child.
    _, seen = harness.wait_prompt(pid, master, seen)
    try:
        time.sleep(settle_s)
        return harness.process_memory(pid)
    finally:
        harness.shutdown(pid, master, seen)


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

    if args.runs < 1:
        parser.error("--runs must be at least 1")
    if not math.isfinite(args.settle) or args.settle < 0:
        parser.error("--settle must be finite and nonnegative")

    if not os.access(args.bin, os.X_OK):
        print(f"no binary at {args.bin} — run: cargo build --release", file=sys.stderr)
        return 1

    if not harness.supports_proc_memory():
        print(
            "this platform has no per-process memory probe",
            file=sys.stderr,
        )
        return 1

    with tempfile.TemporaryDirectory(prefix="demerzel-bench-") as data_home:
        harness.write_auth_fixture(data_home)
        env = harness.bench_env(data_home)
        runs = [sample(args.bin, env, args.settle) for _ in range(args.runs)]

    samples = {metric: [run[metric] for run in runs] for metric in runs[0]}
    report = harness.base_report("memory-idle-repl", args.bin)
    report.update(
        {
            # The default interface on a PTY is the full screen; the
            # benchmark id predates the split and stays for the trend.
            "interface": "tui",
            "runs": args.runs,
            "settle_s": args.settle,
            "samples_kb": samples,
            "stats_kb": {metric: harness.stats(values) for metric, values in samples.items()},
        }
    )
    out_path = args.out or harness.result_path("memory-idle")
    harness.write_report(out_path, report)
    medians = report["stats_kb"]
    if "physical_footprint_kb" in medians:
        summary = (
            f"physical footprint {medians['physical_footprint_kb']['median']} KiB median, "
            f"lifetime peak {medians['peak_physical_footprint_kb']['median']} KiB median"
        )
    else:
        summary = (
            f"anonymous {medians['anonymous_kb']['median']} KiB median, "
            f"PSS {medians['pss_kb']['median']} KiB median"
        )
    print(
        f"idle {report['interface']} session over {args.runs} launches: "
        + summary
    )
    print(f"raw data: {out_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
