#!/usr/bin/env python3
"""Idle-session memory scenario: Layer 1 PSS measurement.

The driver starts the REPL in a pseudo-terminal, waits for the ready
marker, lets the process settle, and samples PSS and USS from
`/proc/<pid>/smaps_rollup`. It reuses the spawn helper from the PTY
boot probe and the rollup reader from `pss.py`.
"""

import argparse
import json
import os
import sys
import tempfile
import time

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "boot"))

import pss
from pty_probe import REPO_ROOT, bench_env, git_rev, shutdown, spawn_ready, write_auth_fixture


def main() -> int:
    parser = argparse.ArgumentParser(description=(__doc__ or "").strip())
    parser.add_argument(
        "--bin",
        default=os.path.join(REPO_ROOT, "target/release/demerzel"),
        help="path to the binary (default: target/release/demerzel)",
    )
    parser.add_argument(
        "--settle", type=float, default=1.0, help="seconds to wait before the sample (default: 1)"
    )
    parser.add_argument("--out", help="output JSON path (default: under bench/results)")
    args = parser.parse_args()

    if not os.access(args.bin, os.X_OK):
        print(f"no binary at {args.bin} — run: cargo build --release", file=sys.stderr)
        return 1

    rev = git_rev()
    with tempfile.TemporaryDirectory(prefix="demerzel-bench-") as data_home:
        write_auth_fixture(data_home)
        proc, master, _ = spawn_ready(args.bin, bench_env(data_home))
        try:
            time.sleep(args.settle)
            fields = pss.rollup(proc.pid)
        finally:
            shutdown(proc, master)

    report = {
        "benchmark": "memory-idle-repl",
        "bin": os.path.relpath(args.bin, REPO_ROOT),
        "git_rev": rev,
        "date": time.strftime("%Y-%m-%d"),
        "settle_s": args.settle,
        "pss_kb": fields.get("Pss", 0),
        "uss_kb": fields.get("Private_Clean", 0) + fields.get("Private_Dirty", 0),
        "rss_kb": fields.get("Rss", 0),
    }
    out_path = args.out or os.path.join(
        REPO_ROOT,
        "bench/results",
        f"memory-idle-{time.strftime('%Y-%m-%d')}-{rev}.json",
    )
    os.makedirs(os.path.dirname(out_path), exist_ok=True)
    with open(out_path, "w") as f:
        json.dump(report, f, indent=2)
        f.write("\n")
    print(f"idle REPL: PSS {report['pss_kb']} KiB, USS {report['uss_kb']} KiB")
    print(f"raw data: {out_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
