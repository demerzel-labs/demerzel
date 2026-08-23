"""macOS-native probes for the bench harness.

`harness.py` loads this module on macOS. It owns the libSystem calls
and the macOS machine-spec fields. The interface is the same as
`platform_linux.py`: `resident_pages`, `evict_once`, `SUPPORTS_EVICT`,
`filesystem`, and `machine_fields`.

Cold-boot eviction is undecided on macOS: `purge` flushes the whole
disk cache and needs root. The decision is step 4 of
`docs/bench-platforms.md`; until it lands, `evict_once` refuses.
"""

import ctypes
import mmap
import os
import subprocess

SUPPORTS_EVICT = False

_libc = ctypes.CDLL("libSystem.B.dylib", use_errno=True)


def resident_pages(path: str) -> int:
    """Pages of `path` in the page cache, via mincore. Same shape as
    the Linux probe; macOS sets bit 0 for a resident page too."""
    size = os.path.getsize(path)
    fd = os.open(path, os.O_RDONLY)
    try:
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
    raise RuntimeError(
        "macOS has no verified page-cache eviction yet; "
        "the decision is step 4 of docs/bench-platforms.md"
    )


def _run(cmd: list[str]) -> str:
    try:
        out = subprocess.run(cmd, capture_output=True, text=True, check=True)
        return out.stdout.strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return ""


def filesystem(path: str) -> str:
    """The filesystem type that holds `path`, from the mount table.
    `df` names the device; the mount line carries the type as the
    first word in its parenthesized list."""
    df = _run(["df", "-P", path]).splitlines()
    if len(df) < 2:
        return ""
    device = df[-1].split()[0]
    for line in _run(["mount"]).splitlines():
        if line.startswith(device + " on "):
            _, _, rest = line.partition(" (")
            return rest.split(",")[0].rstrip(")")
    return ""


def _os_version() -> str:
    name = _run(["sw_vers", "-productName"])
    version = _run(["sw_vers", "-productVersion"])
    build = _run(["sw_vers", "-buildVersion"])
    return f"{name} {version} ({build})".strip()


def _power_source() -> str:
    for line in _run(["pmset", "-g", "batt"]).splitlines():
        if "drawing from" in line:
            return line.partition("drawing from")[2].strip().strip("'")
    return ""


def _low_power_mode() -> str:
    for line in _run(["pmset", "-g"]).splitlines():
        if "lowpowermode" in line:
            return line.split()[-1]
    return ""


def _thermal_state() -> str:
    """Intel Macs report `key = value` lines; Apple Silicon prints
    notes, and no recorded warning means the state is nominal."""
    out = _run(["pmset", "-g", "therm"])
    pairs = [line.strip() for line in out.splitlines() if "=" in line]
    if pairs:
        return "; ".join(pairs)
    return "nominal" if "No thermal warning" in out else out.strip()


def machine_fields(repo_root: str) -> dict:
    """The macOS entries of the machine spec. Power and thermal state
    stand in for the CPU-governor line: they are the conditions that
    move numbers on this platform."""
    return {
        "cpu": _run(["sysctl", "-n", "machdep.cpu.brand_string"]),
        "mem_total_kb": int(_run(["sysctl", "-n", "hw.memsize"]) or 0) // 1024,
        "distro": _os_version(),
        "filesystem": filesystem(repo_root),
        "power_source": _power_source(),
        "low_power_mode": _low_power_mode(),
        "thermal_state": _thermal_state(),
    }
