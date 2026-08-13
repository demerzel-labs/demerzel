#!/usr/bin/env python3
"""PSS/USS sampler: the Layer 1 memory measurement primitive.

Reads /proc/<pid>/smaps_rollup for each given PID and prints PSS and USS in
KiB, plus totals. PSS is the metric that published cross-agent comparisons
use; USS is reported alongside because it is the stricter number.

`idle.py` next to this file drives the idle-session scenario over this
primitive. The N-session and large-transcript scenarios come later.
"""

import sys


def rollup(pid: int) -> dict[str, int]:
    fields = {}
    with open(f"/proc/{pid}/smaps_rollup") as f:
        for line in f:
            key, _, rest = line.partition(":")
            parts = rest.split()
            if len(parts) >= 2 and parts[1] == "kB":
                fields[key] = int(parts[0])
    return fields


def main() -> int:
    if len(sys.argv) < 2:
        print((__doc__ or "").strip(), file=sys.stderr)
        print("\nusage: pss.py <pid> [pid...]", file=sys.stderr)
        return 1
    total_pss = total_uss = 0
    print(f"{'pid':>8} {'pss_kb':>10} {'uss_kb':>10}")
    for arg in sys.argv[1:]:
        pid = int(arg)
        f = rollup(pid)
        pss = f.get("Pss", 0)
        uss = f.get("Private_Clean", 0) + f.get("Private_Dirty", 0)
        total_pss += pss
        total_uss += uss
        print(f"{pid:>8} {pss:>10} {uss:>10}")
    if len(sys.argv) > 2:
        print(f"{'total':>8} {total_pss:>10} {total_uss:>10}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
