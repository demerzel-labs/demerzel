"""Strands-backed agent-loop runner (PLAN.md sec 4-5).

The one module that imports `strands`. It sits behind the `Runner` Protocol so it is
interchangeable with `runner/fake.py`; the stats keys and trace-event vocabulary match
fake.py exactly so the two runners are drop-in swappable.

Everything the loop needs is native to strands-agents 1.45.0 (see
`docs/strands-notes.md`): turn caps (`Limits`), throttling backoff (`ModelRetryStrategy`),
per-call usage + hooks for the cost governor, and an injectable `Model` for hermetic
tests. Runtime facts discovered during the build are cited inline where an API choice
is non-obvious.
"""

from __future__ import annotations

import time
from typing import TYPE_CHECKING

from strands import Agent, ModelRetryStrategy, tool
from strands.hooks import (
    AfterModelCallEvent,
    AfterToolCallEvent,
    BeforeModelCallEvent,
    BeforeToolCallEvent,
    HookProvider,
)
from strands.types.agent import Limits

from prototype.budget import COST_CEILING_PROMPT, CostCeiling, read_cost_ceiling
from prototype.runner import RunResult

if TYPE_CHECKING:
    from strands.hooks import HookRegistry
    from strands.models import Model

    from prototype.pricing import CostMeter
    from prototype.trace import TraceWriter
    from prototype.types import AgentConfig, ModelDecision

# Injected as the tool result when the wall-clock budget trips (mirror of the cost
# soft-stop message in budget.py, but for time rather than money).
_TIMEOUT_PROMPT = (
    "TIME BUDGET REACHED - no more tool calls are available. Produce your final "
    "review NOW using only what you have already gathered, following the required "
    "output format exactly."
)

# The assistant message strands substitutes when a BeforeModelCallEvent hook sets
# cancel=True (observed at runtime 2026-07-04). We skip it when recovering best-effort
# text after a hard stop so the real prior review surfaces instead of the placeholder.
_CANCEL_PLACEHOLDER = "model call denied by hook"

# Set OTel up at most once per process; repeated global-provider setup warns/leaks.
_OTEL_READY = False


# Models whose thinking mode is ADAPTIVE (`{"type": "adaptive"}`, depth controlled by
# output_config.effort). Manual `{"type": "enabled", "budget_tokens": N}` is a 400 on
# opus-4-7/4-8, sonnet-5, fable-5, mythos-5, and deprecated on opus-4-6 / sonnet-4-6.
# Models NOT matched here (haiku, opus-4-5, sonnet-4-5, older) accept only manual
# thinking with a budget_tokens. Verified against Anthropic/AWS docs 2026-07-05
# (docs/strands-notes.md "Thinking & effort"); extend when routing.json gains models.
_ADAPTIVE_THINKING_MODELS = (
    "fable",
    "mythos",
    "sonnet-5",
    "sonnet-4-6",
    "opus-4-6",
    "opus-4-7",
    "opus-4-8",
)


def _uses_adaptive_thinking(model_id: str) -> bool:
    mid = (model_id or "").lower()
    return any(marker in mid for marker in _ADAPTIVE_THINKING_MODELS)


def _model_kwargs(decision: ModelDecision, cfg: AgentConfig, env: dict) -> dict:
    """BedrockConfig kwargs from the routing decision + agent config.

    Pure (no AWS), so the effort / Haiku / thinking / cache branching is unit-testable
    without a live model. Wire shapes verified against the Anthropic/AWS docs and the
    pinned strands source 2026-07-05 (docs/strands-notes.md "Thinking & effort");
    per-account model enablement remains the live smoke's job.
    """
    fields: dict = {}

    # output_config.effort — only when the router asked for an effort AND the model
    # supports it (documented supported set excludes Haiku). Must be a SEPARATE
    # output_config object, never inside `thinking` (ValidationException).
    is_haiku = "haiku" in (decision.model_id or "").lower()
    if decision.effort is not None and not is_haiku:
        fields["output_config"] = {"effort": decision.effort}

    # Thinking is config-driven (`inference.thinking.enabled`), but the wire shape is
    # per model family:
    #   - adaptive models get `{"type": "adaptive"}` — manual enabled+budget_tokens
    #     would 400 on opus-4-7/4-8 & sonnet-5/fable/mythos; depth is effort-controlled.
    #     budget_tokens is dropped; other keys (e.g. `display`) are forwarded.
    #   - legacy manual models need `{"type": "enabled", "budget_tokens": N}`; without a
    #     budget_tokens the block is invalid, so it is skipped (review still runs,
    #     just without thinking) rather than sent broken.
    if cfg.thinking and cfg.thinking.get("enabled"):
        extras = {
            k: v for k, v in cfg.thinking.items() if k not in ("enabled", "budget_tokens", "type")
        }
        if _uses_adaptive_thinking(decision.model_id):
            fields["thinking"] = {"type": "adaptive", **extras}
        elif cfg.thinking.get("budget_tokens"):
            fields["thinking"] = {
                "type": "enabled",
                "budget_tokens": cfg.thinking["budget_tokens"],
                **extras,
            }

    kwargs: dict = {"model_id": decision.model_id, "max_tokens": decision.max_tokens}
    if fields:
        kwargs["additional_request_fields"] = fields
    if cfg.prompt_cache:
        # CacheConfig(strategy="auto") is the current API (cache_prompt is deprecated in
        # strands 1.45.0 and warns per run): auto-injects cache points for Anthropic
        # models to cache the conversation prefix across turns, 5-min TTL by default.
        from strands.models import CacheConfig

        kwargs["cache_config"] = CacheConfig(strategy="auto")
    return kwargs


class _Governor(HookProvider):
    """Cost-ceiling + wall-clock governor, and the per-turn / per-tool tracer.

    Deny-by-default: it fires the hard stop rather than let a run overrun. The
    callbacks are plain methods so tests can drive them with synthetic events and a
    primed CostMeter — no multi-turn fake stream required (PLAN.md sec 5.2).
    """

    def __init__(
        self,
        *,
        meter: CostMeter,
        trace: TraceWriter,
        ceiling: CostCeiling,
        timeout_s: float,
        start: float,
        raw_path: str | None = None,
    ):
        self.meter = meter
        self.trace = trace
        self.ceiling = ceiling
        self.timeout_s = timeout_s
        self.start = start
        self.raw_path = raw_path
        self.turn = 0
        self.tool_calls = 0
        self.blocked_tool_calls = 0
        self.soft_fired = False
        self.hard_fired = False
        self.timed_out = False
        # toolUseId -> BashResult fields, stashed by the bash wrapper so the
        # AfterToolCallEvent hook can mirror the call into the trace (it has no other
        # way to see the tool's block/exit/byte details).
        self.bash_results: dict = {}

    def _raw(self, kind: str, payload: dict) -> None:
        """Append a verbatim record to the optional raw-envelope log (--raw-trace).

        Off unless a path is set. Tool output is captured up to the bash tool's
        output cap (the same bytes the model saw); this is the forensic/compliance
        record the truncated main trace deliberately omits. IO errors are swallowed
        so the raw log can never kill a run.
        """
        if not self.raw_path:
            return
        try:
            import json

            with open(self.raw_path, "a", encoding="utf-8") as f:
                f.write(json.dumps({"kind": kind, **payload}, default=str) + "\n")
        except (OSError, TypeError, ValueError):
            # A bad/circular payload (or IO error) must never kill a run via the raw log.
            pass

    def register_hooks(self, registry: HookRegistry, **kwargs) -> None:
        registry.add_callback(BeforeModelCallEvent, self.before_model)
        registry.add_callback(AfterModelCallEvent, self.after_model)
        registry.add_callback(BeforeToolCallEvent, self.before_tool)
        registry.add_callback(AfterToolCallEvent, self.after_tool)

    def _elapsed(self) -> float:
        return time.monotonic() - self.start

    def before_model(self, event: BeforeModelCallEvent) -> None:
        """Abort the next model call on wall-clock timeout or a hard cost trip."""
        if self._elapsed() > self.timeout_s:
            self.timed_out = True
            event.cancel = True
            return
        if self.ceiling.hard_tripped(self.meter.cost_usd):
            if not self.hard_fired:
                self.hard_fired = True
                self.trace.emit(
                    "cost_ceiling",
                    ceiling=self.ceiling.ceiling,
                    estimate=round(self.meter.cost_usd, 6),
                    action="hard_stop",
                )
            event.cancel = True
        # Capture the verbatim request that is about to be sent (system prompt + full
        # conversation so far) into the raw-envelope log — the forensic record the
        # truncated main trace deliberately omits (PLAN sec 8.1). Skipped if cancelled
        # (no request is sent) or if --raw-trace is off.
        if not event.cancel:
            agent = getattr(event, "agent", None)
            self._raw(
                "model_request",
                {
                    "turn": self.turn + 1,
                    "system_prompt": getattr(agent, "system_prompt", None),
                    "messages": getattr(agent, "messages", None),
                },
            )

    def after_model(self, event: AfterModelCallEvent) -> None:
        """Record this call's usage and emit a `turn` trace event.

        Per-call usage lives on the assistant message's `metadata.usage`, NOT on
        `event.stop_response` directly and NOT yet in `agent.event_loop_metrics
        .accumulated_usage` (which is updated *after* this hook fires — verified at
        runtime 2026-07-04). So the message metadata is the single source of per-call
        truth; end-of-run reconciliation only tops up shortfalls (never double-counts).
        """
        self.turn += 1
        usage: dict = {}
        latency = 0
        stop_reason = "error"
        if event.stop_response is not None:
            message = event.stop_response.message or {}
            meta = message.get("metadata") or {}
            usage = meta.get("usage") or {}
            latency = int((meta.get("metrics") or {}).get("latencyMs", 0) or 0)
            stop_reason = event.stop_response.stop_reason or "end_turn"
        self.meter.record_usage(usage)
        self.trace.emit(
            "turn",
            turn=self.turn,
            stop_reason=stop_reason,
            latency_ms=latency,
            input_tokens=int(usage.get("inputTokens", 0) or 0),
            output_tokens=int(usage.get("outputTokens", 0) or 0),
            cache_read_tokens=int(usage.get("cacheReadInputTokens", 0) or 0),
            cache_write_tokens=int(usage.get("cacheWriteInputTokens", 0) or 0),
            cumulative_cost=round(self.meter.cost_usd, 6),
        )
        self._raw(
            "model",
            {
                "turn": self.turn,
                "stop_reason": stop_reason,
                "usage": usage,
                "response_message": (event.stop_response.message if event.stop_response else None),
            },
        )
        if event.retry:
            self.trace.emit("retry", turn=self.turn)

    def before_tool(self, event: BeforeToolCallEvent) -> None:
        """Cancel the tool on wall-clock timeout or a soft cost trip.

        A cancelled tool's cancel string becomes the tool result the model reads, so it
        stops calling tools and writes its final review. Soft trip fires exactly once.
        """
        if self._elapsed() > self.timeout_s:
            self.timed_out = True
            event.cancel_tool = _TIMEOUT_PROMPT
            return
        if self.ceiling.soft_tripped(self.meter.cost_usd):
            if not self.soft_fired:
                self.soft_fired = True
                self.trace.emit(
                    "cost_ceiling",
                    ceiling=self.ceiling.ceiling,
                    estimate=round(self.meter.cost_usd, 6),
                    action="soft_stop",
                )
            event.cancel_tool = COST_CEILING_PROMPT

    def after_tool(self, event: AfterToolCallEvent) -> None:
        """Mirror a tool call into the trace. One AfterToolCallEvent -> one `tool_call`
        event, so the trace count and the `tool_calls` stat always agree. A call is
        `blocked` when it did not run successfully — security-gated by the bash tool,
        or cancelled by a governor hook (cost/timeout)."""
        tool_use = event.tool_use or {}
        tuid = tool_use.get("toolUseId")
        info = self.bash_results.pop(tuid, None)
        self.tool_calls += 1
        if info is not None:
            blocked = bool(info["blocked"])
            if blocked:
                self.blocked_tool_calls += 1
            self.trace.emit(
                "tool_call",
                command=info["command"],
                allowed=not blocked,
                blocked=blocked,
                block_reason=info["block_reason"],
                exit_code=info["exit_code"],
                duration_ms=info["duration_ms"],
                output_bytes=info["output_bytes"],
                truncated=info["truncated"],
            )
            # Raw log keeps the UNTRUNCATED tool output; the main trace only recorded
            # output_bytes/truncated. full_output falls back to the truncated text only
            # if the tool never captured the full form (blocked/timeout/error cases).
            self._raw(
                "tool",
                {
                    "command": info["command"],
                    "blocked": blocked,
                    "block_reason": info["block_reason"],
                    "exit_code": info["exit_code"],
                    "output": info.get("full_text") or info.get("text", ""),
                },
            )
            return
        # No stashed result: the wrapper never ran because a governor hook cancelled
        # the call. Record it as a blocked call from the tool_use input.
        result = event.result or {}
        blocked = result.get("status") == "error"
        if blocked:
            self.blocked_tool_calls += 1
        command = (tool_use.get("input") or {}).get("command", "")
        self.trace.emit(
            "tool_call",
            command=command,
            allowed=not blocked,
            blocked=blocked,
            block_reason="cancelled_by_governor" if blocked else None,
            exit_code=None,
            duration_ms=0,
            output_bytes=0,
            truncated=False,
        )


class _UsageHook(HookProvider):
    """Records per-call usage into the meter and emits a `turn` trace event. Used by
    the structured-output path (gate), which has no cost/wall-clock governor."""

    def __init__(self, *, meter: CostMeter, trace: TraceWriter):
        self.meter = meter
        self.trace = trace
        self.turn = 0

    def register_hooks(self, registry: HookRegistry, **kwargs) -> None:
        registry.add_callback(AfterModelCallEvent, self.after_model)

    def after_model(self, event: AfterModelCallEvent) -> None:
        self.turn += 1
        usage: dict = {}
        if event.stop_response is not None:
            message = event.stop_response.message or {}
            usage = (message.get("metadata") or {}).get("usage") or {}
        self.meter.record_usage(usage)
        self.trace.emit(
            "turn",
            turn=self.turn,
            stop_reason="structured_output",
            input_tokens=int(usage.get("inputTokens", 0) or 0),
            output_tokens=int(usage.get("outputTokens", 0) or 0),
            cache_read_tokens=int(usage.get("cacheReadInputTokens", 0) or 0),
            cache_write_tokens=int(usage.get("cacheWriteInputTokens", 0) or 0),
            cumulative_cost=round(self.meter.cost_usd, 6),
        )


def _message_text(message) -> str:
    """Concatenate the text content blocks of an assistant message dict."""
    if not message:
        return ""
    blocks = message.get("content") or []
    parts = [b.get("text", "") for b in blocks if isinstance(b, dict) and "text" in b]
    return "".join(parts).strip()


class StrandsRunner:
    """Strands agent-loop backend. See module docstring."""

    def run(
        self,
        *,
        system_prompt: str,
        user_message: str,
        decision: ModelDecision,
        cfg: AgentConfig,
        tools_cwd: str,
        trace: TraceWriter,
        meter: CostMeter,
        env: dict | None = None,
        model: Model | None = None,
    ) -> RunResult:
        import os

        env = os.environ if env is None else env
        start = time.monotonic()
        ceiling = CostCeiling(read_cost_ceiling(env))
        self._maybe_setup_otel(env, trace)

        governor = _Governor(
            meter=meter,
            trace=trace,
            ceiling=ceiling,
            timeout_s=cfg.timeout_s,
            start=start,
            raw_path=env.get("PROTOTYPE_RAW_TRACE_FILE"),
        )

        # model is a test seam: when injected, use it as-is with NO tools so this
        # module's tests never import bash.py. Only the real path wires the bash tool.
        tools: list = []
        if model is None:
            model = self._build_model(decision, cfg, env)
            tools = [self._make_bash_tool(tools_cwd, env, governor)]

        agent = Agent(
            model=model,
            tools=tools,
            system_prompt=system_prompt,
            hooks=[governor],
            retry_strategy=ModelRetryStrategy(),
        )

        result = agent(user_message, limits=Limits(turns=cfg.max_turns))

        outcome = "timeout" if governor.timed_out else "ok"
        final_text = self._final_text(result, agent, governor)
        self._reconcile(meter, result)

        turns = getattr(result.metrics, "cycle_count", 0) or governor.turn
        stats = {
            "turns": turns,
            "max_turns": cfg.max_turns,
            "tool_calls": governor.tool_calls,
            "blocked_tool_calls": governor.blocked_tool_calls,
            "stop_reason": getattr(result, "stop_reason", None),
            "total_s": round(time.monotonic() - start, 3),
            "outcome": outcome,
            **meter.totals,
        }
        return RunResult(final_text=final_text, stats=stats)

    def run_structured(
        self,
        *,
        system_prompt: str,
        user_message: str,
        output_model,
        decision: ModelDecision,
        cfg: AgentConfig,
        trace: TraceWriter,
        meter: CostMeter,
        env: dict | None = None,
        model: Model | None = None,
    ):
        """Single-shot structured-output call: no tools, no governor loop, one model
        call returning a validated pydantic object. Used by the quality gate (sec 6.1).
        Usage is recorded into the shared meter so the gate counts against the run's
        budget and cost totals. Returns the parsed `output_model` instance.
        """
        import os
        from dataclasses import replace

        env = os.environ if env is None else env
        # The gate is a cheap single-shot hygiene pass: it never inherits the review's
        # thinking config (which is sized for the main model, not the gate tier).
        cfg = replace(cfg, thinking={})
        if model is None:
            model = self._build_model(decision, cfg, env)

        # A minimal usage hook (no cost governor — the gate is a single cheap call).
        usage_hook = _UsageHook(meter=meter, trace=trace)
        agent = Agent(model=model, tools=[], system_prompt=system_prompt, hooks=[usage_hook])
        result = agent(user_message, structured_output_model=output_model)
        self._reconcile(meter, result)
        return result.structured_output

    def _build_model(self, decision: ModelDecision, cfg: AgentConfig, env: dict):
        from botocore.config import Config
        from strands.models import BedrockModel

        region = env.get("AWS_REGION", "us-east-1")
        # read_timeout guards a single stuck stream read (PLAN sec 4) — it must be much
        # smaller than the run's wall clock or a hung read blocks the whole budget
        # (responses stream, so healthy calls never sit 300s between chunks). Retries:
        # strands' ModelRetryStrategy owns throttling backoff (6 attempts), so botocore
        # keeps only short transient-network retries — otherwise the two layers stack
        # multiplicatively (6x6 attempts) during a throttling storm.
        boto_config = Config(
            retries={"max_attempts": 3, "mode": "adaptive"},
            connect_timeout=10,
            read_timeout=min(300, max(30, int(cfg.timeout_s))),
        )
        return BedrockModel(
            region_name=region,
            boto_client_config=boto_config,
            **_model_kwargs(decision, cfg, env),
        )

    def _make_bash_tool(self, tools_cwd: str, env: dict, governor: _Governor):
        # Imported lazily so this module (and its tests) don't hard-depend on bash.py.
        import prototype.sandbox as sandbox
        from prototype.tools.bash import BashTool

        tier = sandbox.detect_tier(env)
        bash_tool = BashTool(
            repo_root=tools_cwd,
            command_prefix=sandbox.command_prefix(tier),
        )

        @tool(context=True)
        def bash(command: str, timeout: int = 120, tool_context=None) -> str:
            """Run a read-only shell command inside the repository under review.

            Only safe readers are permitted (grep, cat, git log/show/diff, ...); writes,
            network, and interpreter escapes are blocked. Use it to inspect files and
            history the diff references. Returns combined stdout/stderr.
            """
            res = bash_tool.run(command, timeout)
            # ToolContext is a dataclass (strands 1.45.0): .tool_use is the ToolUse
            # dict carrying toolUseId — used to correlate the stash with after_tool.
            tuid = None
            if tool_context is not None:
                tuid = (tool_context.tool_use or {}).get("toolUseId")
            governor.bash_results[tuid] = {
                "command": command,
                "blocked": res.blocked,
                "block_reason": res.block_reason,
                "exit_code": res.exit_code,
                "duration_ms": res.duration_ms,
                "output_bytes": len(res.text.encode("utf-8")),
                "truncated": res.truncated,
                "text": res.text,  # truncated (what the model saw)
                "full_text": res.full_text,  # untruncated, for the raw-envelope log
            }
            if res.blocked:
                return f"[blocked] {res.block_reason}: {res.text}"
            return res.text

        return bash

    @staticmethod
    def _final_text(result, agent, governor: _Governor) -> str:
        """Assistant text from the run. On a hard stop, strands replaces result.message
        with a cancel placeholder, so we walk agent.messages back for the real review."""
        text = _message_text(getattr(result, "message", None))
        if text and not governor.hard_fired and text != _CANCEL_PLACEHOLDER:
            return text
        for message in reversed(getattr(agent, "messages", []) or []):
            if message.get("role") != "assistant":
                continue
            candidate = _message_text(message)
            if candidate and candidate != _CANCEL_PLACEHOLDER:
                return candidate
        return "" if text == _CANCEL_PLACEHOLDER else text

    @staticmethod
    def _reconcile(meter: CostMeter, result) -> None:
        """Ensure the meter matches result.metrics.accumulated_usage.

        Per-call usage (recorded in the AfterModelCallEvent hook) is the source of
        truth; this only tops up a dimension where the authoritative end-of-run total
        is higher (e.g. a call whose stop_response was missing). It never subtracts, so
        it can never hide spend and never double-counts.
        """
        acc = getattr(getattr(result, "metrics", None), "accumulated_usage", None)
        if not acc:
            return
        meter.add(
            input=max(0, int(acc.get("inputTokens", 0) or 0) - meter.input),
            output=max(0, int(acc.get("outputTokens", 0) or 0) - meter.output),
            cache_read=max(0, int(acc.get("cacheReadInputTokens", 0) or 0) - meter.cache_read),
            cache_write=max(0, int(acc.get("cacheWriteInputTokens", 0) or 0) - meter.cache_write),
        )

    @staticmethod
    def _maybe_setup_otel(env: dict, trace: TraceWriter) -> None:
        """Enable OTLP export only when an endpoint is configured. Off by default =
        no egress. Any failure is swallowed (traced) so telemetry never kills a run."""
        global _OTEL_READY
        if _OTEL_READY or not env.get("OTEL_EXPORTER_OTLP_ENDPOINT"):
            return
        try:
            from strands.telemetry import StrandsTelemetry

            StrandsTelemetry().setup_otlp_exporter()
            _OTEL_READY = True
        except Exception as e:  # noqa: BLE001 - telemetry must never kill a run
            trace.emit("otel_setup_failed", error=str(e))
