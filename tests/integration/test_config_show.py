"""`demerzel config show` end to end against examples/ via typer's CliRunner."""

from __future__ import annotations

import json

from typer.testing import CliRunner

from demerzel.cli import app
from tests.conftest import EXAMPLES

runner = CliRunner()

REPO_ROOT = str(EXAMPLES / "repo")
ORG_CONFIG = str(EXAMPLES / "org" / "demerzel.yml")


def test_config_show_yaml_layers_and_merge() -> None:
    result = runner.invoke(
        app, ["config", "show", "--repo-root", REPO_ROOT, "--org-config", ORG_CONFIG]
    )
    assert result.exit_code == 0, result.output
    lines = result.output.splitlines()

    header = lines[0]
    assert header.startswith("# layers: defaults -> org (")
    assert "team (" in header
    assert "repo (" in header

    # repo's concise tone overrides the org's formal tone
    assert "tone: concise" in result.output
    # team rule and repo rule both present (rules APPEND)
    assert "id: no-plaintext-secrets" in result.output  # from team
    assert "id: no-foo-client" in result.output  # from repo
    # governance carried through from org
    assert "bedrock:*" in result.output


def test_config_show_json_is_valid() -> None:
    result = runner.invoke(
        app,
        [
            "config",
            "show",
            "--repo-root",
            REPO_ROOT,
            "--org-config",
            ORG_CONFIG,
            "--format",
            "json",
        ],
    )
    assert result.exit_code == 0, result.output
    data = json.loads(result.output)
    assert data["review"]["tone"] == "concise"
    assert data["models"]["allowed"] == ["bedrock:*"]
    rule_ids = [rule["id"] for rule in data["review"]["rules"]]
    assert "no-plaintext-secrets" in rule_ids
    assert "no-foo-client" in rule_ids
    # override entries dump as authored (no materialized defaults)
    payments_override = data["review"]["overrides"][0]
    assert set(payments_override) == {"paths", "behavior", "models"}


def test_config_show_unknown_key_exits_2(tmp_path) -> None:
    result = runner.invoke(
        app, ["config", "show", "--repo-root", str(tmp_path), "--set", "review.bogus=1"]
    )
    assert result.exit_code == 2
    assert "Unknown config key 'review.bogus'" in result.output


def test_config_show_nonexistent_repo_root_exits_2() -> None:
    result = runner.invoke(app, ["config", "show", "--repo-root", "/nonexistent/dir"])
    assert result.exit_code == 2
    # rich wraps the message; assert on a fragment that stays on one line
    assert "Invalid value for '--repo-root'" in result.output


def test_config_show_json_has_no_layer_comment() -> None:
    result = runner.invoke(
        app,
        [
            "config",
            "show",
            "--repo-root",
            REPO_ROOT,
            "--org-config",
            ORG_CONFIG,
            "--format",
            "json",
        ],
    )
    assert result.exit_code == 0
    assert not result.output.lstrip().startswith("#")
