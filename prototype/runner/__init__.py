"""Runner seam: the one interface every agent-loop backend implements.

Swapping Strands <-> fake (<-> a future raw-Converse port) touches nothing outside
this package. Only `runner/strands.py` imports `strands`.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Protocol

if TYPE_CHECKING:
    from prototype.pricing import CostMeter
    from prototype.trace import TraceWriter
    from prototype.types import AgentConfig, ModelDecision


@dataclass
class RunResult:
    """What a runner returns to the CLI: the review text plus a stats bundle."""

    final_text: str
    stats: dict = field(default_factory=dict)
    # stats keys: turns, max_turns, tool_calls, blocked_tool_calls, stop_reason,
    # input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
    # cost_usd, total_s, outcome


class Runner(Protocol):
    """The agent-loop contract. Implementations live alongside this module."""

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
    ) -> RunResult: ...


def get_runner(name: str) -> Runner:
    """Resolve a runner by name. Imports the backend lazily so selecting `fake`
    never imports `strands` (and vice versa)."""
    if name == "fake":
        from prototype.runner.fake import FakeRunner

        return FakeRunner()
    if name == "strands":
        from prototype.runner.strands import StrandsRunner

        return StrandsRunner()
    raise ValueError(f"unknown runner: {name!r} (expected 'strands' or 'fake')")
