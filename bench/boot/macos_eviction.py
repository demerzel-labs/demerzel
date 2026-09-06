#!/usr/bin/env python3
"""Record the macOS cache-eviction experiment, without timing a boot.

Each attempt reads the release binary, checks its residency, runs purge,
and checks residency again. The administrator option asks macOS to run
only the fixed purge command with elevated privileges.
"""

import argparse
import hashlib
import os
import platform
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import harness


def verified(attempts: list[dict], pages: int, runs: int) -> bool:
    return len(attempts) == runs and all(
        sample["before_pages"] == pages
        and sample["purge_returncode"] == 0
        and sample["after_pages"] == 0
        for sample in attempts
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", default=os.path.join(harness.REPO_ROOT, "target/release/demerzel"))
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--out", help="Write the JSON receipt to this path.")
    parser.add_argument(
        "--administrator", action="store_true",
        help="Ask macOS to authorize the fixed /usr/sbin/purge command.",
    )
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("This experiment requires macOS.")
    if args.runs < 1:
        parser.error("The run count must be positive.")
    if not os.path.isfile(args.bin) or os.path.getsize(args.bin) == 0:
        parser.error("Build the release binary before this experiment.")

    # Do not elevate the Python interpreter or interpolate a path into
    # privileged shell code. Only this fixed system command needs root.
    command = ["/usr/sbin/purge"]
    if args.administrator:
        command = [
            "/usr/bin/osascript", "-e",
            'do shell script "/usr/sbin/purge" with administrator privileges',
        ]
    page_size = os.sysconf("SC_PAGE_SIZE")
    size = os.path.getsize(args.bin)
    pages = (size + page_size - 1) // page_size
    report = harness.base_report("macos-cache-eviction-decision", args.bin)
    report.update({
        "binary_bytes": size,
        "page_size": page_size,
        "binary_pages": pages,
        "runs_requested": args.runs,
        "command": command,
        "scope": "whole-system disk cache",
        "machine_conditions": harness.machine_fields(),
        "attempts": [],
        "note": "This experiment measures file residency, not cold-boot latency.",
    })
    # A decision experiment can precede the timing session. Keep its
    # actual machine conditions instead of overwriting that day's spec.
    machine_path = harness.result_path("machine-eviction")
    report["machine"] = os.path.basename(machine_path)
    machine = {
        **report.pop("machine_conditions"),
        "date": report["date"], "os": report["os"],
        "git_rev": report["git_rev"], "git_dirty": report["git_dirty"],
        "cores": os.cpu_count(), "kernel": platform.release(),
        "binary_bytes": size,
        "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
    }
    harness.write_report(machine_path, machine)
    for _ in range(args.runs):
        digest = hashlib.sha256()
        with open(args.bin, "rb") as binary:
            while chunk := binary.read(1024 * 1024):
                digest.update(chunk)
        report["binary_sha256"] = digest.hexdigest()
        before = harness.resident_pages(args.bin)
        try:
            result = subprocess.run(command, capture_output=True, text=True, timeout=180)
        except subprocess.TimeoutExpired:
            result = subprocess.CompletedProcess(command, 124, "", "The purge command timed out.")
        after = harness.resident_pages(args.bin)
        report["attempts"].append({
            "before_pages": before,
            "purge_returncode": result.returncode,
            "purge_stdout": result.stdout.strip(),
            "purge_stderr": result.stderr.strip(),
            "after_pages": after,
        })
        # A denied command supplies no eviction evidence. Do not repeat
        # an authorization prompt or disguise that failure as a probe result.
        if result.returncode != 0:
            break
    report["eviction_verified"] = verified(report["attempts"], pages, args.runs)
    path = args.out or harness.result_path("macos-eviction")
    harness.write_report(path, report)
    print(f"Eviction verified: {report['eviction_verified']}")
    print(f"Raw data: {path}")
    return 0 if report["eviction_verified"] else 1


if __name__ == "__main__":
    sys.exit(main())
