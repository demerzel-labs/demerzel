"""Model routing: a pure function from a TaskProfile to a ModelDecision.

No network, no side effects — routing table + signal thresholds from a repo-owned
`routing.json`, so a reviewer can predict every decision by reading the config. This
is the whole cost-control brain in front of the runner (PLAN.md sec 6).

Precedence (highest wins):
  profile pin -> team pins (highest tier among matched) -> first matching signal
  override -> task default -> fallback
then budget-aware degrade (non-pinned only), then PROTOTYPE_DEGRADE (the panic lever,
applied unconditionally). Every decision carries `source` + `rationale`.
"""

from __future__ import annotations

import fnmatch
import json
import os
from dataclasses import dataclass
from pathlib import Path

from prototype.types import ModelDecision, TaskProfile

_DEFAULT_CONFIG = Path(__file__).with_name("routing.json")


def default_config_path() -> Path:
    """Path to the packaged routing.json actually used when no override is passed.
    Exposed so the harness can record its digest for provenance (PLAN sec 8)."""
    return _DEFAULT_CONFIG


class RoutingError(ValueError):
    """Raised for malformed routing.json (fail fast at startup, not mid-run)."""


@dataclass
class Pin:
    """An explicit model/effort request from an override profile or team.

    A pin overrides signal-based routing and is never silently degraded by the budget
    governor (only PROTOTYPE_DEGRADE touches it)."""

    model: str | None = None
    effort: str | None = None
    max_tokens: int | None = None
    source: str = "pin"


def load_config(config_path: str | os.PathLike | None = None) -> dict:
    path = Path(config_path) if config_path else _DEFAULT_CONFIG
    try:
        cfg = json.loads(Path(path).read_text(encoding="utf-8"))
    except FileNotFoundError as e:
        raise RoutingError(f"routing config not found: {path}") from e
    except json.JSONDecodeError as e:
        raise RoutingError(f"routing config is not valid JSON ({path}): {e}") from e
    if "models" not in cfg or not isinstance(cfg["models"], dict):
        raise RoutingError(f"routing config missing 'models' map ({path})")
    return cfg


def _resolve_model_id(models: dict, name: str) -> str:
    """Map an alias (haiku/sonnet/opus) to a Bedrock id; pass through a full id."""
    if name in models:
        return models[name]
    # Already a concrete id (contains a dot or colon) — allow it through.
    if "." in name or ":" in name:
        return name
    raise RoutingError(f"unknown model alias {name!r}; not in routing.json models map")


def _normalize_pin_model(models: dict, name: str) -> str:
    """Resolve a pin's model through the routing.json allowlist, returning the
    canonical *alias*.

    Pins come from overrides.cfg (`[profile:*]`/`[team:*] model=`), which is more
    error-prone than routing.json, so the plan bounds them: a pin may only name a
    model the allowlist knows (PLAN sec 6/7 "pins resolve through the routing models
    allowlist"). Returning the alias (not the full id) also keeps the pinned model on
    the degrade ladder, so PROTOTYPE_DEGRADE applies to pins as documented.
    """
    if name in models:
        return name
    # A pin may also spell out a full id, but only one the allowlist actually maps to.
    for alias, mid in models.items():
        if mid == name:
            return alias
    raise RoutingError(
        f"pin model {name!r} is not in the routing.json models allowlist "
        f"(known: {sorted(models)})"
    )


def _tier_index(models_order: list[str], model_name: str) -> int:
    """Position of a model in the degrade order (lower index = more capable)."""
    try:
        return models_order.index(model_name)
    except ValueError:
        return -1


def _match_override(rule: dict, profile: TaskProfile) -> bool:
    when = rule.get("when", {})
    if "task" in when and when["task"] != profile.task:
        return False
    if "diff_size_gt" in when and not (profile.diff_size > int(when["diff_size_gt"])):
        return False
    if "diff_size_lt" in when and not (profile.diff_size < int(when["diff_size_lt"])):
        return False
    if "repo_glob" in when and not fnmatch.fnmatch(profile.repo, when["repo_glob"]):
        return False
    if "paths_any" in when:
        globs = when["paths_any"]
        if not any(fnmatch.fnmatch(f, g) for f in profile.changed_files for g in globs):
            return False
    return True


def _degrade_env(env: dict) -> int:
    raw = env.get("PROTOTYPE_DEGRADE", "").strip()
    if not raw:
        return 0
    try:
        n = int(raw)
    except ValueError:
        return 0
    return max(0, n)


def resolve(
    profile: TaskProfile,
    *,
    pin: Pin | None = None,
    config_path: str | os.PathLike | None = None,
    config: dict | None = None,
    env: dict | None = None,
) -> ModelDecision:
    """Resolve routing for one run. Deterministic given (profile, pin, config, env)."""
    env = os.environ if env is None else env
    cfg = config if config is not None else load_config(config_path)
    models = cfg["models"]
    degrade_order: list[str] = cfg.get("budget_degrade", {}).get("order", list(models.keys()))

    # --- pick base decision by precedence ---
    pinned = False
    reasons: list[str] = []

    if pin and pin.model:
        model_name = _normalize_pin_model(models, pin.model)
        effort = pin.effort
        max_tokens = pin.max_tokens
        source = pin.source
        pinned = True
        reasons.append(f"pinned by {pin.source}")
    else:
        chosen = None
        for i, rule in enumerate(cfg.get("overrides", [])):
            if _match_override(rule, profile):
                use = rule.get("use", {})
                chosen = (
                    use.get("model"),
                    use.get("effort"),
                    use.get("max_tokens"),
                    f"override:{i}",
                    rule.get("reason", "signal override"),
                )
                break
        if chosen:
            model_name, effort, max_tokens, source, reason = chosen
            reasons.append(reason)
        else:
            defaults = cfg.get("defaults", {})
            if profile.task in defaults:
                d = defaults[profile.task]
                model_name = d.get("model")
                effort = d.get("effort")
                max_tokens = d.get("max_tokens")
                source = f"default:{profile.task}"
                reasons.append(f"task default for {profile.task}")
            else:
                fb = cfg.get("fallback", {})
                if not fb.get("model"):
                    raise RoutingError(
                        f"no default for task {profile.task!r} and no fallback in config"
                    )
                model_name = fb.get("model")
                effort = fb.get("effort")
                max_tokens = fb.get("max_tokens")
                source = "fallback"
                reasons.append(f"no rule matched task {profile.task}; used fallback")

    # Fill max_tokens from the task default / fallback when a pin/override omitted it.
    if max_tokens is None:
        d = cfg.get("defaults", {}).get(profile.task) or cfg.get("fallback", {})
        max_tokens = d.get("max_tokens", 8192)

    degraded = False

    # --- budget-aware degrade (non-pinned only) ---
    bd = cfg.get("budget_degrade", {})
    at_pct = float(bd.get("at_pct", 80))
    if not pinned and profile.month_spend_pct is not None and profile.month_spend_pct >= at_pct:
        new_name, moved = _step_down(model_name, degrade_order, 1)
        if moved:
            reasons.append(
                f"budget at {profile.month_spend_pct:.0f}% >= {at_pct:.0f}%: "
                f"degraded {model_name}->{new_name}"
            )
            model_name = new_name
            degraded = True

    # --- PROTOTYPE_DEGRADE panic lever (applies to everything, even pins) ---
    steps = _degrade_env(env)
    if steps:
        new_name, moved = _step_down(model_name, degrade_order, steps)
        if moved:
            reasons.append(f"PROTOTYPE_DEGRADE={steps}: forced {model_name}->{new_name}")
            model_name = new_name
            degraded = True

    model_id = _resolve_model_id(models, model_name)
    return ModelDecision(
        model_id=model_id,
        effort=effort,
        max_tokens=int(max_tokens),
        degraded=degraded,
        source=source,
        rationale="; ".join(reasons),
    )


def _step_down(model_name: str, order: list[str], steps: int) -> tuple[str, bool]:
    """Move a model down `steps` tiers in the degrade order. Returns (name, moved)."""
    idx = _tier_index(order, model_name)
    if idx < 0:
        return model_name, False
    new_idx = min(idx + steps, len(order) - 1)
    if new_idx == idx:
        return model_name, False
    return order[new_idx], True
