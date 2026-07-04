"""The baked-in harness-defaults layer (lowest precedence).

This is the raw dict for the bottom of the layer chain. Values marked *provisional*
below are placeholders the plan expects to revisit; keep the notes with the values.

Provisional values:

* ``models.tiers.deep`` — ``bedrock:us.anthropic.claude-opus-4-1`` is a placeholder;
  revisit when model URIs are parsed/validated in Sprint 0.2.
* ``budget.max_cost_usd`` — ``1.00`` is provisional until the Sprint 1.2 cost meter
  gives us real per-run cost data to tune against.

Only value-contributing keys live here; governance keys (``locked``/``bounds``/
``teams``/``models.allowed``) belong to the org layer, never to defaults.
"""

from __future__ import annotations

from copy import deepcopy
from typing import Any

_DEFAULTS: dict[str, Any] = {
    "version": 1,
    "review": {
        "tone": "concise",
        "output": {
            "format": "markdown",
            "template": "default",
            "sections": ["summary", "findings"],
        },
        "behavior": {
            "focus": [],
            "ignore_paths": [],
            "report_threshold": "info",
            "fail_threshold": "error",
            "categories": {},
            "max_findings": 20,
        },
        "rules": [],
        "overrides": [],
    },
    "models": {
        "routing": {"plan": "fast", "review": "standard", "verify": "fast"},
        "tiers": {
            "fast": "bedrock:us.amazon.nova-lite-v1:0",
            "standard": "bedrock:us.anthropic.claude-sonnet-4-5",
            # provisional placeholder — revisit in Sprint 0.2 (see module docstring)
            "deep": "bedrock:us.anthropic.claude-opus-4-1",
        },
    },
    # provisional until the Sprint 1.2 cost meter (see module docstring)
    "budget": {"max_cost_usd": 1.00},
    "providers": {
        "default": "bedrock",
        "bedrock": {"region": "us-east-1", "auth": "ambient"},
    },
}


def default_layer_raw() -> dict[str, Any]:
    """Return a fresh deep copy of the harness-defaults raw dict.

    A copy is returned so callers may merge into it without mutating the shared
    module-level constant.
    """

    return deepcopy(_DEFAULTS)
