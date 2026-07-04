"""--set override grammar: values parse via yaml.safe_load; malformed pairs are
usage errors; a flow-list --set flows through the full load and REPLACES."""

from __future__ import annotations

import pytest

from demerzel.config.errors import ConfigError
from demerzel.config.loader import build_cli_dict, load_effective_config


def test_scalar_string() -> None:
    assert build_cli_dict(["review.tone=concise"]) == {"review": {"tone": "concise"}}


def test_bool_number_and_float() -> None:
    result = build_cli_dict(["flag=true", "n=5", "f=0.5"])
    assert result == {"flag": True, "n": 5, "f": 0.5}
    assert result["flag"] is True


def test_flow_list() -> None:
    assert build_cli_dict(["review.behavior.focus=[security, correctness]"]) == {
        "review": {"behavior": {"focus": ["security", "correctness"]}}
    }


def test_quoted_string() -> None:
    assert build_cli_dict(['review.tone="be nice"']) == {"review": {"tone": "be nice"}}


def test_missing_equals_is_usage_error() -> None:
    with pytest.raises(ConfigError) as excinfo:
        build_cli_dict(["review.tone"])
    assert "expected KEY=VALUE" in str(excinfo.value)


def test_empty_key_is_usage_error() -> None:
    with pytest.raises(ConfigError) as excinfo:
        build_cli_dict(["=oops"])
    assert "empty key" in str(excinfo.value)


def test_extends_rejected_in_cli_layer(tmp_path) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    with pytest.raises(ConfigError) as excinfo:
        load_effective_config(repo, cli_overrides=["extends=team.yml"])
    err = excinfo.value
    assert "'extends' cannot be set from the CLI" in str(err)
    assert err.source == "<cli>"
    assert err.key == "extends"


def test_cli_flow_list_replaces_through_full_load(tmp_path) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    (repo / ".demerzel.yml").write_text("review:\n  behavior:\n    focus: [security]\n")
    cfg, _ = load_effective_config(
        repo, cli_overrides=["review.behavior.focus=[performance, style]"]
    )
    assert cfg.review.behavior.focus == ["performance", "style"]
