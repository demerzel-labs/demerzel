# Review Harness Rewrite — Implementation Plan

Independent Python CI review-harness prototype. The package explores an observable,
budgeted, routed Bedrock agent loop with a small caller-mode interface.

Beyond talking to the LLM, the three functions this harness must provide well, in
priority order: **observability**, **budget control**, **intelligent routing**.
Everything below serves those.

**Standing security invariant:** the harness is advisory — it produces a review
artifact and never holds merge or repo-write authority. This caps the blast radius
of any prompt injection through the untrusted diff (§9) and is assumed throughout.

---

## 1. Compatibility contract (frozen caller surface)

The work repo's bash layer invokes the harness like this and must keep working
unmodified:

```bash
python3 -m prototype.harness.bedrock \
    --config  "$AGENT_CONFIG" \
    --prompt-file "$PROMPT_PATH" \
    --output  "$RAW_REVIEW_FILE" \
    --trace   "$TRACE_FILE" \
    --base-dir "$SCRIPT_DIR_ROOT" \
    --repo    "$REPO" \
    --diff-size "${DIFF_SIZE:-0}" \
    --system-prompt-file "${RESULTS_DIR}/${JIRA_KEY}_system_prompt.md" \
    2>"$HARNESS_LOG"
HARNESS_EXIT=$?
```

Frozen semantics:

| Surface | Behavior |
|---|---|
| `--config` | agent config JSON (model, inference, max_turns, timeout_s, prompt files) |
| `--prompt-file` | user message (the assembled review prompt + diff) |
| `--output` | final review markdown; **written even on failure** (empty) so downstream `cat` never breaks |
| `--trace` | JSONL trace artifact |
| `--base-dir` | root for resolving relative paths (defaults to config's `../../..` as today) |
| `--repo` | repo under review, keys override resolution |
| `--diff-size` | on-disk diff bytes, feeds turn scaling + routing |
| `--system-prompt-file` | **output**: assembled system prompt written here for artifact/debugging |
| stderr | human-readable `[harness]` log lines (captured to `$HARNESS_LOG`) |
| exit 0 | success |
| exit 1 | fatal error (exception path: FATAL line + traceback to stderr, empty output written; terminal `error`+`run_end` trace events and manifest still emitted — §8.1) |
| exit 2 | monthly budget exceeded — refused to run (budget status written to trace; manifest emitted) |

Env vars preserved: `AWS_REGION` (default `us-east-1`), `PROTOTYPE_MAX_COST` (per-run
ceiling USD, default 3.0), `PROTOTYPE_BUDGET_NAME` (default `prototype-bedrock-monthly`),
`AWS_ACCOUNT_ID`, `PROTOTYPE_BUDGET_THRESHOLD` (default 95), `PROTOTYPE_OVERRIDES`
(`1` enables override resolution).

**Env surface policy (decided 2026-07-04):** env vars are capped at operational
levers for CI configuration. Everything else (model choice, effort, task defaults,
tier thresholds) lives in config files that deploy with the repo (§6). The
current pile of growing routing env vars gets absorbed into `routing.json`, with a
migration mapping documented for integration.

The full lever set: `PROTOTYPE_MAX_COST`, `PROTOTYPE_BUDGET_NAME`,
`PROTOTYPE_BUDGET_THRESHOLD`, `PROTOTYPE_OVERRIDES`, plus two new ones —
`PROTOTYPE_DEGRADE=<n>` (force every routing decision down n tiers immediately; the
panic lever) and `PROTOTYPE_RUNNER` (`strands` default | `fake` for offline dry-runs).

**New, backward-compatible flag additions** (bash layer may adopt incrementally):

- `--changed-files <path>` — newline-separated changed file list (from
  `git diff --name-only`); enables path-based routing signals and team-level overrides.
- `--repo-root <path>` — path to the checked-out repo under review; enables
  CODEOWNERS-based team attribution (§7).
- `--task <name>` — task type for routing (default: `task` key in config, else `review`).
- `--artifacts-dir <path>` — directory for `run_manifest.json` (+ raw log if enabled),
  with artifact paths listed relative to it, so the bash S3 step can upload one dir
  (§8.1). Without it, the manifest is written next to `--trace` with absolute paths.
- `--raw-trace <path>` — verbatim model request/response + untruncated tool I/O JSONL
  for full-fidelity compliance/forensics (off by default; §8.1).
- `PROTOTYPE_OVERRIDES_FILE` env — override config location (default `<base-dir>/overrides.cfg`).

## 2. Non-goals

- No changes to the bash pipeline, MR/PR posting, preprocessing, or prompt building
  upstream of `--prompt-file`.
- No managed routing services (Bedrock Intelligent Prompt Routing, gateways) — routing
  stays a repo-owned pure function, reviewable in a diff.
- Not a multi-provider harness. Bedrock only.
- No individual-level override granularity — teams are the floor (§7).
- No external artifact upload inside the harness — the harness finalizes and describes
  its artifacts in a manifest, and an external CI step owns upload. Keeps network use
  narrow and upload failures outside the review run (§8.1).

## 3. Repo layout

Package at repo root so the sample CI integration can copy the `prototype/` directory
and set `PYTHONPATH=$SCRIPT_DIR_ROOT`:

```
prototype/
  __init__.py
  harness/
    __init__.py
    bedrock.py        # CLI entry (python3 -m prototype.harness.bedrock) — thin wiring only
  config.py           # agent config JSON load + system prompt assembly (with provenance)
  overrides.py        # override resolution: repo profiles + team layer (§7)
  routing.py          # ModelRouter: TaskProfile -> ModelDecision (§6)
  gate.py             # cheap post-review quality gate (§6.1)
  pricing.py          # price table + estimate_cost + CostMeter (cache-aware)
  budget.py           # AWS Budgets preflight + per-run ceiling + degrade signal (§5)
  trace.py            # structured JSONL trace writer (§8)
  tools/
    __init__.py
    bash.py           # hardened read-only bash tool (§9)
  runner/
    __init__.py       # Runner protocol + RunResult + selection
    strands.py        # primary: Strands Agents runner (only module importing strands)
    fake.py           # deterministic runner for e2e tests and pipeline dry-runs
tests/                # pytest; unit + loop + CLI e2e
examples/             # sample agent config, routing.json, overrides.cfg, prompts
scripts/
  smoke_fake.sh       # mirrors a CI call shape, runs offline (fake runner)
  smoke_bedrock.sh    # real Bedrock smoke test (needs credentials)
PLAN.md  README.md  pyproject.toml
```

Dev tooling: `uv`, `pytest`, `ruff`. Runtime: Python ≥3.10 (work code already uses
`int | None` syntax), `boto3` + `strands-agents` (pinned).

## 4. Agent loop: Strands first-class, behind a thin runner seam

**Decision (revised 2026-07-04): Strands Agents is the primary, first-class runner.**
The initial draft defaulted to a hand-rolled boto3 Converse loop for dependency
conservatism (zero new pip dependencies). That conservatism buys the wrong thing:

- Strands clears both governance gates that killed OpenCode: Apache-2.0 AWS-led OSS
  (auditable, rides the AWS ecosystem), and its only egress is `bedrock-runtime` via
  boto3. Telemetry exports nowhere unless an OTel endpoint is explicitly configured.
- Everything the hand-rolled loop would need building and maintaining — tool
  dispatch, retry/backoff on throttling, streaming, context/conversation management,
  hooks, OTel instrumentation — is exactly the undifferentiated plumbing Strands
  already ships and AWS maintains. Writing a second implementation of that so the
  integration avoids one `pip install` is effort spent on the layer with the least
  durable value.
- The stated priorities — observability and error handling — are where Strands is
  strongest (native OTel traces/metrics with token usage per call, typed hook events
  around every model/tool call, built-in retry semantics).

The pip dependency is the cost: `strands-agents` and its tree (pydantic,
opentelemetry) enter a security-reviewed pipeline. That is a procurement
formality, not an egress or license risk, and it is the one assumption to validate
early (Build order step 0). **Fallback if procurement stalls:** the current
`bedrock.py` keeps working at head — this package only replaces it when it lands, so
there is no gap; a maintained in-house Converse runner is explicitly *not* built on
spec.

The seam stays, because it is cheap and it is what makes the runner replaceable
(including by a future lab CLI or a raw-Converse port if ever forced):

```python
class Runner(Protocol):
    def run(self, *, system_prompt: str, user_message: str,
            decision: ModelDecision, cfg: AgentConfig,
            tools_cwd: str, trace: TraceWriter, meter: CostMeter) -> RunResult

@dataclass
class RunResult:
    final_text: str
    stats: dict   # turns, tool_calls, blocked_tool_calls, stop_reason,
                  # usage totals, cost_usd, total_s, outcome
```

Selected by `runner` key in agent config or `PROTOTYPE_RUNNER` env
(`strands` default, `fake` for hermetic tests and pipeline dry-runs). Nothing
outside `runner/` imports `strands`.

**Strands runner responsibilities** (harness behaviors preserved, mapped onto
Strands primitives):

- `BedrockModel` constructed from the `ModelDecision` (model id, max_tokens,
  `additional_request_fields` for `output_config.effort` — skipped for models that
  reject it, e.g. Haiku — and extended-thinking config; prompt caching enabled via
  the model's cache config, with `cache_read_input_tokens` surfaced in the trace to
  prove it's hitting).
- boto3 client config passed through (`boto_client_config`): adaptive retries,
  explicit connect/read timeouts. Throttling backoff is Strands-native
  (`ModelRetryStrategy`, exponential); retries surface on `AfterModelCallEvent.retry`
  for tracing — we configure and trace it, not reimplement it.
- The bash tool (§9) wrapped as a Strands `@tool` (we keep our own gating, not
  Strands' built-in shell sandbox); allow/block decisions and timings captured by the
  tool, mirrored into the JSONL trace via `BeforeToolCallEvent`/`AfterToolCallEvent`.
- **Turn scaling preserved**: `compute_max_turns` (<5KB→10, <20KB→15, <50KB→25,
  <150KB→35, else config max) → native per-invocation cap `Limits(turns=...)`;
  `stopReason == limit_turns` is the trigger.
- **Cost-ceiling governor** via hooks: an `AfterModelCallEvent` hook tracks cumulative
  cost in `CostMeter`; crossing `PROTOTYPE_MAX_COST` sets `BeforeToolCallEvent
  .cancel_tool = "COST BUDGET REACHED — produce your final review now"` (the string
  becomes the tool result the model reads, so it wraps up) once; past a grace factor a
  `BeforeModelCallEvent.cancel = True` hard-aborts and best-effort final text is
  extracted from `agent.messages`.
- Wall-clock `timeout_s`: a time-governor hook cancels at the next model/tool boundary
  (cooperative, like `Limits`); botocore read-timeout guards a single stuck call.
  Traced `timeout` outcome, best-effort final text.
- Per-call usage incl. `cacheReadInputTokens`/`cacheWriteInputTokens` from
  `MetadataEvent.usage` recorded per turn; totals reconciled from
  `result.metrics.accumulated_usage` into `run_end`.
- Optional OTel: `StrandsTelemetry` OTLP export enabled only when
  `OTEL_EXPORTER_OTLP_ENDPOINT` is set — off by default, no egress otherwise.

Version pinned (**strands-agents 1.45.0**); [`docs/strands-notes.md`](docs/strands-notes.md)
records the exact APIs relied on so upgrades are a reviewed diff. **Spike complete
(step 0): every assumption above verified against 1.45.0; injectable `Model` subclass
confirms the loop is testable with no AWS.**

## 5. Budget control (three layers)

1. **Monthly preflight (existing, kept):** AWS Budgets `describe_budget` against
   `PROTOTYPE_BUDGET_NAME`; at/above `PROTOTYPE_BUDGET_THRESHOLD` % → refuse to run,
   write budget status to trace, empty output, exit 2. Check failure → proceed with a
   loud warning (the per-run ceiling still protects). Budgets client is injectable for
   tests.
2. **Per-run ceiling (existing, kept + hardened):** `PROTOTYPE_MAX_COST` enforced by
   the runner via `CostMeter` (now cache-token-aware); graceful wrap-up injection,
   then hard stop. Every crossing traced.
3. **Budget-aware routing degrade (new):** preflight returns month-to-date spend %;
   past `budget_degrade.at_pct` in `routing.json` (default 80), non-pinned runs drop
   one model tier (opus→sonnet→haiku), `degraded: true` + rationale in the decision
   and trace. Profile/team-pinned runs are never silently degraded — only the
   explicit `PROTOTYPE_DEGRADE` panic lever (§6) overrides pins, and it logs loudly.

## 6. Intelligent routing

One pure function, no network calls, fully predictable from config:

```python
@dataclass
class TaskProfile:
    task: str = "review"
    diff_size: int = 0
    repo: str = ""
    changed_files: list[str] = ...
    month_spend_pct: float | None = None

resolve(profile, pin=None, config_path=None, env=None) -> ModelDecision

@dataclass
class ModelDecision:
    model_id: str
    effort: str | None
    max_tokens: int
    degraded: bool
    source: str      # "profile" | "team:<name>" | "override:<n>" | "default:<task>" | "fallback"
    rationale: str   # human-readable explanation incl. any degrade applied; logged to trace + stderr
```

`routing.json` (JSON, stdlib-parseable; single place Bedrock IDs appear):

```json
{
  "models": {
    "haiku":  "us.anthropic.claude-haiku-4-5-20251001-v1:0",
    "sonnet": "us.anthropic.claude-sonnet-4-6",
    "opus":   "us.anthropic.claude-opus-4-8"
  },
  "defaults": {
    "review":   {"model": "sonnet", "effort": "high",   "max_tokens": 16000},
    "triage":   {"model": "haiku",  "max_tokens": 4096},
    "incident": {"model": "opus",   "effort": "high",   "max_tokens": 32000}
  },
  "overrides": [
    {"when": {"task": "review", "diff_size_gt": 200000},
     "use": {"model": "opus", "effort": "high"}, "reason": "large diff"},
    {"when": {"task": "review", "paths_any": ["**/auth/**", "**/crypto/**", "**/payments/**"]},
     "use": {"model": "opus", "effort": "high"}, "reason": "sensitive paths"},
    {"when": {"repo_glob": "acme/prod/*"},
     "use": {"model": "opus"}, "reason": "critical-tier repo"}
  ],
  "budget_degrade": {"at_pct": 80, "order": ["opus", "sonnet", "haiku"]},
  "fallback": {"model": "sonnet", "effort": "high", "max_tokens": 16000}
}
```

**Config over env (decided):** the current growth of routing env vars stops here —
model choice, effort, thresholds, and task defaults are `routing.json` entries,
deployed and reviewed like code. The existing routing env vars get a documented
migration mapping into `routing.json` during integration. The only env influence on
routing is `PROTOTYPE_DEGRADE=<n>` — the CI-variable panic lever that
forces every decision down n tiers (applied last, always logged, never silently).

**Single source for model selection (decided 2026-07-04):** `routing.json` is the
*only* place a model/effort/max_tokens is chosen, for every stage. The agent config
JSON deliberately carries **no** `model`/`effort`/`max_tokens` — those keys, if
present, are inert (kept in `raw` for provenance, never read) so there is exactly one
auditable answer to "what model ran, and why." The agent config owns only loop/behavior
knobs (`max_turns`, `timeout_s`, `thinking`, `prompt_cache`, `runner`, `task`,
`quality_gate`). Per-stage model config: review = `defaults.review` (+ overrides/pins),
gate = `defaults.gate`.

**Team/group override for cost accounting:** the review model is overridable per repo
and per team via `overrides.cfg` pins (`[profile:*] model=`, `[team:*] model=`, highest
tier among matched teams wins) — so a team can dial its own review tier up or down, and
each run's trace records `matched_teams` + the `model_decision`, so spend rolls up by
team from the traces (and by task/team via the IAM application-inference-profile tags,
§ enforcement). The gate stage is intentionally uniform and cheap (`defaults.gate`,
not team-pinned); make it team-overridable later only if gate spend ever matters.

Precedence (highest wins): **repo-profile pin → team pins (highest tier among
matched teams) → first matching signal override → task default → fallback**, then
budget degrade (§5.3) applied to non-pinned outcomes, then `PROTOTYPE_DEGRADE`
applied unconditionally. First-match-wins top-down for signal overrides. Every
decision emits `source` + `rationale` so a reviewer can predict and audit any run
from the config file.

Signals available now: task, diff bytes, repo glob, changed-file paths (the bash
layer will pass `--changed-files`; confirmed). Escalation-on-failure (re-run at a
higher tier after a failed verify) is pipeline-owned and out of scope here; the
router just exposes clean pins so the pipeline can request it.

## 6.1 Post-review quality gate (cheap agent)

A second, cheap stage that runs after the main review to tighten quality and
precision — the "leverage a cheaper agent for some intelligence" lever. Built as an
quality-gate stage (one invocation still = one budget, one trace, one manifest),
enabled per `quality_gate: true` in the agent config or `PROTOTYPE_QUALITY_GATE=1`.

**Scope split (decided): cheap = hygiene, not deep accuracy.** A cheaper judge
reliably improves output quality and precision-hygiene; it cannot reliably re-judge
substantive correctness and must not try. So the gate is a single **structured-output**
call (no tools) on the `gate` task tier (Haiku by default in `routing.json`) that
takes `(diff, draft review)` and returns a cleaned review + a list of structural drops:

- Drops a finding **only** for a structural reason — its `file:line` isn't in the diff,
  it duplicates another, or it violates the output contract — each recorded with a
  reason and logged to the `gate` trace event.
- Merges duplicates, calibrates severity (**never downgrades Critical/High**), enforces
  the output format, and trims verbose padding (the concision half of the request).
- **May not** fabricate findings, soften substantive findings it disagrees with, or
  flip the verdict toward approval.

**Never-lose-findings fallback:** any gate failure — model error, parse failure, empty
output, or a suspicious shrink with no recorded drops — returns the *original* review
unchanged (logged `accepted: false`). The gate can only help or no-op.

Deep false-positive reduction remains the deferred **adversarial verify** (equal-or-
higher tier, §6 escalation) — explicitly *not* this cheap pass. The gate's tokens are
recorded into the run's cost meter (priced at the review tier — a slight, conservative
overestimate for the gate's few hundred tokens; per-turn trace has exact counts).

Verbosity is also attacked upstream, for free: the invariant **output-discipline**
block in the system prompt (`config.py` GUARDRAILS, non-overridable) instructs terse,
no-preamble, one-line-rationale output — so Opus's verbosity is trimmed at generation
time, and the gate is the backstop, not the only lever.

## 7. Override system v2 — repo profiles + team layer

Current state: `overrides.cfg` (INI) maps repo → profile; a profile can replace the
default prompt section, append context files, or swap the whole agent config. Gated by
`PROTOTYPE_OVERRIDES=1`. Limitations: exact-match repos only, no sharing across repos,
no granularity below the repo, no validation, no provenance.

Kept as INI (stdlib `configparser`, same file, existing entries stay valid).

### Format

```ini
# ---- repo profiles (existing semantics, extended) ----
[profile:acme-foundations]
system_prompt = agents/profiles/acme-foundations/system-prompt.md  ; replaces default section
context = agents/profiles/acme-foundations/review-context.md       ; comma-separated, appended
config = agents/configs/foundations.json   ; optional full agent-config replacement
model = opus                               ; NEW: routing pin (alias or full id)
effort = high                              ; NEW: routing pin
extends = base-profile                     ; NEW: single inheritance, cycle-checked

[repos]
acme/core = acme-foundations
acme/prod/* = acme-platform-infrastructure   ; NEW: fnmatch globs; exact match wins, then file order

# ---- NEW: team profiles — additive only ----
[team:payments]
context = agents/teams/payments/review-context.md
instructions = Flag missing idempotency keys on payment mutations.
model = opus                               ; bounded pin; highest tier among matched teams wins

[team:data-platform]
context = agents/teams/data-platform/review-context.md

# ---- NEW: team ownership maps, per repo or repo-glob ----
[teams:acme/core]
src/payments/**   = payments
db/migrations/**  = data-platform

[teams:acme/prod/*]
**/liquibase/**   = data-platform

# ---- NEW: CODEOWNERS as ownership source ----
[codeowners]
enabled = true                       ; parse <repo-root>/CODEOWNERS (GitLab syntax)
@acme/payments-team = payments     ; owner handle -> team profile
@acme/data-plat     = data-platform
; unmapped owners are ignored (logged once), not errors — CODEOWNERS is broader
; than review config and teams opt in by adding a mapping + [team:] section
```

### How overrides are detected from a PR

The detection chain is: **PR → changed file paths → team attribution → team
profiles**. Two ownership sources feed attribution, explicit map first:

1. `[teams:<repo>]` path globs in `overrides.cfg` — the hand-maintained, exact
   source; wins for any file it claims.
2. **CODEOWNERS** (when `--repo-root` is passed and `[codeowners] enabled`): the
   repo's own CODEOWNERS file is parsed (GitLab syntax: last-matching-pattern wins,
   section headers supported), each file's owners resolved, then owner handles are
   mapped to team profiles via `[codeowners]`. CODEOWNERS stays the org's single
   source of truth for ownership; the harness only maintains the small
   owner→profile mapping.

Files neither source claims fall back to the repo profile alone and are counted as
`unowned` in the trace — which doubles as an ownership-coverage signal per repo.

**Granularity tradeoff (decided):** the team is the floor; per-individual behavior
is out of scope. A PR spanning three teams' paths gets one review whose steering is
the union of the matched teams' additive contexts, each under a labeled
`## Team: <name>` header — deterministic, auditable, and bounded, at the cost of
teams seeing each other's steering in shared PRs. That is the acceptable tradeoff:
review output must stay consistent for the whole MR, and per-author behavior would
make the same diff review differently depending on who opened it. If multi-team
noise becomes real, the escape hatch is per-file finding filtering at post-process
time (pipeline-owned), not more granular prompts.

### Resolution

1. **Repo profile**: exact repo key first, then repo globs in file order.
2. **Team matching**: each path from `--changed-files` is attributed via the
   detection chain above. Result: the set of teams touched by the diff, which files
   matched, and via which source (`map` | `codeowners`) — all recorded.
3. **Effective run assembly**:
   - System prompt order: **baseline (invariant, never overridable) → system section
     (repo profile may replace) → repo context (appended) → matched team contexts +
     inline instructions (appended, each under a labeled `## Team: <name>` header)**,
     joined with `\n\n---\n\n`; the full sources list goes to trace and
     `--system-prompt-file`.
   - Routing pins: per precedence in §6.
   - Full agent-config replacement: repo profiles only. Teams are additive by
     design — they can steer focus and context but cannot swap models arbitrarily
     (pins resolve through the routing `models` allowlist), disable the baseline
     policy, or change the output contract.
   - **Output format/tone is a per-MR (repo-profile) property, not a team one
     (decided 2026-07-04, "Option A").** One MR produces one comment, so per-team
     *format* has no coherent answer when a diff spans teams. In a standalone repo the
     profile's `system_prompt` sets the format; in the monolith the shared profile
     owns one consistent format, and teams customize *content, severity, and tone via
     `instructions`* — not the format section. (Per-team *sectioned* output stays
     deferred; revisit only if real divergence demands it.)
4. **Overrides are best-effort — a broken override never fails the review.**
   Structural mistakes (unknown section/key, unknown profile/team reference,
   inheritance cycle) are caught, logged loudly to stderr + an `override_error` trace
   event, and the run **degrades to the default Prototype review** (baseline + config's
   default section + guardrails, no profile/teams). A missing referenced context/
   prompt file is skipped at assembly (logged + `prompt_files_missing` trace), and the
   rest of the prompt still assembles. A missing profile *config* replacement falls
   back to the base `--config`. The only load-bearing inputs are the base `--config`
   and the hardcoded baseline/guardrails; everything override-derived degrades. This
   is deliberate: overrides enhance a review, they must never block a developer's MR
   from being reviewed. (`validate()` is structural-only; file existence is handled
   gracefully at assembly, not as a hard gate.)
5. **Provenance**: `override_resolved` trace event carries profile name, matched
   teams, file→team attribution, and every contributing source path; summarized on
   stderr.

Multi-team diffs include every matched team's context (labeled). If combined team
context exceeds a size cap (default 32KB), teams with the fewest matched files are
dropped first and the drop is logged loudly.

Deliberately deferred (noted for later): MR-label team hints; per-team output
sections in the review itself; per-individual overrides (rejected, see tradeoff above).

## 8. Observability

`trace.py` writes flush-per-line JSONL. Every event: `{"ts": iso8601, "run_id": uuid,
"event": <type>, ...}`. Event vocabulary:

| Event | Payload highlights |
|---|---|
| `run_start` | argv, repo, task, diff_size, harness version+git sha, python/strands versions, config/routing/overrides digests, sandbox_tier |
| `override_resolved` | profile, matched teams, file→team map, source paths |
| `prompt_assembled` | ordered block sources (incl. each `## Team:` block), bytes, sha256 (matches the system_prompt artifact), dropped_teams, missing files |
| `model_decision` | model_id, effort, max_tokens, degraded, source, rationale |
| `budget_check` | budget name, actual, limit, pct, threshold, status |
| `phase` | phase name (resolve/budget/agent/finalize), duration_ms |
| `turn` | turn #, stop_reason, latency_ms, per-call usage (incl. cache read/write), cumulative cost |
| `tool_call` | command, allowed/blocked (+ block reason), duration_ms, exit code, output bytes, truncated |
| `retry` | attempt, error code, sleep |
| `cost_ceiling` | ceiling, estimate at crossing, action taken |
| `secret_scan` | fired (bool), match count, redactions applied (tripwire — should never fire) |
| `error` | type, message, traceback |
| `run_end` | outcome (ok/error/budget_exceeded/timeout), turns/max_turns, tool call counts, token totals, cost_usd, total_s, model_id |

`run_end` is the per-run metrics record — the pipeline can collect trace files and
roll them up (spend per task/repo/team, escalation rate, cache hit rate) without any
new infrastructure. Cross-check remains AWS-side (Cost Explorer / CloudWatch), where
the tags and IAM ceilings from the Terraform side attribute spend; the trace gives
the per-decision detail AWS can't.

Trace writing never kills a run: IO errors degrade to a one-time stderr warning.
Stderr keeps the human-readable `[harness]` narrative it has today (scaling decision,
override summary, budget status, final stats line).

The trace JSONL is the stable, pipeline-consumed interface. Strands' native OTel
sits alongside it: enabled only when an OTLP endpoint is configured, giving span-level
visibility (agent cycles, model calls, tool calls) in CloudWatch/X-Ray/Jaeger with
zero harness code. The JSONL schema never depends on OTel being on.

### 8.1 Run artifacts, manifest, and external upload seam

**Decision: the harness owns a `finalize` phase that captures and describes the run;
external upload stays a CI step.** The harness writes artifacts and a manifest; an
outside uploader reads them after the review run completes.

- **Crash robustness.** Upload runs after the harness exits. If upload fails after a
  good review, that should be handled as an upload failure. Moving artifact enumeration
  into `run_manifest.json` makes the uploader small, deterministic, and retryable.
- **Egress surface.** The harness's security story is "process egress only to
  `bedrock-runtime`." Adding `s3:PutObject` widens both the IAM role and the egress
  claim in the security review. The bash layer already holds those S3 credentials;
  leaving upload there keeps the harness's egress narrow (§9).

What the harness adds so the bash upload is comprehensive and verifiable: a
**`finalize` step that always runs** (success, budget-refusal, *and* fatal-crash
paths via the top-level handler) and emits a **run manifest** — one
`run_manifest.json` describing everything the run produced:

```json
{
  "run_id": "...", "schema_version": 1,
  "outcome": "ok|error|budget_exceeded|timeout",
  "started_at": "...", "ended_at": "...", "total_s": 12.4,
  "harness_version": "0.1.0", "harness_git_sha": "...",
  "python": "3.12.3", "strands_version": "...", "runner": "strands",
  "repo": "acme/core", "task": "review",
  "model_decision": { "...": "the §6 ModelDecision, verbatim" },
  "sandbox_tier": "A|B|C|D",
  "config_digest": "sha256:...", "routing_digest": "sha256:...",
  "overrides_digest": "sha256:...",
  "usage": { "input": 0, "output": 0, "cache_read": 0, "cache_write": 0 },
  "cost_usd": 0.0, "turns": 3, "max_turns": 15,
  "tool_calls": 7, "blocked_tool_calls": 1,
  "artifacts": [
    {"role": "review",        "path": "...", "bytes": 0, "sha256": "..."},
    {"role": "trace",         "path": "...", "bytes": 0, "sha256": "..."},
    {"role": "system_prompt", "path": "...", "bytes": 0, "sha256": "..."},
    {"role": "harness_log",   "path": "...", "bytes": 0, "sha256": "..."},
    {"role": "raw_envelopes", "path": "...", "bytes": 0, "sha256": "..."}
  ]
}
```

The manifest gives the bash S3 step one file to read to know exactly what to upload
and lets compliance verify integrity (sha256 per artifact) after the fact. Bash can
either keep uploading the explicit paths it already knows, or switch to
"upload everything the manifest lists" — its choice, no coupling either way.

**Maximal structured capture** (the part you actually care about):

- **Optional raw envelope log** (`--raw-trace <path>`, off by default): every model
  request/response and every full tool input/output written verbatim as JSONL —
  the complete, replayable record of the run. The regular `trace` keeps *truncated*
  tool output (what the model saw, to bound context); the raw log keeps the
  *untruncated* output for forensics. Gated because it's large and contains diff
  content; when compliance wants full fidelity, one flag turns it on and it's just
  another manifest artifact for bash to upload.
- **Full provenance in `run_start`/manifest**: harness version + git sha, Python and
  Strands versions, resolved config/routing/overrides digests, argv, sandbox tier,
  model decision — so a run is reconstructable from its artifacts alone, months later.
- **Per-phase timing** trace events (`phase` — resolve/budget/agent/finalize with
  durations) so latency is attributable, not just a single total.
- **Crash-path completeness**: the top-level handler emits a terminal `error` +
  `run_end` event and writes the manifest before exiting 1, so a crashed run is still
  fully described and uploadable. The empty `--output` write (frozen contract) stays.

New optional flag (§1 addition): `--artifacts-dir <path>` — if passed, the harness
writes `run_manifest.json` (and the raw log, if enabled) there and lists all
artifacts relative to it, so the bash step can `aws s3 cp --recursive` one directory.
Without it, the manifest is written next to `--trace` and paths are absolute — fully
backward compatible.

## 9. Tool sandboxing & prompt-injection posture

The diff under review is untrusted by construction — that is the job — so the model
*can* be talked into misbehaving. The design goal is not "prevent injection"
(impossible) but "make a tricked agent unable to do anything that matters" —
capability confinement, a property of the environment tools run in, not of the strings
we inspect.

**Recalibrated 2026-07-04 for CI review runs**, which changes the weighting
decisively:

- Runs are short-lived CI jobs, so local filesystem changes do not carry into later jobs.
- The load-bearing network control is an operator-managed egress policy. If data cannot
  leave to unapproved endpoints, read and analysis commands add little confidentiality
  risk.
- The task is **read-only code review**; the harness is advisory and holds no
  merge/write authority (integrity invariant, below).

An aggressive command allowlist + realpath jail was therefore the wrong trade: it
mainly produces false-blocks that fail tool calls, waste turns, and *degrade review
quality* — the opposite of the goal — while adding little once network egress and
credential isolation are handled. So the tool is **permissive about reads and analysis
(interpreters included)** and leans on cheaper, better-targeted controls.

### Threat model — what actually matters here

- **Integrity** (injection flips the verdict / suppresses findings). Bounded by one
  invariant: **the harness is advisory and never holds merge or repo-write authority**
  — it emits a comment; branch protection and humans gate the merge. A fully injected
  "approve this" merges nothing. Bonus: a diff carrying reviewer-directed instructions
  is itself a *finding* to raise — the attack becomes a detection (prompt guardrail,
  §7 baseline).
- **Exfiltration to unapproved endpoints**. Handled at the network boundary through
  operator-managed egress policy. With that in place, the posted-review channel and any
  tool the agent runs cannot reach an arbitrary attacker endpoint.
- **Confused-deputy writes using ambient CI credentials**. An egress allowlist does not
  stop writes to approved services. The control that does, for near-zero cost and zero
  review-quality impact, is the **environment allowlist** below: the tool subprocess
  never inherits those credentials, so the agent has no token to write with. The review
  agent never needs them; checkout and posting are separate CI steps.

### Controls, by load

1. **Network egress to approved endpoints only** (operator-managed). `sandbox.detect_tier()` records which OS-level tier is actually active
   (`PROTOTYPE_SANDBOX_TIER` / `PROTOTYPE_EGRESS_FIREWALL` / `PROTOTYPE_REPO_RO_MOUNT`, or
   `unshare -rn` probe → tiers A/B/C, else D) in `run_start` so the posture is always
   visible, never silently assumed. Tiers A–D and their detection remain in
   `sandbox.py`; the harness degrades gracefully and logs the tier.
2. **Environment allowlist — cred-free tool subprocess** (in `tools/bash.py`, the
   load-bearing *local* control, zero review-quality cost). Build the subprocess env
   from empty with only `PATH`/`LANG`/`LC_ALL`/`TERM` and a scratch `HOME`; the
   pipeline's `AWS_*`/`*TOKEN*`/`GIT_*` creds are never inherited. Kills the
   confused-deputy write/exfil-with-creds path regardless of egress state.
3. **Output secret-scan tripwire** (`secrets.py`, applied by the CLI before emit).
   High-signal credential shapes redacted + loudly flagged in the trace. Should never
   fire in a healthy run; it's the alarm if something slips.
4. **Command denylist — courtesy guardrail, NOT a boundary** (`tools/bash.py`). Blocks
   the obvious per-segment: network tools (`curl`/`wget`/`nc`/`ssh`/`scp`/`rsync`),
   destructive/privilege (`rm`/`dd`/`chmod`/`chown`/`sudo`/`su`/`mkfs`), git-write
   subcommands, and sensitive-path reads (`~/.aws/credentials`, `~/.ssh/id_*`,
   `/etc/shadow`, `.netrc`, ...). **Interpreters (`python3 -c`, `awk`, `sed -i`,
   `find -exec`) and broad reads are allowed** — this stops accidents and lazy exfil,
   not a determined escape (real containment is 1+2). Enforced per pipe/`&&`/`;`
   segment, skipping leading `VAR=val` so a command can't hide behind an assignment.
5. **Prompt trust boundary** (§7 baseline, non-overridable): the diff/context is
   delimited as untrusted data; reviewer-directed instructions in it are a finding.

Plus operational gates: per-call timeout (default 120s), output truncation with marker,
structured result (text, blocked, block_reason, exit_code, duration_ms) so every call
is traceable; blocked calls return an explanatory tool error the model can adapt to and
count in `run_end` stats.

### Why not the strict allowlist / realpath jail (previous draft)

Dropped deliberately. A realpath-inside-repo jail and an interpreter-free allowlist
buy little once egress is restricted and credentials are out of the tool env — reading
a world-readable file the agent cannot transmit is not an exfil — while their false-blocks
are a direct, ongoing tax on review quality. Kept only what is cheap and on-target
(credential-free env, secret-scan, obvious-write denylist, egress tier detection).

### Tests

`tests/test_bash_security.py`: broad reads + interpreters ALLOWED (regression guard
against re-tightening that would hurt review quality); network/destructive/privilege/
git-write commands and sensitive-path reads BLOCKED; per-segment enforcement (pipe into
`curl`, `&&  rm`); leading-assignment can't hide a denied command; and the cred-free
env asserted by running `env` in the subprocess with `AWS_*`/`*_TOKEN` set and
confirming none leak. Plus `secrets.py` scan tests and `sandbox.py` tier-detection
tests (each capability mocked present/absent).

## 10. Testing strategy

Layered, hermetic by default (no AWS, no network):

1. **Unit**: routing golden decisions (synthetic profiles → expected ModelDecision
   incl. rationale), override resolution matrix (exact/glob/team/multi-team/
   CODEOWNERS/unowned/error cases), prompt assembly order + provenance, cost math
   (incl. cache multipliers, unknown-model conservative fallback), turn scaling,
   budget threshold logic (injected fake budgets client), and the §9 tool suite
   (reads/interpreters allowed; network/destructive/git-write/sensitive-path blocked;
   per-segment gating; cred-free subprocess env; output secret-scan; tier detection).
2. **Loop tests**: the Strands runner against an injected scripted mock model
   (Strands model providers are injectable) — asserts tool dispatch, ceiling
   injection + hard stop, max-turn nudge, timeout outcome, usage accumulation into
   `CostMeter`, and the JSONL trace event sequence. Throttling/retry behavior
   asserted at the hook level.
3. **CLI e2e**: invoke `main(argv)` with tmp files + `PROTOTYPE_RUNNER=fake` —
   asserts output file content, `--system-prompt-file` artifact, trace validity,
   `run_manifest.json` completeness (artifact list + sha256s match on disk), the
   crash path still emitting `run_end`+manifest, and each exit code
   (0 / 1 fatal / 2 budget-exceeded).
4. **Smoke**: `scripts/smoke_fake.sh` runs the sample caller-mode invocation offline;
   `scripts/smoke_bedrock.sh` runs one real review against Bedrock when credentials are
   present (manual; also verifies effort passthrough and model availability per tier).

**Test-access model (decided 2026-07-04):** everything except the live smoke is
hermetic — no AWS, no network — so the full suite runs from `uv sync && uv run
pytest` with zero credentials. The live smoke runs in **both** places: locally for
fast iteration and in GitLab CI as the real check.

- **Credentials: assume-role.** `smoke_bedrock.sh` resolves creds from the standard
  boto3 chain and supports an explicit `AWS_ROLE_ARN` to assume (STS
  `assume_role` → session), mirroring the CI OIDC→STS path so local and CI exercise
  the same auth shape. Locally the base creds come from a profile/exported temp
  creds; in CI from the pipeline's OIDC token. The harness itself never assumes a
  role — it consumes resolved creds — so this lives in the smoke script, not the
  package.
- **Budget preflight in smoke:** pointed at a non-existent budget (→ "check failed,
  proceed with warning") or threshold set high, so the smoke doesn't require a real
  AWS Budgets resource; the real preflight path is covered hermetically with an
  injected fake Budgets client.
- **Model IDs** in `routing.json` are confirmed against what's actually enabled in
  Bedrock access before the smoke is trusted (open question 1).

## 11. Build order

Each step lands with its tests; the suite stays green throughout.

0. **Strands validation spike**: pin `strands-agents`, verify the load-bearing
   assumptions against the pinned version (injectable mock model, hook events for
   model/tool calls, per-call usage incl. cache tokens, event-loop cycle limit,
   `additional_request_fields` passthrough, boto client config). Record findings in
   `docs/strands-notes.md`. Anything that doesn't hold changes §4 before the runner
   is built on it. In parallel: kick off whatever work-side approval the new pip
   dependency needs.
1. **Scaffold**: pyproject, package layout, runner seam types, pytest wiring.
2. **Leaf modules** (parallelizable): `tools/bash.py`, `pricing.py`, `trace.py`.
3. **Routing** (`routing.py` + default `routing.json`, incl. `PROTOTYPE_DEGRADE`).
4. **Overrides v2** (`overrides.py`, incl. CODEOWNERS ingestion).
5. **Config + prompt assembly** (`config.py`).
6. **Budget** (`budget.py`).
7. **Strands runner + fake runner** (governors as hooks, trace wiring).
8. **CLI + finalize** (`harness/bedrock.py`): argument wiring, phase timing,
   crash-safe `finalize` (manifest + terminal trace events on every exit path),
   `--artifacts-dir`/`--raw-trace` handling; e2e tests + smoke scripts.
9. **Docs**: README with integration steps (copy `prototype/`, install pinned
   `strands-agents`, add `routing.json`, extend `overrides.cfg`, new optional flags
   for the bash layer, env-var → routing.json migration table).

## 12. Open questions

*(resolved 2026-07-04: team ownership uses explicit maps + CODEOWNERS ingestion;
bash layer will pass `--changed-files`; env vars reduced to operational levers with
routing config absorbed into `routing.json`; Strands promoted to first-class runner;
tool security recalibrated (§9) to egress-restriction-primary + credential-free
tool env, with a permissive read/analysis denylist instead of a strict allowlist +
realpath jail.)*

1. **Bedrock API coverage per model**: verify effort passthrough and newest-Opus
   availability through Strands' Bedrock provider before relying on T2/T3 tiers
   (`smoke_bedrock.sh` covers this).
2. **Existing routing env vars**: exact names/semantics needed during integration to
   write the migration table into `routing.json`.
3. **Dependency approval**: review `strands-agents` and its transitive tree before use in
   restricted CI environments.
4. **Egress restriction to approved endpoints** (§9, the now-primary control): confirm
   or provision the runner-level egress allowlist. Until it is live, controls 2–4
   (credential-free tool env, secret-scan, denylist) plus short-lived CI jobs hold the
   line. The harness records the active tier in `run_start` regardless.
