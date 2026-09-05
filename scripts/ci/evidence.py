#!/usr/bin/env python3
"""Record CI command evidence and summarize recorded results."""

from __future__ import annotations

import argparse
import html
import json
import os
import platform
import re
import selectors
import subprocess
import sys
import time
import uuid
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

LABEL_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,79}$")
CARGO_RE = re.compile(
    r"test result: (?P<status>ok|FAILED)\. "
    r"(?P<passed>\d+) passed; (?P<failed>\d+) failed; "
    r"(?P<ignored>\d+) ignored; (?P<measured>\d+) measured; "
    r"(?P<filtered_out>\d+) filtered out"
)
CONTRACT_PATHS = [
    "crates/demerzel-core/src/provider.rs",
    "crates/demerzel-core/src/engine.rs",
    "crates/demerzel-core/src/tool.rs",
    "crates/demerzel-core/src/session.rs",
    "crates/demerzel-tools/src",
    "crates/demerzel/src/config.rs",
    "crates/demerzel/src/main.rs",
    "crates/demerzel/src/commands.rs",
]


def safe_label(label: str) -> str:
    if not LABEL_RE.fullmatch(label):
        raise ValueError(
            "label must match [A-Za-z0-9][A-Za-z0-9_.-]{0,79}; "
            "path separators and traversal are not allowed"
        )
    return label


def now() -> str:
    return datetime.now(UTC).isoformat(timespec="seconds").replace("+00:00", "Z")


def run_text(argv: list[str]) -> str | None:
    try:
        done = subprocess.run(argv, text=True, capture_output=True, timeout=10, check=False)
    except (OSError, subprocess.TimeoutExpired):
        return None
    return done.stdout.strip() if done.returncode == 0 else None


def pr_event(path: str | None) -> dict[str, Any]:
    if not path:
        return {}
    try:
        data = json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}
    pr = data.get("pull_request") if isinstance(data, dict) else None
    if not isinstance(pr, dict):
        return {}
    head = pr.get("head") if isinstance(pr.get("head"), dict) else {}
    base = pr.get("base") if isinstance(pr.get("base"), dict) else {}
    return {
        "pull_request": {
            "head": {"sha": head.get("sha"), "ref": head.get("ref")},
            "base": {"sha": base.get("sha"), "ref": base.get("ref")},
        }
    }


def metadata() -> dict[str, Any]:
    env = os.environ
    data = {
        "git_head": run_text(["git", "rev-parse", "HEAD"]),
        "github_sha": env.get("GITHUB_SHA"),
        "github_ref": env.get("GITHUB_REF"),
        "github_run_id": env.get("GITHUB_RUN_ID"),
        "github_run_attempt": env.get("GITHUB_RUN_ATTEMPT"),
        "github_workflow": env.get("GITHUB_WORKFLOW"),
        "github_job": env.get("GITHUB_JOB"),
        "github_repository": env.get("GITHUB_REPOSITORY"),
        "runner_os": env.get("RUNNER_OS") or platform.system(),
        "runner_arch": env.get("RUNNER_ARCH") or platform.machine(),
        "rustc": run_text(["rustc", "-Vv"]),
    }
    data.update(pr_event(env.get("GITHUB_EVENT_PATH")))
    return data


class CargoResults:
    def __init__(self) -> None:
        self.counts = dict.fromkeys(["passed", "failed", "ignored", "measured", "filtered_out"], 0)
        self.lines: list[dict[str, Any]] = []
        self.tail = {"stdout": "", "stderr": ""}

    def feed(self, stream_name: str, chunk: bytes) -> None:
        text = self.tail[stream_name] + chunk.decode("utf-8", errors="replace")
        lines = text.splitlines(keepends=True)
        self.tail[stream_name] = ""
        if lines and not lines[-1].endswith(("\n", "\r")):
            self.tail[stream_name] = lines.pop()[-4096:]
        for line in lines:
            self.parse(line)

    def parse(self, line: str) -> None:
        match = CARGO_RE.search(line)
        if not match:
            return
        parsed: dict[str, Any] = {"status": match.group("status")}
        for key in self.counts:
            value = int(match.group(key))
            self.counts[key] += value
            parsed[key] = value
        self.lines.append(parsed)

    def finish(self) -> dict[str, Any] | None:
        for tail in self.tail.values():
            if tail:
                self.parse(tail)
        if not self.lines:
            return None
        return {
            "note": "Parsed cargo test result lines only; this is not a coverage measure.",
            "result_lines": self.lines,
            "counts": self.counts,
        }


class CommandStartError(OSError):
    pass


def write_json(path: Path, data: dict[str, Any]) -> None:
    path.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def gha_guard_start() -> str | None:
    if os.environ.get("GITHUB_ACTIONS") != "true":
        return None
    token = f"ci-evidence-{uuid.uuid4().hex}"
    print(f"::stop-commands::{token}", flush=True)
    return token


def gha_guard_end(token: str | None) -> None:
    if token:
        sys.stdout.write("\n")
        sys.stdout.flush()
        print(f"::{token}::", flush=True)


def stop_process(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()


def stream(argv: list[str], stdout_log: Path, stderr_log: Path, cargo: CargoResults) -> int:
    selector = selectors.DefaultSelector()
    try:
        process = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, shell=False)
    except OSError as exc:
        selector.close()
        raise CommandStartError(str(exc)) from exc

    try:
        assert process.stdout is not None and process.stderr is not None
        selector.register(process.stdout, selectors.EVENT_READ, "stdout")
        selector.register(process.stderr, selectors.EVENT_READ, "stderr")

        with stdout_log.open("wb") as out_file, stderr_log.open("wb") as err_file:
            while selector.get_map():
                for key, _ in selector.select():
                    stream_name = key.data
                    chunk = os.read(key.fileobj.fileno(), 8192)
                    if not chunk:
                        selector.unregister(key.fileobj)
                        continue
                    cargo.feed(stream_name, chunk)
                    if stream_name == "stdout":
                        out_file.write(chunk)
                        out_file.flush()
                        sys.stdout.buffer.write(chunk)
                        sys.stdout.buffer.flush()
                    else:
                        err_file.write(chunk)
                        err_file.flush()
                        sys.stderr.buffer.write(chunk)
                        sys.stderr.buffer.flush()
        return process.wait()
    except BaseException:
        stop_process(process)
        raise
    finally:
        selector.close()
        if process.stdout is not None:
            process.stdout.close()
        if process.stderr is not None:
            process.stderr.close()


def append_stderr(path: Path, message: bytes, cargo: CargoResults) -> None:
    try:
        with path.open("ab") as handle:
            handle.write(message)
    finally:
        cargo.feed("stderr", message)
        sys.stderr.buffer.write(message)
        sys.stderr.buffer.flush()


def run_command(args: argparse.Namespace) -> int:
    label = safe_label(args.label)
    if not args.command:
        print("ci evidence: command is required after --", file=sys.stderr)
        return 2

    directory = Path(args.directory)
    logs = directory / "logs"
    results = directory / "results"
    logs.mkdir(parents=True, exist_ok=True)
    results.mkdir(parents=True, exist_ok=True)
    stdout_log = logs / f"{label}.stdout.log"
    stderr_log = logs / f"{label}.stderr.log"

    cargo = CargoResults()
    started = now()
    start = time.monotonic()
    exit_code = 127
    error = None
    token = gha_guard_start()
    try:
        try:
            exit_code = stream(args.command, stdout_log, stderr_log, cargo)
        except CommandStartError as exc:
            error = str(exc)
            message = f"failed to start command: {exc}\n".encode()
            stdout_log.write_bytes(b"")
            stderr_log.write_bytes(message)
            cargo.feed("stderr", message)
            sys.stderr.buffer.write(message)
            sys.stderr.buffer.flush()
        except OSError as exc:
            error = str(exc)
            message = f"streaming failed: {exc}\n".encode()
            append_stderr(stderr_log, message, cargo)
    finally:
        gha_guard_end(token)

    data: dict[str, Any] = {
        "schema_version": 1,
        "label": label,
        "command": args.command,
        "ok": exit_code == 0 and error is None,
        "exit_code": exit_code,
        "runtime_seconds": round(time.monotonic() - start, 3),
        "started_at": started,
        "ended_at": now(),
        "logs": {
            "stdout": stdout_log.relative_to(directory).as_posix(),
            "stderr": stderr_log.relative_to(directory).as_posix(),
        },
        "metadata": metadata(),
    }
    parsed = cargo.finish()
    if parsed:
        data["cargo_test"] = parsed
    if error:
        data["error"] = error
    write_json(results / f"{label}.json", data)
    return exit_code


def load_results(directory: Path) -> tuple[list[dict[str, Any]], list[str]]:
    loaded: list[dict[str, Any]] = []
    malformed: list[str] = []
    for path in sorted((directory / "results").glob("*.json")):
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, UnicodeDecodeError, json.JSONDecodeError):
            malformed.append(path.relative_to(directory).as_posix())
            continue
        if (
            isinstance(data, dict)
            and isinstance(data.get("label"), str)
            and type(data.get("ok")) is bool
            and type(data.get("exit_code")) is int
            and data["ok"] == (data["exit_code"] == 0 and not data.get("error"))
        ):
            data.setdefault("result_file", path.relative_to(directory).as_posix())
            loaded.append(data)
        else:
            malformed.append(path.relative_to(directory).as_posix())
    return loaded, malformed


def md(value: Any) -> str:
    text = html.escape("" if value is None else str(value), quote=False)
    return (
        text.replace("\\", "\\\\")
        .replace("`", "&#96;")
        .replace("|", "\\|")
        .replace("\r\n", "\n")
        .replace("\r", "\n")
        .replace("\n", "<br>")
    )

def fenced_text(value: str) -> str:
    return value.replace("```", "`\u200b``")


def quoted_argv(parts: Any) -> str:
    if not isinstance(parts, list):
        return ""
    return " ".join(md(json.dumps(str(part))) for part in parts)


def binary_info(path: str | None) -> dict[str, Any] | None:
    if not path:
        return None
    try:
        return {"path": path, "bytes": Path(path).stat().st_size}
    except OSError as exc:
        return {"path": path, "error": str(exc)}


def contract_diff_stat(base_ref: str | None) -> str | None:
    if not base_ref:
        return None
    argv = ["git", "diff", "--stat", f"{base_ref}..HEAD", "--", *CONTRACT_PATHS]
    try:
        done = subprocess.run(argv, text=True, capture_output=True, timeout=20, check=False)
    except (OSError, subprocess.TimeoutExpired) as exc:
        return f"Could not read contract file-change aid: {exc}"
    output = (done.stdout + done.stderr).strip()
    if done.returncode != 0:
        return output or f"git diff --stat failed with exit {done.returncode}"
    return output or "No file changes in the known contract source paths."


def overall(results: list[dict[str, Any]], malformed: list[str], expected: list[str]) -> tuple[str, list[str]]:
    missing = [label for label in expected if label not in {r.get("label") for r in results}]
    if any(not r.get("ok") for r in results):
        return "fail", missing
    if malformed or missing:
        return "incomplete", missing
    if expected:
        return "pass", missing
    if results:
        return "recorded pass", missing
    return "no evidence results found", missing


def make_summary(
    results: list[dict[str, Any]],
    malformed: list[str],
    expected: list[str],
    binary: dict[str, Any] | None,
    contract_stat: str | None,
) -> tuple[str, str, list[str]]:
    status, missing = overall(results, malformed, expected)
    meta = next((r.get("metadata") for r in results if isinstance(r.get("metadata"), dict)), None)
    if not isinstance(meta, dict):
        meta = metadata()
    lines = ["# CI verification evidence", "", f"Overall: **{status}**", ""]
    if results and not expected:
        lines += ["The result is limited to recorded checks because no expected-label list was supplied.", ""]
    lines += [
        f"- Tested HEAD: `{md(meta.get('git_head') or 'unknown')}`",
        f"- GitHub SHA: `{md(meta.get('github_sha') or 'not set')}`",
        f"- Runner: `{md(meta.get('runner_os') or 'unknown')}` / `{md(meta.get('runner_arch') or 'unknown')}`",
    ]
    if meta.get("rustc"):
        lines.append(f"- Rust: `{md(str(meta['rustc']).splitlines()[0])}`")
    pr = meta.get("pull_request")
    if isinstance(pr, dict):
        head = pr.get("head") if isinstance(pr.get("head"), dict) else {}
        base = pr.get("base") if isinstance(pr.get("base"), dict) else {}
        lines.append(
            f"- Pull request: head `{md(head.get('sha') or 'unknown')}` on `{md(head.get('ref') or 'unknown')}`, "
            f"base `{md(base.get('sha') or 'unknown')}` on `{md(base.get('ref') or 'unknown')}`"
        )
    if expected:
        lines.append(f"- Expected checks: `{md(', '.join(expected))}`")
    if missing:
        lines.append(f"- Missing expected checks: `{md(', '.join(missing))}`")
    if malformed:
        lines.append(f"- Malformed result files: `{md(', '.join(malformed))}`")
    if binary:
        if "bytes" in binary:
            lines.append(f"- Binary: `{md(binary['path'])}` is `{binary['bytes']}` bytes")
        else:
            lines.append(f"- Binary: `{md(binary['path'])}` unavailable: {md(binary.get('error'))}")

    lines += ["", "## Checks", ""]
    if results or missing:
        lines += ["| Check | Status | Exit | Runtime | Command | Logs |", "| --- | --- | ---: | ---: | --- | --- |"]
        for result in results:
            logs = result.get("logs") if isinstance(result.get("logs"), dict) else {}
            log_paths = ", ".join(f"{md(name)}: `{md(path)}`" for name, path in logs.items() if path)
            lines.append(
                f"| {md(result.get('label'))} | {'pass' if result.get('ok') else 'fail'} | "
                f"{md(result.get('exit_code'))} | {md(result.get('runtime_seconds'))}s | "
                f"{quoted_argv(result.get('command'))} | {log_paths} |"
            )
        lines += [f"| {md(label)} | missing |  |  |  |  |" for label in missing]
    else:
        lines.append("No result JSON files were found under `results/`; this summary does not prove that checks passed.")

    cargo = [r for r in results if isinstance(r.get("cargo_test"), dict)]
    if cargo:
        lines += ["", "## Cargo test result lines", ""]
        lines.append("These counts come from cargo `test result` lines only. They are not coverage data.")
        lines += ["", "| Check | Passed | Failed | Ignored | Measured | Filtered out |", "| --- | ---: | ---: | ---: | ---: | ---: |"]
        for result in cargo:
            counts = result["cargo_test"].get("counts", {})
            lines.append(
                f"| {md(result.get('label'))} | {md(counts.get('passed', 0))} | "
                f"{md(counts.get('failed', 0))} | {md(counts.get('ignored', 0))} | "
                f"{md(counts.get('measured', 0))} | {md(counts.get('filtered_out', 0))} |"
            )
    if contract_stat is not None:
        lines += [
            "",
            "## Contract file-change aid",
            "",
            "Read-only `git diff --stat` over known contract source paths. This is a partial file-change aid, not a complete API or schema diff.",
            "",
            "```text",
            fenced_text(contract_stat),
            "```",
        ]
    return "\n".join(lines).rstrip() + "\n", status, missing


def summary_command(args: argparse.Namespace) -> int:
    directory = Path(args.directory)
    directory.mkdir(parents=True, exist_ok=True)
    expected = [safe_label(label) for label in args.expected]
    results, malformed = load_results(directory)
    binary = binary_info(args.binary)
    contract_stat = contract_diff_stat(args.base_ref)
    text, status, missing = make_summary(results, malformed, expected, binary, contract_stat)
    sys.stdout.write(text)
    (directory / "summary.md").write_text(text, encoding="utf-8")
    write_json(
        directory / "evidence.json",
        {
            "schema_version": 1,
            "generated_at": now(),
            "overall": status.replace(" ", "_"),
            "expected": expected,
            "missing_expected": missing,
            "malformed_results": malformed,
            "results": results,
            "binary": binary,
            "contract_file_change_aid": contract_stat,
        },
    )
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command_name", required=True)
    run = sub.add_parser("run", help="run a command and record evidence")
    run.add_argument("--directory", required=True)
    run.add_argument("--label", required=True)
    run.add_argument("command", nargs=argparse.REMAINDER)
    run.set_defaults(func=run_command)
    summary = sub.add_parser("summary", help="summarize recorded evidence")
    summary.add_argument("--directory", required=True)
    summary.add_argument("--binary")
    summary.add_argument("--base-ref")
    summary.add_argument("--expected", nargs="*", default=[])
    summary.set_defaults(func=summary_command)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    if getattr(args, "command", None) and args.command[0] == "--":
        args.command = args.command[1:]
    try:
        return args.func(args)
    except ValueError as exc:
        print(f"ci evidence: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
