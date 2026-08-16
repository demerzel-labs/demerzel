#!/usr/bin/env python3
"""Shared bench harness.

The metric scripts under `boot/` and `memory/` import this module and
hold only measurement logic. The harness owns: the private run
environment, the auth fixture, PTY spawn and shutdown, page-cache
eviction, spawn-floor calibration, and result-file output.

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

import ctypes
import fcntl
import json
import mmap
import os
import select
import signal
import statistics
import struct
import subprocess
import termios
import time

READY_MARKER = b"demerzel ready"
# No trailing space: the full-screen interface draws the prompt cell
# by cell and never writes the blank cell after it.
PROMPT_MARKER = b"dmz>"
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


def base_report(benchmark: str, bin_path: str) -> dict:
    """The envelope every result file shares."""
    return {
        "benchmark": benchmark,
        "bin": os.path.relpath(bin_path, REPO_ROOT),
        "git_rev": git_rev(),
        "git_dirty": git_dirty(),
        "date": time.strftime("%Y-%m-%d"),
        "machine": f"machine-{time.strftime('%Y-%m-%d')}.json",
    }


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


_libc = ctypes.CDLL("libc.so.6", use_errno=True)


def resident_pages(path: str) -> int:
    """Pages of `path` in the page cache, via mincore. The mapping is
    read-only and unmapped before return, so it does not pin pages."""
    size = os.path.getsize(path)
    fd = os.open(path, os.O_RDONLY)
    try:
        # MAP_PRIVATE read-write: ctypes.from_buffer needs a writable
        # buffer to take the address. Nothing writes to the mapping,
        # so no page is copied and mincore sees the file pages.
        mapped = mmap.mmap(
            fd, size, flags=mmap.MAP_PRIVATE, prot=mmap.PROT_READ | mmap.PROT_WRITE
        )
    finally:
        os.close(fd)
    try:
        page = os.sysconf("SC_PAGE_SIZE")
        pages = (size + page - 1) // page
        vec = (ctypes.c_ubyte * pages)()
        buf = (ctypes.c_char * size).from_buffer(mapped)
        rc = _libc.mincore(
            ctypes.c_void_p(ctypes.addressof(buf)), ctypes.c_size_t(size), vec
        )
        del buf
        if rc != 0:
            raise OSError(ctypes.get_errno(), "mincore failed")
        return sum(v & 1 for v in vec)
    finally:
        mapped.close()


def evict_verified(path: str, attempts: int = 6) -> None:
    """Evict `path` from the page cache and verify with mincore.
    One fadvise is advisory and often leaves recently referenced
    pages resident; retry until zero pages remain."""
    resident = -1
    for _ in range(attempts):
        fd = os.open(path, os.O_RDONLY)
        try:
            os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
        finally:
            os.close(fd)
        resident = resident_pages(path)
        if resident == 0:
            return
        time.sleep(0.02)
    raise RuntimeError(f"{resident} pages still resident after {attempts} evictions")
