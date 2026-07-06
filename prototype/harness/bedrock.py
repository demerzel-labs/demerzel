"""Prototype Bedrock harness CLI (PLAN.md sec 1).

    python3 -m prototype.harness.bedrock \\
        --config agents/configs/prototype-review.json \\
        --prompt-file /tmp/prompt.md \\
        --output /tmp/review.md \\
        --trace /tmp/trace.jsonl \\
        --base-dir . --repo acme/core --diff-size 12000 \\
        --system-prompt-file /tmp/system_prompt.md

Thin wiring only: resolve overrides + routing, run the budget preflight, invoke the
selected runner, then finalize (secret-scan output, write manifest). All the real
logic lives in the modules this imports. Exit codes: 0 ok, 1 fatal, 2 budget exceeded.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
import time
import traceback
from pathlib import Path

from prototype import __version__
from prototype import budget as budget_mod
from prototype import sandbox as sandbox_mod
from prototype import secrets as secrets_mod
from prototype.config import (
    ConfigError,
    assemble_system_prompt,
    effective_config_path,
    load_agent_config,
)
from prototype.overrides import OverrideResolution, OverridesConfig
from prototype.routing import RoutingError, default_config_path
from prototype.routing import resolve as resolve_routing
from prototype.runner import get_runner
from prototype.trace import RunManifest, TraceWriter, sha256_file, sha256_text
from prototype.types import TaskProfile

EXIT_OK = 0
EXIT_FATAL = 1
EXIT_BUDGET = 2


def _strands_version() -> str | None:
    try:
        from importlib.metadata import version

        return version("strands-agents")
    except Exception:  # noqa: BLE001 - version lookup is best-effort provenance
        return None

# Turn-scaling thresholds (bytes -> max turns), matching the current harness.
_TURN_SCALE = [(5_000, 10), (20_000, 15), (50_000, 25), (150_000, 35)]


def log(msg: str) -> None:
    print(f"[harness] {msg}", file=sys.stderr)


def _ms(t0: float) -> int:
    return int((time.monotonic() - t0) * 1000)


def compute_max_turns(user_message: str, config_max: int, diff_size: int = 0) -> int:
    """Scale max turns by workload; capped at config_max. Uses the larger of prompt
    length and on-disk diff size so large diffs read via tools still get enough turns."""
    prompt_bytes = len(user_message.encode("utf-8"))
    effective = max(prompt_bytes, diff_size)
    scaled = config_max
    for threshold, turns in _TURN_SCALE:
        if effective < threshold:
            scaled = turns
            break
    return min(scaled, config_max)


def _git_sha(base_dir: str) -> str | None:
    try:
        out = subprocess.run(
            ["git", "-C", base_dir, "rev-parse", "--short", "HEAD"],
            capture_output=True,
            text=True,
            timeout=5,
        )
        return out.stdout.strip() or None
    except (OSError, subprocess.SubprocessError):
        return None


def _read_changed_files(path: str | None) -> list[str]:
    if not path:
        return []
    try:
        return [ln.strip() for ln in Path(path).read_text().splitlines() if ln.strip()]
    except OSError:
        return []


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="prototype.harness.bedrock", description="Prototype Bedrock harness"
    )
    p.add_argument("--config", required=True, help="agent config JSON")
    p.add_argument("--prompt-file", required=True, help="user message (assembled prompt)")
    p.add_argument(
        "--output", required=True, help="final review markdown (written even on failure)"
    )
    p.add_argument("--trace", help="JSONL trace artifact")
    p.add_argument("--base-dir", help="root for resolving relative paths")
    p.add_argument("--repo", default="", help="repo under review")
    p.add_argument("--diff-size", type=int, default=0, help="on-disk diff bytes")
    p.add_argument("--system-prompt-file", help="output: assembled system prompt")
    # New, backward-compatible additions.
    p.add_argument("--changed-files", help="newline-separated changed file list")
    p.add_argument("--repo-root", help="checked-out repo path (enables CODEOWNERS)")
    p.add_argument("--task", help="task type for routing (default: config task or 'review')")
    p.add_argument("--artifacts-dir", help="dir for run_manifest.json (+ raw log)")
    p.add_argument("--raw-trace", help="verbatim model/tool I/O JSONL (full fidelity)")
    return p


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    try:
        args = parser.parse_args(argv)
    except SystemExit as e:
        # argparse exits 2 on a usage error, which collides with the reserved
        # budget-exceeded code. Remap it to the fatal code so the pipeline can't
        # misread a bad invocation as "budget exceeded". `--help` (code 0) is left alone.
        if e.code == 2:
            log("FATAL argument error (see usage above)")
            return EXIT_FATAL
        raise
    env = os.environ

    # Output is written even on failure so downstream `cat` never breaks.
    output_path = Path(args.output)
    try:
        output_path.parent.mkdir(parents=True, exist_ok=True)
    except OSError:
        pass
    # Clear any stale output up front: if --output is reused and this run fails before
    # producing a review, downstream must see an empty file, never a prior run's review.
    # The fatal handler's no-clobber check then distinguishes "this run wrote a review"
    # (non-empty) from "nothing produced yet" (empty).
    _write(output_path, "")

    trace = TraceWriter(args.trace)
    manifest = RunManifest(run_id=trace.run_id)
    _finalized = False

    def finalize(outcome: str) -> None:
        # Idempotent: the success path calls this once; if it (or anything after it)
        # raises, the top-level handler calls it again — the guard stops a second
        # run_end / manifest from being emitted.
        # `run_end` is the terminal trace event by convention (consumers read the last
        # line), so this is the last emit; manifest write + close follow it.
        nonlocal _finalized
        if _finalized:
            return
        _finalized = True
        payload = {k: v for k, v in _summary.items() if k != "outcome"}
        trace.emit("run_end", outcome=outcome, **payload)
        manifest.set(**payload)
        manifest.add_artifact("review", output_path)
        manifest.add_artifact("trace", args.trace)
        manifest.add_artifact("system_prompt", args.system_prompt_file)
        manifest.add_artifact("raw_envelopes", args.raw_trace)
        if args.artifacts_dir:
            man_path = Path(args.artifacts_dir) / "run_manifest.json"
        elif args.trace:
            man_path = Path(args.trace).with_name("run_manifest.json")
        else:
            man_path = output_path.with_name("run_manifest.json")
        try:
            manifest.write(man_path, outcome=outcome)
        except Exception as e:  # noqa: BLE001 - finalize must never propagate (keeps
            # run_end the terminal trace event and stops a double-finalize in the handler)
            log(f"WARN manifest write failed: {e}")
        trace.close()

    _summary: dict = {"harness_version": __version__}

    try:
        return _run(args, argv, env, trace, manifest, output_path, _summary, finalize)
    except SystemExit:
        raise
    except Exception as e:  # noqa: BLE001 - top-level fatal handler
        log(f"FATAL {type(e).__name__}: {e}")
        traceback.print_exc(file=sys.stderr)
        # Frozen contract: an empty output on failure so downstream `cat` never breaks.
        # But never clobber a review that already landed on disk — a crash in finalize
        # after a successful review must not turn a real review into an empty file.
        if not _output_has_content(output_path):
            _write(output_path, "")
        trace.emit("error", type=type(e).__name__, message=str(e), traceback=traceback.format_exc())
        _summary["error"] = f"{type(e).__name__}: {e}"
        try:
            finalize("error")
        except Exception:  # noqa: BLE001 - finalize must never mask the original error
            pass
        return EXIT_FATAL


def _output_has_content(path: Path) -> bool:
    try:
        return path.stat().st_size > 0
    except OSError:
        return False


def _write(path: Path, text: str) -> None:
    try:
        path.write_text(text, encoding="utf-8")
    except OSError as e:
        log(f"WARN could not write output {path}: {e}")


def _run(args, argv, env, trace, manifest, output_path, _summary, finalize) -> int:
    base_dir = args.base_dir or str(Path(args.config).resolve().parent.parent.parent)
    changed_files = _read_changed_files(args.changed_files)
    user_message = Path(args.prompt_file).read_text(encoding="utf-8")

    # Provenance digests — resolved up front so run_start (and, on a crash, the manifest)
    # can prove which routing table and overrides file actually ran (PLAN sec 8).
    overrides_enabled = env.get("PROTOTYPE_OVERRIDES", "0") == "1"
    ov_path = env.get("PROTOTYPE_OVERRIDES_FILE") or str(Path(base_dir) / "overrides.cfg")
    routing_digest = sha256_file(default_config_path())
    overrides_digest = (
        sha256_file(ov_path) if overrides_enabled and Path(ov_path).exists() else None
    )

    tier = sandbox_mod.detect_tier(env)
    trace.emit(
        "run_start",
        argv=list(argv) if argv is not None else sys.argv[1:],
        repo=args.repo,
        task=args.task or "",
        diff_size=args.diff_size,
        base_dir=base_dir,
        config=args.config,
        config_sha256=sha256_file(args.config),
        routing_digest=routing_digest,
        overrides_digest=overrides_digest,
        harness_version=__version__,
        harness_git_sha=_git_sha(base_dir),
        python=sys.version.split()[0],
        strands_version=_strands_version(),
        sandbox_tier=tier,
        changed_files_count=len(changed_files),
    )
    # Seed the summary with provenance now, so even a crash-path manifest carries repo,
    # versions, and digests (not just harness_version).
    _summary.update(
        {
            "repo": args.repo,
            "task": args.task or "",
            "python": sys.version.split()[0],
            "strands_version": _strands_version(),
            "harness_git_sha": _git_sha(base_dir),
            "routing_digest": routing_digest,
            "overrides_digest": overrides_digest,
            "sandbox_tier": tier,
        }
    )
    log(
        f"start repo={args.repo} diff_size={args.diff_size} "
        f"sandbox_tier={tier} ({sandbox_mod.describe(tier)})"
    )

    _t_resolve = time.monotonic()

    # --- override resolution (opt-in) ---
    resolution = OverrideResolution()
    if overrides_enabled:
        if Path(ov_path).exists():
            # Overrides are best-effort enhancement: a broken override config degrades
            # to the default Prototype review (no profile/teams) with a loud log + trace,
            # and never fails the review itself.
            try:
                ov = OverridesConfig.load(ov_path)
                ov.validate()
                resolution = ov.resolve(
                    args.repo, changed_files=changed_files, repo_root=args.repo_root
                )
                trace.emit("override_resolved", **resolution.to_trace())
                if resolution.profile or resolution.matched_teams:
                    prof = resolution.profile.name if resolution.profile else "(none)"
                    log(f"override profile={prof} teams={resolution.matched_teams}")
            except Exception as e:  # noqa: BLE001 - overrides never fail the review
                resolution = OverrideResolution()
                log(f"WARN override config error, falling back to default review: {e}")
                trace.emit("override_error", message=f"{type(e).__name__}: {e}")
        else:
            log(f"overrides enabled but no file at {ov_path}; using defaults")

    # --- load agent config (profile may replace it; missing replacement -> default) ---
    cfg_path = effective_config_path(resolution, args.config, base_dir)
    loaded_cfg_path = cfg_path
    try:
        cfg = load_agent_config(cfg_path)
    except ConfigError as e:
        if str(cfg_path) != str(args.config):
            log(f"WARN profile config {cfg_path} unusable, falling back to --config: {e}")
            trace.emit("override_error", message=f"config replacement failed: {e}")
            cfg = load_agent_config(args.config)  # base config is load-bearing; may raise
            loaded_cfg_path = Path(args.config)  # provenance must name the file that ran
        else:
            raise
    task = args.task or cfg.task
    # Hash the config that actually ran (a profile may have replaced the base --config,
    # or a broken profile config may have degraded back to it), so provenance names the
    # real file, not the pre-resolution or failed one.
    _summary["task"] = task
    _summary["config_effective"] = str(loaded_cfg_path)
    _summary["config_effective_sha256"] = sha256_file(loaded_cfg_path)

    # --- assemble system prompt ---
    assembled = assemble_system_prompt(base_dir, cfg, resolution)
    if args.system_prompt_file:
        _write(Path(args.system_prompt_file), assembled.text)
    # Self-contained audit of the composed prompt: the ordered block provenance (incl.
    # each `## Team:` block's sources), size, and a sha256 that matches the
    # system_prompt artifact in the manifest — so "did team X's override actually make
    # it into the prompt?" is answerable from the trace alone, without opening the file.
    trace.emit(
        "prompt_assembled",
        sources=assembled.sources,
        bytes=len(assembled.text.encode("utf-8")),
        sha256=sha256_text(assembled.text),
        dropped_teams=assembled.dropped_teams,
        missing=assembled.missing,
    )
    if assembled.dropped_teams:
        log(f"WARN team context cap: dropped {assembled.dropped_teams}")
    if assembled.missing:
        log(f"WARN skipped missing prompt/context files (defaulted): {assembled.missing}")
        trace.emit("prompt_files_missing", files=assembled.missing)
    trace.emit("phase", phase="resolve", duration_ms=_ms(_t_resolve))

    # --- budget preflight ---
    _t_budget = time.monotonic()
    status = budget_mod.preflight(env)
    trace.emit("budget_check", **status.to_dict())
    trace.emit("phase", phase="budget", duration_ms=_ms(_t_budget))
    log(status.message)
    month_pct = status.pct if status.status != "check_failed" else None
    if status.should_refuse:
        _write(output_path, "")
        _summary.update(
            {
                "repo": args.repo,
                "task": task,
                "budget": status.to_dict(),
                "outcome": "budget_exceeded",
            }
        )
        finalize("budget_exceeded")
        return EXIT_BUDGET

    # --- routing ---
    profile = TaskProfile(
        task=task,
        diff_size=args.diff_size,
        repo=args.repo,
        changed_files=changed_files,
        month_spend_pct=month_pct,
    )
    try:
        decision = resolve_routing(profile, pin=resolution.pin, env=env)
    except RoutingError as e:
        # A bad *pin* (typo'd alias, or one outside the models allowlist) comes from an
        # override, so it must degrade, not fail the review: drop the pin and re-resolve
        # with default routing. A RoutingError with no pin means routing.json itself is
        # broken — that is load-bearing, so it propagates.
        if resolution.pin is not None:
            log(f"WARN routing pin unusable, ignoring it and using default routing: {e}")
            trace.emit("override_error", message=f"routing pin failed: {e}")
            decision = resolve_routing(profile, pin=None, env=env)
        else:
            raise
    trace.emit(
        "model_decision",
        model_id=decision.model_id,
        effort=decision.effort,
        max_tokens=decision.max_tokens,
        degraded=decision.degraded,
        source=decision.source,
        rationale=decision.rationale,
    )
    log(
        f"route model={decision.model_id} effort={decision.effort} "
        f"source={decision.source} :: {decision.rationale}"
    )

    # --- turn scaling ---
    max_turns = compute_max_turns(user_message, cfg.max_turns, args.diff_size)
    cfg.max_turns = max_turns
    log(f"max_turns={max_turns} (config cap {cfg.raw.get('max_turns', 50)})")

    # --- run ---
    from prototype.pricing import CostMeter

    meter = CostMeter(decision.model_id)
    runner_name = env.get("PROTOTYPE_RUNNER", cfg.runner)
    runner = get_runner(runner_name)
    tools_cwd = args.repo_root or base_dir
    if args.raw_trace:
        env = {**env, "PROTOTYPE_RAW_TRACE_FILE": args.raw_trace}

    _t_agent = time.monotonic()
    result = runner.run(
        system_prompt=assembled.text,
        user_message=user_message,
        decision=decision,
        cfg=cfg,
        tools_cwd=tools_cwd,
        trace=trace,
        meter=meter,
        env=env,
    )

    # --- optional cheap post-review quality gate (sec 6.1) ---
    # The gate can only help or no-op (PLAN sec 6.1): the ENTIRE stage — routing, trace,
    # and the model call — is wrapped, so any failure keeps the original review verbatim.
    review_text = result.final_text
    gate_on = cfg.quality_gate or env.get("PROTOTYPE_QUALITY_GATE", "0") == "1"
    if gate_on and review_text.strip():
        try:
            from prototype import gate as gate_mod

            gate_decision = resolve_routing(
                TaskProfile(task="gate", repo=args.repo, month_spend_pct=month_pct),
                pin=None,  # the gate is deliberately cheap; review pins never force it up
                env=env,
            )
            trace.emit(
                "model_decision",
                stage="gate",
                model_id=gate_decision.model_id,
                effort=gate_decision.effort,
                max_tokens=gate_decision.max_tokens,
                degraded=gate_decision.degraded,
                source=gate_decision.source,
                rationale=gate_decision.rationale,
            )
            review_text = gate_mod.run_quality_gate(
                diff=user_message,
                review=review_text,
                runner=runner,
                decision=gate_decision,
                cfg=cfg,
                trace=trace,
                meter=meter,
                env=env,
            )
            log(f"quality gate model={gate_decision.model_id}")
        except Exception as e:  # noqa: BLE001 - the gate can only help or no-op
            log(f"WARN quality gate stage failed, keeping original review: {e}")
            trace.emit("gate", accepted=False, reason=f"stage error: {type(e).__name__}: {e}")
            review_text = result.final_text
    trace.emit("phase", phase="agent", duration_ms=_ms(_t_agent))

    # --- finalize: secret-scan output before emit (sec 9) ---
    _t_finalize = time.monotonic()
    scan = secrets_mod.scan(review_text)
    trace.emit("secret_scan", **scan.to_trace())
    if scan.fired:
        log(
            f"WARN secret-scan tripwire fired: {sorted(set(scan.matches))} "
            f"({scan.redactions} redacted)"
        )
    _write(output_path, scan.text)

    _summary.update(
        {
            "repo": args.repo,
            "task": task,
            "runner": runner_name,
            "model_id": decision.model_id,
            "model_decision": {
                "model_id": decision.model_id,
                "effort": decision.effort,
                "max_tokens": decision.max_tokens,
                "source": decision.source,
                "degraded": decision.degraded,
                "rationale": decision.rationale,
            },
            "sandbox_tier": tier,
            "secret_scan_fired": scan.fired,
            "usage": {
                "input": result.stats.get("input_tokens", 0),
                "output": result.stats.get("output_tokens", 0),
                "cache_read": result.stats.get("cache_read_tokens", 0),
                "cache_write": result.stats.get("cache_write_tokens", 0),
            },
            **result.stats,
        }
    )
    # Emit the finalize-phase timing before finalize() (which emits the terminal
    # run_end): covers secret-scan + output write, keeping run_end the last line.
    trace.emit("phase", phase="finalize", duration_ms=_ms(_t_finalize))
    finalize(result.stats.get("outcome", "ok"))

    stats = result.stats
    log(
        f"done: {stats.get('total_s')}s, {stats.get('tool_calls')} tool calls, "
        f"{stats.get('turns')}/{stats.get('max_turns')} turns, "
        f"${stats.get('cost_usd')} ({stats.get('input_tokens')}in/{stats.get('output_tokens')}out)"
    )
    return EXIT_OK


if __name__ == "__main__":
    sys.exit(main())
