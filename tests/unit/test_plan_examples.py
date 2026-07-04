"""The two verbatim PLAN.md example YAMLs parse under their schemas (parse-only;
the s3:// extends URL is a plain string at the schema level, so no fetching)."""

from __future__ import annotations

import yaml

from demerzel.config.schema import OrgConfig, RepoConfig
from tests.conftest import PLAN_EXAMPLES


def test_plan_repo_example_parses() -> None:
    raw = yaml.safe_load((PLAN_EXAMPLES / "repo.yml").read_text())
    model = RepoConfig.model_validate(raw)
    assert model.extends == "s3://org-bucket/demerzel/teams/payments.yml"
    assert [rule.id for rule in model.review.rules] == ["no-foo-client", "migration-safety"]
    # the `never` threshold appears in a path-scoped override
    assert model.review.overrides[1].behavior is not None
    assert model.review.overrides[1].behavior.fail_threshold == "never"


def test_plan_org_example_parses() -> None:
    raw = yaml.safe_load((PLAN_EXAMPLES / "org.yml").read_text())
    model = OrgConfig.model_validate(raw)
    assert model.locked == ["review.behavior.categories.security"]
    assert model.models.allowed == ["bedrock:*"]
    assert model.teams[0].extends == "s3://org-bucket/demerzel/teams/payments.yml"
