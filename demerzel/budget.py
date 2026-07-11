"""Budget control: monthly AWS Budgets preflight + per-run cost ceiling (PLAN.md sec 5).

Three layers total; two live here:
  1. Monthly preflight — refuse to run (exit 2) if the month-to-date AWS Budget is at
     or above the threshold. Returns month-to-date spend %, which routing uses for the
     third layer (budget-aware tier degrade, in routing.py).
  2. Per-run cost ceiling — the DEMERZEL_MAX_COST value plus the governor decision
     logic (soft wrap-up vs hard stop). Enforcement is wired into the runner via hooks
     using CostMeter; the *decisions* live here so they're testable without Strands.

The Budgets client is injectable so the whole module tests with no AWS.
"""

from __future__ import annotations

import os
import sys
from dataclasses import dataclass

DEFAULT_COST_CEILING = 3.0
DEFAULT_BUDGET_NAME = "demerzel-bedrock-monthly"
DEFAULT_BUDGET_THRESHOLD = 95.0
DEFAULT_REGION = "us-east-1"  # AWS Budgets is global; client uses us-east-1

# Injected once as a tool result / user message when the soft ceiling trips. The
# concrete output format is enforced by the system prompt; this just says "wrap up".
COST_CEILING_PROMPT = (
    "COST BUDGET REACHED - no more tool calls are available. "
    "Produce your final review NOW using only what you have already gathered, "
    "following the required output format exactly. Do not apologize for incomplete "
    "coverage; if you found issues, report them."
)


@dataclass
class BudgetStatus:
    """Outcome of the monthly preflight."""

    budget_name: str
    status: str  # ok | exceeded | check_failed
    actual: float | None = None
    limit: float | None = None
    pct: float | None = None
    threshold_pct: float = DEFAULT_BUDGET_THRESHOLD
    message: str = ""

    @property
    def should_refuse(self) -> bool:
        return self.status == "exceeded"

    def to_dict(self) -> dict:
        return {
            "budget_name": self.budget_name,
            "status": self.status,
            "actual": self.actual,
            "limit": self.limit,
            "pct": self.pct,
            "threshold_pct": self.threshold_pct,
            "message": self.message,
        }


def _f(env: dict, key: str, default: float) -> float:
    try:
        return float(env.get(key, default))
    except (TypeError, ValueError):
        return default


def preflight(
    env: dict | None = None,
    *,
    client=None,
    account_id: str | None = None,
) -> BudgetStatus:
    """Check the monthly AWS Budget. Never raises — a check failure returns
    status='check_failed' so the run proceeds (the per-run ceiling still protects)."""
    env = os.environ if env is None else env
    budget_name = env.get("DEMERZEL_BUDGET_NAME", DEFAULT_BUDGET_NAME)
    threshold = _f(env, "DEMERZEL_BUDGET_THRESHOLD", DEFAULT_BUDGET_THRESHOLD)
    if threshold <= 0:
        # A non-positive threshold makes every run refuse (exit 2) — almost certainly a
        # fat-fingered project variable, not intent. Fall back to the default, loudly.
        print(
            f"[harness] WARN DEMERZEL_BUDGET_THRESHOLD={env.get('DEMERZEL_BUDGET_THRESHOLD')!r} "
            f"is non-positive; using default {DEFAULT_BUDGET_THRESHOLD}",
            file=sys.stderr,
        )
        threshold = DEFAULT_BUDGET_THRESHOLD
    account_id = account_id or env.get("AWS_ACCOUNT_ID")

    if client is None:
        try:
            import boto3

            region = env.get("AWS_REGION", DEFAULT_REGION)
            client = boto3.client("budgets", region_name=region)
        except Exception as e:  # noqa: BLE001 - any client build failure -> proceed
            return BudgetStatus(
                budget_name=budget_name,
                status="check_failed",
                threshold_pct=threshold,
                message=f"could not create budgets client: {e}",
            )

    try:
        resp = client.describe_budget(AccountId=account_id, BudgetName=budget_name)
        budget = resp["Budget"]
        actual = float(budget["CalculatedSpend"]["ActualSpend"]["Amount"])
        limit = float(budget["BudgetLimit"]["Amount"])
        pct = (actual / limit * 100.0) if limit > 0 else 0.0
    except Exception as e:  # noqa: BLE001 - describe failure -> proceed with warning
        return BudgetStatus(
            budget_name=budget_name,
            status="check_failed",
            threshold_pct=threshold,
            message=f"budget check failed: {e}",
        )

    if pct >= threshold:
        return BudgetStatus(
            budget_name=budget_name,
            status="exceeded",
            actual=actual,
            limit=limit,
            pct=pct,
            threshold_pct=threshold,
            message=(
                f"budget ${actual:.2f}/${limit:.2f} ({pct:.1f}%) >= "
                f"threshold {threshold:.0f}%: refusing to run"
            ),
        )
    return BudgetStatus(
        budget_name=budget_name,
        status="ok",
        actual=actual,
        limit=limit,
        pct=pct,
        threshold_pct=threshold,
        message=f"budget ${actual:.2f}/${limit:.2f} ({pct:.1f}%) under {threshold:.0f}%",
    )


def read_cost_ceiling(env: dict | None = None) -> float:
    """Per-run USD ceiling from DEMERZEL_MAX_COST (default 3.0).

    A non-positive ceiling would trip on the very first model call and return an empty
    review with a healthy exit 0 — a silent malformed-review footgun. Reject it loudly
    and fall back to the default instead."""
    env = os.environ if env is None else env
    ceiling = _f(env, "DEMERZEL_MAX_COST", DEFAULT_COST_CEILING)
    if ceiling <= 0:
        print(
            f"[harness] WARN DEMERZEL_MAX_COST={env.get('DEMERZEL_MAX_COST')!r} is "
            f"non-positive; using default ${DEFAULT_COST_CEILING}",
            file=sys.stderr,
        )
        return DEFAULT_COST_CEILING
    return ceiling


@dataclass
class CostCeiling:
    """Governor decisions for the per-run ceiling.

    soft: cost has reached the ceiling -> inject the wrap-up message once, stop tools.
    hard: cost has blown past ceiling*grace -> abort the next model call outright.
    """

    ceiling: float
    grace: float = 1.5

    def soft_tripped(self, cost_usd: float) -> bool:
        return cost_usd >= self.ceiling

    def hard_tripped(self, cost_usd: float) -> bool:
        return cost_usd >= self.ceiling * self.grace
