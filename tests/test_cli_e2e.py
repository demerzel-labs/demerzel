"""End-to-end CLI tests via main(argv) + DEMERZEL_RUNNER=fake. No AWS, no network.

Exercises the full wiring: overrides, routing, budget preflight, runner, secret-scan,
manifest, and every exit code (0 ok / 1 fatal / 2 budget exceeded).
"""

import json

import pytest

from demerzel.harness.bedrock import EXIT_BUDGET, EXIT_FATAL, EXIT_OK, compute_max_turns, main


@pytest.fixture
def base(tmp_path, monkeypatch):
    """A minimal working invocation setup rooted at tmp_path."""
    (tmp_path / "agents/configs").mkdir(parents=True)
    (tmp_path / "agents/prompts").mkdir(parents=True)
    cfg = tmp_path / "agents/configs/review.json"
    cfg.write_text(
        json.dumps(
            {
                "model": "sonnet",
                "task": "review",
                "runner": "strands",
                "inference": {"max_tokens": 16000, "effort": "high"},
                "system_prompt_file": "agents/prompts/default.md",
            }
        )
    )
    (tmp_path / "agents/prompts/default.md").write_text("Review the diff carefully.")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("Please review this diff:\n" + "x" * 2000)

    # Force the fake runner and make the budget preflight degrade to check_failed
    # (no boto3 call succeeds in the hermetic env) so the run proceeds.
    monkeypatch.setenv("DEMERZEL_RUNNER", "fake")
    monkeypatch.setenv("DEMERZEL_BUDGET_NAME", "does-not-exist-budget")
    monkeypatch.delenv("DEMERZEL_OVERRIDES", raising=False)
    monkeypatch.delenv("DEMERZEL_DEGRADE", raising=False)

    return {
        "cfg": cfg,
        "prompt": prompt,
        "out": tmp_path / "review.md",
        "trace": tmp_path / "trace.jsonl",
        "sysprompt": tmp_path / "system_prompt.md",
        "base_dir": tmp_path,
    }


def _argv(base, **extra):
    argv = [
        "--config",
        str(base["cfg"]),
        "--prompt-file",
        str(base["prompt"]),
        "--output",
        str(base["out"]),
        "--trace",
        str(base["trace"]),
        "--base-dir",
        str(base["base_dir"]),
        "--repo",
        "acme/core",
        "--diff-size",
        "2000",
        "--system-prompt-file",
        str(base["sysprompt"]),
    ]
    for k, v in extra.items():
        argv += [f"--{k.replace('_', '-')}", str(v)]
    return argv


def _events(trace_path):
    return [json.loads(line) for line in trace_path.read_text().splitlines() if line.strip()]


def test_happy_path_exit_0(base):
    rc = main(_argv(base))
    assert rc == EXIT_OK
    assert base["out"].read_text().strip()  # review written
    assert base["sysprompt"].read_text().startswith("You are Demerzel")  # baseline present

    events = _events(base["trace"])
    kinds = [e["event"] for e in events]
    assert kinds[0] == "run_start"
    assert "model_decision" in kinds
    assert "budget_check" in kinds
    assert "secret_scan" in kinds
    assert kinds[-1] == "run_end"
    end = events[-1]
    assert end["outcome"] == "ok"
    assert end["cost_usd"] > 0


def test_manifest_written_and_integrity(base):
    main(_argv(base))
    man_path = base["trace"].with_name("run_manifest.json")
    assert man_path.exists()
    man = json.loads(man_path.read_text())
    assert man["outcome"] == "ok"
    assert man["repo"] == "acme/core"
    roles = {a["role"]: a for a in man["artifacts"]}
    assert roles["review"]["exists"] is True
    # sha256 in manifest matches the actual review file
    import hashlib

    actual = hashlib.sha256(base["out"].read_bytes()).hexdigest()
    assert roles["review"]["sha256"] == actual


def test_artifacts_dir_places_manifest(base, tmp_path):
    art = tmp_path / "artifacts"
    art.mkdir()
    main(_argv(base, artifacts_dir=art))
    assert (art / "run_manifest.json").exists()


def test_raw_trace_written(base, tmp_path):
    raw = tmp_path / "raw.jsonl"
    main(_argv(base, raw_trace=raw))
    assert raw.exists()
    lines = [json.loads(x) for x in raw.read_text().splitlines() if x.strip()]
    assert any(x["kind"] == "model" for x in lines)


def test_fatal_path_exit_1_writes_empty_output_and_manifest(base, monkeypatch):
    monkeypatch.setenv("DEMERZEL_FAKE_RAISE", "boom in runner")
    rc = main(_argv(base))
    assert rc == EXIT_FATAL
    assert base["out"].read_text() == ""  # frozen contract: empty output on failure
    events = _events(base["trace"])
    kinds = [e["event"] for e in events]
    assert "error" in kinds
    assert kinds[-1] == "run_end"
    man = json.loads(base["trace"].with_name("run_manifest.json").read_text())
    assert man["outcome"] == "error"


def test_budget_exceeded_exit_2(base, monkeypatch):
    # Inject a fake budgets client via monkeypatching preflight's boto path is hard;
    # instead drive through the public seam: patch budget_mod.preflight.
    from demerzel import budget as budget_mod
    from demerzel.budget import BudgetStatus

    def fake_preflight(env=None, **kw):
        return BudgetStatus(
            budget_name="x",
            status="exceeded",
            actual=99,
            limit=100,
            pct=99.0,
            message="over budget",
        )

    monkeypatch.setattr(budget_mod, "preflight", fake_preflight)
    rc = main(_argv(base))
    assert rc == EXIT_BUDGET
    assert base["out"].read_text() == ""
    events = _events(base["trace"])
    assert events[-1]["outcome"] == "budget_exceeded"


def test_assembled_prompt_is_auditable(base, monkeypatch, tmp_path):
    # The final prompt must be auditable three ways: the --system-prompt-file artifact,
    # a self-contained `prompt_assembled` trace event (ordered sources + sha256), and
    # the `override_resolved` event showing which team matched. All must agree.
    (tmp_path / "overrides.cfg").write_text(
        "[profile:foundations]\n"
        "context = agents/prompts/default.md\n"
        "[repos]\n"
        "acme/core = foundations\n"
        "[team:records-sync]\n"
        "context = agents/prompts/default.md\n"
        "instructions = For records-sync files only, report Critical and High only.\n"
        "[teams:acme/core]\n"
        "src/records-sync/** = records-sync\n"
    )
    changed = tmp_path / "changed.txt"
    changed.write_text("src/records-sync/sync.py\n")
    monkeypatch.setenv("DEMERZEL_OVERRIDES", "1")

    rc = main(_argv(base, changed_files=changed))
    assert rc == EXIT_OK

    # 1) artifact: the literal composed prompt shows the team block.
    sysprompt = base["sysprompt"].read_text()
    assert "## Team: records-sync" in sysprompt
    assert "report Critical and High only" in sysprompt
    assert sysprompt.startswith("You are Demerzel")  # baseline intact

    events = _events(base["trace"])
    # 2) override_resolved: the team was matched and attributed to the file.
    ov = next(e for e in events if e["event"] == "override_resolved")
    assert "records-sync" in ov["matched_teams"]
    # 3) prompt_assembled: self-contained provenance + sha256 matching the artifact.
    pa = next(e for e in events if e["event"] == "prompt_assembled")
    assert "<team:records-sync:instructions>" in pa["sources"]
    import hashlib

    assert pa["sha256"] == hashlib.sha256(sysprompt.encode("utf-8")).hexdigest()


def test_overrides_applied_when_enabled(base, monkeypatch, tmp_path):
    # Write an overrides.cfg that pins opus for acme/core and adds a team.
    (tmp_path / "overrides.cfg").write_text(
        "[profile:foundations]\n"
        "model = opus\n"
        "context = agents/prompts/default.md\n"
        "[repos]\n"
        "acme/core = foundations\n"
        "[team:payments]\n"
        "context = agents/prompts/default.md\n"
        "[teams:acme/core]\n"
        "src/payments/** = payments\n"
    )
    changed = tmp_path / "changed.txt"
    changed.write_text("src/payments/charge.py\nREADME.md\n")
    monkeypatch.setenv("DEMERZEL_OVERRIDES", "1")

    rc = main(_argv(base, changed_files=changed))
    assert rc == EXIT_OK
    events = _events(base["trace"])
    ov = next(e for e in events if e["event"] == "override_resolved")
    assert ov["profile"] == "foundations"
    assert "payments" in ov["matched_teams"]
    decision = next(e for e in events if e["event"] == "model_decision")
    assert "opus" in decision["model_id"]  # profile pin honored
    assert decision["source"] == "profile"


def test_broken_override_degrades_to_default_review(base, monkeypatch, tmp_path):
    # A structurally broken overrides.cfg must NOT fail the run — it logs and falls
    # back to the default Demerzel review, which still posts.
    (tmp_path / "overrides.cfg").write_text(
        "[repos]\nacme/core = ghost-profile-that-does-not-exist\n"
    )
    monkeypatch.setenv("DEMERZEL_OVERRIDES", "1")
    rc = main(_argv(base))
    assert rc == EXIT_OK  # review still posted
    assert base["out"].read_text().strip()
    events = _events(base["trace"])
    assert any(e["event"] == "override_error" for e in events)
    # no profile applied -> default review
    assert not any(e["event"] == "override_resolved" for e in events)


def test_missing_context_file_still_posts_review(base, monkeypatch, tmp_path):
    (tmp_path / "overrides.cfg").write_text(
        "[profile:p]\ncontext = agents/gone.md\n[repos]\nacme/core = p\n"
    )
    monkeypatch.setenv("DEMERZEL_OVERRIDES", "1")
    rc = main(_argv(base))
    assert rc == EXIT_OK
    assert base["out"].read_text().strip()
    events = _events(base["trace"])
    # profile resolved fine; the missing file is logged, not fatal
    assert any(e["event"] == "override_resolved" for e in events)
    assert any(e["event"] == "prompt_files_missing" for e in events)


def test_degrade_panic_lever(base, monkeypatch):
    monkeypatch.setenv("DEMERZEL_DEGRADE", "1")
    main(_argv(base))
    events = _events(base["trace"])
    decision = next(e for e in events if e["event"] == "model_decision")
    assert decision["degraded"] is True


def test_quality_gate_runs_when_enabled(base, monkeypatch):
    monkeypatch.setenv("DEMERZEL_QUALITY_GATE", "1")
    monkeypatch.setenv("DEMERZEL_FAKE_OUTPUT", "## Findings\n- [ ] **Bug** app.py:1 — x\n\nAPPROVE")
    rc = main(_argv(base))
    assert rc == EXIT_OK
    events = _events(base["trace"])
    # a second model_decision for the gate stage, plus a gate event
    gate_decisions = [
        e for e in events if e["event"] == "model_decision" and e.get("stage") == "gate"
    ]
    assert len(gate_decisions) == 1
    assert "haiku" in gate_decisions[0]["model_id"]
    gate = next(e for e in events if e["event"] == "gate")
    assert gate["accepted"] is True
    # default fake gate is a faithful no-op: the finding survives
    assert "Bug" in base["out"].read_text()


def test_quality_gate_off_by_default(base):
    main(_argv(base))
    events = _events(base["trace"])
    assert not any(e["event"] == "gate" for e in events)


def test_quality_gate_failure_keeps_review(base, monkeypatch):
    monkeypatch.setenv("DEMERZEL_QUALITY_GATE", "1")
    monkeypatch.setenv("DEMERZEL_FAKE_OUTPUT", "## Findings\n- [ ] **Keep** a.py:1 — y\n\nAPPROVE")
    monkeypatch.setenv("DEMERZEL_FAKE_GATE_RAISE", "gate boom")
    rc = main(_argv(base))
    assert rc == EXIT_OK  # gate failure never fails the run
    assert "Keep" in base["out"].read_text()  # original review preserved


def test_broken_profile_config_type_degrades_not_crashes(base, monkeypatch, tmp_path):
    # A profile config-replacement with a wrong field type (max_turns: "x") must NOT
    # crash the run — it degrades to the base --config and still posts (PLAN sec 7.4).
    (tmp_path / "agents/configs/bad.json").write_text(json.dumps({"max_turns": "not-a-number"}))
    (tmp_path / "overrides.cfg").write_text(
        "[profile:p]\nconfig = agents/configs/bad.json\n[repos]\nacme/core = p\n"
    )
    monkeypatch.setenv("DEMERZEL_OVERRIDES", "1")
    rc = main(_argv(base))
    assert rc == EXIT_OK
    assert base["out"].read_text().strip()  # review still posted
    events = _events(base["trace"])
    assert any(e["event"] == "override_error" for e in events)


def test_typoed_pin_alias_degrades_not_crashes(base, monkeypatch, tmp_path):
    # A typo'd model alias in a profile pin must degrade to default routing, not exit 1.
    (tmp_path / "overrides.cfg").write_text(
        "[profile:p]\nmodel = opsu\ncontext = agents/prompts/default.md\n[repos]\nacme/core = p\n"
    )
    monkeypatch.setenv("DEMERZEL_OVERRIDES", "1")
    rc = main(_argv(base))
    assert rc == EXIT_OK
    assert base["out"].read_text().strip()
    events = _events(base["trace"])
    assert any(
        e["event"] == "override_error" and "routing pin failed" in e.get("message", "")
        for e in events
    )
    decision = next(e for e in events if e["event"] == "model_decision")
    assert decision["source"] == "default:review"  # fell back past the broken pin


def test_argparse_error_exits_fatal_not_budget(base):
    # A malformed invocation must not exit 2 (reserved for budget-exceeded).
    assert main(["--config", "only-this-flag"]) == EXIT_FATAL


def test_manifest_write_failure_does_not_lose_review(base, monkeypatch):
    # A manifest write failure must not fail the review or clobber it: the review is the
    # product; the manifest is a compliance sidecar. run_end stays the terminal event.
    from demerzel.trace import RunManifest

    monkeypatch.setenv(
        "DEMERZEL_FAKE_OUTPUT", "## Findings\n- a real, completed finding\n\nNEEDS_WORK"
    )

    def boom(self, path, outcome):
        raise RuntimeError("manifest write blew up after the review landed")

    monkeypatch.setattr(RunManifest, "write", boom)
    rc = main(_argv(base))
    assert rc == EXIT_OK  # review still succeeds
    assert "completed finding" in base["out"].read_text()  # NOT clobbered
    events = _events(base["trace"])
    assert sum(1 for e in events if e["event"] == "run_end") == 1
    assert events[-1]["event"] == "run_end"  # run_end stays terminal


def test_fatal_before_review_preserves_no_clobber_and_terminal_run_end(base, monkeypatch):
    # A genuine crash before any review: empty output (contract), run_end terminal, once.
    monkeypatch.setenv("DEMERZEL_FAKE_RAISE", "boom in runner")
    rc = main(_argv(base))
    assert rc == EXIT_FATAL
    assert base["out"].read_text() == ""
    events = _events(base["trace"])
    assert sum(1 for e in events if e["event"] == "run_end") == 1
    assert events[-1]["event"] == "run_end"


def test_stale_output_cleared_on_early_failure(base, monkeypatch):
    # If --output already holds a previous run's review and THIS run fails before
    # producing one, the stale review must not survive (downstream would post it).
    base["out"].write_text("## STALE previous review\n- old finding\n")
    monkeypatch.setenv("DEMERZEL_FAKE_RAISE", "boom before any review")
    rc = main(_argv(base))
    assert rc == EXIT_FATAL
    assert base["out"].read_text() == ""  # stale content gone, empty per contract


def test_gate_stage_routing_failure_keeps_review(base, monkeypatch):
    # A failure in the gate STAGE (routing/trace, outside run_quality_gate) must keep the
    # completed review and exit 0 — the gate can only help or no-op (PLAN sec 6.1).
    from demerzel import harness
    from demerzel.routing import RoutingError

    real_resolve = harness.bedrock.resolve_routing

    def resolve_or_boom(profile, **kw):
        if profile.task == "gate":
            raise RoutingError("gate tier misconfigured")
        return real_resolve(profile, **kw)

    monkeypatch.setattr(harness.bedrock, "resolve_routing", resolve_or_boom)
    monkeypatch.setenv("DEMERZEL_QUALITY_GATE", "1")
    monkeypatch.setenv("DEMERZEL_FAKE_OUTPUT", "## Findings\n- keep me\n\nAPPROVE")
    rc = main(_argv(base))
    assert rc == EXIT_OK
    assert "keep me" in base["out"].read_text()
    events = _events(base["trace"])
    gate = next(e for e in events if e["event"] == "gate")
    assert gate["accepted"] is False


def test_provenance_and_phase_events(base):
    main(_argv(base))
    events = _events(base["trace"])
    start = next(e for e in events if e["event"] == "run_start")
    for key in ("argv", "routing_digest", "strands_version", "harness_git_sha"):
        assert key in start
    phases = {e["phase"] for e in events if e["event"] == "phase"}
    assert {"resolve", "budget", "agent", "finalize"} <= phases

    man = json.loads(base["trace"].with_name("run_manifest.json").read_text())
    assert man["routing_digest"]  # digest of the routing table that actually ran
    assert man["config_effective_sha256"]
    assert man["model_decision"]["rationale"]  # verbatim decision, not a subset
    assert set(man["usage"]) == {"input", "output", "cache_read", "cache_write"}


def test_secret_scan_redacts_output(base, monkeypatch):
    monkeypatch.setenv("DEMERZEL_FAKE_OUTPUT", "leak AKIAIOSFODNN7EXAMPLE in review")
    main(_argv(base))
    out = base["out"].read_text()
    assert "AKIAIOSFODNN7EXAMPLE" not in out
    assert "[REDACTED-SECRET]" in out
    events = _events(base["trace"])
    scan = next(e for e in events if e["event"] == "secret_scan")
    assert scan["fired"] is True


# ---- turn scaling unit ----


@pytest.mark.parametrize(
    "size,cap,expected",
    [
        (100, 50, 10),
        (10_000, 50, 15),
        (30_000, 50, 25),
        (100_000, 50, 35),
        (500_000, 50, 50),
        (500_000, 20, 20),  # never exceed config cap
    ],
)
def test_compute_max_turns(size, cap, expected):
    assert compute_max_turns("x" * size, cap, diff_size=size) == expected
