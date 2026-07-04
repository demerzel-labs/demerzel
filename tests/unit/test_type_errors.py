"""Type/enum validation: errors name file + key; the `never` sentinel is a valid
threshold but not a valid rule severity; assume_role needs an ARN; version must be 1."""

from __future__ import annotations

import pytest

from demerzel.config.errors import ConfigError
from demerzel.config.loader import build_layers, load_effective_config


def _write_repo(tmp_path, body: str):
    repo = tmp_path / "repo"
    repo.mkdir()
    (repo / ".demerzel.yml").write_text(body)
    return repo


def test_type_error_names_file_and_key(tmp_path) -> None:
    repo = _write_repo(tmp_path, "review:\n  behavior:\n    max_findings: not-an-int\n")
    with pytest.raises(ConfigError) as excinfo:
        build_layers(repo, None, [])
    err = excinfo.value
    assert "review.behavior.max_findings" in str(err)
    assert err.source is not None and err.source.endswith(".demerzel.yml")


def test_never_rejected_as_rule_severity(tmp_path) -> None:
    repo = _write_repo(
        tmp_path,
        "review:\n  rules:\n    - id: r1\n      instruction: x\n      severity: never\n",
    )
    with pytest.raises(ConfigError) as excinfo:
        build_layers(repo, None, [])
    assert "review.rules[0].severity" in str(excinfo.value)


def test_never_accepted_as_threshold(tmp_path) -> None:
    repo = _write_repo(tmp_path, "review:\n  behavior:\n    fail_threshold: never\n")
    cfg, _ = load_effective_config(repo)
    assert cfg.review.behavior.fail_threshold == "never"


def test_assume_role_without_arn_rejected(tmp_path) -> None:
    repo = _write_repo(tmp_path, "providers:\n  bedrock:\n    auth: assume_role\n")
    with pytest.raises(ConfigError) as excinfo:
        build_layers(repo, None, [])
    assert "assume_role_arn" in str(excinfo.value)


def test_version_not_one_rejected(tmp_path) -> None:
    repo = _write_repo(tmp_path, "version: 2\n")
    with pytest.raises(ConfigError) as excinfo:
        build_layers(repo, None, [])
    assert "version" in str(excinfo.value)
