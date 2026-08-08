#!/usr/bin/env python3
"""PTY time-to-interactive probe: Layer 1 boot measurement.

The probe spawns the binary in a pseudo-terminal and measures the time
from spawn to the ready marker on the merged output stream. The REPL
prints the marker first, after credential resolution.

Each run gets a private data home with a synthetic auth store, so a
run needs no login and makes no network call.

Modes:
- warm (default): warmup runs, then measured runs with the binary in
  the page cache.
- cold (--cold): evict the binary from the page cache before each
  measured run, verified with mincore. Shared libraries and metadata
  caches stay warm, so the cold number tracks the binary read; a true
  cold start needs a fresh boot.

The report records the probe's own spawn floor, measured through the
identical path with `printf`. The floor sits inside every sample.
"""

import argparse
import os
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import harness


def measure_once(bin_path: str, env: dict[str, str], cold: bool) -> float:
    if cold:
        harness.evict_verified(bin_path)
    pid, master, elapsed_ns, seen = harness.spawn_ready(bin_path, env)
    harness.shutdown(pid, master, seen)
    return elapsed_ns / 1e6


def main() -> int:
    parser = argparse.ArgumentParser(description=(__doc__ or "").strip())
    parser.add_argument(
        "--bin",
        default=os.path.join(harness.REPO_ROOT, "target/release/demerzel"),
        help="path to the binary (default: target/release/demerzel)",
    )
    parser.add_argument("--runs", type=int, default=50, help="measured runs (default: 50)")
    parser.add_argument("--warmup", type=int, default=3, help="warmup runs (default: 3)")
    parser.add_argument(
        "--cold",
        action="store_true",
        help="evict the binary from the page cache before each measured run",
    )
    parser.add_argument("--out", help="output JSON path (default: under bench/results)")
    args = parser.parse_args()

    if not os.access(args.bin, os.X_OK):
        print(f"no binary at {args.bin} — run: cargo build --release", file=sys.stderr)
        return 1

    mode = "cold" if args.cold else "warm"
    with tempfile.TemporaryDirectory(prefix="demerzel-bench-") as data_home:
        harness.write_auth_fixture(data_home)
        env = harness.bench_env(data_home)
        floor = harness.spawn_floor_ms(env)
        # Warmup runs use the same mode as the measured runs, so the
        # first cold sample is not the only unwarmed one.
        for _ in range(args.warmup):
            measure_once(args.bin, env, cold=args.cold)
        samples = [measure_once(args.bin, env, cold=args.cold) for _ in range(args.runs)]

    report = harness.base_report("pty-time-to-interactive", args.bin)
    report.update(
        {
            "mode": mode,
            "warmup": args.warmup,
            "runs": args.runs,
            "env_size": len(env),
            "spawn_floor_ms": floor,
            "samples_ms": [round(s, 3) for s in samples],
            "stats_ms": harness.stats(samples),
        }
    )
    out_path = args.out or os.path.join(
        harness.REPO_ROOT,
        "bench/results",
        f"pty-boot-{mode}-{report['date']}-{report['git_rev']}.json",
    )
    harness.write_report(out_path, report)
    print(
        f"{mode}: median {report['stats_ms']['median']} ms over {args.runs} runs "
        f"(spawn floor {floor['median']} ms)"
    )
    print(f"raw data: {out_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
