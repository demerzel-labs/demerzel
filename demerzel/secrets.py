"""Output secret-scan (PLAN.md sec 9, control 4).

The review is an egress channel (the attacker can read the posted comment), so scan
the final text for secret shapes before emit and redact + flag on hit. Under the
filesystem/env confinement controls nothing secret should be reachable, so this is a
tripwire on confinement failure — it should never fire in a healthy run.

Conservative by design: a few high-signal patterns, not a general DLP engine. False
positives redact a token in the review (annoying, safe); the alternative — leaking a
real credential into a PR comment — is the outcome we refuse.
"""

from __future__ import annotations

import math
import re
from dataclasses import dataclass, field

_REDACTION = "[REDACTED-SECRET]"

# High-signal, low-false-positive credential shapes.
_PATTERNS: list[tuple[str, re.Pattern]] = [
    ("aws_access_key_id", re.compile(r"\b(?:AKIA|ASIA|AGPA|AIDA|AROA|ANPA)[0-9A-Z]{16}\b")),
    ("aws_secret_access_key", re.compile(r"(?i)aws_secret_access_key\s*[=:]\s*\S{40}")),
    ("github_token", re.compile(r"\bgh[pousr]_[A-Za-z0-9]{36,}\b")),
    ("gitlab_pat", re.compile(r"\bglpat-[A-Za-z0-9_-]{20,}\b")),
    ("slack_token", re.compile(r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b")),
    (
        "private_key_block",
        re.compile(r"-----BEGIN (?:RSA |EC |OPENSSH |DSA |PGP )?PRIVATE KEY-----"),
    ),
    ("jwt", re.compile(r"\beyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b")),
    (
        "generic_secret_assign",
        re.compile(
            r"(?i)\b(?:secret|password|passwd|api[_-]?key|token)\b\s*[=:]\s*['\"][^'\"\s]{8,}['\"]"
        ),
    ),
]

# Long opaque base64-ish blobs that look like keys.
_HIGH_ENTROPY = re.compile(r"\b[A-Za-z0-9+/]{40,}={0,2}\b")
_ENTROPY_THRESHOLD = 4.0  # bits/char; random base64 ~6, English prose ~2.5-3


@dataclass
class ScanResult:
    fired: bool = False
    matches: list[str] = field(default_factory=list)  # kinds that matched
    redactions: int = 0
    text: str = ""

    def to_trace(self) -> dict:
        return {
            "fired": self.fired,
            "match_kinds": sorted(set(self.matches)),
            "redactions": self.redactions,
        }


def _shannon_entropy(s: str) -> float:
    if not s:
        return 0.0
    counts: dict[str, int] = {}
    for ch in s:
        counts[ch] = counts.get(ch, 0) + 1
    n = len(s)
    return -sum((c / n) * math.log2(c / n) for c in counts.values())


def scan(text: str, *, check_entropy: bool = True) -> ScanResult:
    """Scan text for secret shapes; return a ScanResult with redacted `text`."""
    result = ScanResult(text=text)
    redacted = text

    for kind, pattern in _PATTERNS:

        def _sub(_m, k=kind):
            result.matches.append(k)
            result.redactions += 1
            return _REDACTION

        redacted = pattern.sub(_sub, redacted)

    if check_entropy:

        def _entropy_sub(m):
            token = m.group(0)
            if _shannon_entropy(token) >= _ENTROPY_THRESHOLD:
                result.matches.append("high_entropy")
                result.redactions += 1
                return _REDACTION
            return token

        redacted = _HIGH_ENTROPY.sub(_entropy_sub, redacted)

    result.text = redacted
    result.fired = result.redactions > 0
    return result
