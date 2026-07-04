"""Shared test helpers: fixture paths and synthetic-layer construction."""

from __future__ import annotations

from pathlib import Path
from typing import Any

from demerzel.config.loader import resolve
from demerzel.config.merge import Layer

PROJECT_ROOT = Path(__file__).resolve().parent.parent
FIXTURES = Path(__file__).resolve().parent / "fixtures"
PLAN_EXAMPLES = FIXTURES / "plan_examples"
EXAMPLES = PROJECT_ROOT / "examples"

#: The frozen layer chain order (lowest -> highest precedence).
CHAIN_ORDER = ["defaults", "org", "team", "repo", "directory", "cli"]


def layer(kind: str, raw: dict[str, Any]) -> Layer:
    """Build a synthetic :class:`Layer` for a given kind."""

    return Layer(kind, kind, raw)


def build_chain(per_kind: dict[str, dict[str, Any]]) -> list[Layer]:
    """Build a layer chain in canonical order from a ``{kind: raw}`` mapping."""

    return [layer(kind, per_kind[kind]) for kind in CHAIN_ORDER if kind in per_kind]


def resolve_kinds(per_kind: dict[str, dict[str, Any]]):
    """Fold a ``{kind: raw}`` chain (canonical order) into an EffectiveConfig."""

    return resolve(build_chain(per_kind))
