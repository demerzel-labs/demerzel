"""Scalar precedence: every layer beats the one below; the directory slot sits
between repo and cli."""

from __future__ import annotations

import pytest

from demerzel.config.loader import resolve
from demerzel.config.merge import Layer, directory_layers
from tests.conftest import CHAIN_ORDER, build_chain, resolve_kinds

# Adjacent (lower, higher) pairs across the full frozen chain.
ADJACENT_PAIRS = list(zip(CHAIN_ORDER, CHAIN_ORDER[1:], strict=False))


@pytest.mark.parametrize(("lower", "higher"), ADJACENT_PAIRS)
def test_higher_layer_wins_on_tone(lower: str, higher: str) -> None:
    cfg = resolve_kinds(
        {
            lower: {"review": {"tone": lower}},
            higher: {"review": {"tone": higher}},
        }
    )
    assert cfg.review.tone == higher


@pytest.mark.parametrize(("lower", "higher"), ADJACENT_PAIRS)
def test_higher_layer_wins_on_max_findings(lower: str, higher: str) -> None:
    lo, hi = CHAIN_ORDER.index(lower), CHAIN_ORDER.index(higher)
    cfg = resolve_kinds(
        {
            lower: {"review": {"behavior": {"max_findings": 10 + lo}}},
            higher: {"review": {"behavior": {"max_findings": 10 + hi}}},
        }
    )
    assert cfg.review.behavior.max_findings == 10 + hi


@pytest.mark.parametrize(("lower", "higher"), ADJACENT_PAIRS)
def test_higher_layer_wins_on_budget(lower: str, higher: str) -> None:
    lo, hi = CHAIN_ORDER.index(lower), CHAIN_ORDER.index(higher)
    cfg = resolve_kinds(
        {
            lower: {"budget": {"max_cost_usd": 1.0 + lo}},
            higher: {"budget": {"max_cost_usd": 1.0 + hi}},
        }
    )
    assert cfg.review.behavior is not None
    assert cfg.budget.max_cost_usd == 1.0 + hi


def test_full_chain_top_wins() -> None:
    per_kind = {kind: {"review": {"tone": kind}} for kind in CHAIN_ORDER}
    cfg = resolve_kinds(per_kind)
    assert cfg.review.tone == "cli"


def test_directory_slot_between_repo_and_cli() -> None:
    """A synthetic directory layer overrides repo but is overridden by cli."""

    chain = build_chain(
        {
            "repo": {"review": {"tone": "repo"}},
            "directory": {"review": {"tone": "directory"}},
        }
    )
    # directory beats repo
    assert resolve(chain).review.tone == "directory"

    # ...and cli beats directory
    chain.append(Layer("cli", "cli", {"review": {"tone": "cli"}}))
    assert resolve(chain).review.tone == "cli"


def test_directory_layers_slot_is_empty_in_0_1(tmp_path) -> None:
    assert directory_layers(tmp_path, tmp_path) == []
