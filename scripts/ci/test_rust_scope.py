from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("rust_scope.py").resolve()


class RustScopeCliTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.repo = Path(temporary.name)
        self.git("init", "--quiet", "--initial-branch=main")
        self.git("config", "user.name", "CI test")
        self.git("config", "user.email", "ci@example.invalid")
        self.base = self.commit({})

    def git(self, *args):
        return subprocess.run(
            [
                "git",
                "-c", "core.hooksPath=/dev/null",
                "-c", "commit.gpgsign=false",
                *args,
            ],
            cwd=self.repo,
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()

    def commit(self, files):
        for name, content in files.items():
            path = self.repo / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content, encoding="utf-8")
        self.git("add", "--all")
        self.git("commit", "--quiet", "--allow-empty", "-m", "Test change")
        return self.git("rev-parse", "HEAD")

    def run_cli(self, *args):
        return subprocess.run(
            [sys.executable, str(SCRIPT), *args],
            cwd=self.repo,
            capture_output=True,
            text=True,
        )

    def assert_scope(self, base, head, requires_rust):
        result = self.run_cli(base, head)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), f"rust={str(requires_rust).lower()}")

    def test_known_documentation_only_skips_rust(self):
        paths = (
            "README.md",
            "AGENTS.md",
            "CLAUDE.md",
            "bench/README.md",
            "docs/assets/readme-header.png",
            "docs/bench-platforms.md",
            "docs/ci.md",
            "docs/contributing.md",
            "docs/history-commit-map.tsv",
            "docs/history.md",
            "docs/m1-plan.md",
            "docs/m2-plan.md",
            "docs/m2.5-plan.md",
            "docs/m3-plan.md",
            "docs/plan.md",
            "docs/prior-art.md",
        )
        head = self.commit(dict.fromkeys(paths, "Documentation\n"))
        self.assert_scope(self.base, head, False)

    def test_code_config_ci_and_unknown_paths_require_rust(self):
        paths = (
            "crates/demerzel/src/main.rs",
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            "docs/config.md",
            ".github/workflows/ci.yml",
            "scripts/ci/rust_scope.py",
            "docs/new-guide.md",
            "unknown.txt",
        )
        for path in paths:
            with self.subTest(path=path):
                base = self.git("rev-parse", "HEAD")
                head = self.commit({path: "Changed\n"})
                self.assert_scope(base, head, True)

    def test_mixed_documentation_and_code_requires_rust(self):
        head = self.commit({"README.md": "Docs\n", "src/lib.rs": "Code\n"})
        self.assert_scope(self.base, head, True)

    def test_earlier_code_commit_still_requires_rust(self):
        self.commit({"src/lib.rs": "Code\n"})
        head = self.commit({"README.md": "Docs\n"})
        self.assert_scope(self.base, head, True)

    def test_unrelated_base_branch_code_does_not_require_rust(self):
        self.git("switch", "--quiet", "-c", "docs")
        head = self.commit({"README.md": "Docs\n"})
        self.git("switch", "--quiet", "main")
        base = self.commit({"src/lib.rs": "Unrelated code\n"})
        self.assert_scope(base, head, False)

    def test_renaming_code_to_documentation_requires_rust(self):
        base = self.commit({"source.rs": "Code\n"})
        self.git("config", "diff.renames", "true")
        self.git("mv", "source.rs", "README.md")
        head = self.commit({})
        self.assert_scope(base, head, True)

    def test_deleting_code_requires_rust(self):
        base = self.commit({"source.rs": "Code\n"})
        self.git("rm", "source.rs")
        head = self.commit({})
        self.assert_scope(base, head, True)

    def test_newline_in_filename_does_not_become_two_allowed_paths(self):
        head = self.commit({"README.md\nAGENTS.md": "Unknown file\n"})
        self.assert_scope(self.base, head, True)

    def test_empty_diff_requires_rust(self):
        head = self.commit({})
        self.assert_scope(self.base, head, True)

    def test_invalid_or_missing_base_fails(self):
        for args in (("missing-base", self.base), (self.base,)):
            with self.subTest(args=args):
                result = self.run_cli(*args)
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("rust=false", result.stdout)


if __name__ == "__main__":
    unittest.main()
