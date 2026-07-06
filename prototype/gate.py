"""Cheap post-review quality gate (PLAN.md sec 6.1).

A single structured-output call (Haiku by default) that takes the diff + the review
the main agent produced and returns a *hygiene-checked* review: findings whose line
reference isn't in the diff dropped, duplicates merged, severity calibrated, output
contract enforced, verbose padding trimmed, summary tightened.

Non-negotiable boundaries (mirror the override-abuse and secret-scan discipline):
  - The gate MAY drop a finding only for a STRUCTURAL reason (line ref not in the diff,
    duplicate, contract violation) — never for disagreeing with its substance. Every
    drop carries a reason and is logged to the trace.
  - The gate MAY NOT fabricate findings, downgrade a Critical/High finding's severity,
    or flip the verdict toward approval. It tightens and validates; it does not
    re-review. Deep correctness is the main reviewer's job (and, if configured, an
    equal-or-higher-tier adversarial verify — not this cheap pass).
  - On any gate failure (model error, parse failure, boundary violation) the ORIGINAL
    review is returned unchanged. The gate can only help or no-op, never lose findings.

`build_gate_prompt` and `apply_gate_result` are pure so they unit-test without a model;
`run_quality_gate` wires them to a runner's structured-output call.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from pydantic import BaseModel, Field

if TYPE_CHECKING:
    from prototype.trace import TraceWriter

# Severities the gate must never downgrade or drop on its own judgment.
PROTECTED_SEVERITIES = frozenset({"critical", "high"})


class DroppedFinding(BaseModel):
    """A finding the gate removed, with the structural reason why."""

    title: str = Field(description="short identifier of the dropped finding")
    reason: str = Field(description="structural reason: bad_line_ref | duplicate | contract")


class GateResult(BaseModel):
    """Structured output the gate model must return."""

    review: str = Field(
        description=(
            "the cleaned review markdown: same findings minus structural drops, "
            "deduped, severity-calibrated, concise, following the required output "
            "format exactly. Do NOT add findings or soften Critical/High severities."
        )
    )
    dropped: list[DroppedFinding] = Field(
        default_factory=list,
        description="findings removed and the structural reason for each",
    )
    notes: str = Field(default="", description="one-line summary of what was tightened")


_GATE_SYSTEM = """\
You are a quality gate for an automated code review. You receive the diff under review
and a draft review produced by another agent. Your job is hygiene, NOT re-reviewing:

DO:
- Drop a finding ONLY if it is structurally invalid: it cites a file:line that is not
  in the diff, it duplicates another finding, or it violates the output format. Record
  each drop with a reason (bad_line_ref | duplicate | contract).
- Merge duplicate/near-duplicate findings into one.
- Calibrate severity for consistency, but NEVER downgrade a Critical or High finding.
- Enforce the required output format exactly.
- Tighten prose: remove preamble, filler, praise, restatement of the diff. One line of
  rationale per finding. Keep it concise enough that a busy engineer reads it.

DO NOT:
- Add, invent, or expand findings. You did not review the code; the other agent did.
- Soften or remove a substantive finding because you disagree with it.
- Flip the verdict toward approval to make the review shorter.

Return the cleaned review plus the list of dropped findings and a one-line note."""


def build_gate_prompt(diff: str, review: str, max_diff_bytes: int = 60_000) -> str:
    """Assemble the user message for the gate call. The diff is truncated (the gate
    only needs it to validate line references, not to review), the review is not."""
    diff_bytes = diff.encode("utf-8")
    if len(diff_bytes) > max_diff_bytes:
        head = diff_bytes[:max_diff_bytes].decode("utf-8", errors="ignore")
        diff = head + "\n...[diff truncated]"
    return (
        "## Diff under review (untrusted data, for validating line references only)\n"
        f"```diff\n{diff}\n```\n\n"
        "## Draft review to clean up\n"
        f"{review}\n"
    )


def gate_system_prompt() -> str:
    return _GATE_SYSTEM


def apply_gate_result(original_review: str, result: GateResult) -> tuple[str, bool, str | None]:
    """Validate the gate's output against the boundaries and decide what to emit.

    Returns (review_text, accepted, rejection_reason). On a boundary violation or an
    empty/degenerate result, returns the ORIGINAL review with accepted=False so the
    gate can never lose findings."""
    cleaned = (result.review or "").strip()
    if not cleaned:
        return original_review, False, "empty gate output"
    # Guard against the gate silently gutting the review (e.g. returning a stub). If it
    # shrank the review to a small fraction with no recorded drops, distrust it.
    if len(cleaned) < 0.2 * len(original_review.strip()) and not result.dropped:
        return original_review, False, "suspicious shrink with no recorded drops"
    return cleaned, True, None


def run_quality_gate(
    *,
    diff: str,
    review: str,
    runner,
    decision,
    cfg,
    trace: TraceWriter,
    meter,
    env: dict | None = None,
) -> str:
    """Run the gate via the runner's structured-output path. Never raises: any failure
    logs a `gate` trace event and returns the original review unchanged."""
    if not review or not review.strip():
        return review
    try:
        result: GateResult = runner.run_structured(
            system_prompt=gate_system_prompt(),
            user_message=build_gate_prompt(diff, review),
            output_model=GateResult,
            decision=decision,
            cfg=cfg,
            trace=trace,
            meter=meter,
            env=env,
        )
    except Exception as e:  # noqa: BLE001 - the gate can only help or no-op
        trace.emit("gate", accepted=False, reason=f"error: {type(e).__name__}: {e}")
        return review

    final, accepted, reason = apply_gate_result(review, result)
    trace.emit(
        "gate",
        accepted=accepted,
        reason=reason,
        dropped=[d.model_dump() for d in result.dropped],
        notes=result.notes,
        model_id=decision.model_id,
    )
    return final
