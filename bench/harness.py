#!/usr/bin/env python3
"""Shared bench harness.

The metric scripts keep measurement logic. This module owns the
private run environment, auth fixture, PTY spawn and shutdown,
page-cache eviction, spawn-floor calibration, and result-file output.

Spawn notes:
- `os.posix_spawn` starts the child, not `subprocess`. The Python
  spawn layer would add ~0.25 ms inside the timed span.
- The child runs in its own session with no controlling terminal, so
  the line editor uses the PTY through stdin and stdout. A real
  interactive run opens `/dev/tty`; the paths differ only there.
- Shutdown waits for the input prompt before it sends Ctrl-D. The
  line editor drops an EOF that arrives before the terminal switches
  to raw mode.
"""

import fcntl
import glob
import json
import os
import platform
import select
import signal
import statistics
import struct
import subprocess
import sys
import termios
import time

# The native probe layer: page-cache reads, eviction, machine-spec
# fields. Everything platform-specific lives behind this import, and
# callers reach it only through the wrappers below.
if sys.platform == "linux":
    import platform_linux as _native
elif sys.platform == "darwin":
    import platform_macos as _native
else:
    raise ImportError(f"the bench suite supports Linux and macOS, not {sys.platform}")

READY_MARKER = b"demerzel ready"
# The prompt glyph without its trailing space: the full-screen
# interface draws the prompt cell by cell and never writes the blank
# cell after it.
PROMPT_MARKER = "❯".encode()
READY_TIMEOUT_S = 10.0
PROMPT_TIMEOUT_S = 2.0
EXIT_TIMEOUT_S = 5.0

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

FLOOR_BIN = "/usr/bin/printf"
FLOOR_ARGV = ["printf", "demerzel ready"]

AUTH_FIXTURE = {
    "version": 1,
    "openai_codex": {
        "access_token": "bench-access",
        "refresh_token": "bench-refresh",
    },
}


def _git(*args: str) -> str:
    out = subprocess.run(
        ["git", "-C", REPO_ROOT, *args], capture_output=True, text=True, check=True
    )
    return out.stdout.strip()


def git_rev() -> str:
    return _git("rev-parse", "--short", "HEAD")


def git_dirty() -> bool:
    """True when tracked files outside `bench/results/` differ from
    HEAD. Result files do not produce numbers, so they do not count;
    untracked files do not count either."""
    return bool(_git("status", "--porcelain", "-uno", "--", ".", ":(exclude)bench/results"))


def os_slug() -> str:
    """The OS-arch slug in result-file names, e.g. `linux-x86_64`,
    `macos-aarch64`. Two reference machines produce receipts, so the
    name must say which one. The arch is the interpreter's: a Rosetta
    Python would mislabel the receipt, so run a native one."""
    os_name = {"linux": "linux", "darwin": "macos"}[sys.platform]
    arch = platform.machine()
    if arch == "arm64":
        arch = "aarch64"
    return f"{os_name}-{arch}"


def base_report(benchmark: str, bin_path: str) -> dict:
    """The envelope every result file shares."""
    date = time.strftime("%Y-%m-%d")
    slug = os_slug()
    return {
        "benchmark": benchmark,
        "bin": os.path.relpath(bin_path, REPO_ROOT),
        "git_rev": git_rev(),
        "git_dirty": git_dirty(),
        "date": date,
        "os": slug,
        "machine": f"machine-{date}-{slug}.json",
    }


def result_path(prefix: str) -> str:
    """The one result-name template:
    `bench/results/<prefix>-<date>-<rev>-<slug>.json`. Every script
    builds its output path here, so the name and the envelope agree."""
    name = f"{prefix}-{time.strftime('%Y-%m-%d')}-{git_rev()}-{os_slug()}.json"
    return os.path.join(REPO_ROOT, "bench", "results", name)


def filesystem(path: str) -> str:
    """The filesystem type that holds `path`, via the native probe."""
    return _native.filesystem(path)


def machine_fields() -> dict:
    """The platform entries of the machine spec."""
    return _native.machine_fields(REPO_ROOT)


def supports_evict() -> bool:
    """Whether this platform has verified page-cache eviction."""
    return _native.SUPPORTS_EVICT


def resident_pages(path: str) -> int:
    """Read the number of resident file pages through the native probe."""
    return _native.resident_pages(path)


def supports_proc_memory() -> bool:
    """Whether this platform has the per-process memory probe."""
    return _native.SUPPORTS_PROC_MEMORY


def process_memory(pid: int) -> dict[str, int]:
    """Read the platform's process memory counters, in KiB."""
    return _native.process_memory(pid)


def bench_env(data_home: str) -> dict[str, str]:
    """Environment for a spawned run: private data home and HOME, no
    demerzel overrides. HOME must move too, or a future auth-store
    change could fall back to the real `~/.codex` import."""
    env = {k: v for k, v in os.environ.items() if not k.startswith("DEMERZEL_")}
    env["XDG_DATA_HOME"] = data_home
    env["HOME"] = data_home
    env.setdefault("TERM", "xterm-256color")
    return env


def write_auth_fixture(data_home: str) -> None:
    store_dir = os.path.join(data_home, "demerzel")
    os.makedirs(store_dir, exist_ok=True)
    path = os.path.join(store_dir, "auth.json")
    with open(path, "w") as f:
        json.dump(AUTH_FIXTURE, f)
    os.chmod(path, 0o600)


def stats(samples: list[float]) -> dict:
    n = len(samples)
    stddev = statistics.stdev(samples) if n > 1 else 0.0
    mean = statistics.mean(samples)
    half = 1.96 * stddev / (n**0.5) if n > 1 else 0.0
    return {
        "mean": round(mean, 3),
        "median": round(statistics.median(samples), 3),
        "stddev": round(stddev, 3),
        "min": round(min(samples), 3),
        "max": round(max(samples), 3),
        "ci95_mean": [round(mean - half, 3), round(mean + half, 3)],
    }


def criterion_benches(target: str, group: str | None = None, since: float = 0.0) -> dict:
    """Every criterion estimate under `target/criterion`, one entry per
    bench, raw samples included. `group` narrows to one benchmark
    group; without it the ungrouped benches are collected. `since` is
    a Unix time: an estimate written before it is stale — a bench that
    did not run this time — and stays out, so a report never carries
    an old number under a new revision. A bench that declares a byte
    throughput also gets `bytes_per_second` from criterion's typical
    estimate — the slope when there is one, else the mean — the same
    number the tool prints; an element throughput gets
    `elements_per_second`."""
    pattern = os.path.join(target, "criterion", group or "", "*", "new", "estimates.json")
    benches = {}
    for est_path in sorted(glob.glob(pattern)):
        name = est_path.split(os.sep)[-3]
        if name == "report" or os.path.getmtime(est_path) < since:
            continue
        with open(est_path) as f:
            est = json.load(f)
        entry = {
            "mean_ns": round(est["mean"]["point_estimate"]),
            "median_ns": round(est["median"]["point_estimate"]),
            "stddev_ns": round(est["std_dev"]["point_estimate"]),
            "mean_ci95_ns": [
                round(est["mean"]["confidence_interval"]["lower_bound"]),
                round(est["mean"]["confidence_interval"]["upper_bound"]),
            ],
        }
        if est.get("slope"):
            entry["slope_ns"] = round(est["slope"]["point_estimate"])
        new_dir = os.path.dirname(est_path)
        bench_path = os.path.join(new_dir, "benchmark.json")
        if os.path.exists(bench_path):
            with open(bench_path) as f:
                throughput = json.load(f).get("throughput") or {}
            typical_ns = (est.get("slope") or est["mean"])["point_estimate"]
            if "Bytes" in throughput:
                entry["bytes"] = throughput["Bytes"]
                entry["bytes_per_second"] = round(throughput["Bytes"] / (typical_ns / 1e9))
            if "Elements" in throughput:
                entry["elements"] = throughput["Elements"]
                entry["elements_per_second"] = round(
                    throughput["Elements"] / (typical_ns / 1e9)
                )
        sample_path = os.path.join(new_dir, "sample.json")
        if os.path.exists(sample_path):
            with open(sample_path) as f:
                entry["samples"] = json.load(f)
        benches[name] = entry
    return benches


def write_report(path: str, report: dict) -> None:
    parent = os.path.dirname(path)
    if parent:
        os.makedirs(parent, exist_ok=True)
    with open(path, "w") as f:
        json.dump(report, f, indent=2)
        f.write("\n")


def _read_until(master: int, marker: bytes, timeout_s: float) -> tuple[str, bytes]:
    """Read the merged stream until `marker`. Returns a status out of
    "found", "closed", "timeout", plus the bytes seen."""
    buf = b""
    deadline = time.monotonic() + timeout_s
    while marker not in buf:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return "timeout", buf
        ready, _, _ = select.select([master], [], [], remaining)
        if not ready:
            continue
        try:
            chunk = os.read(master, 4096)
        except OSError:
            chunk = b""
        if not chunk:
            return "closed", buf
        buf += chunk
    return "found", buf


def spawn_ready(
    bin_path: str, env: dict[str, str], argv: list[str] | None = None
) -> tuple[int, int, int, bytes]:
    """Spawn `bin_path` in a PTY and wait for the ready marker on the
    merged stream. Returns (pid, master_fd, elapsed_ns, seen_bytes)."""
    master, slave = os.openpty()
    # A fresh PTY reports zero rows and columns. Give it a real window
    # or the full-screen interface has nothing to draw into.
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
    dup2 = [(os.POSIX_SPAWN_DUP2, slave, fd) for fd in (0, 1, 2)]
    start = time.perf_counter_ns()
    try:
        pid = os.posix_spawn(bin_path, argv or [bin_path], env, file_actions=dup2, setsid=True)
    except OSError:
        os.close(master)
        os.close(slave)
        raise
    os.close(slave)
    status, buf = _read_until(master, READY_MARKER, READY_TIMEOUT_S)
    elapsed_ns = time.perf_counter_ns() - start
    if status != "found":
        shutdown(pid, master, wait_prompt=False)
        raise RuntimeError(
            f"{status} before the ready marker; output: {buf.decode(errors='replace')!r}"
        )
    return pid, master, elapsed_ns, buf


def wait_prompt(pid: int, master: int, seen: bytes) -> tuple[int, bytes]:
    """Continue reading after the ready marker until the input prompt.
    Returns the extra nanoseconds and the full output. The prompt
    covers what the marker does not: the screen switch, the history
    load, and the first frame. On failure the child is cleaned up
    before the error raises."""
    if PROMPT_MARKER in seen:
        return 0, seen
    start = time.perf_counter_ns()
    status, buf = _read_until(master, PROMPT_MARKER, PROMPT_TIMEOUT_S)
    elapsed_ns = time.perf_counter_ns() - start
    if status != "found":
        shutdown(pid, master, wait_prompt=False)
        raise RuntimeError(
            f"{status} before the prompt; output: {buf.decode(errors='replace')!r}"
        )
    return elapsed_ns, seen + buf


def _wait_exit(pid: int, timeout_s: float) -> bool:
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        done, _ = os.waitpid(pid, os.WNOHANG)
        if done:
            return True
        time.sleep(0.005)
    return False


def shutdown(pid: int, master: int, seen: bytes = b"", wait_prompt: bool = True) -> None:
    """End the child: wait for the input prompt, send Ctrl-D, then
    kill the process group on timeout. `seen` is output already read;
    when it holds the prompt, the wait is skipped."""
    if wait_prompt and PROMPT_MARKER not in seen:
        _read_until(master, PROMPT_MARKER, PROMPT_TIMEOUT_S)
    try:
        os.write(master, b"\x04")
    except OSError:
        pass
    if not _wait_exit(pid, EXIT_TIMEOUT_S):
        try:
            os.killpg(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        os.waitpid(pid, 0)
    os.close(master)


def spawn_floor_ms(env: dict[str, str], runs: int = 10) -> dict:
    """The probe's own cost through the identical spawn path: a PTY
    spawn of `printf` that emits the ready marker and exits."""
    samples = []
    for _ in range(runs):
        pid, master, elapsed_ns, _ = spawn_ready(FLOOR_BIN, env, FLOOR_ARGV)
        if not _wait_exit(pid, EXIT_TIMEOUT_S):
            try:
                os.killpg(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.waitpid(pid, 0)
        os.close(master)
        samples.append(elapsed_ns / 1e6)
    return {"runs": runs, **stats(samples)}


def evict_verified(path: str, attempts: int = 6) -> None:
    """Evict `path` from the page cache and verify with mincore.
    One eviction attempt is advisory and often leaves recently
    referenced pages resident; retry until zero pages remain. On a
    platform without eviction (`supports_evict` is false) the first
    attempt raises with the reason. The retry loop fits advisory
    per-file eviction; a global mechanism must replace the loop."""
    resident = -1
    for _ in range(attempts):
        _native.evict_once(path)
        resident = _native.resident_pages(path)
        if resident == 0:
            return
        time.sleep(0.02)
    raise RuntimeError(f"{resident} pages still resident after {attempts} evictions")
