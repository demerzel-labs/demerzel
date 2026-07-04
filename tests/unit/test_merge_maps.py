"""Maps deep-merge across layers; later layer wins on key collision."""

from __future__ import annotations

from tests.conftest import resolve_kinds


def test_categories_deep_merge_combines_keys() -> None:
    cfg = resolve_kinds(
        {
            "org": {"review": {"behavior": {"categories": {"security": "error"}}}},
            "repo": {"review": {"behavior": {"categories": {"style": "ignore"}}}},
        }
    )
    assert cfg.review.behavior.categories == {"security": "error", "style": "ignore"}


def test_categories_later_layer_wins_on_collision() -> None:
    cfg = resolve_kinds(
        {
            "org": {"review": {"behavior": {"categories": {"style": "warning"}}}},
            "repo": {"review": {"behavior": {"categories": {"style": "ignore"}}}},
        }
    )
    assert cfg.review.behavior.categories == {"style": "ignore"}


def test_models_routing_deep_merge_combines_keys() -> None:
    cfg = resolve_kinds(
        {
            "org": {"models": {"routing": {"plan": "fast"}}},
            "repo": {"models": {"routing": {"review": "deep"}}},
        }
    )
    assert cfg.models.routing == {"plan": "fast", "review": "deep"}


def test_models_routing_later_layer_wins_on_collision() -> None:
    cfg = resolve_kinds(
        {
            "org": {"models": {"routing": {"review": "standard"}}},
            "repo": {"models": {"routing": {"review": "deep"}}},
        }
    )
    assert cfg.models.routing == {"review": "deep"}
