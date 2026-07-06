# Review Harness — Hardening Review Record

Historical record of the adversarial review of this package, kept so future maintainers
can see what was checked, what was wrong, and what was changed before the harness was
trusted in CI. Point-in-time; not a living spec (the spec is [`../PLAN.md`](../PLAN.md)).

- **Date:** 2026-07-04
- **Baseline reviewed:** the initial rewrite at commit `3647dcd` ("Refresh HANDOFF.md
  …") — 216 tests, ruff-clean, hermetic.
- **After fixes:** 230 tests (14 regression tests added), ruff-clean, offline smoke
  (`scripts/smoke_fake.sh`) green.
- **Method:** two rounds. Round 1 was an adversarial read of every module against
  PLAN.md + HANDOFF.md, with each suspected bug reproduced by executing the CLI against
  the `fake` runner (no AWS). A subagent round then covered performance, stability, and
  auditing/logging in parallel. Round 2 re-verified every fix with the original repros
  and ran an independent adversarial pass over the diff to catch fix-induced
  regressions.
- **Verdict:** ready to vendor into the pipeline. No open issue would operationally
  degrade a run, hide a bug, or produce a malformed/lost review. Residual items are
  non-blocking and tracked below.

The standing security invariant held throughout: the harness is advisory and holds no
merge/write authority, so the blast radius of every bug below was a *lost or blocked
review*, never a bad merge.

---

## Round 1 — findings

### Design-alignment violations (code vs the updated design)

These broke guarantees the docs state explicitly. All three reproduced live.

1. **Overrides were not actually best-effort** (violates PLAN §7.4). A profile
   config-replacement with a wrong field type (`max_turns: "x"`) raised a bare
   `ValueError` from `int()` that the `ConfigError`-only handler didn't catch → exit 1,
   empty review. A typo'd model alias in a profile/team pin raised `RoutingError` at
   resolve time, outside the override try/except → also exit 1.
2. **The quality gate could sink a completed review** (violates PLAN §6.1 "the gate can
   only help or no-op"). The gate's routing + `model_decision` trace ran *outside*
   `run_quality_gate`'s try/except and *before* the review was written, so a bad
   `defaults.gate` alias discarded an already-produced review → exit 1.
3. **The crash handler erased a review that already succeeded.** The fatal handler
   unconditionally wrote empty output; a crash in `finalize`/`manifest.write` after the
   review was on disk turned a good review into an empty file, and double-emitted
   `run_end`.
4. **Full-model-id pins escaped both `PROTOTYPE_DEGRADE` and the models allowlist**
   (contradicts PLAN §6/§7): `_resolve_model_id` passed through any `.`/`:`-containing
   string, and the degrade ladder couldn't find a full id to step it down.
5. **`--raw-trace` captured neither model requests nor untruncated tool output**
   (contradicts PLAN §8.1 "verbatim … untruncated"); it recorded only the response and
   re-used the already-truncated tool text.
6. **Effective config / routing.json / overrides.cfg had no digest.** `config_sha256`
   hashed the *base* `--config` before override resolution, so a profile-swapped config
   left provenance pointing at a file that didn't run.

### Stability & the frozen exit-code contract (PLAN §1)

7. **argparse usage errors exited 2**, colliding with the reserved
   budget-exceeded code, and wrote no output.
8. **`manifest.write` computed `stat()` outside its `try`**, so a vanished artifact
   could throw out of `finalize` and skip the manifest.
9. **`TraceWriter.emit` didn't catch `ValueError`** — a circular payload could kill a
   run, violating "trace writing never kills a run."
10. **`PROTOTYPE_MAX_COST<=0` / `PROTOTYPE_BUDGET_THRESHOLD<=0`** parsed "successfully"
    and silently made every run return an empty review (exit 0) / refuse (exit 2).

### Auditing & logging gaps (observability is the stated #1 priority)

11. **`phase` events were entirely missing** (PLAN §8 promised resolve/budget/agent/
    finalize timing).
12. **Gate-stage events were second-class:** the gate's `model_decision` dropped
    `effort`/`max_tokens`/`degraded`; gate `turn` events collided with review-stage turn
    numbers.
13. **`tool_call` never recorded the block reason** in the main trace; **`run_start` /
    manifest lacked** argv, strands version, digests, a usage block, and repo/task on
    the crash path.

### Performance (CI-scale)

14. Bash tool buffered full subprocess output before applying the 100 KB cap;
    team/CODEOWNERS attribution is O(files × patterns); large `--raw-trace` hashed on
    the critical path. All acceptable at current scale — noted, not fixed.

---

## Fixes applied

| # | Fix | Where |
|---|---|---|
| 1 | `load_agent_config` raises `ConfigError` on bad field types (→ degrade to base config); the review routing call is wrapped so a bad **pin** drops the pin and re-resolves with default routing. A broken override now degrades, never fails. | `config.py`, `harness/bedrock.py` |
| 2 | The whole quality-gate stage (routing + trace + call) is wrapped; any failure keeps the original review and logs `gate accepted=false`. | `harness/bedrock.py` |
| 3 | `--output` is cleared up front; the fatal handler writes empty output *only* when the file has no content; `finalize()` is idempotent (`_finalized` flag) and never propagates (`manifest.write` failures are logged). `run_end` stays the terminal trace event on every path. | `harness/bedrock.py` |
| 4 | `main()` catches `SystemExit` and remaps argparse's code 2 → fatal (1); `--help` (0) is untouched. Exit 2 is now budget-only. | `harness/bedrock.py` |
| 5 | `run_start` + manifest carry argv, strands version, routing/overrides/effective-config digests (the effective digest names the file *actually* loaded after any degrade), and a usage block; `phase` events emit for each stage. | `harness/bedrock.py`, `trace.py` |
| 6 | `before_model` records the verbatim request (system prompt + messages); `BashResult.full_text` keeps untruncated output for the raw log; `tool_call` now carries `block_reason`. | `runner/strands.py`, `tools/bash.py` |
| 7 | `_normalize_pin_model` resolves a pin through the allowlist (off-allowlist → `RoutingError` → degraded per fix 1) and maps full ids back to an alias so `PROTOTYPE_DEGRADE` reaches them. Team "highest-tier-wins" logic is unchanged. | `routing.py` |
| 8 | Bash tool uses `Popen(start_new_session=True)` + process-group `SIGKILL` on timeout, clamps the model-supplied timeout to `max_timeout`, and cleans its scratch `HOME` via `atexit`. | `tools/bash.py` |
| — | Robustness: `TraceWriter.emit` catches `ValueError`; `Artifact.describe` wraps `stat()`; non-positive `PROTOTYPE_MAX_COST` / `PROTOTYPE_BUDGET_THRESHOLD` fall back to defaults with a loud warning. | `trace.py`, `budget.py` |

Each behavior-changing fix has a regression test (`tests/test_cli_e2e.py`,
`tests/test_routing.py`, `tests/test_bash_security.py`, `tests/test_budget.py`).

---

## Round 2 — verification and fix-induced regressions

Every round-1 repro was re-run and now behaves correctly. The adversarial pass over the
diff then found three holes the fixes themselves introduced — all fixed in the same
session, each with a guard test:

1. **`run_end` was no longer the terminal trace event** when `manifest.write` threw
   inside `finalize`. Fixed by catching the write failure inside `finalize` (logged,
   never propagated), so `run_end` stays last and a manifest failure never fails a good
   review.
2. **A reused `--output` could keep a stale prior-run review** if this run failed before
   producing one. Fixed by clearing `--output` up front.
3. **The effective-config digest named the *failed* profile config** after a fix-1
   degrade. Fixed to hash the file actually loaded.

---

## Residual risks (non-blocking)

- **Governor tool-call stash keyed by `toolUseId`** (`runner/strands.py`) is not
  concurrency-safe: if Strands ever runs multiple tool uses in one turn concurrently,
  the shared `bash_results` dict and counters race, mis-attributing `tool_call` trace
  events. Observability-only (never a wrong review); review runs are effectively
  sequential today. Add a lock before enabling parallel tools.
- **The command denylist is a courtesy guardrail, not a boundary** (by design, PLAN
  §9): wrapper forms (`command`/`env`/`nohup curl`), nested `bash -c`, and `>` redirect
  writes still pass it. Real containment is egress restriction + the cred-free
  subprocess env — do not over-trust the denylist before the egress firewall is live.
- **Live-Bedrock unknowns remain** (expected, and the drop-in's job to close): effort
  passthrough per model, exact enabled model IDs, and the egress firewall are only
  verifiable through `scripts/smoke_bedrock.sh` and archived integration notes.

---

## Addendum — doc-verification round (2026-07-05)

A follow-up pass verified the assembled Bedrock request shapes against the live
Anthropic/AWS docs and the pinned strands 1.45.0 source (previously flagged "open" in
`strands-notes.md`). Findings and fixes:

1. **The thinking knob would have 400'd on the default review model.** The config-driven
   `{"type": "enabled", "budget_tokens": N}` block is rejected by Opus 4.8 (adaptive-only)
   and deprecated on Sonnet 4.6. Fixed: `_model_kwargs` now maps the knob per model
   family (adaptive vs legacy-manual), legacy models without a `budget_tokens` skip the
   block, and the gate's structured call strips thinking entirely.
2. **`cache_prompt` is deprecated in strands 1.45.0** (warned every run). Switched to
   `CacheConfig(strategy="auto")` — conversation-prefix caching, the current API.
3. **Retry stacking + read-timeout**: botocore retries cut to 3 (strands'
   `ModelRetryStrategy` owns throttling backoff; 6x6 attempts stacked multiplicatively),
   and `read_timeout` capped at 300s instead of the full `timeout_s` (a hung stream read
   no longer consumes the whole run budget).
4. **Verified clean**: `output_config.effort` shape and values, the Haiku effort skip,
   all three routing.json model IDs, and the `pricing.py` table (Opus 4.8 $5/$25,
   Sonnet 4.6 $3/$15, Haiku 4.5 $1/$5, cache read ~0.1x) all match current docs.
5. `examples/agents/configs/prototype-review.json` now ships `thinking.enabled: true`
   (adaptive on the default models); review `max_tokens` raised to 32k since thinking
   tokens count against it on adaptive models.

Full contract detail: `strands-notes.md` "Thinking & effort".

## Observable changes for the drop-in

Nothing in the frozen §1 contract (flags, env vars, exit-code *meanings*) changed, but
the trace/manifest are now *richer*, and two failure modes changed shape. Consumers
should be aware:

- **Trace:** new `phase` events; `run_start` gains `argv`, `strands_version`,
  `routing_digest`, `overrides_digest`; `tool_call` gains `block_reason`; the gate's
  `model_decision` now carries the full field set.
- **Manifest (`run_manifest.json`):** now includes `routing_digest`, `overrides_digest`,
  `config_effective_sha256`, a `usage` block, `python`/`strands_version`/
  `harness_git_sha`, and repo/task even on the crash path.
- **Exit codes:** a malformed harness invocation (bad/missing flag) now exits **1**, not
  2 — so exit 2 unambiguously means "monthly budget exceeded."
- **Env levers:** a non-positive `PROTOTYPE_MAX_COST` or `PROTOTYPE_BUDGET_THRESHOLD` now
  falls back to its default with a stderr warning instead of silently no-op'ing every
  run.
