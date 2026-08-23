"""macOS-native probes for the bench harness.

`harness.py` loads this module on macOS. It owns the libSystem access
and the macOS machine-spec fields. The interface is the same as
`platform_linux.py`: `resident_pages`, `evict_once`, `SUPPORTS_EVICT`,
`SUPPORTS_PROC_MEMORY`, `filesystem`, and `machine_fields`.

Cold-boot eviction is undecided on macOS: `purge` flushes the whole
disk cache and needs root. The decision is step 4 of
`docs/bench-platforms.md`; until it lands, `evict_once` refuses. The
per-process memory probe is step 3.
"""

import ctypes
import re
import subprocess

import _mincore

SUPPORTS_EVICT = False
SUPPORTS_PROC_MEMORY = False

_libc_handle = None


def _libc():
    """The libSystem handle, loaded on first use."""
    global _libc_handle
    if _libc_handle is None:
        _libc_handle = ctypes.CDLL("libSystem.B.dylib", use_errno=True)
    return _libc_handle


def resident_pages(path: str) -> int:
    return _mincore.resident_pages(_libc(), path)


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
    `df` names the mount point; the mount line carries the type as
    the first word of its last parenthesized list."""
    df = _run(["df", "-P", path]).splitlines()
    if len(df) < 2:
        return ""
    # The mount point is everything after the capacity column; it can
    # contain spaces, so split from the percent sign, not by fields.
    match = re.search(r"\d+%\s+(.+)$", df[-1])
    if not match:
        return ""
    mountpoint = match.group(1).strip()
    for line in _run(["mount"]).splitlines():
        if f" on {mountpoint} (" in line:
            _, _, rest = line.rpartition(" (")
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
    return "unknown"


def _low_power_mode() -> str:
    for line in _run(["pmset", "-g"]).splitlines():
        if "lowpowermode" in line:
            return line.split()[-1]
    return "unknown"


def _thermal_state() -> str:
    """Intel Macs report `key = value` lines; Apple Silicon prints
    notes, and no recorded warning means the state is nominal."""
    out = _run(["pmset", "-g", "therm"])
    pairs = [line.strip() for line in out.splitlines() if "=" in line]
    if pairs:
        return "; ".join(pairs)
    if "No thermal warning" in out:
        return "nominal"
    return out.strip() or "unknown"


def machine_fields(repo_root: str) -> dict:
    """The macOS entries of the machine spec. Power and thermal state
    stand in for the CPU-governor line: they are the conditions that
    move numbers on this platform. The shared fields stay in
    `machine.py` and win on a key clash."""
    return {
        "cpu": _run(["sysctl", "-n", "machdep.cpu.brand_string"]),
        "mem_total_kb": int(_run(["sysctl", "-n", "hw.memsize"]) or 0) // 1024,
        "distro": _os_version(),
        "filesystem": filesystem(repo_root),
        "power_source": _power_source(),
        "low_power_mode": _low_power_mode(),
        "thermal_state": _thermal_state(),
    }
