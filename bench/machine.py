#!/usr/bin/env python3
"""Machine-spec recorder. Every published bench number references one
of these files. It records the CPU, RAM, OS, filesystem, and
toolchain of the machine that produced the numbers. The
platform-specific fields come from the native probe layer; on macOS
they include the power and thermal state, the conditions that move
numbers there.
"""

import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import harness

REPO_ROOT = harness.REPO_ROOT


def run(cmd: list[str]) -> str:
    try:
        out = subprocess.run(cmd, capture_output=True, text=True, check=True)
        return out.stdout.strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return ""


def main() -> int:
    binary = os.path.join(REPO_ROOT, "target/release/demerzel")
    spec = harness.machine_fields()
    # The shared fields land after the spread, so a platform module
    # can never clobber them.
    spec.update(
        {
            "date": time.strftime("%Y-%m-%d"),
            "os": harness.os_slug(),
            "cores": os.cpu_count(),
            "kernel": run(["uname", "-r"]),
            "rustc": run(["rustc", "--version"]),
            "binary_bytes": os.path.getsize(binary) if os.path.exists(binary) else None,
            "git_rev": harness.git_rev(),
            "git_dirty": harness.git_dirty(),
        }
    )
    out_path = os.path.join(
        REPO_ROOT, "bench/results", f"machine-{spec['date']}-{spec['os']}.json"
    )
    harness.write_report(out_path, spec)
    print(f"machine spec: {out_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
