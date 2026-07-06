"""Deterministic runner for hermetic tests and offline pipeline dry-runs.

Selected with PROTOTYPE_RUNNER=fake. Imports no `strands` and makes no network calls,
so the full CLI (routing, overrides, budget, trace, manifest, exit codes) is
end-to-end testable with zero AWS.

Behavior is scripted via env so tests and smoke_fake.sh can drive it:
  PROTOTYPE_FAKE_OUTPUT        - exact review text to emit (default: a canned review)
  PROTOTYPE_FAKE_TURNS         - number of `turn` trace events to emit (default 1)
  PROTOTYPE_FAKE_TOOL_CALLS    - number of `tool_call` trace events to emit (default 0)
  PROTOTYPE_FAKE_INPUT_TOKENS  - input tokens to record in the meter (default 1000)
  PROTOTYPE_FAKE_OUTPUT_TOKENS - output tokens to record (default 500)
  PROTOTYPE_FAKE_RAISE         - if set, raise RuntimeError(<value>) to exercise the
                                CLI fatal path
"""

from __future__ import annotations

import os
import time
from typing import TYPE_CHECKING

from prototype.runner import RunResult

if TYPE_CHECKING:
    from prototype.pricing import CostMeter
    from prototype.trace import TraceWriter
    from prototype.types import AgentConfig, ModelDecision


def _int(env: dict, key: str, default: int) -> int:
    try:
        return int(env.get(key, default))
    except (TypeError, ValueError):
        return default


class FakeRunner:
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
    ) -> RunResult:
        env = os.environ if env is None else env
        t0 = time.monotonic()

        if env.get("PROTOTYPE_FAKE_RAISE"):
            raise RuntimeError(env["PROTOTYPE_FAKE_RAISE"])

        turns = _int(env, "PROTOTYPE_FAKE_TURNS", 1)
        tool_calls = _int(env, "PROTOTYPE_FAKE_TOOL_CALLS", 0)
        in_tok = _int(env, "PROTOTYPE_FAKE_INPUT_TOKENS", 1000)
        out_tok = _int(env, "PROTOTYPE_FAKE_OUTPUT_TOKENS", 500)

        raw_path = env.get("PROTOTYPE_RAW_TRACE_FILE")

        def _raw(kind: str, payload: dict) -> None:
            if not raw_path:
                return
            try:
                import json

                with open(raw_path, "a", encoding="utf-8") as f:
                    f.write(json.dumps({"kind": kind, **payload}) + "\n")
            except OSError:
                pass

        for i in range(tool_calls):
            trace.emit(
                "tool_call",
                command=f"grep -rn TODO . # fake {i}",
                allowed=True,
                blocked=False,
                block_reason=None,
                exit_code=0,
                duration_ms=1,
                output_bytes=42,
                truncated=False,
            )
            _raw("tool", {"command": f"grep -rn TODO . # fake {i}", "output": "..."})

        per_turn_in = max(1, in_tok // max(1, turns))
        per_turn_out = max(1, out_tok // max(1, turns))
        for t in range(1, turns + 1):
            meter.record_usage({"inputTokens": per_turn_in, "outputTokens": per_turn_out})
            trace.emit(
                "turn",
                turn=t,
                stop_reason="end_turn" if t == turns else "tool_use",
                latency_ms=1,
                input_tokens=per_turn_in,
                output_tokens=per_turn_out,
                cache_read_tokens=0,
                cache_write_tokens=0,
                cumulative_cost=round(meter.cost_usd, 6),
            )
            _raw(
                "model",
                {
                    "turn": t,
                    "request": {"system_bytes": len(system_prompt)},
                    "response": {"input_tokens": per_turn_in, "output_tokens": per_turn_out},
                },
            )

        default_output = (
            "## Summary\n"
            f"Fake review by runner=fake, model={decision.model_id}. "
            f"Reviewed {len(user_message)} bytes of prompt.\n\n"
            "APPROVE\n"
        )
        final_text = env.get("PROTOTYPE_FAKE_OUTPUT", default_output)

        stats = {
            "turns": turns,
            "max_turns": cfg.max_turns,
            "tool_calls": tool_calls,
            "blocked_tool_calls": 0,
            "stop_reason": "end_turn",
            "total_s": round(time.monotonic() - t0, 3),
            "outcome": "ok",
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
    ):
        """Deterministic structured-output stand-in for the quality gate.

        Scripted via env for e2e tests:
          PROTOTYPE_FAKE_GATE_REVIEW    - the cleaned review text to return
          PROTOTYPE_FAKE_GATE_DROPPED   - comma-separated "title:reason" drops
          PROTOTYPE_FAKE_GATE_RAISE     - if set, raise to exercise the gate's no-op path
        Defaults to echoing the draft review unchanged (a faithful no-op gate).
        """
        env = os.environ if env is None else env
        if env.get("PROTOTYPE_FAKE_GATE_RAISE"):
            raise RuntimeError(env["PROTOTYPE_FAKE_GATE_RAISE"])

        meter.record_usage({"inputTokens": 200, "outputTokens": 100})
        trace.emit(
            "turn",
            turn=1,
            stop_reason="structured_output",
            input_tokens=200,
            output_tokens=100,
            cache_read_tokens=0,
            cache_write_tokens=0,
            cumulative_cost=round(meter.cost_usd, 6),
        )

        dropped = []
        raw_drops = env.get("PROTOTYPE_FAKE_GATE_DROPPED", "")
        for item in (d for d in raw_drops.split(",") if d.strip()):
            title, _, reason = item.partition(":")
            dropped.append({"title": title.strip(), "reason": (reason or "contract").strip()})

        # Extract the draft review from the prompt when no explicit override is given,
        # so the default fake gate is a faithful no-op (never loses findings).
        default_review = env.get("PROTOTYPE_FAKE_GATE_REVIEW")
        if default_review is None:
            marker = "## Draft review to clean up\n"
            default_review = (
                user_message.split(marker, 1)[1].strip() if marker in user_message else user_message
            )
        return output_model(review=default_review, dropped=dropped, notes="fake gate")
