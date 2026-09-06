"""Linux-native probes for the bench harness.

`harness.py` loads this module on Linux. It owns everything that
reads `/proc` or `/sys`, the libc access, and the Linux machine-spec
fields. The interface is the same as `platform_macos.py`:
`resident_pages`, `evict_once`, `process_memory`, `SUPPORTS_EVICT`,
`SUPPORTS_PROC_MEMORY`, `filesystem`, and `machine_fields`.
"""

import ctypes
import os
import subprocess

import _mincore

SUPPORTS_EVICT = True
EVICTION_SCOPE = "file"
SUPPORTS_PROC_MEMORY = True

_libc_handle = None


def _libc():
    """The libc handle, loaded on first use so importing the harness
    never depends on the soname."""
    global _libc_handle
    if _libc_handle is None:
        _libc_handle = ctypes.CDLL("libc.so.6", use_errno=True)
    return _libc_handle


def resident_pages(path: str) -> int:
    return _mincore.resident_pages(_libc(), path)


def process_memory(pid: int) -> dict[str, int]:
    """Read the existing Linux counters without substituting other metrics."""
    fields = {}
    with open(f"/proc/{pid}/smaps_rollup") as f:
        for line in f:
            key, _, rest = line.partition(":")
            parts = rest.split()
            if len(parts) >= 2 and parts[1] == "kB":
                fields[key] = int(parts[0])
    return {
        "anonymous_kb": fields["Anonymous"],
        "private_dirty_kb": fields["Private_Dirty"],
        "pss_kb": fields["Pss"],
        "uss_kb": fields["Private_Clean"] + fields["Private_Dirty"],
        "rss_kb": fields["Rss"],
    }


def evict_once(path: str) -> None:
    """One eviction attempt: advisory, often partial. The caller
    verifies with `resident_pages` and retries."""
    fd = os.open(path, os.O_RDONLY)
    try:
        os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
    finally:
        os.close(fd)


def _run(cmd: list[str]) -> str:
    try:
        out = subprocess.run(cmd, capture_output=True, text=True, check=True)
        return out.stdout.strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return ""


def filesystem(path: str) -> str:
    """The filesystem type that holds `path`."""
    return _run(["findmnt", "-no", "FSTYPE", "--target", path])


def _read_first(path: str) -> str:
    try:
        with open(path) as f:
            return f.read().strip()
    except OSError:
        return ""


def _cpu_model() -> str:
    with open("/proc/cpuinfo") as f:
        for line in f:
            if line.startswith("model name"):
                return line.partition(":")[2].strip()
    return ""


def _mem_total_kb() -> int:
    with open("/proc/meminfo") as f:
        for line in f:
            if line.startswith("MemTotal:"):
                return int(line.split()[1])
    return 0


def _distro() -> str:
    for line in _read_first("/etc/os-release").splitlines():
        if line.startswith("PRETTY_NAME="):
            return line.partition("=")[2].strip('"')
    return ""


def machine_fields(repo_root: str) -> dict:
    """The Linux entries of the machine spec. The shared fields —
    date, os, cores, kernel, rustc, binary, git — stay in
    `machine.py` and win on a key clash."""
    return {
        "cpu": _cpu_model(),
        "cpu_governor": _read_first(
            "/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor"
        ),
        "mem_total_kb": _mem_total_kb(),
        "distro": _distro(),
        "filesystem": filesystem(repo_root),
    }
