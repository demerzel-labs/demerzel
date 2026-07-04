"""Scheme-based config fetcher registry.

A config location is either a bare filesystem path or a ``scheme://`` URL. Fetching
is dispatched by scheme through :data:`_REGISTRY`, so Sprint 4.1 can add ``https://``
and ``s3://`` fetchers by registering a callable — the loader never changes.

Sprint 0.1 status:

* ``file://`` and bare paths — implemented (bare path == local file).
* ``https://`` and ``s3://`` — recognized but raise a clear "not supported yet
  (arrives in Sprint 4.1)" :class:`ConfigError`.
* any other scheme — :class:`ConfigError` (unknown scheme).

Relative-path policy is the caller's: the loader passes ``base_dir`` = the directory
of the file that declared an ``extends:`` (so relative extends resolve next to their
declaring file), and CWD for a relative ``--org-config``.
"""

from __future__ import annotations

import re
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

from .errors import ConfigError

_SCHEME_RE = re.compile(r"^([a-zA-Z][a-zA-Z0-9+.\-]*)://")


@dataclass(frozen=True)
class FetchResult:
    """The outcome of fetching a config location.

    Attributes
    ----------
    text:
        Raw file contents (YAML source).
    source:
        Human-readable source identifier (resolved absolute path or URL) used in
        error messages and the ``config show`` layer header.
    base_dir:
        Directory against which a relative ``extends:`` declared *inside* this
        content should resolve, or ``None`` for non-filesystem sources.
    """

    text: str
    source: str
    base_dir: Path | None


def scheme_of(location: str) -> str:
    """Return the URL scheme of ``location``, or ``""`` for a bare path."""

    match = _SCHEME_RE.match(location)
    return match.group(1).lower() if match else ""


def _fetch_local(location: str, base_dir: Path) -> FetchResult:
    """Fetch a bare path or ``file://`` URL relative to ``base_dir``."""

    path_str = location[len("file://") :] if location.startswith("file://") else location
    path = Path(path_str)
    if not path.is_absolute():
        path = base_dir / path
    resolved = path.resolve()
    if not resolved.is_file():
        raise ConfigError(f"config file not found: {resolved}", source=str(resolved))
    return FetchResult(resolved.read_text(), str(resolved), resolved.parent)


def _unsupported(scheme: str, sprint: str) -> Callable[[str, Path], FetchResult]:
    """Build a fetcher that reports ``scheme`` as not-yet-supported."""

    def _fetch(location: str, base_dir: Path) -> FetchResult:
        raise ConfigError(
            f"{scheme}:// config locations are not supported yet "
            f"(arrives in Sprint {sprint}): {location}",
            source=location,
        )

    return _fetch


#: scheme -> fetch callable. Sprint 4.1 replaces the ``https``/``s3`` entries with
#: real fetchers without touching the loader.
_REGISTRY: dict[str, Callable[[str, Path], FetchResult]] = {
    "": _fetch_local,
    "file": _fetch_local,
    "https": _unsupported("https", "4.1"),
    "s3": _unsupported("s3", "4.1"),
}


def fetch(location: str, *, base_dir: Path) -> FetchResult:
    """Fetch a config location, dispatching on its scheme.

    Raises :class:`ConfigError` for unsupported (``https``/``s3``) or unknown
    schemes, and for a missing local file.
    """

    scheme = scheme_of(location)
    fetcher = _REGISTRY.get(scheme)
    if fetcher is None:
        raise ConfigError(
            f"unsupported config location scheme '{scheme}://': {location}", source=location
        )
    return fetcher(location, base_dir)
