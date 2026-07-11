"""OS-level tool confinement tier detection (PLAN.md sec 9).

The harness detects the strongest confinement the runner supports and degrades
gracefully, recording the actual tier so the posture is always visible:

  A - network namespace (`unshare -rn`): tool subprocess has no network at all.
  B - egress firewall (operator-declared): only approved endpoints reachable.
  C - bind-mounted read-only repo (infra-declared): kernel-enforced fs confinement.
  D - none: portable in-process controls (sec 9) are the whole line.

A/C/B beyond A are declared by the runner via env because the harness cannot probe
operator-managed state from inside; tier A is probed directly. Portable controls apply at every
tier — the tier only adds OS-level defense on top.
"""

from __future__ import annotations

import os
import shutil
import subprocess
from collections.abc import Callable

VALID_TIERS = ("A", "B", "C", "D")


def _probe_unshare(run: Callable = subprocess.run) -> bool:
    """True if `unshare -rn` actually works here (Linux with user+net namespaces)."""
    if shutil.which("unshare") is None:
        return False
    try:
        result = run(
            ["unshare", "-rn", "true"],
            capture_output=True,
            timeout=5,
        )
        return result.returncode == 0
    except (OSError, subprocess.SubprocessError):
        return False


def detect_tier(
    env: dict | None = None,
    *,
    can_unshare: Callable[[], bool] | None = None,
) -> str:
    """Resolve the confinement tier. `DEMERZEL_SANDBOX_TIER` forces a value (for CI
    that knows its own capability); otherwise probe A, then honor declared B/C, else D.
    """
    env = os.environ if env is None else env
    override = env.get("DEMERZEL_SANDBOX_TIER")
    if override in VALID_TIERS:
        return override
    probe = can_unshare if can_unshare is not None else _probe_unshare
    if probe():
        return "A"
    if env.get("DEMERZEL_EGRESS_FIREWALL") == "1":
        return "B"
    if env.get("DEMERZEL_REPO_RO_MOUNT") == "1":
        return "C"
    return "D"


def command_prefix(tier: str) -> list[str]:
    """Argv prefix that enforces the tier at exec time. Only tier A wraps the command
    (network namespace); B/C are enforced by infra, D adds nothing."""
    if tier == "A":
        return ["unshare", "-rn", "--"]
    return []


def describe(tier: str) -> str:
    return {
        "A": "network namespace (no network for tools)",
        "B": "egress firewall (approved endpoints only)",
        "C": "read-only bind-mounted repo",
        "D": "no OS-level confinement (portable controls only)",
    }.get(tier, "unknown")
