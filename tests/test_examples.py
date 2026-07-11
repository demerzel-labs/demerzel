"""The shipped example configs must stay valid so they can't rot."""

from pathlib import Path

from demerzel.config import assemble_system_prompt, load_agent_config
from demerzel.overrides import OverridesConfig
from demerzel.routing import load_config
from demerzel.types import TaskProfile

EXAMPLES = Path(__file__).resolve().parent.parent / "examples"


def test_example_agent_config_loads():
    cfg = load_agent_config(EXAMPLES / "agents/configs/demerzel-review.json")
    assert cfg.task == "review"
    assert cfg.quality_gate is True
    assert cfg.runner == "strands"


def test_example_overrides_validates_and_resolves():
    ov = OverridesConfig.load(EXAMPLES / "overrides.cfg")
    ov.validate(EXAMPLES)  # all referenced files exist, no cycles, known teams

    # A PR touching payments + a migration attributes both teams.
    res = ov.resolve(
        "acme/core",
        changed_files=["src/payments/charge.py", "db/migrations/007.sql", "README.md"],
    )
    assert res.profile.name == "acme-foundations"
    assert set(res.matched_teams) == {"payments", "data-platform"}
    assert "README.md" in res.unowned_files

    # Glob repo maps to the platform-infra profile with an opus pin.
    res2 = ov.resolve("acme/prod/billing")
    assert res2.profile.name == "acme-platform-infrastructure"
    assert res2.pin is not None and res2.pin.model == "opus"


def test_example_prompt_assembles_with_teams():
    ov = OverridesConfig.load(EXAMPLES / "overrides.cfg")
    ov.validate(EXAMPLES)
    cfg = load_agent_config(EXAMPLES / "agents/configs/demerzel-review.json")
    res = ov.resolve("acme/core", changed_files=["src/payments/charge.py"])
    out = assemble_system_prompt(EXAMPLES, cfg, res)
    assert "## Team: payments" in out.text
    assert out.text.startswith("You are Demerzel")


def test_bundled_routing_json_is_valid():
    cfg = load_config()  # the shipped demerzel/routing.json
    assert "sonnet" in cfg["models"]
    from demerzel.routing import resolve

    dec = resolve(TaskProfile(task="review", repo="acme/prod/x"), config=cfg, env={})
    assert "opus" in dec.model_id  # review default is opus (signal overrides dropped)
