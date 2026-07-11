"""Sandbox tier detection: override, probe, declared tiers, graceful default."""

from demerzel import sandbox


def test_explicit_override_wins():
    assert sandbox.detect_tier({"DEMERZEL_SANDBOX_TIER": "B"}, can_unshare=lambda: True) == "B"


def test_invalid_override_ignored_then_probes():
    assert sandbox.detect_tier({"DEMERZEL_SANDBOX_TIER": "Z"}, can_unshare=lambda: True) == "A"


def test_probe_success_is_tier_a():
    assert sandbox.detect_tier({}, can_unshare=lambda: True) == "A"


def test_declared_firewall_is_tier_b():
    assert sandbox.detect_tier({"DEMERZEL_EGRESS_FIREWALL": "1"}, can_unshare=lambda: False) == "B"


def test_declared_ro_mount_is_tier_c():
    assert sandbox.detect_tier({"DEMERZEL_REPO_RO_MOUNT": "1"}, can_unshare=lambda: False) == "C"


def test_no_capability_is_tier_d():
    assert sandbox.detect_tier({}, can_unshare=lambda: False) == "D"


def test_command_prefix_only_wraps_tier_a():
    assert sandbox.command_prefix("A") == ["unshare", "-rn", "--"]
    assert sandbox.command_prefix("B") == []
    assert sandbox.command_prefix("D") == []


def test_probe_unshare_missing_binary(monkeypatch):
    monkeypatch.setattr(sandbox.shutil, "which", lambda _: None)
    assert sandbox._probe_unshare() is False


def test_probe_unshare_handles_failure(monkeypatch):
    monkeypatch.setattr(sandbox.shutil, "which", lambda _: "/usr/bin/unshare")

    def boom(*a, **k):
        raise OSError("nope")

    assert sandbox._probe_unshare(run=boom) is False


def test_describe_covers_all_tiers():
    for t in sandbox.VALID_TIERS:
        assert sandbox.describe(t)
