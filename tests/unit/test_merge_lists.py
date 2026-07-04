"""List merge policy: rules/ignore_paths/overrides APPEND (lower first);
focus/sections REPLACE wholesale."""

from __future__ import annotations

from tests.conftest import resolve_kinds


def test_rules_append_lower_first() -> None:
    cfg = resolve_kinds(
        {
            "team": {"review": {"rules": [{"id": "team-rule", "instruction": "t"}]}},
            "repo": {"review": {"rules": [{"id": "repo-rule", "instruction": "r"}]}},
        }
    )
    assert [rule.id for rule in cfg.review.rules] == ["team-rule", "repo-rule"]


def test_ignore_paths_append_lower_first() -> None:
    cfg = resolve_kinds(
        {
            "team": {"review": {"behavior": {"ignore_paths": ["team/**"]}}},
            "repo": {"review": {"behavior": {"ignore_paths": ["repo/**"]}}},
        }
    )
    assert cfg.review.behavior.ignore_paths == ["team/**", "repo/**"]


def test_overrides_append_lower_first() -> None:
    cfg = resolve_kinds(
        {
            "team": {"review": {"overrides": [{"paths": ["team/**"]}]}},
            "repo": {"review": {"overrides": [{"paths": ["repo/**"]}]}},
        }
    )
    assert [entry.paths for entry in cfg.review.overrides] == [["team/**"], ["repo/**"]]


def test_focus_replaces_wholesale() -> None:
    cfg = resolve_kinds(
        {
            "team": {"review": {"behavior": {"focus": ["security", "correctness"]}}},
            "repo": {"review": {"behavior": {"focus": ["performance"]}}},
        }
    )
    assert cfg.review.behavior.focus == ["performance"]


def test_sections_replace_wholesale() -> None:
    cfg = resolve_kinds(
        {
            "team": {"review": {"output": {"sections": ["summary", "findings", "nitpicks"]}}},
            "repo": {"review": {"output": {"sections": ["summary"]}}},
        }
    )
    assert cfg.review.output.sections == ["summary"]
