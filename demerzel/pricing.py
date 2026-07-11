"""Cost estimation and the per-run cost meter.

Prices are a static, versioned table (1P Bedrock list prices, verified 2026-06-09 in
the vault design note). They drift — treat this as a starting point and reconcile
against AWS Cost Explorer, which is the source of truth. Regional endpoints carry a
~+10% premium over global; adjust `REGION_MULTIPLIER` if the account uses regional.

Token buckets follow Bedrock/Anthropic usage accounting, which reports them
separately (no double counting):
  input        = uncached input tokens
  output       = output tokens
  cache_read   = tokens read from prompt cache (~0.1x input)
  cache_write  = tokens written to prompt cache (~1.25x input, 5-min TTL)
"""

from __future__ import annotations

from dataclasses import dataclass


# USD per million tokens.
@dataclass(frozen=True)
class Price:
    input: float
    output: float
    cache_read: float
    cache_write: float


# Keyed by canonical alias. `resolve_price` matches a full Bedrock model id to one of
# these by substring so new inference-profile ids (us./eu. prefixes, dated suffixes)
# resolve without a table edit.
PRICES: dict[str, Price] = {
    "haiku": Price(input=1.0, output=5.0, cache_read=0.10, cache_write=1.25),
    "sonnet": Price(input=3.0, output=15.0, cache_read=0.30, cache_write=3.75),
    "opus": Price(input=5.0, output=25.0, cache_read=0.50, cache_write=6.25),
    "fable": Price(input=10.0, output=50.0, cache_read=1.0, cache_write=12.50),
}

# Unknown models bill at the ceiling tier so budget math over-estimates rather than
# under-estimates — a conservative fallback never hides spend.
FALLBACK_PRICE = PRICES["fable"]

REGION_MULTIPLIER = 1.0  # set to 1.10 if the account bills on a regional endpoint

_MTOK = 1_000_000.0


def resolve_price(model_id: str) -> tuple[Price, bool]:
    """Return (price, is_known). Match by alias substring, case-insensitive."""
    mid = (model_id or "").lower()
    for alias, price in PRICES.items():
        if alias in mid:
            return price, True
    return FALLBACK_PRICE, False


def estimate_cost(
    model_id: str,
    *,
    input: int = 0,
    output: int = 0,
    cache_read: int = 0,
    cache_write: int = 0,
) -> float:
    """USD estimate for one set of token counts."""
    price, _ = resolve_price(model_id)
    raw = (
        input * price.input
        + output * price.output
        + cache_read * price.cache_read
        + cache_write * price.cache_write
    ) / _MTOK
    return raw * REGION_MULTIPLIER


# Strands Usage TypedDict keys -> our bucket names.
_USAGE_KEYS = {
    "inputTokens": "input",
    "outputTokens": "output",
    "cacheReadInputTokens": "cache_read",
    "cacheWriteInputTokens": "cache_write",
}


class CostMeter:
    """Accumulates token usage across model calls and reports running USD cost.

    Model-scoped: constructed with the model id the run is billed at. `known` is False
    when that id didn't match the price table (cost then uses the conservative
    fallback), which the trace surfaces.
    """

    def __init__(self, model_id: str):
        self.model_id = model_id
        _, self.known = resolve_price(model_id)
        self.input = 0
        self.output = 0
        self.cache_read = 0
        self.cache_write = 0

    def add(
        self,
        *,
        input: int = 0,
        output: int = 0,
        cache_read: int = 0,
        cache_write: int = 0,
    ) -> None:
        self.input += input
        self.output += output
        self.cache_read += cache_read
        self.cache_write += cache_write

    def record_usage(self, usage: dict) -> None:
        """Add a Strands `Usage` dict (inputTokens/outputTokens/cache* keys)."""
        mapped = {dest: int(usage.get(src, 0) or 0) for src, dest in _USAGE_KEYS.items()}
        self.add(**mapped)

    @property
    def cost_usd(self) -> float:
        return estimate_cost(
            self.model_id,
            input=self.input,
            output=self.output,
            cache_read=self.cache_read,
            cache_write=self.cache_write,
        )

    @property
    def totals(self) -> dict:
        return {
            "input_tokens": self.input,
            "output_tokens": self.output,
            "cache_read_tokens": self.cache_read,
            "cache_write_tokens": self.cache_write,
            "cost_usd": round(self.cost_usd, 6),
            "price_known": self.known,
        }
