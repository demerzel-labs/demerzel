"""The layer chain and the per-field merge engine.

Layer chain (frozen order, lowest to highest precedence)::

    defaults -> org -> team -> repo -> directory -> cli

The chain is an explicit ordered list of :class:`Layer` objects. The merge engine
folds the *raw dicts* (only the keys each file actually set) in order, so a higher
layer overrides a lower one and injecting a synthetic directory layer in tests
exercises that slot. Merging raw dicts (not validated models) preserves
"explicitly set" semantics — defaults are materialized only once, by validating the
final merged dict into :class:`~demerzel.config.schema.EffectiveConfig`.

**Merge semantics (fixed per field):**

* scalars — higher layer wins (replace).
* maps — deep-merge (recursive; higher layer wins on key collision). This covers
  ``review.behavior.categories``, ``models.routing``, ``models.tiers``, ``bounds``.
* lists — per-field policy in :data:`LIST_POLICY`; anything not listed REPLACES.
"""

from __future__ import annotations

from copy import deepcopy
from dataclasses import dataclass
from pathlib import Path
from typing import Any

#: List-merge policy keyed by dotted path. ``"append"`` concatenates lower layers
#: first then higher; every other list field REPLACES (higher layer wins) — that is
#: the documented default for any path absent from this table.
LIST_POLICY: dict[str, str] = {
    "review.rules": "append",
    "review.behavior.ignore_paths": "append",
    "review.overrides": "append",
    "review.behavior.focus": "replace",
    "review.output.sections": "replace",
}


@dataclass(frozen=True)
class Layer:
    """One participant in the layer chain.

    Attributes
    ----------
    kind:
        One of ``defaults``/``org``/``team``/``repo``/``directory``/``cli``.
    source:
        Human-readable source detail (file path/URL, or ``"N overrides"`` for the
        CLI layer). ``None`` for the baked-in defaults layer.
    raw:
        The raw config dict for this layer (only keys actually present).
    """

    kind: str
    source: str | None
    raw: dict[str, Any]

    def label(self) -> str:
        """Render this layer for the ``config show`` header, e.g. ``org (path)``."""

        return self.kind if self.source is None else f"{self.kind} ({self.source})"


def directory_layers(repo_root: Path, target_path: Path) -> list[Layer]:
    """Return the directory-config layers for ``target_path`` under ``repo_root``.

    Sprint 0.1 always returns ``[]``. The nearest-ancestor ``.demerzel.yml``
    cascade (ESLint-style, ordered root->nearest) lands in Sprint 3.1; the slot
    exists now so the resolution order is correct and tests can inject synthetic
    directory layers between ``repo`` and ``cli`` to prove the slot's position.
    When implemented, this returns multiple layers ordered root->nearest.
    """

    return []


def _list_policy(path: str) -> str:
    return LIST_POLICY.get(path, "replace")


def _deep_merge(base: dict[str, Any], incoming: dict[str, Any], prefix: str) -> dict[str, Any]:
    """Merge ``incoming`` (higher layer) into ``base`` (accumulated lower layers).

    ``base`` is mutated and returned. ``prefix`` is the dotted path used to look up
    list policies. Callers pass an already-deep-copied ``incoming`` so layer raws
    are never mutated by reference sharing.
    """

    for key, value in incoming.items():
        path = f"{prefix}.{key}" if prefix else key
        if key not in base:
            base[key] = value
            continue
        existing = base[key]
        if isinstance(existing, dict) and isinstance(value, dict):
            base[key] = _deep_merge(existing, value, path)
        elif isinstance(existing, list) and isinstance(value, list):
            base[key] = existing + value if _list_policy(path) == "append" else value
        else:
            base[key] = value
    return base


def fold_layers(layers: list[Layer]) -> dict[str, Any]:
    """Fold the layer chain into a single merged raw dict, low to high.

    Returns a new dict; input layer raws are not mutated.
    """

    merged: dict[str, Any] = {}
    for layer in layers:
        _deep_merge(merged, deepcopy(layer.raw), "")
    return merged
