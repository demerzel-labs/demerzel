"""Hermetic tests for the Strands runner — no AWS, no network.

Every test drives the loop through the `model=` injection seam with a scripted
`strands.models.Model` subclass, or drives the governor's hook callbacks directly with
synthetic events. The StreamEvent sequence yielded by ScriptedModel mirrors the real
strands stream format (verified at runtime against strands-agents 1.45.0):
messageStart -> contentBlock(Start/Delta/Stop) -> messageStop{stopReason} ->
metadata{usage, metrics}.
"""

from __future__ import annotations

import json

import pytest

strands = pytest.importorskip("strands")

from strands import Agent, tool  # noqa: E402
from strands.models import Model  # noqa: E402
from strands.types.agent import Limits  # noqa: E402

from demerzel.budget import COST_CEILING_PROMPT, CostCeiling  # noqa: E402
from demerzel.pricing import CostMeter  # noqa: E402
from demerzel.runner.strands import StrandsRunner, _Governor, _model_kwargs  # noqa: E402
from demerzel.trace import TraceWriter  # noqa: E402
from demerzel.types import AgentConfig, ModelDecision  # noqa: E402

# --- scripted model -------------------------------------------------------------


class ScriptedModel(Model):
    """A Model whose stream() replays a canned list of StreamEvent lists (one per
    model call). Implements the four abstract methods a hermetic loop needs."""

    def __init__(self, scripts: list[list[dict]]):
        self._scripts = scripts
        self._call = 0
        self._config: dict = {}

    def get_config(self) -> dict:
        return self._config

    def update_config(self, **kwargs) -> None:
        self._config.update(kwargs)

    async def structured_output(self, *args, **kwargs):  # pragma: no cover
        raise NotImplementedError

    async def count_tokens(self, *args, **kwargs) -> int:  # pragma: no cover
        return 0

    async def stream(self, messages, tool_specs=None, system_prompt=None, **kwargs):
        events = self._scripts[self._call]
        self._call += 1
        for event in events:
            yield event


def _text_answer(text: str, usage: dict) -> list[dict]:
    return [
        {"messageStart": {"role": "assistant"}},
        {"contentBlockDelta": {"delta": {"text": text}}},
        {"contentBlockStop": {}},
        {"messageStop": {"stopReason": "end_turn"}},
        {"metadata": {"usage": usage, "metrics": {"latencyMs": 42}}},
    ]


def _tool_round(tool_use_id: str, name: str, tool_input: dict, usage: dict) -> list[dict]:
    return [
        {"messageStart": {"role": "assistant"}},
        {"contentBlockStart": {"start": {"toolUse": {"toolUseId": tool_use_id, "name": name}}}},
        {"contentBlockDelta": {"delta": {"toolUse": {"input": json.dumps(tool_input)}}}},
        {"contentBlockStop": {}},
        {"messageStop": {"stopReason": "tool_use"}},
        {"metadata": {"usage": usage, "metrics": {"latencyMs": 30}}},
    ]


def _cfg(**kw) -> AgentConfig:
    base = dict(max_turns=10, timeout_s=2700)
    base.update(kw)
    return AgentConfig(**base)


def _decision(model_id="us.anthropic.claude-sonnet-4-6", effort="high", max_tokens=8192):
    return ModelDecision(model_id=model_id, effort=effort, max_tokens=max_tokens)


def _read_events(path):
    events = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                events.append(json.loads(line))
    return events


# --- no-tool run ----------------------------------------------------------------


def test_no_tool_run(tmp_path):
    usage = {"inputTokens": 1000, "outputTokens": 400, "totalTokens": 1400}
    model = ScriptedModel([_text_answer("## Summary\nLooks good. APPROVE", usage)])
    trace = TraceWriter(tmp_path / "t.jsonl")
    meter = CostMeter("us.anthropic.claude-sonnet-4-6")

    result = StrandsRunner().run(
        system_prompt="you are a reviewer",
        user_message="review this diff",
        decision=_decision(),
        cfg=_cfg(),
        tools_cwd=str(tmp_path),
        trace=trace,
        meter=meter,
        env={},
        model=model,
    )
    trace.close()

    assert result.final_text == "## Summary\nLooks good. APPROVE"

    # meter recorded the injected usage
    assert meter.input == 1000
    assert meter.output == 400
    assert meter.cost_usd > 0

    # shared stats keys (match fake.py) + outcome
    for key in (
        "turns",
        "max_turns",
        "tool_calls",
        "blocked_tool_calls",
        "stop_reason",
        "total_s",
        "outcome",
        "input_tokens",
        "output_tokens",
        "cache_read_tokens",
        "cache_write_tokens",
        "cost_usd",
    ):
        assert key in result.stats
    assert result.stats["outcome"] == "ok"
    assert result.stats["turns"] == 1
    assert result.stats["stop_reason"] == "end_turn"
    assert result.stats["cost_usd"] > 0

    # a `turn` event was emitted with the fake.py field vocabulary
    events = _read_events(tmp_path / "t.jsonl")
    turns = [e for e in events if e["event"] == "turn"]
    assert len(turns) == 1
    turn = turns[0]
    for key in (
        "turn",
        "stop_reason",
        "latency_ms",
        "input_tokens",
        "output_tokens",
        "cache_read_tokens",
        "cache_write_tokens",
        "cumulative_cost",
    ):
        assert key in turn
    assert turn["input_tokens"] == 1000
    assert turn["output_tokens"] == 400


def test_usage_reconciliation_no_double_count(tmp_path):
    """Totals equal the injected usage exactly — per-call recording plus end-of-run
    reconciliation must not double-count."""
    usage = {
        "inputTokens": 1234,
        "outputTokens": 567,
        "totalTokens": 1801,
        "cacheReadInputTokens": 800,
        "cacheWriteInputTokens": 100,
    }
    model = ScriptedModel([_text_answer("done", usage)])
    trace = TraceWriter(tmp_path / "t.jsonl")
    meter = CostMeter("us.anthropic.claude-sonnet-4-6")

    result = StrandsRunner().run(
        system_prompt="s",
        user_message="u",
        decision=_decision(),
        cfg=_cfg(),
        tools_cwd=str(tmp_path),
        trace=trace,
        meter=meter,
        env={},
        model=model,
    )
    trace.close()

    assert meter.input == 1234
    assert meter.output == 567
    assert meter.cache_read == 800
    assert meter.cache_write == 100
    assert result.stats["input_tokens"] == 1234
    assert result.stats["output_tokens"] == 567
    assert result.stats["cache_read_tokens"] == 800
    assert result.stats["cache_write_tokens"] == 100


def test_cache_tokens_prove_caching_hit(tmp_path):
    usage = {"inputTokens": 10, "outputTokens": 5, "cacheReadInputTokens": 4096}
    model = ScriptedModel([_text_answer("cached", usage)])
    trace = TraceWriter(tmp_path / "t.jsonl")
    meter = CostMeter("us.anthropic.claude-sonnet-4-6")
    StrandsRunner().run(
        system_prompt="s",
        user_message="u",
        decision=_decision(),
        cfg=_cfg(),
        tools_cwd=str(tmp_path),
        trace=trace,
        meter=meter,
        env={},
        model=model,
    )
    trace.close()
    turn = next(e for e in _read_events(tmp_path / "t.jsonl") if e["event"] == "turn")
    assert turn["cache_read_tokens"] == 4096


# --- full tool round (real loop, trivial in-test tool) --------------------------


def test_tool_round_traces_and_counts(tmp_path):
    """A scripted tool-use round through a real Agent + governor. The trivial in-test
    tool stashes into governor.bash_results the same way the real bash wrapper does, so
    the AfterToolCallEvent -> `tool_call` trace path is exercised without bash.py."""
    trace = TraceWriter(tmp_path / "t.jsonl")
    meter = CostMeter("us.anthropic.claude-sonnet-4-6")
    governor = _Governor(
        meter=meter,
        trace=trace,
        ceiling=CostCeiling(100.0),  # far above any spend here
        timeout_s=9999,
        start=__import__("time").monotonic(),
    )

    # Strands 1.45 runs synchronous decorated tools in the event loop's default
    # executor, whose shutdown can deadlock across repeated Agent tests. The production
    # BashTool's synchronous behavior has its own suite; this fixture covers Agent/tool
    # hooks and trace accounting.
    @tool(context=True)
    async def bash(command: str, timeout: int = 120, tool_context=None) -> str:
        """Trivial reviewer tool."""
        tuid = (tool_context.tool_use or {}).get("toolUseId") if tool_context else None
        governor.bash_results[tuid] = {
            "command": command,
            "blocked": False,
            "block_reason": None,
            "exit_code": 0,
            "duration_ms": 3,
            "output_bytes": 11,
            "truncated": False,
        }
        return "file: a.py"

    model = ScriptedModel(
        [
            _tool_round("tu1", "bash", {"command": "ls"}, {"inputTokens": 50, "outputTokens": 10}),
            _text_answer("final review", {"inputTokens": 80, "outputTokens": 20}),
        ]
    )
    agent = Agent(model=model, tools=[bash], hooks=[governor], system_prompt="s")
    result = agent("go", limits=Limits(turns=10))
    trace.close()

    assert governor.tool_calls == 1
    assert governor.blocked_tool_calls == 0
    tool_events = [e for e in _read_events(tmp_path / "t.jsonl") if e["event"] == "tool_call"]
    assert len(tool_events) == 1
    tc = tool_events[0]
    assert tc["command"] == "ls"
    assert tc["allowed"] is True and tc["blocked"] is False
    assert tc["exit_code"] == 0
    assert tc["output_bytes"] == 11
    # two turns recorded, usage summed
    assert meter.input == 130
    assert result.metrics.accumulated_usage["inputTokens"] == 130


def test_after_tool_traces_blocked_bash_result(tmp_path):
    """Hook-level: a security-blocked BashResult stashed by the wrapper is mirrored to
    the trace as blocked and counted."""
    trace = TraceWriter(tmp_path / "t.jsonl")
    meter = CostMeter("...sonnet...")
    governor = _Governor(
        meter=meter,
        trace=trace,
        ceiling=CostCeiling(100.0),
        timeout_s=9999,
        start=0.0,
    )
    governor.bash_results["tu9"] = {
        "command": "rm -rf /",
        "blocked": True,
        "block_reason": "denylisted: rm",
        "exit_code": None,
        "duration_ms": 0,
        "output_bytes": 20,
        "truncated": False,
    }
    event = _FakeAfterTool(tool_use={"toolUseId": "tu9", "input": {"command": "rm -rf /"}})
    governor.after_tool(event)
    trace.close()

    assert governor.tool_calls == 1
    assert governor.blocked_tool_calls == 1
    tc = next(e for e in _read_events(tmp_path / "t.jsonl") if e["event"] == "tool_call")
    assert tc["blocked"] is True and tc["allowed"] is False
    assert tc["command"] == "rm -rf /"


# --- governor unit coverage (synthetic events) ----------------------------------


class _FakeBeforeModel:
    def __init__(self):
        self.cancel = False


class _FakeBeforeTool:
    def __init__(self, tool_use=None):
        self.tool_use = tool_use or {"toolUseId": "t", "input": {"command": "grep x"}}
        self.cancel_tool = False


class _FakeAfterTool:
    def __init__(self, tool_use=None, result=None):
        self.tool_use = tool_use or {}
        self.result = result


def _governor(ceiling_usd, meter, trace, timeout_s=9999, start=None):
    import time

    return _Governor(
        meter=meter,
        trace=trace,
        ceiling=CostCeiling(ceiling_usd),
        timeout_s=timeout_s,
        start=start if start is not None else time.monotonic(),
    )


def test_soft_trip_cancels_tool_once(tmp_path):
    trace = TraceWriter(tmp_path / "t.jsonl")
    meter = CostMeter("us.anthropic.claude-sonnet-4-6")
    # prime meter past a tiny ceiling
    meter.add(input=1_000_000, output=1_000_000)
    gov = _governor(0.01, meter, trace)
    assert gov.ceiling.soft_tripped(meter.cost_usd)

    e1 = _FakeBeforeTool()
    gov.before_tool(e1)
    assert e1.cancel_tool == COST_CEILING_PROMPT
    assert gov.soft_fired is True

    # fires only once: a second before_tool still cancels but emits no new event
    e2 = _FakeBeforeTool()
    gov.before_tool(e2)
    assert e2.cancel_tool == COST_CEILING_PROMPT
    trace.close()

    ceiling_events = [e for e in _read_events(tmp_path / "t.jsonl") if e["event"] == "cost_ceiling"]
    assert len(ceiling_events) == 1
    assert ceiling_events[0]["action"] == "soft_stop"


def test_soft_trip_not_fired_under_ceiling(tmp_path):
    trace = TraceWriter(tmp_path / "t.jsonl")
    meter = CostMeter("us.anthropic.claude-sonnet-4-6")
    meter.add(input=10, output=5)
    gov = _governor(1000.0, meter, trace)
    e = _FakeBeforeTool()
    gov.before_tool(e)
    assert e.cancel_tool is False
    assert gov.soft_fired is False


def test_hard_trip_cancels_model_call(tmp_path):
    trace = TraceWriter(tmp_path / "t.jsonl")
    meter = CostMeter("us.anthropic.claude-sonnet-4-6")
    meter.add(input=2_000_000, output=2_000_000)
    gov = _governor(0.01, meter, trace)  # grace 1.5x -> hard tripped well past
    assert gov.ceiling.hard_tripped(meter.cost_usd)

    e = _FakeBeforeModel()
    gov.before_model(e)
    assert e.cancel is True
    assert gov.hard_fired is True
    trace.close()

    ceiling_events = [e for e in _read_events(tmp_path / "t.jsonl") if e["event"] == "cost_ceiling"]
    assert len(ceiling_events) == 1
    assert ceiling_events[0]["action"] == "hard_stop"


def test_wall_clock_timeout_cancels(tmp_path):
    trace = TraceWriter(tmp_path / "t.jsonl")
    meter = CostMeter("us.anthropic.claude-sonnet-4-6")
    # start far in the past so elapsed > timeout_s immediately
    gov = _governor(1000.0, meter, trace, timeout_s=1, start=0.0)

    em = _FakeBeforeModel()
    gov.before_model(em)
    assert em.cancel is True
    assert gov.timed_out is True

    et = _FakeBeforeTool()
    gov.before_tool(et)
    assert et.cancel_tool  # a wrap-up string


# --- pure model-kwargs coverage -------------------------------------------------


def test_model_kwargs_effort_and_cache():
    from strands.models import CacheConfig

    kw = _model_kwargs(
        _decision(model_id="us.anthropic.claude-sonnet-4-6", effort="high"),
        _cfg(prompt_cache=True),
        {},
    )
    assert kw["model_id"] == "us.anthropic.claude-sonnet-4-6"
    assert kw["max_tokens"] == 8192
    assert kw["additional_request_fields"]["output_config"] == {"effort": "high"}
    # cache_config replaces the deprecated cache_prompt (strands 1.45.0 warns on it).
    assert kw["cache_config"] == CacheConfig(strategy="auto")
    assert "cache_prompt" not in kw


def test_model_kwargs_haiku_rejects_effort():
    kw = _model_kwargs(
        _decision(model_id="us.anthropic.claude-haiku-4-5", effort="high"),
        _cfg(),
        {},
    )
    assert "additional_request_fields" not in kw or "output_config" not in kw.get(
        "additional_request_fields", {}
    )


def test_model_kwargs_effort_none_omitted():
    kw = _model_kwargs(_decision(effort=None), _cfg(), {})
    assert "additional_request_fields" not in kw


def test_model_kwargs_thinking_adaptive_on_modern_models():
    # opus-4-8 (and sonnet-4-6 etc.) reject/deprecate manual enabled+budget_tokens:
    # the knob maps to adaptive, budget_tokens is dropped, extras are forwarded.
    for mid in ("us.anthropic.claude-opus-4-8", "us.anthropic.claude-sonnet-4-6"):
        kw = _model_kwargs(
            _decision(model_id=mid, effort=None),
            _cfg(thinking={"enabled": True, "budget_tokens": 4096, "display": "summarized"}),
            {},
        )
        thinking = kw["additional_request_fields"]["thinking"]
        assert thinking == {"type": "adaptive", "display": "summarized"}


def test_model_kwargs_thinking_manual_on_legacy_models():
    # Haiku 4.5 supports only manual thinking with a budget.
    kw = _model_kwargs(
        _decision(model_id="us.anthropic.claude-haiku-4-5-20251001-v1:0", effort=None),
        _cfg(thinking={"enabled": True, "budget_tokens": 4096}),
        {},
    )
    thinking = kw["additional_request_fields"]["thinking"]
    assert thinking == {"type": "enabled", "budget_tokens": 4096}


def test_model_kwargs_thinking_skipped_without_budget_on_legacy():
    # Manual-thinking models require budget_tokens; a bare {"enabled": true} would be
    # an invalid request, so the block is skipped instead of sent broken.
    kw = _model_kwargs(
        _decision(model_id="us.anthropic.claude-haiku-4-5-20251001-v1:0", effort=None),
        _cfg(thinking={"enabled": True}),
        {},
    )
    assert "additional_request_fields" not in kw


def test_run_structured_strips_thinking(tmp_path, monkeypatch):
    # The gate's structured call must never inherit the review's thinking config.
    seen = {}
    runner = StrandsRunner()

    def spy_build(decision, cfg, env):
        seen["thinking"] = cfg.thinking
        raise RuntimeError("stop before AWS")

    monkeypatch.setattr(runner, "_build_model", spy_build)
    with pytest.raises(RuntimeError):
        runner.run_structured(
            system_prompt="s",
            user_message="u",
            output_model=None,
            decision=_decision(),
            cfg=_cfg(thinking={"enabled": True, "budget_tokens": 4096}),
            trace=TraceWriter(tmp_path / "t.jsonl"),
            meter=CostMeter("x"),
            env={},
        )
    assert seen["thinking"] == {}
