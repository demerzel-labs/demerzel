"""Governance keys are schema-only in 0.1: they parse and appear in the effective
config, but nothing is enforced (enforcement lands in Sprint 2.2)."""

from __future__ import annotations

from demerzel.config.loader import build_layers, load_effective_config


def _write_org(tmp_path, body: str):
    org = tmp_path / "org.yml"
    org.write_text(body)
    return org


def test_governance_keys_parse_and_appear(tmp_path) -> None:
    org = _write_org(
        tmp_path,
        "locked: [review.behavior.categories.security]\n"
        "bounds:\n"
        "  budget.max_cost_usd: { max: 0.50 }\n"
        "teams:\n"
        "  - match: [payments-*, billing-*]\n"
        "    extends: teams/payments.yml\n"
        "models:\n"
        "  allowed: [bedrock:*]\n",
    )
    cfg, _ = load_effective_config(tmp_path / "repo", org_config=str(org))
    assert cfg.locked == ["review.behavior.categories.security"]
    assert cfg.bounds["budget.max_cost_usd"].max == 0.50
    assert cfg.teams[0].match == ["payments-*", "billing-*"]
    assert cfg.teams[0].extends == "teams/payments.yml"
    assert cfg.models.allowed == ["bedrock:*"]


def test_repo_budget_above_org_bound_passes_in_0_1(tmp_path) -> None:
    # Governance bounds are NOT enforced in Sprint 0.1 (enforcement lands in 2.2).
    # A repo budget HIGHER than the org's bound is therefore accepted here; this
    # test documents that non-enforcement contract.
    org = _write_org(tmp_path, "bounds:\n  budget.max_cost_usd: { max: 0.50 }\n")
    repo = tmp_path / "repo"
    repo.mkdir()
    (repo / ".demerzel.yml").write_text("budget:\n  max_cost_usd: 5.00\n")

    # build_layers must not raise despite the repo exceeding the org bound.
    build_layers(repo, str(org), [])
    cfg, _ = load_effective_config(repo, org_config=str(org))
    assert cfg.budget.max_cost_usd == 5.00
    assert cfg.bounds["budget.max_cost_usd"].max == 0.50
