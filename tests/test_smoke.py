"""Scaffold smoke test: the package imports and core types are constructible."""

import demerzel
from demerzel.types import AgentConfig, ModelDecision, TaskProfile


def test_version():
    assert demerzel.__version__


def test_core_types_construct():
    profile = TaskProfile(task="review", diff_size=1234, repo="acme/core")
    assert profile.changed_files == []

    decision = ModelDecision(model_id="x", effort="high", max_tokens=16000)
    assert decision.degraded is False

    cfg = AgentConfig()
    assert cfg.runner == "strands"
    assert cfg.raw == {}
