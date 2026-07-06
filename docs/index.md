# Review Harness docs

- [`../PLAN.md`](../PLAN.md) — full design and rationale (the spec).
- [`HANDOFF.md`](HANDOFF.md) — archived integration notes.
- [`strands-notes.md`](strands-notes.md) — pinned strands-agents API contract.
- [`REVIEW.md`](REVIEW.md) — hardening review record (2026-07-04): what was checked,
  what was wrong, and what was fixed before the harness was trusted.

## Module map

| Module | Responsibility |
|---|---|
| `prototype/harness/bedrock.py` | CLI entry; wiring + finalize/manifest. The frozen contract (PLAN §1). |
| `prototype/routing.py` + `routing.json` | Pure-function model routing (PLAN §6). |
| `prototype/overrides.py` | Repo profiles + team layer + CODEOWNERS (PLAN §7). |
| `prototype/config.py` | Agent config load (loop/behavior knobs only; model selection lives in routing.json) + system-prompt assembly with invariants (PLAN §5,7,9). |
| `prototype/budget.py` | AWS Budgets preflight + per-run cost ceiling (PLAN §5). |
| `prototype/pricing.py` | Price table + `CostMeter` (cache-aware). |
| `prototype/trace.py` | JSONL trace writer + run manifest (PLAN §8). |
| `prototype/gate.py` | Cheap post-review quality gate: structured hygiene pass (PLAN §6.1). |
| `prototype/secrets.py` | Output secret-scan tripwire (PLAN §9). |
| `prototype/sandbox.py` | OS-level confinement tier detection (PLAN §9). |
| `prototype/tools/bash.py` | Hardened read-only bash tool (PLAN §9). |
| `prototype/runner/` | Runner seam: `strands` (primary), `fake` (tests/dry-run). |

## Operational levers (env)

| Var | Effect |
|---|---|
| `PROTOTYPE_MAX_COST` | Per-run USD ceiling (default 3.0). |
| `PROTOTYPE_BUDGET_NAME` / `PROTOTYPE_BUDGET_THRESHOLD` | Monthly AWS Budget name / refuse-at %. |
| `PROTOTYPE_OVERRIDES=1` | Enable override resolution. |
| `PROTOTYPE_OVERRIDES_FILE` | Override config path (default `<base-dir>/overrides.cfg`). |
| `PROTOTYPE_DEGRADE=<n>` | Panic lever: force routing down n tiers. |
| `PROTOTYPE_QUALITY_GATE=1` | Run the cheap post-review quality gate (PLAN §6.1). |
| `PROTOTYPE_RUNNER` | `strands` (default) or `fake`. |
| `PROTOTYPE_SANDBOX_TIER` / `PROTOTYPE_EGRESS_FIREWALL` / `PROTOTYPE_REPO_RO_MOUNT` | Declare OS confinement tier. |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | Enable Strands OTel export (off by default). |
