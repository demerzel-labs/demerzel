"""Read-only bash tool for the code-review agent (PLAN.md sec 9).

Threat model (recalibrated 2026-07-04): the agent runs in a short-lived CI job. CI
credentials can exist in the parent job environment, but the tool subprocess receives a
small allowlisted environment. Network egress is an operator-managed control, not a
command-allowlist control, so this tool is deliberately permissive about reads and
analysis (interpreters included) — over-strict gating just makes tool calls fail, waste
turns, and degrade review quality.

Containment, in order of load:
  1. Network egress restricted to approved endpoints (operator-managed primary control,
     see sandbox.detect_tier and PLAN sec 9).
  2. Environment allowlist (HERE, zero review-quality cost): the subprocess env is built
     from empty with only PATH/LANG/LC_ALL/TERM and a scratch HOME. CI credentials are
     NOT inherited, so a confused-deputy agent has no token to write with.
  3. Output secret-scan tripwire (in secrets.py, applied by the CLI to the review).
  4. Command denylist (HERE): a courtesy guardrail that blocks the obvious
     destructive / network / privilege / git-write commands per pipeline segment. NOT a
     containment boundary — interpreters are allowed, so it cannot stop a determined
     escape; it stops accidents and lazy exfil, and real containment is (1)+(2).

`classify()` is a pure function so the suite can exercise gating without a subprocess.
"""

from __future__ import annotations

import atexit
import os
import re
import shlex
import shutil
import signal
import subprocess
import tempfile
import time
from dataclasses import dataclass

# --- Denylist (courtesy guardrail; see docstring) --------------------------------

# Denied leading binaries, checked per pipeline segment (precise, low false-positive:
# `grep rm file` is fine because rm is not the segment's leading command).
DENIED_BINARIES = frozenset(
    {
        # network egress
        "curl",
        "wget",
        "nc",
        "ncat",
        "netcat",
        "socat",
        "ssh",
        "scp",
        "sftp",
        "telnet",
        "ftp",
        "rsync",
        # privilege
        "sudo",
        "su",
        "doas",
        # destructive filesystem
        "rm",
        "rmdir",
        "shred",
        "dd",
        "mkfs",
        "fdisk",
        "mkswap",
        "chmod",
        "chown",
        "chgrp",
        "ln",
    }
)

# git subcommands that mutate state (reads like log/show/diff/blame/status pass).
GIT_WRITE_SUBCOMMANDS = frozenset(
    {
        "push",
        "commit",
        "merge",
        "rebase",
        "reset",
        "clean",
        "am",
        "apply",
        "cherry-pick",
        "revert",
        "fetch",
        "pull",
        "remote",
        "config",
        "gc",
        "prune",
        "update-ref",
        "filter-branch",
        "worktree",
        "stash",
        "mv",
        "rm",
        "checkout",
        "switch",
        "restore",
        "tag",
        "branch",
        "init",
        "clone",
    }
)

# Sensitive path fragments that are almost never legitimate review targets. Substring
# match; kept tight to avoid blocking code that merely mentions these strings.
SENSITIVE_PATHS = (
    ".aws/credentials",
    ".git-credentials",
    "id_rsa",
    "id_ed25519",
    "/etc/shadow",
    "/etc/sudoers",
    ".netrc",
    ".kube/config",
    ".pypirc",
    "/proc/self/environ",
)

_ASSIGNMENT = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*=")


@dataclass
class BashResult:
    text: str  # what the model sees: truncated to max_output_bytes
    blocked: bool
    block_reason: str | None
    exit_code: int | None
    duration_ms: int
    truncated: bool
    full_text: str | None = None  # untruncated combined output, for the raw-envelope log


# --- Pure classification ---------------------------------------------------------


def split_segments(command: str) -> list[str]:
    """Split on ``|`` / ``&&`` / ``||`` / ``;`` / newline, honoring quotes and escapes.
    Used to find each segment's leading command for the binary denylist."""
    segments: list[str] = []
    buf: list[str] = []
    quote: str | None = None
    i, n = 0, len(command)
    while i < n:
        c = command[i]
        if quote:
            buf.append(c)
            if c == quote:
                quote = None
            i += 1
            continue
        if c in ("'", '"'):
            quote = c
            buf.append(c)
            i += 1
            continue
        if c == "\\":
            buf.append(c)
            if i + 1 < n:
                buf.append(command[i + 1])
            i += 2
            continue
        if command[i : i + 2] in ("&&", "||"):
            segments.append("".join(buf))
            buf = []
            i += 2
            continue
        if c in ("|", ";", "\n"):
            segments.append("".join(buf))
            buf = []
            i += 1
            continue
        buf.append(c)
        i += 1
    segments.append("".join(buf))
    return [s for s in segments if s.strip()]


def _leading_tokens(seg: str) -> list[str]:
    """Best-effort token list for a segment; skips leading VAR=val assignments so the
    real command is found. Falls back to a whitespace split on quoting errors."""
    try:
        tokens = shlex.split(seg, comments=False, posix=True)
    except ValueError:
        tokens = seg.split()
    while tokens and _ASSIGNMENT.match(tokens[0]):
        tokens = tokens[1:]
    return tokens


def _git_write_reason(tokens: list[str]) -> str | None:
    skip_value = False
    for tok in tokens[1:]:
        if skip_value:
            skip_value = False
            continue
        if tok in ("-C", "-c", "--git-dir", "--work-tree", "--namespace"):
            skip_value = True
            continue
        if tok.startswith("-"):
            continue
        if tok in GIT_WRITE_SUBCOMMANDS:
            return f"git write subcommand: {tok}"
        return None  # first non-flag token is a read subcommand
    return None


def _segment_reason(seg: str) -> str | None:
    tokens = _leading_tokens(seg)
    if not tokens:
        return None
    binary = os.path.basename(tokens[0])
    if binary in DENIED_BINARIES or binary.startswith("mkfs."):
        return f"denied command: {binary}"
    if binary == "git":
        return _git_write_reason(tokens)
    return None


def classify(command: str) -> tuple[bool, str | None]:
    """Pure gating decision -> ``(allowed, reason)``. Denylist-primary: allow reads and
    analysis (interpreters included); block obvious destructive / network / privilege /
    git-write commands and sensitive-path reads. No subprocess is launched here."""
    if not command or not command.strip():
        return False, "empty command"
    lowered = command.lower()
    for frag in SENSITIVE_PATHS:
        if frag in lowered:
            return False, f"sensitive path access: {frag}"
    for seg in split_segments(command):
        reason = _segment_reason(seg)
        if reason:
            return False, reason
    return True, None


# --- Execution -------------------------------------------------------------------


class BashTool:
    """Read-only bash tool: gate with ``classify`` (courtesy guardrail), then run in a
    fresh, cred-free environment (the load-bearing local control). Blocked calls never
    touch the subprocess."""

    def __init__(
        self,
        repo_root: str,
        *,
        scratch_home: str | None = None,
        timeout_default: int = 120,
        max_timeout: int = 300,
        max_output_bytes: int = 100_000,
        command_prefix: list[str] | None = None,
    ):
        self.repo_root = os.path.realpath(repo_root)
        if scratch_home:
            self.scratch_home = scratch_home
        else:
            # Own the scratch HOME we create and clean it up at process exit so an
            # ephemeral job (or the test suite) doesn't leak /tmp dirs.
            self.scratch_home = tempfile.mkdtemp(prefix="prototype-bash-home-")
            atexit.register(shutil.rmtree, self.scratch_home, ignore_errors=True)
        self.timeout_default = timeout_default
        self.max_timeout = max_timeout
        self.max_output_bytes = max_output_bytes
        self.command_prefix = list(command_prefix) if command_prefix else []

    def _build_env(self) -> dict[str, str]:
        """Inverted allowlist: start empty, add only what tools need. CI credentials
        (AWS_*, *TOKEN*, GIT_*, ...) are never inherited, so the agent has no credential
        to write with. HOME is a scratch dir so ``~/.aws`` / ``~/.ssh`` resolve to
        nothing."""
        env: dict[str, str] = {}
        for key in ("PATH", "LANG", "LC_ALL", "TERM"):
            val = os.environ.get(key)
            if val is not None:
                env[key] = val
        env.setdefault("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        env["HOME"] = self.scratch_home
        return env

    def _truncate(self, text: str) -> tuple[str, bool]:
        encoded = text.encode("utf-8")
        if len(encoded) <= self.max_output_bytes:
            return text, False
        dropped = len(encoded) - self.max_output_bytes
        head = encoded[: self.max_output_bytes].decode("utf-8", errors="ignore")
        return f"{head}\n...[truncated {dropped} bytes]", True

    def run(self, command: str, timeout: int | None = None) -> BashResult:
        t0 = time.monotonic()

        def _ms() -> int:
            return int((time.monotonic() - t0) * 1000)

        allowed, reason = classify(command)
        if not allowed:
            return BashResult(
                text=f"blocked: {reason}",
                blocked=True,
                block_reason=reason,
                exit_code=None,
                duration_ms=_ms(),
                truncated=False,
                full_text=f"blocked: {reason}",
            )
        # The timeout is model-controlled, so clamp it to a hard ceiling: a tool call can
        # never be asked to run for an unbounded time.
        try:
            requested = int(timeout) if timeout is not None else self.timeout_default
        except (TypeError, ValueError):
            requested = self.timeout_default
        limit = min(max(1, requested), self.max_timeout)

        argv = [*self.command_prefix, "bash", "-c", command]
        try:
            # start_new_session=True puts the command in its own process group so a
            # timeout can kill the whole tree (piped/backgrounded/interpreter children),
            # not just the direct child.
            proc = subprocess.Popen(
                argv,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                cwd=self.repo_root,
                env=self._build_env(),
                start_new_session=True,
            )
        except OSError as e:
            return BashResult(
                text=f"error: {e}",
                blocked=False,
                block_reason="exec_error",
                exit_code=None,
                duration_ms=_ms(),
                truncated=False,
                full_text=f"error: {e}",
            )
        try:
            stdout, stderr = proc.communicate(timeout=limit)
        except subprocess.TimeoutExpired:
            self._kill_group(proc)
            try:
                proc.communicate(timeout=5)  # reap so no zombie is left behind
            except (subprocess.TimeoutExpired, OSError):
                pass
            text = f"...[timed out after {limit}s]"
            return BashResult(
                text=text,
                blocked=False,
                block_reason="timeout",
                exit_code=None,
                duration_ms=_ms(),
                truncated=False,
                full_text=text,
            )
        combined = (stdout or "") + (stderr or "")
        text, truncated = self._truncate(combined)
        return BashResult(
            text=text,
            blocked=False,
            block_reason=None,
            exit_code=proc.returncode,
            duration_ms=_ms(),
            truncated=truncated,
            full_text=combined,
        )

    @staticmethod
    def _kill_group(proc: subprocess.Popen) -> None:
        """Kill the command's whole process group; fall back to the direct child."""
        try:
            os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
        except (OSError, ProcessLookupError):
            try:
                proc.kill()
            except OSError:
                pass
