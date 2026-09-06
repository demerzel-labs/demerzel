"""Check the evidence required for verified macOS file eviction."""

import unittest
from unittest import mock

import harness
import platform_macos

from boot.macos_eviction import verified


class EvictionDecisionTests(unittest.TestCase):
    def test_requires_successful_eviction_of_a_fully_resident_binary_each_time(self):
        sample = {"before_pages": 10, "purge_returncode": 0, "after_pages": 0}
        self.assertTrue(verified([sample, sample, sample], 10, 3))
        for field, value in (("before_pages", 0), ("before_pages", 9),
                             ("purge_returncode", 1), ("after_pages", 1)):
            with self.subTest(field=field, value=value):
                self.assertFalse(verified([sample, {**sample, field: value}, sample], 10, 3))
        self.assertFalse(verified([sample], 10, 3))
        self.assertFalse(verified([], 10, 3))

    def test_system_eviction_checks_residency_and_never_retries_global_purge(self):
        with mock.patch.object(harness, "_native") as native:
            native.EVICTION_SCOPE = "system"
            native.resident_pages.return_value = 1
            with self.assertRaises(RuntimeError):
                harness.evict_verified("binary")
            native.evict_once.assert_called_once_with("binary")
            native.resident_pages.assert_called_once_with("binary")

    def test_system_eviction_accepts_zero_residency(self):
        with mock.patch.object(harness, "_native") as native:
            native.EVICTION_SCOPE = "system"
            native.resident_pages.return_value = 0
            harness.evict_verified("binary")
            native.evict_once.assert_called_once_with("binary")

    def test_linux_keeps_per_file_retries(self):
        with mock.patch.object(harness, "_native") as native:
            native.EVICTION_SCOPE = "file"
            native.resident_pages.side_effect = [1, 0]
            harness.evict_verified("binary")
            self.assertEqual(native.evict_once.call_count, 2)

    def test_native_purge_elevates_only_a_fixed_command(self):
        with mock.patch.object(platform_macos.subprocess, "run") as run:
            run.return_value.returncode = 0
            platform_macos.evict_once("an arbitrary file path")
            self.assertEqual(run.call_args.args[0], [
                "/usr/bin/osascript", "-e",
                'do shell script "/usr/sbin/purge" with administrator privileges',
            ])
            self.assertNotIn("shell", run.call_args.kwargs)
            run.return_value.returncode = 1
            with self.assertRaises(RuntimeError):
                platform_macos.evict_once("binary")


if __name__ == "__main__":
    unittest.main()
