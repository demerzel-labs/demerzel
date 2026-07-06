"""Shared dataclasses used across the harness.

Kept dependency-free (stdlib only) so every other module can import these without
creating cycles.
"""

from __future__ import annotations

from dataclasses import dataclass, field


@dataclass
class TaskProfile:
    """Inputs to a routing decision — everything the router is allowed to see."""

    task: str = "review"
    diff_size: int = 0
    repo: str = ""
    changed_files: list[str] = field(default_factory=list)
    month_spend_pct: float | None = None


@dataclass
class ModelDecision:
    """Output of the router: which model, at which effort, and why."""

    model_id: str
    effort: str | None
    max_tokens: int
    degraded: bool = False
    source: str = ""  # profile | team:<name> | override:<n> | default:<task> | fallback
    rationale: str = ""


@dataclass
class AgentConfig:
    """Resolved agent configuration (from --config JSON + overrides).

    Model selection (which model, effort, max_tokens) is deliberately NOT here — it is
    owned solely by routing.json per stage (`defaults.review`, `defaults.gate`, +
    overrides and repo/team pins), so there is one auditable place Bedrock IDs appear.
    This config carries only the loop/behavior knobs. `raw` keeps the untouched source
    dict so anything not modeled here is still available and traceable.
    """

    max_turns: int = 50
    timeout_s: int = 2700
    thinking: dict = field(default_factory=dict)
    runner: str = "strands"  # strands | fake
    task: str = "review"
    prompt_cache: bool = False
    quality_gate: bool = False  # run the cheap post-review quality gate (sec 6.1)
    raw: dict = field(default_factory=dict)
