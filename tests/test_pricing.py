"""Cost math: price resolution, cache multipliers, conservative unknown fallback."""

import pytest

from prototype.pricing import (
    FALLBACK_PRICE,
    PRICES,
    CostMeter,
    estimate_cost,
    resolve_price,
)


@pytest.mark.parametrize(
    "model_id,alias",
    [
        ("us.anthropic.claude-sonnet-4-6", "sonnet"),
        ("anthropic.claude-opus-4-8", "opus"),
        ("us.anthropic.claude-haiku-4-5-20251001-v1:0", "haiku"),
        ("anthropic.claude-fable-5", "fable"),
    ],
)
def test_resolve_price_by_substring(model_id, alias):
    price, known = resolve_price(model_id)
    assert known is True
    assert price is PRICES[alias]


def test_unknown_model_falls_back_to_ceiling():
    price, known = resolve_price("some.future.model-v9")
    assert known is False
    assert price is FALLBACK_PRICE  # conservative: bill unknowns at the top tier


def test_estimate_cost_matches_table():
    # 1M input + 1M output on sonnet = $3 + $15
    cost = estimate_cost("sonnet", input=1_000_000, output=1_000_000)
    assert cost == pytest.approx(18.0)


def test_cache_read_is_cheap():
    full = estimate_cost("opus", input=1_000_000)
    cached = estimate_cost("opus", cache_read=1_000_000)
    assert cached == pytest.approx(full * 0.10)


def test_meter_accumulates_and_costs():
    m = CostMeter("us.anthropic.claude-sonnet-4-6")
    assert m.known is True
    m.record_usage({"inputTokens": 500_000, "outputTokens": 100_000})
    m.record_usage(
        {"inputTokens": 500_000, "outputTokens": 100_000, "cacheReadInputTokens": 200_000}
    )
    assert m.input == 1_000_000
    assert m.output == 200_000
    assert m.cache_read == 200_000
    # 1M*3 + 0.2M*15 + 0.2M*0.30 = 3 + 3 + 0.06
    assert m.cost_usd == pytest.approx(6.06)
    totals = m.totals
    assert totals["input_tokens"] == 1_000_000
    assert totals["price_known"] is True


def test_meter_unknown_model_flagged():
    m = CostMeter("mystery-model")
    assert m.known is False
    assert m.totals["price_known"] is False
