from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("evidence.py")
SPEC = importlib.util.spec_from_file_location("evidence", MODULE_PATH)
assert SPEC is not None
evidence = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules["evidence"] = evidence
SPEC.loader.exec_module(evidence)


class EvidenceCliTests(unittest.TestCase):
    def run_cli(self, *args: str, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
        command = [sys.executable, str(MODULE_PATH), *args]
        merged_env = os.environ.copy()
        if env:
            merged_env.update(env)
        return subprocess.run(command, text=True, capture_output=True, env=merged_env, check=False)

    def test_run_propagates_exit_and_keeps_stdout_stderr_logs(self):
        with tempfile.TemporaryDirectory() as tmp:
            result = self.run_cli(
                "run",
                "--directory",
                tmp,
                "--label",
                "unit-fail",
                "--",
                sys.executable,
                "-c",
                "import sys; print('out-line'); print('err-line', file=sys.stderr); sys.exit(3)",
            )
            self.assertEqual(result.returncode, 3)
            self.assertIn("out-line", result.stdout)
            self.assertIn("err-line", result.stderr)

            base = Path(tmp)
            self.assertEqual((base / "logs" / "unit-fail.stdout.log").read_text(), "out-line\n")
            self.assertEqual((base / "logs" / "unit-fail.stderr.log").read_text(), "err-line\n")
            self.assertFalse((base / "logs" / "unit-fail.log").exists())
            data = json.loads((base / "results" / "unit-fail.json").read_text())
            self.assertFalse(data["ok"])
            self.assertEqual(data["exit_code"], 3)
            self.assertEqual(data["command"][:2], [sys.executable, "-c"])
            self.assertIn("git_head", data["metadata"])
            self.assertNotIn("env", data)

    def test_run_parses_cargo_test_result_lines_without_claiming_coverage(self):
        with tempfile.TemporaryDirectory() as tmp:
            sample = (
                "test result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s\n"
                "test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.02s\n"
            )
            result = self.run_cli(
                "run",
                "--directory",
                tmp,
                "--label",
                "cargo-test",
                "--",
                sys.executable,
                "-c",
                f"print({sample!r}, end='')",
            )
            self.assertEqual(result.returncode, 0)
            data = json.loads((Path(tmp) / "results" / "cargo-test.json").read_text())
            self.assertEqual(data["cargo_test"]["counts"]["passed"], 5)
            self.assertEqual(data["cargo_test"]["counts"]["ignored"], 1)
            self.assertEqual(data["cargo_test"]["counts"]["filtered_out"], 1)
            self.assertIn("not a coverage measure", data["cargo_test"]["note"])

    def test_summary_with_no_results_is_not_a_false_pass(self):
        with tempfile.TemporaryDirectory() as tmp:
            result = self.run_cli("summary", "--directory", tmp, "--expected", "fmt", "test")
            self.assertEqual(result.returncode, 0)
            self.assertIn("Overall: **incomplete**", result.stdout)
            self.assertIn("Missing expected checks: `fmt, test`", result.stdout)
            aggregate = json.loads((Path(tmp) / "evidence.json").read_text())
            self.assertEqual(aggregate["overall"], "incomplete")
            self.assertEqual(aggregate["missing_expected"], ["fmt", "test"])

    def test_summary_without_expected_labels_says_recorded_pass_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            results = Path(tmp) / "results"
            results.mkdir()
            (results / "ok.json").write_text(
                json.dumps(
                    {
                        "label": "ok",
                        "ok": True,
                        "exit_code": 0,
                        "runtime_seconds": 0.1,
                        "command": ["python", "-c", "print('ok')"],
                        "logs": {"stdout": "logs/ok.stdout.log", "stderr": "logs/ok.stderr.log"},
                        "metadata": {"git_head": "abc", "runner_os": "Linux", "runner_arch": "X64"},
                    }
                ),
                encoding="utf-8",
            )
            result = self.run_cli("summary", "--directory", tmp)
            self.assertEqual(result.returncode, 0)
            self.assertIn("Overall: **recorded pass**", result.stdout)
            self.assertIn("limited to recorded checks", result.stdout)
            aggregate = json.loads((Path(tmp) / "evidence.json").read_text())
            self.assertEqual(aggregate["overall"], "recorded_pass")

    def test_summary_lists_failed_checks_and_escapes_markdown_table_cells(self):
        with tempfile.TemporaryDirectory() as tmp:
            results = Path(tmp) / "results"
            results.mkdir()
            (results / "bad.json").write_text(
                json.dumps(
                    {
                        "label": "bad",
                        "ok": False,
                        "exit_code": 1,
                        "runtime_seconds": 0.5,
                        "command": ["python", "-c", "print('a|b`<tag>')"],
                        "logs": {"stdout": "logs/bad.stdout.log"},
                        "metadata": {"git_head": "abc", "runner_os": "Linux", "runner_arch": "X64"},
                    }
                ),
                encoding="utf-8",
            )
            result = self.run_cli("summary", "--directory", tmp, "--expected", "bad")
            self.assertEqual(result.returncode, 0)
            self.assertIn("Overall: **fail**", result.stdout)
            self.assertIn("a\\|b&#96;&lt;tag&gt;", result.stdout)
            self.assertIn("| bad | fail | 1 | 0.5s |", result.stdout)
            self.assertEqual((Path(tmp) / "summary.md").read_text(), result.stdout)

    def test_summary_marks_malformed_results_incomplete(self):
        with tempfile.TemporaryDirectory() as tmp:
            results = Path(tmp) / "results"
            results.mkdir()
            (results / "bad.json").write_text("{", encoding="utf-8")
            result = self.run_cli("summary", "--directory", tmp)
            self.assertEqual(result.returncode, 0)
            self.assertIn("Overall: **incomplete**", result.stdout)
            self.assertIn("Malformed result files: `results/bad.json`", result.stdout)
            aggregate = json.loads((Path(tmp) / "evidence.json").read_text())
            self.assertEqual(aggregate["malformed_results"], ["results/bad.json"])

    def test_label_rejects_traversal_and_path_separators(self):
        with tempfile.TemporaryDirectory() as tmp:
            result = self.run_cli(
                "run",
                "--directory",
                tmp,
                "--label",
                "../outside",
                "--",
                sys.executable,
                "-c",
                "print('should not run')",
            )
            self.assertEqual(result.returncode, 2)
            self.assertIn("traversal", result.stderr)
            self.assertFalse((Path(tmp).parent / "outside.log").exists())

    def test_summary_rejects_missing_or_inconsistent_status_fields(self):
        for fields in ({"ok": "false", "exit_code": 1}, {"ok": True}, {"ok": True, "exit_code": 1}):
            with self.subTest(fields=fields), tempfile.TemporaryDirectory() as tmp:
                results = Path(tmp) / "results"
                results.mkdir()
                (results / "check.json").write_text(json.dumps({"label": "check", **fields}))
                result = self.run_cli("summary", "--directory", tmp, "--expected", "check")
                self.assertEqual(result.returncode, 0)
                aggregate = json.loads((Path(tmp) / "evidence.json").read_text())
                self.assertEqual(aggregate["overall"], "incomplete")
                self.assertEqual(aggregate["missing_expected"], ["check"])

    def test_github_actions_stream_disables_workflow_commands_but_keeps_raw_log(self):
        with tempfile.TemporaryDirectory() as tmp:
            result = self.run_cli(
                "run",
                "--directory",
                tmp,
                "--label",
                "gha",
                "--",
                sys.executable,
                "-c",
                "print('::error::raw child text')",
                env={"GITHUB_ACTIONS": "true"},
            )
            self.assertEqual(result.returncode, 0)
            self.assertIn("::stop-commands::ci-evidence-", result.stdout)
            self.assertIn("::error::raw child text", result.stdout)
            self.assertLess(
                result.stdout.index("::stop-commands::ci-evidence-"),
                result.stdout.index("::error::raw child text"),
            )
            self.assertRegex(result.stdout, r"\n::ci-evidence-[0-9a-f]+::")
            self.assertIn("::error::raw child text", (Path(tmp) / "logs" / "gha.stdout.log").read_text())


class HelperTests(unittest.TestCase):
    def test_md_escapes_table_and_html_control_characters(self):
        self.assertEqual(evidence.md("a|b\\c`<x>\nd"), "a\\|b\\\\c&#96;&lt;x&gt;<br>d")

    def test_fenced_text_breaks_nested_fences(self):
        self.assertEqual(evidence.fenced_text("a```b"), "a`\u200b``b")

    def test_cargo_result_tails_are_independent_per_stream(self):
        parser = evidence.CargoResults()
        parser.feed("stdout", b"test result: ok. 2 passed; 0 failed;")
        parser.feed(
            "stderr",
            b"test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n",
        )
        parser.feed("stdout", b" 3 ignored; 0 measured; 0 filtered out\n")
        parsed = parser.finish()
        self.assertIsNotNone(parsed)
        assert parsed is not None
        self.assertEqual(parsed["counts"]["passed"], 3)
        self.assertEqual(parsed["counts"]["ignored"], 3)
        self.assertEqual(len(parsed["result_lines"]), 2)

    def test_summary_uses_recorded_metadata_without_spawning_fallback_commands(self):
        old_metadata = evidence.metadata
        try:
            evidence.metadata = lambda: self.fail("metadata fallback should not run")
            text, status, missing = evidence.make_summary(
                [
                    {
                        "label": "fmt",
                        "ok": True,
                        "exit_code": 0,
                        "runtime_seconds": 0.1,
                        "command": ["cargo", "fmt", "--check"],
                        "logs": {},
                        "metadata": {"git_head": "recorded", "runner_os": "Linux", "runner_arch": "X64"},
                    }
                ],
                [],
                ["fmt"],
                None,
                None,
            )
        finally:
            evidence.metadata = old_metadata
        self.assertEqual(status, "pass")
        self.assertEqual(missing, [])
        self.assertIn("`recorded`", text)

    def test_stream_failure_preserves_captured_log_and_reaps_child(self):
        class BrokenBuffer:
            def write(self, chunk):
                if b"known chunk" in chunk:
                    raise OSError("stdout sink failed")

            def flush(self):
                pass

        class BrokenStdout:
            buffer = BrokenBuffer()

        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            pid_path = base / "pid"
            stdout_log = base / "stdout.log"
            stderr_log = base / "stderr.log"
            old_stdout = evidence.sys.stdout
            try:
                evidence.sys.stdout = BrokenStdout()
                with self.assertRaises(OSError):
                    evidence.stream(
                        [
                            sys.executable,
                            "-c",
                            (
                                "import os, sys, time; "
                                "open(sys.argv[1], 'w').write(str(os.getpid())); "
                                "sys.stdout.write('known chunk\\n'); sys.stdout.flush(); "
                                "time.sleep(30)"
                            ),
                            str(pid_path),
                        ],
                        stdout_log,
                        stderr_log,
                        evidence.CargoResults(),
                    )
            finally:
                evidence.sys.stdout = old_stdout

            self.assertEqual(stdout_log.read_text(), "known chunk\n")
            self.assertTrue(pid_path.exists())
            pid = int(pid_path.read_text())
            with self.assertRaises(ProcessLookupError):
                os.kill(pid, 0)

    def test_contract_diff_stat_is_read_only_and_bounded_to_contract_paths(self):
        paths = " ".join(evidence.CONTRACT_PATHS)
        self.assertIn("crates/demerzel-core/src/provider.rs", paths)
        self.assertIn("crates/demerzel-tools/src", paths)
        self.assertIn("crates/demerzel/src/main.rs", paths)


if __name__ == "__main__":
    unittest.main()
