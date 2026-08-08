#!/usr/bin/env python3
"""Machine-spec recorder. Every published bench number references one
of these files. It records the CPU, RAM, kernel, filesystem, and
toolchain of the machine that produced the numbers.
"""

import json
import os
import subprocess
import sys
import time

REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))


def run(cmd: list[str]) -> str:
    try:
        out = subprocess.run(cmd, capture_output=True, text=True, check=True)
        return out.stdout.strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return ""


def read_first(path: str) -> str:
    try:
        with open(path) as f:
            return f.read().strip()
    except OSError:
        return ""


def cpu_model() -> str:
    with open("/proc/cpuinfo") as f:
        for line in f:
            if line.startswith("model name"):
                return line.partition(":")[2].strip()
    return ""


def mem_total_kb() -> int:
    with open("/proc/meminfo") as f:
        for line in f:
            if line.startswith("MemTotal:"):
                return int(line.split()[1])
    return 0


def distro() -> str:
    for line in read_first("/etc/os-release").splitlines():
        if line.startswith("PRETTY_NAME="):
            return line.partition("=")[2].strip('"')
    return ""


def main() -> int:
    binary = os.path.join(REPO_ROOT, "target/release/demerzel")
    spec = {
        "date": time.strftime("%Y-%m-%d"),
        "cpu": cpu_model(),
        "cores": os.cpu_count(),
        "cpu_governor": read_first("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor"),
        "mem_total_kb": mem_total_kb(),
        "kernel": run(["uname", "-r"]),
        "distro": distro(),
        "filesystem": run(["findmnt", "-no", "FSTYPE", "--target", REPO_ROOT]),
        "rustc": run(["rustc", "--version"]),
        "binary_bytes": os.path.getsize(binary) if os.path.exists(binary) else None,
        "git_rev": run(["git", "-C", REPO_ROOT, "rev-parse", "--short", "HEAD"]),
    }
    out_path = os.path.join(REPO_ROOT, "bench/results", f"machine-{spec['date']}.json")
    os.makedirs(os.path.dirname(out_path), exist_ok=True)
    with open(out_path, "w") as f:
        json.dump(spec, f, indent=2)
        f.write("\n")
    print(f"machine spec: {out_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
