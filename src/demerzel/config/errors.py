"""Configuration error type.

A single :class:`ConfigError` carries the *source* (a config file path or the
``<cli>`` sentinel) and the offending dotted *key* so the CLI can render a
one-line ``error: ...`` message and exit ``2`` (harness error, per the plan's
exit-code contract) instead of dumping a traceback for a user's config mistake.
"""

from __future__ import annotations


class ConfigError(Exception):
    """Raised for any configuration problem: unknown/invalid keys, bad layer
    sources, unsupported fetch schemes, or malformed CLI overrides.

    Parameters
    ----------
    message:
        Human-readable, already formatted to name the source and key where
        relevant (e.g. ``Unknown config key 'review.behaviour' in /p/.demerzel.yml``).
    source:
        The config source the error is about — a file path/URL or ``<cli>`` —
        retained for programmatic inspection and testing.
    key:
        The dotted key path (with list indices like ``review.rules[0].severity``)
        the error is about, when applicable.
    """

    def __init__(self, message: str, *, source: str | None = None, key: str | None = None) -> None:
        super().__init__(message)
        self.message = message
        self.source = source
        self.key = key

    def __str__(self) -> str:
        return self.message
