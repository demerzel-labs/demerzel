"""Quality gate: pure helpers, boundary guards, and the never-lose-findings fallback."""

from demerzel.gate import (
    GateResult,
    apply_gate_result,
    build_gate_prompt,
    gate_system_prompt,
    run_quality_gate,
)
from demerzel.pricing import CostMeter
from demerzel.runner.fake import FakeRunner
from demerzel.trace import TraceWriter
from demerzel.types import AgentConfig, ModelDecision


def _decision():
    return ModelDecision(model_id="us.anthropic.claude-haiku-4-5", effort=None, max_tokens=8192)


def test_build_prompt_contains_diff_and_review():
    p = build_gate_prompt("diff-body", "review-body")
    assert "diff-body" in p
    assert "review-body" in p
    assert "Draft review to clean up" in p


def test_build_prompt_truncates_large_diff():
    big = "x" * 200_000
    p = build_gate_prompt(big, "r", max_diff_bytes=1000)
    assert "[diff truncated]" in p
    assert len(p) < 10_000


def test_system_prompt_states_boundaries():
    s = gate_system_prompt()
    assert "NOT" in s or "not" in s
    assert "Critical" in s  # never downgrade Critical/High


def test_apply_accepts_cleaned_review():
    original = "## Findings\n- [ ] **X** file.py:1 — a real thing\n\nAPPROVE"
    cleaned = "## Findings\n- [ ] **X** file.py:1 — a real thing\nAPPROVE"
    result = GateResult(review=cleaned, notes="tightened")
    text, accepted, reason = apply_gate_result(original, result)
    assert accepted is True
    assert reason is None
    assert "APPROVE" in text


def test_apply_rejects_empty_output():
    original = "a substantial review with findings " * 10
    text, accepted, reason = apply_gate_result(original, GateResult(review="  "))
    assert accepted is False
    assert text == original
    assert "empty" in reason


def test_apply_rejects_suspicious_shrink_without_drops():
    original = "a substantial review with several findings " * 20
    # gate returns a tiny stub and records no drops -> distrust, keep original
    text, accepted, reason = apply_gate_result(original, GateResult(review="ok", dropped=[]))
    assert accepted is False
    assert text == original
    assert "shrink" in reason


def test_apply_allows_shrink_when_drops_recorded():
    original = "finding one is here " * 30
    result = GateResult(
        review="finding one is here",
        dropped=[{"title": "dup", "reason": "duplicate"}] * 5,
    )
    text, accepted, reason = apply_gate_result(original, result)
    assert accepted is True


def test_run_quality_gate_records_usage_and_trace(tmp_path):
    trace = TraceWriter(tmp_path / "t.jsonl")
    meter = CostMeter("us.anthropic.claude-haiku-4-5")
    original = "## Findings\n- [ ] **Bug** app.py:10 — off by one\n\nREQUEST CHANGES"
    out = run_quality_gate(
        diff="@@ app.py @@\n+ code",
        review=original,
        runner=FakeRunner(),
        decision=_decision(),
        cfg=AgentConfig(),
        trace=trace,
        meter=meter,
        env={},  # default fake gate echoes the draft unchanged
    )
    trace.close()
    assert "Bug" in out  # no findings lost
    assert meter.cost_usd > 0  # gate tokens counted against the run budget
    import json

    events = [json.loads(x) for x in (tmp_path / "t.jsonl").read_text().splitlines() if x.strip()]
    gate_events = [e for e in events if e["event"] == "gate"]
    assert len(gate_events) == 1
    assert gate_events[0]["accepted"] is True


def test_run_quality_gate_survives_runner_error(tmp_path):
    trace = TraceWriter(tmp_path / "t.jsonl")
    meter = CostMeter("us.anthropic.claude-haiku-4-5")
    original = "## Findings\n- [ ] **Bug** app.py:10 — off by one\n\nREQUEST CHANGES"
    out = run_quality_gate(
        diff="d",
        review=original,
        runner=FakeRunner(),
        decision=_decision(),
        cfg=AgentConfig(),
        trace=trace,
        meter=meter,
        env={"DEMERZEL_FAKE_GATE_RAISE": "boom"},
    )
    trace.close()
    assert out == original  # gate failure returns the original review unchanged
    import json

    events = [json.loads(x) for x in (tmp_path / "t.jsonl").read_text().splitlines() if x.strip()]
    gate = next(e for e in events if e["event"] == "gate")
    assert gate["accepted"] is False
    assert "error" in gate["reason"]


def test_run_quality_gate_records_drops(tmp_path):
    trace = TraceWriter(tmp_path / "t.jsonl")
    meter = CostMeter("us.anthropic.claude-haiku-4-5")
    out = run_quality_gate(
        diff="d",
        review="## Findings\n- [ ] **Real** a.py:1 — keep\n\nAPPROVE",
        runner=FakeRunner(),
        decision=_decision(),
        cfg=AgentConfig(),
        trace=trace,
        meter=meter,
        env={
            "DEMERZEL_FAKE_GATE_REVIEW": "## Findings\n- [ ] **Real** a.py:1 — keep\n\nAPPROVE",
            "DEMERZEL_FAKE_GATE_DROPPED": "Phantom:bad_line_ref,DupNit:duplicate",
        },
    )
    trace.close()
    import json

    events = [json.loads(x) for x in (tmp_path / "t.jsonl").read_text().splitlines() if x.strip()]
    gate = next(e for e in events if e["event"] == "gate")
    assert {d["title"] for d in gate["dropped"]} == {"Phantom", "DupNit"}
    assert "Real" in out


def test_empty_review_skips_gate(tmp_path):
    trace = TraceWriter(tmp_path / "t.jsonl")
    meter = CostMeter("us.anthropic.claude-haiku-4-5")
    out = run_quality_gate(
        diff="d",
        review="",
        runner=FakeRunner(),
        decision=_decision(),
        cfg=AgentConfig(),
        trace=trace,
        meter=meter,
        env={},
    )
    trace.close()
    assert out == ""
