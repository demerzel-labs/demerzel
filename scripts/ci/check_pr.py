#!/usr/bin/env python3
"""Check pull request metadata fetched from the GitHub API."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any


TITLE_RE = re.compile(
    r"^(?P<type>[a-z]+)\((?P<area>[a-z0-9][a-z0-9-]*)\): (?P<desc>\S.*)$"
)
CONTROL_RE = re.compile(r"[\x00-\x1f\x7f]")
FENCE_RE = re.compile(r"^(?P<indent> {0,3})(?P<marker>`{3,}|~{3,})")
VALID_TYPES = {
    "feat",
    "fix",
    "docs",
    "style",
    "refactor",
    "perf",
    "test",
    "build",
    "ci",
    "chore",
    "revert",
}
REQUIRED_SECTIONS = ["Decisions", "Verify", "Review", "Contract growth"]
PLACEHOLDER_RE = re.compile(
    r"""
    ^\s*
    (?:
        tbd
        |todo
        |pending
        |n/a
        |none\s+yet
        |coming\s+soon
        |fill\s+in
        |replace\s+this
        |template\s+instructions?
        |write\s+(?:the|a)\s+
        |add\s+(?:details|summary|verification|review|decisions)
    )
    \s*[:.]?\s*$
    """,
    re.IGNORECASE | re.VERBOSE,
)


class Heading:
    def __init__(self, title: str, content: str) -> None:
        self.title = title
        self.content = content


def load_json(path: Path, label: str) -> Any:
    try:
        with path.open("r", encoding="utf-8") as handle:
            return json.load(handle)
    except FileNotFoundError:
        raise ValueError(f"{label} file does not exist")
    except (OSError, UnicodeDecodeError):
        raise ValueError(f"{label} file could not be read")
    except json.JSONDecodeError as exc:
        raise ValueError(f"{label} file is not valid JSON at line {exc.lineno}")


def validate_subject(subject: Any, label: str) -> list[str]:
    errors: list[str] = []
    if not isinstance(subject, str) or not subject:
        return [f"{label}: subject is missing"]
    if "\n" in subject or "\r" in subject:
        return [f"{label}: subject must be one line"]
    if CONTROL_RE.search(subject):
        return [f"{label}: subject contains a control character"]
    if subject != subject.rstrip():
        return [f"{label}: subject has trailing whitespace"]

    match = TITLE_RE.match(subject)
    if not match:
        return [
            f"{label}: subject must use '<type>(area): description' with lowercase area"
        ]

    if match.group("type") not in VALID_TYPES:
        errors.append(f"{label}: subject type is not allowed")
    if match.group("desc").rstrip().endswith("."):
        errors.append(f"{label}: description must not end with a period")
    return errors


def strip_html_comments_from_line(line: str, in_comment: bool) -> tuple[str, bool]:
    visible = ""
    rest = line
    while rest:
        if in_comment:
            end = rest.find("-->")
            if end == -1:
                return visible, True
            rest = rest[end + 3 :]
            in_comment = False
            continue

        start = rest.find("<!--")
        if start == -1:
            return visible + rest, False
        visible += rest[:start]
        rest = rest[start + 4 :]
        in_comment = True
    return visible, in_comment


def iter_visible_body_lines(text: str) -> list[tuple[int, str, bool]]:
    lines: list[tuple[int, str, bool]] = []
    fence_marker: str | None = None
    fence_len = 0
    in_comment = False
    for line_no, line in enumerate(text.splitlines(), 1):
        if fence_marker is not None:
            closer = FENCE_RE.match(line)
            if (
                closer
                and closer.group("marker")[0] == fence_marker
                and len(closer.group("marker")) >= fence_len
                and line[closer.end() :].strip() == ""
            ):
                fence_marker = None
                fence_len = 0
                continue
            lines.append((line_no, line, True))
            continue

        visible, in_comment = strip_html_comments_from_line(line, in_comment)
        opener = FENCE_RE.match(visible)
        if opener:
            fence_marker = opener.group("marker")[0]
            fence_len = len(opener.group("marker"))
            continue
        lines.append((line_no, visible, False))
    return lines


def parse_h2_sections(body: str) -> tuple[str, list[Heading]]:
    lines = iter_visible_body_lines(body)
    headings: list[tuple[int, str, int]] = []
    for index, (line_no, line, in_fence) in enumerate(lines):
        if in_fence:
            continue
        match = re.match(r"^##\s+(.+?)\s*$", line)
        if match:
            headings.append((index, match.group(1).strip(), line_no))

    if not headings:
        return "\n".join(line for _, line, _ in lines), []

    opening = "\n".join(line for _, line, _ in lines[: headings[0][0]])
    sections: list[Heading] = []
    for pos, (line_index, title, line_no) in enumerate(headings):
        next_index = headings[pos + 1][0] if pos + 1 < len(headings) else len(lines)
        content = "\n".join(line for _, line, _ in lines[line_index + 1 : next_index])
        sections.append(Heading(title=title, content=content))
    return opening, sections


def is_placeholder_line(line: str) -> bool:
    clean = line.strip()
    while True:
        original = clean
        for left, right in (("**", "**"), ("__", "__"), ("`", "`"), ("*", "*"), ("_", "_")):
            if clean.startswith(left) and clean.endswith(right) and len(clean) >= len(left) + len(right):
                clean = clean[len(left) : -len(right)].strip()
        if clean == original:
            break
    return bool(PLACEHOLDER_RE.fullmatch(clean))


def has_real_content(content: str) -> bool:
    useful_lines = []
    for line in content.splitlines():
        clean = line.strip()
        clean = re.sub(r"^(?:[-*+]|\d+[.)])\s+", "", clean)
        if not clean:
            continue
        if re.fullmatch(r"[-*_]{3,}", clean):
            continue
        if clean.startswith("[comment]:"):
            continue
        useful_lines.append(clean)

    if not useful_lines:
        return False

    if all(is_placeholder_line(line) for line in useful_lines):
        return False
    return True


def validate_body(body: Any) -> list[str]:
    if not isinstance(body, str) or not body.strip():
        return ["pull request body: body is missing"]

    opening, sections = parse_h2_sections(body)
    by_title: dict[str, Heading] = {}
    for section in sections:
        if section.title in {"Summary", *REQUIRED_SECTIONS} and section.title in by_title:
            by_title[section.title + "\0duplicate"] = section
            continue
        by_title.setdefault(section.title, section)

    errors: list[str] = []
    explicit_summary = by_title.get("Summary")
    if explicit_summary is not None:
        summary_ok = has_real_content(explicit_summary.content)
    else:
        summary_ok = has_real_content(opening)
    if not summary_ok:
        errors.append("pull request body: summary is missing or placeholder-only")

    required_order = ["Summary", *REQUIRED_SECTIONS] if explicit_summary else REQUIRED_SECTIONS
    observed_required = [
        section.title for section in sections if section.title in required_order
    ]
    expected_observed = [title for title in required_order if title in observed_required]
    if observed_required != expected_observed:
        errors.append("pull request body: required sections are out of order")

    for title in ["Summary", *REQUIRED_SECTIONS]:
        if title + "\0duplicate" in by_title:
            errors.append(f"pull request body: duplicate '## {title}' section")

    for title in REQUIRED_SECTIONS:
        section = by_title.get(title)
        if section is None:
            errors.append(f"pull request body: missing '## {title}' section")
            continue
        if not has_real_content(section.content):
            errors.append(f"pull request body: '## {title}' section has no real content")

    return errors


def flatten_commit_pages(data: Any, pr_count: Any) -> tuple[list[dict[str, Any]], list[str]]:
    errors: list[str] = []
    if not isinstance(data, list):
        return [], ["commits file: expected an array of pages"]
    if not data:
        return [], ["commits file: no commit pages returned"]

    commits: list[dict[str, Any]] = []
    for page_index, page in enumerate(data, 1):
        if not isinstance(page, list):
            errors.append(f"commits file: page {page_index} is not an array")
            continue
        if not page:
            errors.append(f"commits file: page {page_index} is empty")
            continue
        for commit_index, item in enumerate(page, 1):
            if not isinstance(item, dict):
                errors.append(
                    f"commits file: page {page_index} item {commit_index} is not an object"
                )
                continue
            commits.append(item)

    if type(pr_count) is not int or pr_count < 0:
        errors.append("pull request JSON: commits count is missing")
    elif len(commits) != pr_count:
        errors.append(
            f"commits file: expected {pr_count} commits, found {len(commits)}"
        )
    if not commits:
        errors.append("commits file: no commits returned")
    return commits, errors


def commit_sha(commit: dict[str, Any]) -> Any:
    return commit.get("sha")


def commit_subject(commit: dict[str, Any]) -> Any:
    payload = commit.get("commit")
    if not isinstance(payload, dict):
        return None
    message = payload.get("message")
    if not isinstance(message, str) or not message:
        return None
    return message.splitlines()[0]


def validate_commits(pr: dict[str, Any], commits_data: Any) -> list[str]:
    commits, errors = flatten_commit_pages(commits_data, pr.get("commits"))
    head = pr.get("head")
    expected_head = head.get("sha") if isinstance(head, dict) else None
    if not isinstance(expected_head, str) or not expected_head:
        errors.append("pull request JSON: head SHA is missing")

    if commits and isinstance(expected_head, str):
        final_sha = commit_sha(commits[-1])
        if final_sha != expected_head:
            errors.append("commits file: final commit does not match pull request head")

    for index, commit in enumerate(commits, 1):
        sha = commit_sha(commit)
        label = f"commit {index}"
        if isinstance(sha, str) and re.fullmatch(r"[0-9a-fA-F]{7,40}", sha):
            label = f"commit {index} ({sha[:7].lower()})"
        else:
            errors.append(f"{label}: SHA is missing")
        errors.extend(validate_subject(commit_subject(commit), label))
    return errors


def validate_pr(pr: Any, commits_data: Any) -> list[str]:
    if not isinstance(pr, dict):
        return ["pull request JSON: expected an object"]

    errors: list[str] = []
    errors.extend(validate_subject(pr.get("title"), "pull request title"))
    errors.extend(validate_body(pr.get("body")))
    errors.extend(validate_commits(pr, commits_data))
    return errors


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Validate pull request metadata from GitHub API JSON files."
    )
    parser.add_argument("--pr", required=True, type=Path, help="Path to PR JSON.")
    parser.add_argument(
        "--commits",
        required=True,
        type=Path,
        help="Path to paginated commits JSON from gh --slurp.",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    errors: list[str] = []

    try:
        pr = load_json(args.pr, "pull request JSON")
    except ValueError as exc:
        errors.append(str(exc))
        pr = None

    try:
        commits = load_json(args.commits, "commits JSON")
    except ValueError as exc:
        errors.append(str(exc))
        commits = None

    if pr is None and not any(error.startswith("pull request JSON") for error in errors):
        errors.append("pull request JSON: expected an object")
    if commits is None and not any(error.startswith("commits JSON") for error in errors):
        errors.append("commits file: expected an array of pages")
    if pr is not None and commits is not None:
        errors.extend(validate_pr(pr, commits))

    if errors:
        print("PR metadata check failed:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1

    print("PR metadata check passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
