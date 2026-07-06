"""Routing golden decisions: defaults, signal overrides, budget degrade, panic lever, pins."""

import json

import pytest

from prototype.routing import Pin, RoutingError, load_config, resolve
from prototype.types import TaskProfile

CONFIG = {
    "models": {
        "haiku": "us.anthropic.claude-haiku-4-5-x",
        "sonnet": "us.anthropic.claude-sonnet-4-6",
        "opus": "us.anthropic.claude-opus-4-8",
    },
    "defaults": {
        "review": {"model": "sonnet", "effort": "high", "max_tokens": 16000},
        "triage": {"model": "haiku", "max_tokens": 4096},
        "incident": {"model": "opus", "effort": "high", "max_tokens": 32000},
    },
    "overrides": [
        {
            "when": {"task": "review", "diff_size_gt": 200000},
            "use": {"model": "opus", "effort": "high"},
            "reason": "large diff",
        },
        {
            "when": {"task": "review", "paths_any": ["**/auth/**", "**/payments/**"]},
            "use": {"model": "opus", "effort": "high"},
            "reason": "sensitive path",
        },
        {
            "when": {"repo_glob": "acme/prod/*"},
            "use": {"model": "opus"},
            "reason": "critical repo",
        },
    ],
    "budget_degrade": {"at_pct": 80, "order": ["opus", "sonnet", "haiku"]},
    "fallback": {"model": "sonnet", "effort": "high", "max_tokens": 16000},
}


def r(profile, **kw):
    return resolve(profile, config=CONFIG, env=kw.pop("env", {}), **kw)


def test_task_default_review():
    d = r(TaskProfile(task="review", repo="acme/core"))
    assert d.model_id == CONFIG["models"]["sonnet"]
    assert d.effort == "high"
    assert d.max_tokens == 16000
    assert d.source == "default:review"
    assert not d.degraded


def test_task_default_triage_haiku_no_effort():
    d = r(TaskProfile(task="triage"))
    assert d.model_id == CONFIG["models"]["haiku"]
    assert d.effort is None
    assert d.source == "default:triage"


def test_large_diff_escalates_to_opus():
    d = r(TaskProfile(task="review", diff_size=250_000, repo="acme/core"))
    assert d.model_id == CONFIG["models"]["opus"]
    assert d.source == "override:0"
    assert "large diff" in d.rationale


def test_sensitive_path_escalates():
    d = r(
        TaskProfile(
            task="review",
            repo="acme/core",
            changed_files=["src/auth/login.py", "README.md"],
        )
    )
    assert d.model_id == CONFIG["models"]["opus"]
    assert d.source == "override:1"


def test_first_match_wins_diff_before_path():
    # both large diff and sensitive path -> override:0 (diff) matches first
    d = r(
        TaskProfile(
            task="review",
            diff_size=250_000,
            repo="acme/core",
            changed_files=["src/payments/charge.py"],
        )
    )
    assert d.source == "override:0"


def test_repo_glob_override():
    d = r(TaskProfile(task="review", repo="acme/prod/data-layer"))
    assert d.model_id == CONFIG["models"]["opus"]
    assert d.source == "override:2"


def test_unknown_task_uses_fallback():
    d = r(TaskProfile(task="mystery"))
    assert d.model_id == CONFIG["models"]["sonnet"]
    assert d.source == "fallback"


def test_budget_degrade_steps_down_one_tier():
    d = r(TaskProfile(task="incident", month_spend_pct=85.0))
    # incident default is opus; degrade one -> sonnet
    assert d.model_id == CONFIG["models"]["sonnet"]
    assert d.degraded is True
    assert "degraded" in d.rationale


def test_budget_degrade_not_applied_below_threshold():
    d = r(TaskProfile(task="incident", month_spend_pct=50.0))
    assert d.model_id == CONFIG["models"]["opus"]
    assert d.degraded is False


def test_budget_degrade_does_not_touch_pins():
    d = r(
        TaskProfile(task="review", month_spend_pct=95.0),
        pin=Pin(model="opus", effort="high", source="profile"),
    )
    assert d.model_id == CONFIG["models"]["opus"]
    assert d.degraded is False
    assert d.source == "profile"


def test_pin_wins_over_signals():
    d = r(
        TaskProfile(task="review", diff_size=250_000, repo="acme/prod/x"),
        pin=Pin(model="haiku", source="team:docs"),
    )
    assert d.model_id == CONFIG["models"]["haiku"]
    assert d.source == "team:docs"


def test_prototype_degrade_panic_lever_forces_down():
    d = r(TaskProfile(task="review"), env={"PROTOTYPE_DEGRADE": "1"})
    # sonnet -> haiku
    assert d.model_id == CONFIG["models"]["haiku"]
    assert d.degraded is True
    assert "PROTOTYPE_DEGRADE" in d.rationale


def test_prototype_degrade_overrides_even_pins():
    d = r(
        TaskProfile(task="review"),
        pin=Pin(model="opus", source="profile"),
        env={"PROTOTYPE_DEGRADE": "2"},
    )
    # opus -> haiku (2 steps)
    assert d.model_id == CONFIG["models"]["haiku"]
    assert d.degraded is True


def test_degrade_clamps_at_cheapest():
    d = r(TaskProfile(task="triage"), env={"PROTOTYPE_DEGRADE": "5"})
    # haiku is already cheapest; stays haiku, not moved
    assert d.model_id == CONFIG["models"]["haiku"]


def test_pin_full_id_must_be_in_allowlist():
    # A pin spelled as a full id is accepted ONLY when the allowlist maps to it, and it
    # resolves back to the alias so the degrade ladder still applies (PLAN sec 6/7).
    known_id = CONFIG["models"]["opus"]
    d = r(TaskProfile(task="review"), pin=Pin(model=known_id, source="profile"))
    assert d.model_id == known_id
    assert d.source == "profile"


def test_pin_off_allowlist_raises_routing_error():
    # A pin naming a model the allowlist doesn't know is rejected (the harness degrades
    # this to default routing rather than failing the review).
    with pytest.raises(RoutingError):
        r(TaskProfile(task="review"), pin=Pin(model="eu.anthropic.custom-1:0", source="profile"))


def test_prototype_degrade_applies_to_full_id_pin():
    # A pin given as a known full id still moves under the panic lever (regression:
    # previously a full-id pin escaped PROTOTYPE_DEGRADE).
    d = r(
        TaskProfile(task="review"),
        pin=Pin(model=CONFIG["models"]["opus"], source="profile"),
        env={"PROTOTYPE_DEGRADE": "2"},
    )
    assert d.model_id == CONFIG["models"]["haiku"]
    assert d.degraded is True


def test_bad_config_missing_models(tmp_path):
    bad = tmp_path / "routing.json"
    bad.write_text(json.dumps({"defaults": {}}))
    with pytest.raises(RoutingError):
        load_config(bad)


def test_bundled_default_config_loads():
    # the shipped routing.json must be valid
    cfg = load_config()
    assert "sonnet" in cfg["models"]
    d = resolve(TaskProfile(task="review", repo="x"), config=cfg, env={})
    assert d.model_id


def test_rationale_always_present():
    for prof in [
        TaskProfile(task="review"),
        TaskProfile(task="triage"),
        TaskProfile(task="review", diff_size=999_999),
    ]:
        assert r(prof).rationale
