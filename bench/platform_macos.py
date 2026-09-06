"""macOS-native probes for the bench harness.

`harness.py` loads this module on macOS. It owns the libSystem access
and the macOS machine-spec fields. The interface is the same as
`platform_linux.py`: `resident_pages`, `evict_once`, `process_memory`,
`SUPPORTS_EVICT`, `SUPPORTS_PROC_MEMORY`, `filesystem`, and `machine_fields`.

Cold-boot eviction uses `purge`, which flushes the whole disk cache.
The caller must select system-wide cold mode. macOS authorizes only
the fixed purge command; the benchmark process stays unprivileged.
"""

import ctypes
import re
import subprocess

import _mincore

SUPPORTS_EVICT = True
EVICTION_SCOPE = "system"
SUPPORTS_PROC_MEMORY = True

_RUSAGE_INFO_V4 = 4


class _RusageInfoV4(ctypes.Structure):
    # The complete sys/resource.h layout. The kernel writes all fields,
    # including those after the two footprint counters that we read.
    _fields_ = [
        ("ri_uuid", ctypes.c_uint8 * 16),
        ("ri_user_time", ctypes.c_uint64),
        ("ri_system_time", ctypes.c_uint64),
        ("ri_pkg_idle_wkups", ctypes.c_uint64),
        ("ri_interrupt_wkups", ctypes.c_uint64),
        ("ri_pageins", ctypes.c_uint64),
        ("ri_wired_size", ctypes.c_uint64),
        ("ri_resident_size", ctypes.c_uint64),
        ("ri_phys_footprint", ctypes.c_uint64),
        ("ri_proc_start_abstime", ctypes.c_uint64),
        ("ri_proc_exit_abstime", ctypes.c_uint64),
        ("ri_child_user_time", ctypes.c_uint64),
        ("ri_child_system_time", ctypes.c_uint64),
        ("ri_child_pkg_idle_wkups", ctypes.c_uint64),
        ("ri_child_interrupt_wkups", ctypes.c_uint64),
        ("ri_child_pageins", ctypes.c_uint64),
        ("ri_child_elapsed_abstime", ctypes.c_uint64),
        ("ri_diskio_bytesread", ctypes.c_uint64),
        ("ri_diskio_byteswritten", ctypes.c_uint64),
        ("ri_cpu_time_qos_default", ctypes.c_uint64),
        ("ri_cpu_time_qos_maintenance", ctypes.c_uint64),
        ("ri_cpu_time_qos_background", ctypes.c_uint64),
        ("ri_cpu_time_qos_utility", ctypes.c_uint64),
        ("ri_cpu_time_qos_legacy", ctypes.c_uint64),
        ("ri_cpu_time_qos_user_initiated", ctypes.c_uint64),
        ("ri_cpu_time_qos_user_interactive", ctypes.c_uint64),
        ("ri_billed_system_time", ctypes.c_uint64),
        ("ri_serviced_system_time", ctypes.c_uint64),
        ("ri_logical_writes", ctypes.c_uint64),
        ("ri_lifetime_max_phys_footprint", ctypes.c_uint64),
        ("ri_instructions", ctypes.c_uint64),
        ("ri_cycles", ctypes.c_uint64),
        ("ri_billed_energy", ctypes.c_uint64),
        ("ri_serviced_energy", ctypes.c_uint64),
        ("ri_interval_max_phys_footprint", ctypes.c_uint64),
        ("ri_runnable_time", ctypes.c_uint64),
    ]

_libc_handle = None


def _libc():
    """The libSystem handle, loaded on first use."""
    global _libc_handle
    if _libc_handle is None:
        _libc_handle = ctypes.CDLL("libSystem.B.dylib", use_errno=True)
        _libc_handle.proc_pid_rusage.argtypes = [
            ctypes.c_int,
            ctypes.c_int,
            ctypes.POINTER(ctypes.c_void_p),
        ]
        _libc_handle.proc_pid_rusage.restype = ctypes.c_int
    return _libc_handle


def resident_pages(path: str) -> int:
    return _mincore.resident_pages(_libc(), path)


def evict_once(path: str) -> None:
    result = subprocess.run(
        ["/usr/bin/osascript", "-e",
         'do shell script "/usr/sbin/purge" with administrator privileges'],
        capture_output=True, text=True, timeout=180,
    )
    if result.returncode != 0:
        raise RuntimeError("macOS did not authorize or complete the system cache purge")


def process_memory(pid: int) -> dict[str, int]:
    """Current and lifetime peak physical footprint, in integer KiB."""
    if type(pid) is not int:
        raise TypeError("The process ID must be an integer")
    if not 0 < pid <= 0x7FFFFFFF:
        raise ValueError("The process ID must be a positive signed 32-bit integer")
    usage = _RusageInfoV4()
    # rusage_info_t is void *, but the API writes the structure directly
    # at the supplied address, as the SDK's callers do.
    buffer = ctypes.cast(ctypes.byref(usage), ctypes.POINTER(ctypes.c_void_p))
    if _libc().proc_pid_rusage(pid, _RUSAGE_INFO_V4, buffer) != 0:
        raise OSError(ctypes.get_errno(), f"Cannot read process memory for PID {pid}")
    return {
        "physical_footprint_kb": usage.ri_phys_footprint // 1024,
        "peak_physical_footprint_kb": usage.ri_lifetime_max_phys_footprint // 1024,
    }


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
