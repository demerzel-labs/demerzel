#!/usr/bin/env python3
"""Read process memory in KiB on Linux or macOS.

Linux reports PSS and USS. macOS reports current physical footprint and
lifetime peak physical footprint. These metrics do not compare across OSes.

The idle driver uses the same native probe. Concurrent-session and
large-transcript scenarios remain separate work.
"""

import argparse
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import harness


def main() -> int:
    parser = argparse.ArgumentParser(description=(__doc__ or "").strip())
    parser.add_argument("pid", type=int, nargs="+", help="process ID")
    args = parser.parse_args()
    if any(pid <= 0 or pid > 2**31 - 1 for pid in args.pid):
        parser.error("each process ID must be a positive signed 32-bit integer")
    try:
        rows = [harness.process_memory(pid) for pid in args.pid]
    except (OSError, ValueError, KeyError) as error:
        print(f"cannot read process memory: {error}", file=sys.stderr)
        return 1
    metrics = (
        ("pss_kb", "uss_kb")
        if "pss_kb" in rows[0]
        else ("physical_footprint_kb", "peak_physical_footprint_kb")
    )
    widths = [max(10, len(metric)) for metric in metrics]
    print(f"{'pid':>8} " + " ".join(f"{metric:>{width}}" for metric, width in zip(metrics, widths)))
    for pid, row in zip(args.pid, rows):
        print(f"{pid:>8} " + " ".join(f"{row[metric]:>{width}}" for metric, width in zip(metrics, widths)))
    if len(rows) > 1:
        # Peaks occurred at separate instants; their sum is not a group peak.
        totals = [
            "-" if metric == "peak_physical_footprint_kb" else str(sum(row[metric] for row in rows))
            for metric in metrics
        ]
        print(f"{'total':>8} " + " ".join(f"{value:>{width}}" for value, width in zip(totals, widths)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
