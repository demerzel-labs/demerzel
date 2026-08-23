"""Linux-native probes for the bench harness.

`harness.py` loads this module on Linux. It owns everything that
reads `/proc` or `/sys`, the libc calls, and the Linux machine-spec
fields. The interface is the same as `platform_macos.py`:
`resident_pages`, `evict_once`, `SUPPORTS_EVICT`, `filesystem`, and
`machine_fields`.
"""

import ctypes
import mmap
import os
import subprocess

SUPPORTS_EVICT = True

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


def evict_once(path: str) -> None:
    """One eviction attempt: advisory, often partial. The caller
    verifies with `resident_pages` and retries."""
    fd = os.open(path, os.O_RDONLY)
    try:
        os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
    finally:
        os.close(fd)


def filesystem(path: str) -> str:
    """The filesystem type that holds `path`."""
    out = subprocess.run(
        ["findmnt", "-no", "FSTYPE", "--target", path],
        capture_output=True,
        text=True,
    )
    return out.stdout.strip()


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
    date, cores, kernel, rustc, binary, git — stay in `machine.py`."""
    return {
        "cpu": _cpu_model(),
        "cpu_governor": _read_first(
            "/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor"
        ),
        "mem_total_kb": _mem_total_kb(),
        "distro": _distro(),
        "filesystem": filesystem(repo_root),
    }
