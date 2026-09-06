"""Check the evidence required for verified macOS file eviction."""

import unittest

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


if __name__ == "__main__":
    unittest.main()
