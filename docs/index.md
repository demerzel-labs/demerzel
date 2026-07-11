# Demerzel docs

- [`../PLAN.md`](../PLAN.md): current product direction, invocation and provider plan,
  build sequence, and the imported code-review baseline in Appendix A.
- [`HANDOFF.md`](HANDOFF.md): archived integration notes.
- [`strands-notes.md`](strands-notes.md): pinned strands-agents API contract.
- [`REVIEW.md`](REVIEW.md): archived hardening review (2026-07-04). The names remain
  aligned with the reviewed baseline.

## Module map

| Module | Responsibility |
|---|---|
| `demerzel/harness/bedrock.py` | CLI entry; wiring + finalize/manifest. The frozen contract (PLAN §1). |
| `demerzel/routing.py` + `routing.json` | Pure-function model routing (PLAN §6). |
| `demerzel/overrides.py` | Repo profiles + team layer + CODEOWNERS (PLAN §7). |
| `demerzel/config.py` | Agent config load (loop/behavior knobs only; model selection lives in routing.json) + system-prompt assembly with invariants (PLAN §5,7,9). |
| `demerzel/budget.py` | AWS Budgets preflight + per-run cost ceiling (PLAN §5). |
| `demerzel/pricing.py` | Price table + `CostMeter` (cache-aware). |
| `demerzel/trace.py` | JSONL trace writer + run manifest (PLAN §8). |
| `demerzel/gate.py` | Cheap post-review quality gate: structured hygiene pass (PLAN §6.1). |
| `demerzel/secrets.py` | Output secret-scan tripwire (PLAN §9). |
| `demerzel/sandbox.py` | OS-level confinement tier detection (PLAN §9). |
| `demerzel/tools/bash.py` | Hardened read-only bash tool (PLAN §9). |
| `demerzel/runner/` | Runner seam: `strands` (primary), `fake` (tests/dry-run). |

## Operational levers (env)

| Var | Effect |
|---|---|
| `DEMERZEL_MAX_COST` | Per-run USD ceiling (default 3.0). |
| `DEMERZEL_BUDGET_NAME` / `DEMERZEL_BUDGET_THRESHOLD` | Monthly AWS Budget name / refuse-at %. |
| `DEMERZEL_OVERRIDES=1` | Enable override resolution. |
| `DEMERZEL_OVERRIDES_FILE` | Override config path (default `<base-dir>/overrides.cfg`). |
| `DEMERZEL_DEGRADE=<n>` | Panic lever: force routing down n tiers. |
| `DEMERZEL_QUALITY_GATE=1` | Run the cheap post-review quality gate (PLAN §6.1). |
| `DEMERZEL_RUNNER` | `strands` (default) or `fake`. |
| `DEMERZEL_SANDBOX_TIER` / `DEMERZEL_EGRESS_FIREWALL` / `DEMERZEL_REPO_RO_MOUNT` | Declare OS confinement tier. |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | Enable Strands OTel export (off by default). |
