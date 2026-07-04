"""Unknown keys at every layer (and nested in rules/overrides) are hard errors
that name the source and the dotted key."""

from __future__ import annotations

import pytest

from demerzel.config.errors import ConfigError
from demerzel.config.loader import build_layers


def test_unknown_key_in_org_file(tmp_path) -> None:
    org = tmp_path / "org.yml"
    org.write_text("bogus: 1\n")
    with pytest.raises(ConfigError) as excinfo:
        build_layers(tmp_path / "repo", str(org), [])
    err = excinfo.value
    assert "Unknown config key 'bogus'" in str(err)
    assert err.source is not None and err.source.endswith("org.yml")
    assert err.key == "bogus"


def test_unknown_key_in_repo_file(tmp_path) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    (repo / ".demerzel.yml").write_text("review:\n  bogus: 1\n")
    with pytest.raises(ConfigError) as excinfo:
        build_layers(repo, None, [])
    assert "Unknown config key 'review.bogus'" in str(excinfo.value)
    assert excinfo.value.key == "review.bogus"


def test_unknown_key_in_team_file(tmp_path) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    (repo / ".demerzel.yml").write_text("extends: team.yml\n")
    (repo / "team.yml").write_text("review:\n  bogus: 1\n")
    with pytest.raises(ConfigError) as excinfo:
        build_layers(repo, None, [])
    assert "Unknown config key 'review.bogus'" in str(excinfo.value)
    assert excinfo.value.source is not None and excinfo.value.source.endswith("team.yml")


def test_unknown_key_in_cli_override(tmp_path) -> None:
    with pytest.raises(ConfigError) as excinfo:
        build_layers(tmp_path / "repo", None, ["review.bogus=1"])
    err = excinfo.value
    assert "Unknown config key 'review.bogus' in <cli>" in str(err)
    assert err.source == "<cli>"


def test_unknown_key_nested_in_rule(tmp_path) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    (repo / ".demerzel.yml").write_text(
        "review:\n"
        "  rules:\n"
        "    - id: r1\n"
        "      instruction: do a thing\n"
        "      severty: error\n"  # typo'd key
    )
    with pytest.raises(ConfigError) as excinfo:
        build_layers(repo, None, [])
    assert "review.rules[0].severty" in str(excinfo.value)


def test_unknown_key_nested_in_override(tmp_path) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    (repo / ".demerzel.yml").write_text(
        "review:\n"
        "  overrides:\n"
        "    - paths: [services/payments/**]\n"
        "      behaviour: {}\n"  # British spelling -> unknown key
    )
    with pytest.raises(ConfigError) as excinfo:
        build_layers(repo, None, [])
    assert "review.overrides[0].behaviour" in str(excinfo.value)
