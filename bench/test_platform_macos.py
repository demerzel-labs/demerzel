"""Native process memory checks. Run with unittest discovery in bench/."""

import ctypes
import errno
import os
from pathlib import Path
import select
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

import platform_macos


class ProcessMemoryContractTests(unittest.TestCase):
    def test_returns_integer_kib_for_both_footprint_counters(self):
        def read_usage(pid, flavor, buffer):
            usage = ctypes.cast(
                buffer, ctypes.POINTER(platform_macos._RusageInfoV4)
            ).contents
            usage.ri_phys_footprint = 3 * 1024 + 512
            usage.ri_lifetime_max_phys_footprint = 7 * 1024 + 128
            usage.ri_resident_size = 11 * 1024
            return 0

        with mock.patch.object(platform_macos, "_libc") as libc:
            libc.return_value.proc_pid_rusage.side_effect = read_usage
            self.assertEqual(
                platform_macos.process_memory(123),
                {"physical_footprint_kb": 3, "peak_physical_footprint_kb": 7},
            )

    def test_preserves_native_error(self):
        def fail(*args):
            ctypes.set_errno(errno.EPERM)
            return -1

        with mock.patch.object(platform_macos, "_libc") as libc:
            libc.return_value.proc_pid_rusage.side_effect = fail
            with self.assertRaises(OSError) as caught:
                platform_macos.process_memory(123)
            self.assertEqual(caught.exception.errno, errno.EPERM)

    def test_rejects_invalid_pids_before_native_conversion(self):
        with mock.patch.object(platform_macos, "_libc") as libc:
            for pid in (0, -1, 2**31, 2**32 + os.getpid()):
                with self.subTest(pid=pid), self.assertRaises(ValueError):
                    platform_macos.process_memory(pid)
            for pid in (True, 1.5, "123"):
                with self.subTest(pid=pid), self.assertRaises(TypeError):
                    platform_macos.process_memory(pid)
            libc.assert_not_called()


@unittest.skipUnless(sys.platform == "darwin", "Requires macOS native probes")
class NativeProcessMemoryTests(unittest.TestCase):
    def test_structure_matches_installed_sdk(self):
        if shutil.which("xcrun") is None:
            self.skipTest("Requires the macOS SDK")
        source = """
#include <stddef.h>
#include <stdio.h>
#include <sys/resource.h>

int main(void) {
    printf("%d %zu %zu %zu %zu\\n",
           RUSAGE_INFO_V4,
           sizeof(struct rusage_info_v4),
           _Alignof(struct rusage_info_v4),
           offsetof(struct rusage_info_v4, ri_phys_footprint),
           offsetof(struct rusage_info_v4, ri_lifetime_max_phys_footprint));
    return 0;
}
"""
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "rusage-layout"
            subprocess.run(
                ["xcrun", "clang", "-x", "c", "-", "-o", str(binary)],
                input=source,
                text=True,
                capture_output=True,
                check=True,
                timeout=30,
            )
            output = subprocess.check_output([str(binary)], text=True, timeout=5)
        layout = platform_macos._RusageInfoV4
        self.assertEqual(
            [int(value) for value in output.split()],
            [
                platform_macos._RUSAGE_INFO_V4,
                ctypes.sizeof(layout),
                ctypes.alignment(layout),
                layout.ri_phys_footprint.offset,
                layout.ri_lifetime_max_phys_footprint.offset,
            ],
        )

    def test_reads_current_process(self):
        memory = platform_macos.process_memory(os.getpid())
        self.assertGreater(memory["physical_footprint_kb"], 0)
        self.assertGreaterEqual(
            memory["peak_physical_footprint_kb"], memory["physical_footprint_kb"]
        )

    def test_exited_process_is_an_error(self):
        with subprocess.Popen([sys.executable, "-c", "pass"]) as process:
            process.wait(timeout=5)
            with self.assertRaises(ProcessLookupError) as caught:
                platform_macos.process_memory(process.pid)
            self.assertEqual(caught.exception.errno, errno.ESRCH)

    def test_peak_survives_freeing_private_memory(self):
        program = """
import mmap
import os
import sys

print("ready", flush=True)
sys.stdin.readline()
size = 32 * 1024 * 1024
memory = mmap.mmap(-1, size, flags=mmap.MAP_PRIVATE | mmap.MAP_ANON)
for offset in range(0, size, os.sysconf("SC_PAGE_SIZE")):
    memory[offset] = 1
print("allocated", flush=True)
sys.stdin.readline()
memory.close()
print("freed", flush=True)
sys.stdin.readline()
"""
        process = subprocess.Popen(
            [sys.executable, "-c", program],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            text=True,
        )

        def sample_after(expected):
            readable, _, _ = select.select([process.stdout], [], [], 5)
            self.assertTrue(readable, "The memory fixture did not respond")
            self.assertEqual(process.stdout.readline().strip(), expected)
            return platform_macos.process_memory(process.pid)

        def advance():
            process.stdin.write("\n")
            process.stdin.flush()

        try:
            before = sample_after("ready")
            advance()
            allocated = sample_after("allocated")
            advance()
            freed = sample_after("freed")
            advance()
            process.wait(timeout=5)
        finally:
            if process.poll() is None:
                process.kill()
            process.wait(timeout=5)
            process.stdin.close()
            process.stdout.close()

        self.assertGreater(
            allocated["physical_footprint_kb"] - before["physical_footprint_kb"],
            16 * 1024,
        )
        self.assertGreater(
            allocated["physical_footprint_kb"] - freed["physical_footprint_kb"],
            16 * 1024,
        )
        self.assertGreaterEqual(
            freed["peak_physical_footprint_kb"],
            allocated["peak_physical_footprint_kb"],
        )
        self.assertGreaterEqual(
            allocated["peak_physical_footprint_kb"],
            allocated["physical_footprint_kb"],
        )


if __name__ == "__main__":
    unittest.main()
