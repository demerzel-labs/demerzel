from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("check_pr.py")
SPEC = importlib.util.spec_from_file_location("check_pr", MODULE_PATH)
assert SPEC is not None
check_pr = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules["check_pr"] = check_pr
SPEC.loader.exec_module(check_pr)


GOOD_BODY_OPENING = """This PR adds the repository metadata check.

## Decisions

- Check only PR metadata fetched by the workflow.

## Verify

- Ran `python scripts/ci/check_pr.py`.

## Review

- Checked parser edge cases.

## Contract growth

Contract growth: none
"""


GOOD_BODY_EXPLICIT = """## Summary

This PR adds the repository metadata check.

## Decisions

- Check only PR metadata fetched by the workflow.

## Verify

- Ran `python scripts/ci/check_pr.py`.

## Review

- Checked parser edge cases.

## Contract growth

Contract growth: none
"""


def pr_json(**overrides):
    pr = {
        "title": "ci(repo): add PR metadata checks",
        "body": GOOD_BODY_OPENING,
        "commits": 2,
        "head": {"sha": "bbbbbbb"},
    }
    pr.update(overrides)
    return pr


def commit_json(subject: str, sha: str = "aaaaaaa", body: str = ""):
    message = subject if not body else f"{subject}\n\n{body}"
    return {"sha": sha, "commit": {"message": message}}


def commits_json(*commits):
    return [list(commits)]


class ValidateSubjectTests(unittest.TestCase):
    def test_valid_opening_summary(self):
        errors = check_pr.validate_pr(
            pr_json(),
            commits_json(
                commit_json("ci(repo): add PR metadata checks", "aaaaaaa"),
                commit_json("test(repo): cover PR metadata checks", "bbbbbbb"),
            ),
        )
        self.assertEqual(errors, [])

    def test_valid_explicit_summary(self):
        errors = check_pr.validate_pr(
            pr_json(body=GOOD_BODY_EXPLICIT, commits=1),
            commits_json(commit_json("ci(repo): add PR metadata checks", "bbbbbbb")),
        )
        self.assertEqual(errors, [])

    def test_bad_title_type_scope_and_shape(self):
        cases = [
            "feature(repo): add PR metadata checks",
            "ci(Repo): add PR metadata checks",
            "ci(): add PR metadata checks",
            "ci(repo) add PR metadata checks",
            "ci(repo): add PR metadata checks.",
            "ci(repo): add PR metadata checks ",
            "ci(repo): add PR metadata checks\x1b",
            "ci(repo): add\tPR metadata checks",
        ]
        for title in cases:
            with self.subTest(title=title):
                errors = check_pr.validate_pr(
                    pr_json(title=title, commits=1),
                    commits_json(commit_json("ci(repo): add PR metadata checks", "bbbbbbb")),
                )
                self.assertTrue(errors)

    def test_each_commit_subject_is_checked(self):
        errors = check_pr.validate_pr(
            pr_json(commits=2),
            commits_json(
                commit_json("ci(repo): add PR metadata checks", "aaaaaaa"),
                commit_json("Fix repo metadata", "bbbbbbb"),
            ),
        )
        self.assertTrue(errors)

    def test_multiline_commit_message_body_is_allowed(self):
        errors = check_pr.validate_pr(
            pr_json(commits=1),
            commits_json(
                commit_json(
                    "ci(repo): add PR metadata checks",
                    "bbbbbbb",
                    body="The body can include more detail.\n\nIt is not the subject.",
                )
            ),
        )
        self.assertEqual(errors, [])


class ValidateBodyTests(unittest.TestCase):
    def test_missing_body_sections(self):
        for heading in ("Decisions", "Verify", "Review", "Contract growth"):
            with self.subTest(heading=heading):
                body = GOOD_BODY_OPENING.replace(
                    f"## {heading}\n", f"## Optional {heading}\n"
                )
                self.assertTrue(check_pr.validate_body(body))

    def test_body_sections_must_be_in_order(self):
        body = """Summary text.

## Verify

- Ran tests.

## Decisions

- Keep the checker small.

## Review

- Reviewed the diff.

## Contract growth

Contract growth: none
"""
        errors = check_pr.validate_body(body)
        self.assertTrue(errors)

    def test_placeholder_content_is_rejected(self):
        body = """Summary text.

## Decisions

TODO

## Verify

- Ran tests.

## Review

- Reviewed the diff.

## Contract growth

Contract growth: none
"""
        errors = check_pr.validate_body(body)
        self.assertTrue(errors)

    def test_standalone_bold_placeholder_is_rejected(self):
        body = """Summary text.

## Decisions

**TODO**

## Verify

- Ran tests.

## Review

- Reviewed the diff.

## Contract growth

Contract growth: none
"""
        errors = check_pr.validate_body(body)
        self.assertTrue(errors)

    def test_pending_with_real_result_is_not_placeholder(self):
        body = """Summary text.

## Decisions

Pending CI; local test passed.

## Verify

- Ran tests.

## Review

- Reviewed the diff.

## Contract growth

Contract growth: none
"""
        errors = check_pr.validate_body(body)
        self.assertEqual(errors, [])

    def test_bulleted_placeholder_content_is_rejected(self):
        body = """Summary text.

## Decisions

- TODO

## Verify

- Ran tests.

## Review

- Reviewed the diff.

## Contract growth

Contract growth: none
"""
        errors = check_pr.validate_body(body)
        self.assertTrue(errors)

    def test_fenced_and_commented_fake_headings_do_not_count(self):
        body = """Summary text.

```markdown
## Decisions

Fake
```

<!--
## Verify
Fake
-->

## Decisions

- Keep the checker small.

## Review

- Reviewed the diff.

## Contract growth

Contract growth: none
"""
        errors = check_pr.validate_body(body)
        self.assertTrue(errors)
        complete_body = body.replace(
            "## Review\n", "## Verify\n\n- Ran tests.\n\n## Review\n"
        )
        self.assertEqual(check_pr.validate_body(complete_body), [])

    def test_tilde_fenced_fake_headings_do_not_count(self):
        body = """Summary text.

~~~markdown
## Decisions
Fake
~~~

## Verify

- Ran tests.

## Review

- Reviewed the diff.

## Contract growth

Contract growth: none
"""
        errors = check_pr.validate_body(body)
        self.assertTrue(errors)

    def test_real_fenced_verify_content_counts(self):
        body = """Summary text.

## Decisions

- Keep the checker small.

## Verify

```text
$ python scripts/ci/check_pr.py --pr pr.json --commits commits.json
PR metadata check passed.
```

## Review

- Reviewed the diff.

## Contract growth

Contract growth: none
"""
        errors = check_pr.validate_body(body)
        self.assertEqual(errors, [])

    def test_fenced_heading_text_counts_as_content_only(self):
        body = """Summary text.

## Decisions

- Keep the checker small.

## Verify

```
## Fake evidence
```

## Review

- Reviewed the diff.

## Contract growth

Contract growth: none
"""
        errors = check_pr.validate_body(body)
        self.assertEqual(errors, [])

    def test_fenced_heading_text_does_not_satisfy_missing_heading(self):
        body = """Summary text.

## Decisions

- Keep the checker small.

## Verify

```
## Review

- Fake fenced review.
```

## Contract growth

Contract growth: none
"""
        errors = check_pr.validate_body(body)
        self.assertTrue(errors)

    def test_fenced_shell_comment_counts_as_content(self):
        body = """Summary text.

## Decisions

- Keep the checker small.

## Verify

```text
# cargo test --workspace passed
```

## Review

- Reviewed the diff.

## Contract growth

Contract growth: none
"""
        errors = check_pr.validate_body(body)
        self.assertEqual(errors, [])

    def test_html_comment_inside_fence_counts_as_content(self):
        body = """Summary text.

## Decisions

- Keep the checker small.

## Verify

```html
<!-- command output stayed literal -->
```

## Review

- Reviewed the diff.

## Contract growth

Contract growth: none
"""
        errors = check_pr.validate_body(body)
        self.assertEqual(errors, [])

    def test_duplicate_required_heading_fails(self):
        body = """Summary text.

## Decisions

- Keep the checker small.

## Verify

- Ran tests.

## Verify

- Ran more tests.

## Review

- Reviewed the diff.

## Contract growth

Contract growth: none
"""
        errors = check_pr.validate_body(body)
        self.assertTrue(errors)

    def test_backtick_closer_requires_spaces_after_marker(self):
        body = """Summary text.

## Decisions

- Keep the checker small.

## Verify

```
```not a closer
$ python scripts/ci/check_pr.py --pr pr.json --commits commits.json
## Review

- Fake fenced review.
```

## Review

- Reviewed the diff.

## Contract growth

Contract growth: none
"""
        errors = check_pr.validate_body(body)
        self.assertEqual(errors, [])

    def test_unclosed_html_comment_hides_through_eof(self):
        body = """Summary text.

## Decisions

- Keep the checker small.

<!--
## Verify

- Fake hidden verification.

## Review

- Fake hidden review.

## Contract growth

Contract growth: none
"""
        errors = check_pr.validate_body(body)
        self.assertTrue(errors)


class CommitListTests(unittest.TestCase):
    def test_empty_api_list_fails(self):
        errors = check_pr.validate_pr(pr_json(commits=1), [])
        self.assertTrue(errors)

    def test_empty_page_fails(self):
        errors = check_pr.validate_pr(
            pr_json(commits=1),
            [[], [commit_json("ci(repo): add PR metadata checks", "bbbbbbb")]],
        )
        self.assertTrue(errors)

    def test_mismatched_commit_count_fails(self):
        errors = check_pr.validate_pr(
            pr_json(commits=2),
            commits_json(commit_json("ci(repo): add PR metadata checks", "bbbbbbb")),
        )
        self.assertTrue(errors)

    def test_final_commit_must_match_head_sha(self):
        errors = check_pr.validate_pr(
            pr_json(commits=1, head={"sha": "ccccccc"}),
            commits_json(commit_json("ci(repo): add PR metadata checks", "bbbbbbb")),
        )
        self.assertTrue(errors)

    def test_commit_sha_is_required(self):
        item = {"commit": {"message": "ci(repo): add PR metadata checks"}}
        errors = check_pr.validate_pr(
            pr_json(commits=2, head={"sha": "bbbbbbb"}),
            commits_json(
                item, commit_json("ci(repo): add PR metadata checks", "bbbbbbb")
            ),
        )
        self.assertTrue(errors)

    def test_bool_commit_count_is_rejected(self):
        errors = check_pr.validate_pr(
            pr_json(commits=True),
            commits_json(commit_json("ci(repo): add PR metadata checks", "bbbbbbb")),
        )
        self.assertTrue(errors)


class CliTests(unittest.TestCase):
    def run_cli(self, pr, commits):
        with tempfile.TemporaryDirectory() as tmp:
            pr_path = Path(tmp) / "pr.json"
            commits_path = Path(tmp) / "commits.json"
            pr_path.write_text(json.dumps(pr), encoding="utf-8")
            commits_path.write_text(json.dumps(commits), encoding="utf-8")
            stdout = io.StringIO()
            stderr = io.StringIO()
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                code = check_pr.main(["--pr", str(pr_path), "--commits", str(commits_path)])
            return code, stdout.getvalue(), stderr.getvalue()

    def test_cli_passes_valid_input(self):
        code, _stdout, stderr = self.run_cli(
            pr_json(commits=1),
            commits_json(commit_json("ci(repo): add PR metadata checks", "bbbbbbb")),
        )
        self.assertEqual(code, 0)
        self.assertEqual(stderr, "")

    def test_hostile_text_is_inert_and_not_echoed(self):
        hostile = "ci(repo): $(echo secret)"
        code, _stdout, stderr = self.run_cli(
            pr_json(title=hostile, commits=1),
            commits_json(commit_json("ci(repo): add PR metadata checks", "bbbbbbb")),
        )
        self.assertEqual(code, 0)
        self.assertNotIn("$(echo secret)", stderr)

    def test_json_null_fails_closed(self):
        code, _stdout, stderr = self.run_cli(
            None,
            commits_json(commit_json("ci(repo): add PR metadata checks", "bbbbbbb")),
        )
        self.assertEqual(code, 1)
        self.assertTrue(stderr.strip())

        code, _stdout, stderr = self.run_cli(pr_json(commits=1), None)
        self.assertEqual(code, 1)
        self.assertTrue(stderr.strip())

    def test_bad_utf8_is_sanitized(self):
        with tempfile.TemporaryDirectory() as tmp:
            pr_path = Path(tmp) / "pr.json"
            commits_path = Path(tmp) / "commits.json"
            pr_path.write_bytes(b"\xff")
            commits_path.write_text(
                json.dumps(
                    commits_json(
                        commit_json("ci(repo): add PR metadata checks", "bbbbbbb")
                    )
                ),
                encoding="utf-8",
            )
            stdout = io.StringIO()
            stderr = io.StringIO()
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                code = check_pr.main(["--pr", str(pr_path), "--commits", str(commits_path)])
            self.assertEqual(code, 1)
            stderr_text = stderr.getvalue()
            self.assertTrue(stderr_text.strip())
            self.assertNotIn("\xff", stderr_text)

    def test_os_error_is_sanitized(self):
        with tempfile.TemporaryDirectory() as tmp:
            commits_path = Path(tmp) / "commits.json"
            commits_path.write_text(
                json.dumps(
                    commits_json(
                        commit_json("ci(repo): add PR metadata checks", "bbbbbbb")
                    )
                ),
                encoding="utf-8",
            )
            stdout = io.StringIO()
            stderr = io.StringIO()
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                code = check_pr.main(["--pr", tmp, "--commits", str(commits_path)])
            self.assertEqual(code, 1)
            self.assertTrue(stderr.getvalue().strip())


if __name__ == "__main__":
    unittest.main()
