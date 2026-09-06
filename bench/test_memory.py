"""Memory receipts must preserve native metrics and fail on missing data."""

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import mock_open, patch

import harness
import platform_linux


BENCH = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("idle", BENCH / "memory" / "idle.py")
idle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(idle)


class LinuxMemoryTests(unittest.TestCase):
    def test_rollup_preserves_linux_units_and_uss(self):
        data = """00400000-00600000 r--p 00000000 00:00 0 [rollup]
Rss:                 144 kB
Pss:                 123 kB
Private_Clean:        24 kB
Private_Dirty:        70 kB
Anonymous:            66 kB
"""
        with patch("builtins.open", mock_open(read_data=data)):
            self.assertEqual(platform_linux.process_memory(123), {
                "anonymous_kb": 66, "private_dirty_kb": 70,
                "pss_kb": 123, "uss_kb": 94, "rss_kb": 144,
            })

    def test_missing_counter_does_not_become_zero(self):
        with patch("builtins.open", mock_open(read_data="Pss: 12 kB\n")):
            with self.assertRaises(KeyError):
                platform_linux.process_memory(123)

    @unittest.skipUnless(sys.platform == "linux", "Linux native probe")
    def test_live_linux_process(self):
        counters = harness.process_memory(os.getpid())
        self.assertGreater(counters["pss_kb"], 0)
        self.assertGreater(counters["uss_kb"], 0)
        self.assertGreaterEqual(counters["rss_kb"], counters["uss_kb"])


class IdleMemoryTests(unittest.TestCase):
    def test_receipt_keeps_platform_fields_and_raw_samples(self):
        for counters in (
            {"physical_footprint_kb": 4096, "peak_physical_footprint_kb": 8192},
            {"anonymous_kb": 800, "private_dirty_kb": 820, "pss_kb": 2000,
             "uss_kb": 1900, "rss_kb": 2500},
        ):
            with self.subTest(counters=counters), tempfile.TemporaryDirectory() as tmp:
                output = Path(tmp) / "receipt.json"
                with patch.object(sys, "argv", ["idle.py", "--bin", sys.executable,
                                               "--runs", "2", "--out", str(output)]), \
                     patch.object(idle, "sample", return_value=counters), \
                     contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(idle.main(), 0)
                report = json.loads(output.read_text())
                self.assertEqual(report["runs"], 2)
                self.assertEqual(report["samples_kb"], {key: [value, value] for key, value in counters.items()})
                self.assertEqual(set(report["stats_kb"]), set(counters))
                self.assertEqual(report["interface"], "tui")

    def test_probe_failure_still_shuts_down_the_child(self):
        with patch.object(harness, "spawn_ready", return_value=(123, 9, 0, b"ready")), \
             patch.object(harness, "wait_prompt", return_value=(0, b"prompt")), \
             patch.object(harness, "process_memory", side_effect=ProcessLookupError), \
             patch.object(harness, "shutdown") as shutdown:
            with self.assertRaises(ProcessLookupError):
                idle.sample("binary", {}, 0)
            shutdown.assert_called_once_with(123, 9, b"prompt")

    def test_invalid_run_options_do_not_create_a_receipt(self):
        for arguments in (["--runs", "0"], ["--settle", "-1"], ["--settle", "nan"], ["--settle", "inf"]):
            with self.subTest(arguments=arguments), tempfile.TemporaryDirectory() as tmp:
                output = Path(tmp) / "receipt.json"
                result = subprocess.run([sys.executable, str(BENCH / "memory" / "idle.py"),
                                         *arguments, "--out", str(output)], capture_output=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(output.exists())

    def test_pid_cli_accepts_a_live_process_and_rejects_invalid_pids(self):
        for name in ("memory.py", "pss.py"):
            result = subprocess.run([sys.executable, str(BENCH / "memory" / name), str(os.getpid())], capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
        for pid in ("0", "-1", str(2**32 + os.getpid())):
            result = subprocess.run([sys.executable, str(BENCH / "memory" / "memory.py"), pid], capture_output=True)
            self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
