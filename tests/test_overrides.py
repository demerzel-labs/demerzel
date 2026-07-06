"""Override resolution: profiles, globs, inheritance, teams, CODEOWNERS, validation."""

import pytest

from prototype.overrides import OverrideError, OverridesConfig


def write_cfg(tmp_path, body: str, files: dict | None = None):
    """Write an overrides.cfg plus any referenced files under tmp_path (base_dir)."""
    cfg = tmp_path / "overrides.cfg"
    cfg.write_text(body)
    for rel, content in (files or {}).items():
        p = tmp_path / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(content)
    return cfg


BASE = """
[profile:base]
context = agents/base.md

[profile:foundations]
extends = base
system_prompt = agents/foundations/system.md
context = agents/foundations/ctx.md
model = opus
effort = high

[repos]
acme/core = foundations
acme/prod/* = foundations

[team:payments]
context = agents/teams/payments.md
instructions = Flag missing idempotency keys.
model = opus

[team:data-platform]
context = agents/teams/data.md

[teams:acme/core]
src/payments/** = payments
db/migrations/** = data-platform

[codeowners]
enabled = true
@acme/payments-team = payments
@acme/data-plat = data-platform
"""

FILES = {
    "agents/base.md": "base ctx",
    "agents/foundations/system.md": "foundations system",
    "agents/foundations/ctx.md": "foundations ctx",
    "agents/teams/payments.md": "payments ctx",
    "agents/teams/data.md": "data ctx",
}


def load(tmp_path, body=BASE, files=FILES):
    write_cfg(tmp_path, body, files)
    cfg = OverridesConfig.load(tmp_path / "overrides.cfg")
    cfg.validate(tmp_path)
    return cfg


def test_exact_repo_profile(tmp_path):
    cfg = load(tmp_path)
    res = cfg.resolve("acme/core")
    assert res.profile.name == "foundations"
    # inheritance: base context prepended, then foundations context
    assert res.context_sources[:2] == ["agents/base.md", "agents/foundations/ctx.md"]
    assert res.system_prompt_path == "agents/foundations/system.md"


def test_repo_glob_profile(tmp_path):
    cfg = load(tmp_path)
    res = cfg.resolve("acme/prod/data-layer")
    assert res.profile.name == "foundations"


def test_no_profile_for_unlisted_repo(tmp_path):
    cfg = load(tmp_path)
    res = cfg.resolve("other/repo")
    assert res.profile is None
    assert res.pin is None


def test_profile_pin_from_model(tmp_path):
    cfg = load(tmp_path)
    res = cfg.resolve("acme/core")
    assert res.pin is not None
    assert res.pin.model == "opus"
    assert res.pin.source == "profile"


def test_team_attribution_via_map(tmp_path):
    cfg = load(tmp_path)
    res = cfg.resolve(
        "acme/core",
        changed_files=["src/payments/charge.py", "db/migrations/001.sql", "README.md"],
    )
    assert set(res.matched_teams) == {"payments", "data-platform"}
    assert res.file_attribution["src/payments/charge.py"] == ["payments"]
    assert res.attribution_source["db/migrations/001.sql"] == "map"
    assert res.unowned_files == ["README.md"]


def test_profile_pin_beats_team_pin(tmp_path):
    cfg = load(tmp_path)
    # foundations profile pins opus; even with a team touched, profile wins
    res = cfg.resolve("acme/core", changed_files=["src/payments/charge.py"])
    assert res.pin.source == "profile"


def test_team_pin_when_no_profile_pin(tmp_path):
    body = BASE.replace("model = opus\neffort = high", "")  # drop profile pin
    cfg = load(tmp_path, body=body)
    res = cfg.resolve("acme/core", changed_files=["src/payments/charge.py"])
    assert res.pin is not None
    assert res.pin.source == "team:payments"
    assert res.pin.model == "opus"


def test_codeowners_attribution(tmp_path):
    files = dict(FILES)
    files["CODEOWNERS"] = "# comment\nsrc/api/** @acme/payments-team\ndocs/ @acme/data-plat\n"
    cfg = load(tmp_path, files=files)
    # api file not in explicit map -> falls to CODEOWNERS
    res = cfg.resolve(
        "acme/core",
        changed_files=["src/api/handler.py", "docs/readme.md"],
        repo_root=tmp_path,
    )
    assert res.file_attribution["src/api/handler.py"] == ["payments"]
    assert res.attribution_source["src/api/handler.py"] == "codeowners"
    assert res.file_attribution["docs/readme.md"] == ["data-platform"]


def test_explicit_map_wins_over_codeowners(tmp_path):
    files = dict(FILES)
    files["CODEOWNERS"] = "src/payments/** @acme/data-plat\n"  # conflicts with map
    cfg = load(tmp_path, files=files)
    res = cfg.resolve("acme/core", changed_files=["src/payments/charge.py"], repo_root=tmp_path)
    # explicit [teams:] map says payments; must win over CODEOWNERS' data-plat
    assert res.file_attribution["src/payments/charge.py"] == ["payments"]
    assert res.attribution_source["src/payments/charge.py"] == "map"


def test_trace_dict_shape(tmp_path):
    cfg = load(tmp_path)
    res = cfg.resolve("acme/core", changed_files=["src/payments/x.py"])
    t = res.to_trace()
    assert t["profile"] == "foundations"
    assert "payments" in t["matched_teams"]
    assert t["pin"]["source"] == "profile"


# ---- validation errors ----------------------------------------------------


def test_unknown_section_rejected(tmp_path):
    write_cfg(tmp_path, "[bogus]\nx = y\n")
    with pytest.raises(OverrideError, match="unknown section"):
        OverridesConfig.load(tmp_path / "overrides.cfg")


def test_unknown_profile_key_rejected(tmp_path):
    write_cfg(tmp_path, "[profile:x]\nnonsense = 1\n")
    with pytest.raises(OverrideError, match="unknown keys"):
        OverridesConfig.load(tmp_path / "overrides.cfg")


def test_repos_unknown_profile_rejected(tmp_path):
    write_cfg(tmp_path, "[repos]\nacme/core = ghost\n")
    cfg = OverridesConfig.load(tmp_path / "overrides.cfg")
    with pytest.raises(OverrideError, match="unknown profile"):
        cfg.validate(tmp_path)


def test_team_map_unknown_team_rejected(tmp_path):
    body = "[teams:acme/core]\nsrc/** = ghostteam\n"
    write_cfg(tmp_path, body)
    cfg = OverridesConfig.load(tmp_path / "overrides.cfg")
    with pytest.raises(OverrideError, match="unknown team"):
        cfg.validate(tmp_path)


def test_inheritance_cycle_rejected(tmp_path):
    body = "[profile:a]\nextends = b\n[profile:b]\nextends = a\n"
    write_cfg(tmp_path, body)
    cfg = OverridesConfig.load(tmp_path / "overrides.cfg")
    with pytest.raises(OverrideError, match="cycle"):
        cfg.validate(tmp_path)


def test_missing_referenced_file_is_not_a_validation_error(tmp_path):
    # Missing context/prompt files must NOT fail validation — they degrade gracefully
    # (skip + log) at assembly time so a broken override never blocks the review.
    body = "[profile:x]\ncontext = agents/missing.md\n[repos]\nr = x\n"
    write_cfg(tmp_path, body)
    cfg = OverridesConfig.load(tmp_path / "overrides.cfg")
    cfg.validate(tmp_path)  # structural-only: no raise for the missing file
    res = cfg.resolve("r")
    assert res.profile.name == "x"  # resolution still works


def test_missing_file_raises_on_load(tmp_path):
    with pytest.raises(OverrideError, match="not found"):
        OverridesConfig.load(tmp_path / "nope.cfg")
