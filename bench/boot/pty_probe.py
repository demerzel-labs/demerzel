#!/usr/bin/env python3
"""PTY time-to-interactive probe: Layer 1 boot measurement.

The probe spawns the binary in a pseudo-terminal and measures the time
from spawn to the ready marker on the merged output stream. The REPL
prints the marker first, after credential resolution.

The probe gives the process a private XDG_DATA_HOME with a synthetic
auth store, so a run needs no login and makes no network call.

Modes:
- warm (default): discard warmup runs, then measure. The page cache
  holds the binary.
- cold (--cold): evict the binary from the page cache before each
  measured run with posix_fadvise. Shared libraries stay warm; a true
  cold start needs a fresh boot.
"""

import argparse
import json
import os
import select
import signal
import statistics
import subprocess
import sys
import tempfile
import time

READY_MARKER = b"demerzel ready"
READY_TIMEOUT_S = 10.0
EXIT_TIMEOUT_S = 5.0

REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))

AUTH_FIXTURE = {
    "version": 1,
    "openai_codex": {
        "access_token": "bench-access",
        "refresh_token": "bench-refresh",
    },
}


def git_rev() -> str:
    out = subprocess.run(
        ["git", "rev-parse", "--short", "HEAD"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    return out.stdout.strip()


def bench_env(data_home: str) -> dict[str, str]:
    """Environment for a spawned run: private data home, no overrides."""
    env = dict(os.environ)
    env["XDG_DATA_HOME"] = data_home
    env.pop("DEMERZEL_MODEL", None)
    env.pop("DEMERZEL_BASE_URL", None)
    env.setdefault("TERM", "xterm-256color")
    return env


def write_auth_fixture(data_home: str) -> None:
    store_dir = os.path.join(data_home, "demerzel")
    os.makedirs(store_dir, exist_ok=True)
    path = os.path.join(store_dir, "auth.json")
    with open(path, "w") as f:
        json.dump(AUTH_FIXTURE, f)
    os.chmod(path, 0o600)


def evict_from_page_cache(path: str) -> None:
    fd = os.open(path, os.O_RDONLY)
    try:
        os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
    finally:
        os.close(fd)


def spawn_ready(bin_path: str, env: dict[str, str]):
    """Spawn the REPL in a PTY and wait for the ready marker.

    Returns (process, master_fd, elapsed_ns). The caller shuts the
    process down with `shutdown`.
    """
    master, slave = os.openpty()
    start = time.perf_counter_ns()
    proc = subprocess.Popen(
        [bin_path],
        stdin=slave,
        stdout=slave,
        stderr=slave,
        env=env,
        cwd=REPO_ROOT,
        start_new_session=True,
    )
    os.close(slave)
    buf = b""
    deadline = time.monotonic() + READY_TIMEOUT_S
    while READY_MARKER not in buf:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            shutdown(proc, master)
            raise TimeoutError(
                f"no ready marker after {READY_TIMEOUT_S}s; output so far: "
                f"{buf.decode(errors='replace')!r}"
            )
        ready, _, _ = select.select([master], [], [], remaining)
        if not ready:
            continue
        try:
            chunk = os.read(master, 4096)
        except OSError:
            chunk = b""
        if not chunk:
            shutdown(proc, master)
            raise RuntimeError(
                f"process exited before ready; output: {buf.decode(errors='replace')!r}"
            )
        buf += chunk
    elapsed_ns = time.perf_counter_ns() - start
    return proc, master, elapsed_ns


def shutdown(proc: subprocess.Popen, master: int) -> None:
    """End the REPL: send Ctrl-D, then kill the process group on timeout."""
    try:
        os.write(master, b"\x04")
    except OSError:
        pass
    try:
        proc.wait(timeout=EXIT_TIMEOUT_S)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        proc.wait()
    os.close(master)


def measure_once(bin_path: str, env: dict[str, str], cold: bool) -> float:
    if cold:
        evict_from_page_cache(bin_path)
    proc, master, elapsed_ns = spawn_ready(bin_path, env)
    shutdown(proc, master)
    return elapsed_ns / 1e6


def main() -> int:
    parser = argparse.ArgumentParser(description=(__doc__ or "").strip())
    parser.add_argument(
        "--bin",
        default=os.path.join(REPO_ROOT, "target/release/demerzel"),
        help="path to the binary (default: target/release/demerzel)",
    )
    parser.add_argument("--runs", type=int, default=20, help="measured runs (default: 20)")
    parser.add_argument(
        "--warmup", type=int, default=3, help="warmup runs, warm mode only (default: 3)"
    )
    parser.add_argument(
        "--cold",
        action="store_true",
        help="evict the binary from the page cache before each run",
    )
    parser.add_argument("--out", help="output JSON path (default: under bench/results)")
    args = parser.parse_args()

    if not os.access(args.bin, os.X_OK):
        print(f"no binary at {args.bin} — run: cargo build --release", file=sys.stderr)
        return 1

    mode = "cold" if args.cold else "warm"
    rev = git_rev()
    out_path = args.out or os.path.join(
        REPO_ROOT,
        "bench/results",
        f"pty-boot-{mode}-{time.strftime('%Y-%m-%d')}-{rev}.json",
    )

    with tempfile.TemporaryDirectory(prefix="demerzel-bench-") as data_home:
        write_auth_fixture(data_home)
        env = bench_env(data_home)
        if not args.cold:
            for _ in range(args.warmup):
                measure_once(args.bin, env, cold=False)
        samples = [measure_once(args.bin, env, cold=args.cold) for _ in range(args.runs)]

    report = {
        "benchmark": "pty-time-to-interactive",
        "bin": os.path.relpath(args.bin, REPO_ROOT),
        "git_rev": rev,
        "date": time.strftime("%Y-%m-%d"),
        "mode": mode,
        "warmup": 0 if args.cold else args.warmup,
        "runs": args.runs,
        "samples_ms": [round(s, 3) for s in samples],
        "stats_ms": {
            "mean": round(statistics.mean(samples), 3),
            "median": round(statistics.median(samples), 3),
            "stddev": round(statistics.stdev(samples), 3) if len(samples) > 1 else 0.0,
            "min": round(min(samples), 3),
            "max": round(max(samples), 3),
        },
    }
    os.makedirs(os.path.dirname(out_path), exist_ok=True)
    with open(out_path, "w") as f:
        json.dump(report, f, indent=2)
        f.write("\n")
    print(f"{mode}: mean {report['stats_ms']['mean']} ms over {args.runs} runs")
    print(f"raw data: {out_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
