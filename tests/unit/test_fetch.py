"""Fetcher: file:// and bare paths work (extends resolves next to its declaring
file); missing files and unsupported/unknown schemes error clearly; team configs
may not chain extends."""

from __future__ import annotations

from pathlib import Path

import pytest

from demerzel.config.errors import ConfigError
from demerzel.config.fetch import fetch
from demerzel.config.loader import build_layers, load_effective_config


def test_file_url_scheme(tmp_path) -> None:
    target = tmp_path / "org.yml"
    target.write_text("review:\n  tone: friendly\n")
    result = fetch(f"file://{target}", base_dir=Path("/nonexistent"))
    assert "friendly" in result.text
    assert result.source == str(target.resolve())


def test_bare_relative_path_resolves_to_base_dir(tmp_path) -> None:
    (tmp_path / "team.yml").write_text("review:\n  tone: teamtone\n")
    result = fetch("team.yml", base_dir=tmp_path)
    assert "teamtone" in result.text


def test_extends_relative_to_declaring_file(tmp_path) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    (repo / ".demerzel.yml").write_text("extends: team.yml\nreview:\n  tone: repotone\n")
    (repo / "team.yml").write_text("review:\n  tone: teamtone\n  behavior:\n    max_findings: 7\n")
    cfg, layers = load_effective_config(repo)
    # team layer contributes; repo tone still wins over team
    assert cfg.review.tone == "repotone"
    assert cfg.review.behavior.max_findings == 7
    assert [layer.kind for layer in layers] == ["defaults", "team", "repo"]


def test_missing_file_errors(tmp_path) -> None:
    with pytest.raises(ConfigError) as excinfo:
        build_layers(tmp_path / "repo", str(tmp_path / "nope.yml"), [])
    assert "config file not found" in str(excinfo.value)


def test_https_scheme_not_supported() -> None:
    with pytest.raises(ConfigError) as excinfo:
        fetch("https://example.com/org.yml", base_dir=Path("."))
    assert "not supported yet" in str(excinfo.value)
    assert "Sprint 4.1" in str(excinfo.value)


def test_s3_scheme_not_supported() -> None:
    with pytest.raises(ConfigError) as excinfo:
        fetch("s3://bucket/org.yml", base_dir=Path("."))
    assert "not supported yet" in str(excinfo.value)


def test_unknown_scheme_errors() -> None:
    with pytest.raises(ConfigError) as excinfo:
        fetch("ftp://host/org.yml", base_dir=Path("."))
    assert "unsupported config location scheme" in str(excinfo.value)


def test_team_config_with_extends_is_chaining_error(tmp_path) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    (repo / ".demerzel.yml").write_text("extends: team.yml\n")
    (repo / "team.yml").write_text("extends: grandparent.yml\n")
    (repo / "grandparent.yml").write_text("review:\n  tone: nope\n")
    with pytest.raises(ConfigError) as excinfo:
        build_layers(repo, None, [])
    assert "extends chaining is not supported" in str(excinfo.value)
