# Demerzel — Agent Harness Plan

A pipeline-first agent harness for organizations. Code review first, CI triage and
remediation second. Built as a private prototype with an external-shaped design:
initial review-harness use is the proving ground; commercial deployment into other
orgs' tenancies is the validated-only second act. Private preview.

## Positioning — reread this at every sprint exit

> **Demerzel drops governed AI agents into the CI pipelines you already have — running in your cloud, on your model contracts, with audit-grade control over behavior, quality, and cost.**

Compressed: *governed AI agents for your existing CI — your tenancy, your model spend,
your policy.*

Same pitch, three registers — pick by audience:

- **External buyer** (Phase 2): the one-liner above — ownership and governance SaaS
  can't offer.
- **Early adopter** (Phase 1): *"one governed way to put an agent in a pipeline —
  budgeted, audited, measured — instead of each team hand-rolling its own."*
- **Security/platform sign-off**: *"a deterministic envelope around a stochastic
  core"* — topology, config, budgets, and artifacts behave like normal CI
  infrastructure; only the model calls inside are stochastic.

**The problem, stated once.** Enterprises whose compliance posture rules out AI-review
SaaS get nothing from the tooling wave except DIY glue scripts — many hand-rolled
review agents with no shared governance, no cost accounting, and no way to measure
whether any of it works. Demerzel consolidates that sprawl into one embeddable agent
runtime: a CLI that existing pipelines call like any other step, in the org's tenancy,
on model spend the org already committed to, under org-defined policy, with quality
proven by a benchmark the org can run itself.

**Three promises — every sprint must strengthen at least one:**

- **Owned**: runs where the code lives; IAM-native; no demerzel-hosted anything.
- **Governed**: policy, not preferences — locked keys, bounded knobs, provenance.
- **Measurable**: precision/recall/cost as numbers from an eval suite, not adjectives.

**One word ban:** Demerzel does not *orchestrate*. It is *orchestrated* — by the CI
system the org already owns. The pitch is a pipeline step, never a platform.

## What it is / what it is not

**Is:**

- A Python library + CLI that runs agents inside CI pipelines (GitHub Actions, GitLab CI,
Jenkins, CodeBuild) and produces machine- and human-readable artifacts.
- A configuration system that lets teams override agent tone, output format, and behavior
at org, team, repo, directory, and path granularity without forking the harness.
- Provider-flexible: Bedrock via assumed IAM role, API-key providers (Anthropic,
OpenAI), and cloud-native endpoints (Azure OpenAI, Vertex) via LiteLLM until native
providers earn their keep.
- Cost-aware by construction: model tiering, budget caps, token accounting per run.
- Proprietary. Distributed as a private artifact (wheel/container); commercially
licensed if externalized. Open-sourcing is a deliberate non-goal — the review-agent
OSS niche is already occupied and returns nothing.

**Is not:**

- A replacement for any existing review system. Existing callers can invoke it as an
alternate review engine via a narrow CLI contract (diff in; markdown + JSON sidecar out).
- A chat assistant or IDE tool. No interactive sessions in v1.
- An SCM bot. Posting comments to PRs/MRs is an adapter at the edge, not the core.
- A hosted SaaS. It always runs in the customer's tenancy on the customer's model
access; there is no demerzel-hosted anything, ever. That constraint *is* the product.



## Identity & bet structure

**The framing: prototype evaluation with an external-shaped design, commercialized
only after validation.** The guaranteed return is a better review engine with
governance, cost accounting, and measurable quality. The external return, if the story
lands, is deployment into other orgs: proprietary, licensed, running in *their* tenancy
on *their* model spend. Every design decision below serves that two-phase structure.

### Position in the market

**Not a CodeRabbit competitor on their terms.** CodeRabbit, Greptile, Copilot code
review, and Bugbot are vertically integrated SaaS: their app, their hosted inference,
per-seat pricing. They win on zero-setup polish and distribution. What they
structurally *cannot* do — their business model requires code to leave the building —
is the target market: orgs whose compliance posture makes SaaS a non-starter, running
on model commitments they already pay for (Bedrock, Azure OpenAI, Vertex — the same
constraint profile exists on every cloud, and the segment skews Azure at least as hard
as AWS). That constraint is compliance-driven and durable; it does not evaporate when
laggard orgs "catch up on AI."

**Not a generic enterprise agent orchestrator either.** That lane (LangGraph Platform,
Bedrock AgentCore) is being commoditized by AWS and the labs. More importantly,
engineering orgs already own an orchestrator for this domain: the CI system. Demerzel
builds no scheduler, control plane, or fleet manager — one run is one pipeline
invocation that produces artifacts. CI decides when and where.

**Nearest real comp is Qodo's PR-Agent** — open source, self-hostable, Bedrock-capable,
repo-level config. The margin over "just deploy PR-Agent" is governance depth (locked
keys, bounded knobs, provenance — which it doesn't have), the embeddable artifact
contract, and an owned eval loop tuned on real MRs. That margin *is* the product, and
it must stay visibly ahead of the free alternative.

### Asset hierarchy: where value accrues

Ranked by durability against model progress, framework churn, and vendor sherlocking.
Investment and defensiveness follow this ranking:

1. **Eval data** — fixtures grown from real MRs and real misses. The only asset nobody
  can copy and the only credible basis for external claims. Treated as a first-class
   deliverable, not a testing afterthought: every real-world miss becomes a fixture as
   part of normal operation.
2. **Config governance layer** — locked keys, bounded knobs, provenance, audit trail.
  Aligned with where agentic orgs are heading: the pressure moves from "can we run an
   agent" to "who changed its behavior, what did it cost, who may override what."
   Survives any change of agent loop underneath.
3. **Artifact & CLI contract** — exit codes, JSON sidecar with cost/provenance,
  embeddability. What makes it a component instead of a destination.
4. **Pipelines & prompt composition** — semi-durable; the slot system and verification
  pattern carry across model generations even as their tuning changes.
5. **The agent loop** — replaceable plumbing, and the layer models absorb over time.
  Strands today; the `engine/` module must be able to wrap a lab CLI (Claude Code
   headless, Codex CLI) tomorrow with layers 1–4 unchanged. Never invest here beyond
   need; expect the cost-routing margin to shrink every year as model prices fall.



### The two-phase bet

**Phase 1 — guaranteed floor (M0–M4).** Sprint to the first review-harness integration.
Evaluation ROI is independent of any external outcome. The side-by-side numbers against
a baseline engine (quality, cost, latency) double as the seed of the external story.

**Phase 2 — validation-gated options (M5+).** Exercised only if the Phase 1 gate
passes. External motion: case study with real numbers → one or two design-partner pilots
at orgs with the same constraint profile (Bedrock commitments, SaaS-prohibited,
GitLab/self-hosted SCM heavy) → annual per-org licensing. Pricing is
deliberately decoupled from inference — customers bring their own model spend; the
license covers the governance layer, updates, and the benchmark tooling ("run our
eval suite against your models in your account, pick your tiers with numbers" is the
sales artifact).

### The sprawl thesis: paved road, not framework

The platform problem, restated: teams keep putting agents in pipelines with no standard
way to do it. That is a governance problem, not a capability problem — running an agent
in CI is easy, which is exactly why sprawl happens. Demerzel's competitor is not another
framework; it is *ungoverned deployment*.

The claim that wins platform/security sign-off: **a deterministic envelope around a
stochastic core.** Deterministic: pipeline topology, config resolution, budget
enforcement, exit codes, artifact schemas, prompt composition. Stochastic: the model
calls inside. Auditable in between: provenance, cost sidecar, replay, evals.

Consequences:

- **Never pitch "generic agent runner."** It reopens "why not Strands directly?" and
  invites every team's pet agent as a requirement before review proves out. Verticals
  earn trust; the shared layers become the paved road afterward.
- **Extensibility opens after the gate, deliberately.** Teams defining custom pipelines
  on the governance/model/artifact layers is Phase 2-adjacent. When it opens, a team
  shipping a non-review agent on the harness is the strongest adoption evidence that
  exists.
- **Intake discipline during Phase 1.** The answer to "I have an agent idea" is a
  triage, not a redirect: review-shaped → Demerzel today (often just config — a rule,
  a focus area, a path override); triage-shaped → roadmap (M7); neither → not yet,
  *and the ask gets recorded* (team, use case, date) in an `INTAKE.md` alongside the
  worklog. That list is gate evidence and the demand-ranked extensibility backlog —
  "no for now" becomes data instead of a closed door.
- **Scope boundary, permanent:** Demerzel is for *pipeline-shaped* agent workflows —
  batch, CI-invoked, artifact-producing. Chat assistants, IDE agents, long-running
  services, and event-driven agents outside CI are out of scope at every phase; the
  hammer stays specialized or it drowns in the sprawl it exists to fix.
- **The unit of CI standardization is a shared CI template** that calls `demerzel` —
  one line in a pipeline. Getting that template adopted is worth as much as any sprint,
  and it is nobody's deliverable unless it is explicitly owned.

**Litmus test for future features: does it strengthen *owned, governed, measurable*?**
A web dashboard drifts toward SaaS; a scheduler drifts toward orchestrator; both fail
the test. SARIF output, config provenance, and eval tooling all pass it.

## Framework decision: Strands Agents

Strands is the right starting point, validated against current docs:

- **Bedrock is first-class.** `BedrockModel(boto_session=...)` accepts a custom
`boto3.Session`, so the existing assume-role flow works without hand-rolling boto3
calls. `boto_client_config` gives retry/timeout control.
- **API-key providers exist natively**: `AnthropicModel`, `OpenAIModel`, and
`LiteLLMModel` as the escape hatch for everything else (Gemini, Mistral, vLLM, etc.).
- **Structured output** via Pydantic models (`structured_output_model=`) — findings come
back as typed objects, not parsed markdown.
- **Observability**: OTel traces/metrics including token usage per model call, which
feeds cost accounting directly.
- **Testing**: model providers are injectable, so a mock/replay model slots in cleanly.

Alternatives considered: Pydantic AI (excellent typing, weaker Bedrock story), LangGraph
(more orchestration control than we need, heavier), raw SDKs (maximum control, but we'd
rebuild the tool loop, retries, and provider abstraction — exactly the boto3 hand-rolling
this project exists to avoid).

Speed and efficiency are not framework-bound: overhead is negligible next to model
latency. The real levers are model routing, parallel per-file review, prompt caching
(Bedrock and Anthropic both support it), and aggressive context pruning. All of those
live in our code, not the framework.

**Strands' exact role — four jobs, no more:** execute model calls (our `models/` layer
decides URI/tier/auth and constructs Strands model objects as its last step), run the
agent loop (the hand-rolled `converse` loop it replaces), return structured output as
validated Pydantic objects, and surface per-call token usage for our cost meter. The
review pipeline's topology (gather → plan → review → verify → compose) is a fixed,
deterministic sequence and therefore plain Python in `pipelines/` — deliberately *not*
a Strands Graph/Swarm, which would couple the durable pipeline layer to the replaceable
engine layer for no benefit.

**Swap-safety rule:** only the `engine/` module imports `strands`, behind one narrow
contract per stage: composed prompt + context in → typed result + usage out. Config,
prompt composition, diff parsing, pipeline topology, and rendering stay
framework-agnostic. If Strands stops being the right choice, the blast radius is one
module. This rule is load-bearing for the asset hierarchy: the plausible end-state is
not "a better framework" but wrapping a lab CLI (Claude Code headless, Codex CLI) as
the execution engine while the governance, config, eval, and artifact layers — the
actual product — carry over unchanged.

## Architecture

```
demerzel/
  pyproject.toml            # uv-managed, Python 3.12+
  src/demerzel/
    cli.py                  # typer CLI: review, config, doctor, eval (triage in v2)
    config/                 # pydantic schemas, layered loader/merger
    prompts/                # core prompt parts + slot-based composer
    models/                 # provider factory, tiers, routing, cost meter
    engine/                 # ONLY module importing strands; stage-contract execution
    pipelines/
      review/               # gather -> plan -> review -> verify -> compose
      triage/               # v2: classify -> root-cause -> remediate
    scm/                    # unified diff parsing; github/gitlab adapters (later)
    render/                 # markdown/json renderers, jinja templates
  tests/
    unit/  golden/  integration/  evals/
```



### 0. Cloud surface policy: AWS-first, not AWS-only

AWS is the first surface because it fits the initial prototype sequence, not because
of the architecture. The actual AWS bindings are three, each behind an abstraction that
generalizes:


| Concern           | AWS (Phase 1)          | Generalizes to                                                                                                  |
| ----------------- | ---------------------- | --------------------------------------------------------------------------------------------------------------- |
| Model access      | Bedrock provider       | Azure OpenAI / Vertex via LiteLLM now; native providers when a buyer demands                                    |
| Workload identity | STS assume-role        | per-provider `auth` modes: ambient CI credentials (OIDC federation, managed identity), assume-role, API key env |
| Org config store  | `s3://` fetcher scheme | scheme-based fetcher: `file://`, `https://`, `s3://`                                                            |


Rules that fall out:

- **Isolation, same discipline as the Strands rule**: `boto3` may only be imported by
the Bedrock provider and the `s3://` fetcher. Layers 1–4 of the asset hierarchy
(eval, governance, contract, pipelines) stay cloud-neutral — if AWS assumptions leak
upward, the external market halves, because the SaaS-prohibited segment skews Azure.
- **Git-backed org config is the strongest governance story** and is nearly free once
the fetcher is scheme-based: org policy in a repo means policy changes are PRs —
reviewed, attributable, audit-trailed by git history rather than S3 object
versioning. A raw-file HTTPS endpoint plus a token env var covers GitHub and GitLab.
S3 is the initial artifact-store default; git-via-HTTPS is the recommended external default.
- **CI-agnostic by construction**: the harness surface is a CLI, exit codes, files,
and env vars — identical on GitHub Actions, GitLab CI, Jenkins, CodeBuild. Per-CI
code exists only at the edges (M5 comment-posting adapters), never in the core.



### 1. Layered configuration (the override system)

Granularity comes from two axes: *scope* (where config applies) and *knobs* (what it
tunes), with governance deciding who wins when layers disagree.

**Scope layers**, precedence lowest to highest:

1. **Harness defaults** — baked in.
2. **Org config** — a config the org controls, fetched by scheme (`file://`,
  `https://` — including git raw endpoints — or `s3://`) via `--org-config`. Can also
   map repo patterns to team configs so unmigrated repos inherit team defaults.
3. **Team config** — a shared file a repo opts into via `extends:`. Teams that own
  many repos maintain one conventions file instead of copy-pasting.
4. **Repo config** — `.demerzel.yml` at repo root.
5. **Directory configs** (monolith mode) — `.demerzel.yml` in any subdirectory,
   nearest-ancestor cascade, ESLint-style. How sparse-ownership monoliths get
   *distributed* config ownership: each component's config lives next to its code,
   protected by CODEOWNERS like any other file, instead of one contested root file.
6. **Run overrides** — CLI flags / env vars.

Within any layer, **path-scoped** `overrides` apply settings to matching paths only
(ESLint-overrides style). This is what makes monorepos work: the review pipeline is
per-file anyway, so each file's effective config is resolved before its review agent
spins up — different directories in one PR can get different depth, models, tone, and
thresholds.

**Monoliths and sparse ownership.** Demerzel consumes ownership; it never defines it.
Where component ownership already lives in YAML manifests (CODEOWNERS-adjacent
component files), the org or repo config maps path patterns — or the manifests
themselves — to team configs, so a PR crossing three components resolves three team
layers without anyone maintaining a parallel ownership map. Paths no layer claims fall
back to org defaults and are marked `unowned` in provenance — rolling the harness
across a monolith doubles as an audit of ownership coverage. Brownfield vs greenfield
strictness is per-path policy, not per-repo: legacy paths run report-only
(`fail_threshold: never`) while greenfield paths gate hard, and because review is
diff-scoped, enforcement ratchets on changed code without requiring the monolith be
cleaned up first. Out of scope for v1: cross-component semantic impact analysis —
findings are grounded in the diff and gathered context, not a whole-monolith
dependency graph.

**Merge semantics are fixed per field, never guessed:** scalars — higher layer wins;
maps — deep merge; lists — documented per field (`rules` and `ignore_paths` append
across layers; `focus` and `sections` replace). Pydantic validation at every layer;
unknown keys are hard errors so config drift surfaces in CI, not in silently-ignored
behavior.

```yaml
# .demerzel.yml (repo layer)
version: 1
extends: s3://org-bucket/demerzel/teams/payments.yml   # opt into the team layer
review:
  tone: concise              # preset (concise|friendly|formal) or free text
  output:
    format: markdown         # markdown | json | sarif (later)
    template: default        # or repo-relative path to a jinja template
    sections: [summary, findings, nitpicks]
  behavior:
    focus: [security, correctness]
    ignore_paths: ["vendor/**", "*.lock"]
    report_threshold: info   # what gets reported...
    fail_threshold: error    # ...vs what breaks the build (exit code 1)
    categories: { style: ignore }    # per-category mute/remap
    max_findings: 20
  rules:                     # named + additive across layers; findings cite the id
    - id: no-foo-client
      paths: ["**/*.py"]
      severity: error
      instruction: "FooClient is deprecated; flag new usage. Suggest BarClient."
    - id: migration-safety
      paths: ["db/migrations/**"]
      severity: warning
      instruction: "Flag migrations that lock large tables or break rollback."
  overrides:                 # path-scoped: monorepo granularity
    - paths: ["services/payments/**"]
      behavior: { report_threshold: info }
      models:
        routing: { review: deep }    # payments pays for the deep tier
    - paths: ["tools/scripts/**"]
      behavior: { fail_threshold: never }   # report, never block
models:
  routing:                   # which tier each pipeline stage uses
    plan: fast
    review: standard
    verify: fast
  tiers:                     # optional remap of tier -> concrete model
    fast: bedrock:us.amazon.nova-lite-v1:0
    standard: bedrock:us.anthropic.claude-sonnet-4-5
budget:
  max_cost_usd: 0.50         # hard cap per run; exceed = graceful degrade + warning
providers:
  default: bedrock
  bedrock:
    region: us-east-1
    auth: assume_role        # or `ambient` (CI OIDC / instance / env credentials)
    assume_role_arn: arn:aws:iam::123456789:role/agent-runner
  anthropic:
    auth: api_key
    api_key_env: ANTHROPIC_API_KEY
  litellm:                   # bridge for Azure OpenAI / Vertex / self-hosted vLLM
    api_key_env: AZURE_API_KEY
```

**Governance** — what makes granular safe for an org:

```yaml
# org config (s3://org-bucket/demerzel.yml)
locked: [review.behavior.categories.security]   # untouchable by lower layers
bounds:
  budget.max_cost_usd: { max: 0.50 }            # lower layers may lower, never raise
teams:                       # repos matching these patterns inherit team config
  - match: ["payments-*", "billing-*"]
    extends: s3://org-bucket/demerzel/teams/payments.yml
models:
  allowed: ["bedrock:*"]     # tier remaps must stay within this list
```

- `locked` **keys**: untouchable — a lower layer setting one is a hard config error at
pipeline start, caught by `demerzel doctor`. Distinct from bounds.
- **Bounded knobs** (`bounds`): constrained mutation — teams may lower budget caps but
not raise them; tier remaps are restricted to the org's allowed-model list.
- **Provenance**: the JSON sidecar records which layer set every effective value, and
`demerzel config resolve --path services/payments/foo.py` prints the effective
config for any file — "why did the reviewer nag about X" is answerable from run
artifacts instead of mentally merging four YAML files.

All of this is pure config-resolution logic — no models involved — so it lives in the
fast unit/snapshot layers of the test pyramid.

### 2. Prompt composition: slots, not replacement

Teams override *slots*; they never replace the system prompt. Composition order:

1. **Identity & mission** (invariant) — reviewer role, what a finding is.
2. **Operating principles** (invariant) — speed, accuracy, verify-before-report,
  prefer fewer high-confidence findings over exhaustive noise.
3. **Tone slot** (overridable).
4. **Output contract slot** (derived from `output` config — schema the model must fill).
5. **Team behavior slot** (overridable — focus areas, extra instructions).
6. **Guardrails** (invariant, always last) — overrides cannot disable adversarial
  verification, cannot change the structured output schema, cannot instruct the agent
   to approve/ignore categories of defects silently.

Placing invariants first and last bounds what a hostile or careless override can do.
Composed prompts are pure functions of config — snapshot-tested (see Testing).

### 3. Model layer: tiers, routing, cost

- **Provider factory** resolves `bedrock:` / `anthropic:` / `openai:` / `litellm:`
URIs to Strands model objects. Bedrock resolution performs the assume-role dance
(STS `assume_role` → `boto3.Session` → `BedrockModel(boto_session=...)`) with
credential refresh for long runs.
- **Tiers** (`fast` / `standard` / `deep`) decouple pipeline stages from concrete
models. Orgs remap tiers per environment; pipelines only speak in tiers.
- **Routing defaults** encode the cost thesis: cheap models for classification,
chunk triage, and verification of simple claims; the standard model only where
judgment is needed. Escalation path: if verifier (fast) and reviewer (standard)
disagree, re-verify once at standard tier.
- **Cost meter** wraps every model call: tokens in/out per tier, cumulative USD
estimate, enforced against `budget.max_cost_usd`. Emitted in the JSON sidecar
and as OTel metrics.



### 4. Review pipeline (v1 vertical)

```
gather ──> plan (fast) ──> review (standard, parallel per-file) ──> verify (fast) ──> compose
```

- **Gather**: parse unified diff, pull minimal surrounding context per hunk (not whole
files unless small), apply `ignore_paths`.
- **Plan**: fast model buckets changed files by risk and picks review depth per bucket.
Trivial changes (lockfiles, generated code, formatting) skip review entirely — this is
where most of the speed/cost win lives.
- **Review**: per-file (or per-bucket) agents run in parallel, each returning
`Finding` objects via structured output: file, line span, severity, category,
claim, suggested fix, confidence.
- **Verify (adversarial check)**: a verifier agent receives each finding plus the
relevant code and actively tries to falsify it — wrong line? claim contradicted by
code the reviewer didn't see? style opinion dressed as a bug? Findings must survive
to be reported. This is the false-positive control that makes the tool trustworthy
enough for teams to keep enabled.
- **Compose**: render surviving findings through the configured template/format.
Markdown to stdout or file; JSON sidecar always written (findings, timings, tokens,
cost, models used).



### 5. Review harness caller contract

A narrow, stable CLI boundary lets an existing caller invoke this instead of bespoke Python scripts:

```bash
demerzel review \
  --diff pr.diff            # or --diff - for stdin
  --repo-root . \
  --org-config s3://org-bucket/demerzel.yml \
  --out review.md --json-out review.json
# exit 0 = clean, 1 = findings at/above fail_threshold, 2 = harness error
```

Existing callers can keep their own PR/MR posting logic initially; the harness just
produces the comment body. Native GitHub/GitLab posting adapters come later for external
users.

### 6. CI triage & remediation (v2 vertical)

Same skeleton, different pipeline: ingest failing job logs → classify failure type
(fast model: flaky test / infra / real regression / dependency) → root-cause against
the diff and recent commits (standard) → propose remediation (comment, or a patch as
a follow-up commit/PR when confidence is high). Deferred until the review vertical
is proven; the config/model/render layers are shared unchanged.

## Testing strategy

Layered so the expensive stuff is rare and the cheap stuff is everywhere:

1. **Pure unit tests** (no models, no network): config merging/precedence, prompt
  composition, diff parsing, ignore-path filtering, renderers, cost math. The bulk
   of the suite; runs in milliseconds.
2. **Golden/snapshot tests**: composed system prompts for representative config
  combinations, and rendered markdown for fixed finding sets. Catches accidental
   prompt drift — a prompt change must show up in a reviewed diff.
3. **Agent-loop tests with a mock model**: inject a scripted model through the
  provider factory to exercise the full pipeline deterministically — tool calls,
   structured output parsing, verifier kill-path, budget-cap degradation.
4. **Replay integration tests**: recorded real model responses (cassettes) replayed
  in CI; a small live smoke test runs on-demand/nightly, never per-commit.
5. **Eval suite**: fixture diffs with seeded, labeled bugs → precision/recall per
  category, plus cost and wall-clock per run. This is the regression gate for prompt
   and routing changes ("did routing verify to the fast tier hurt recall?").
   Start with ~20 hand-built cases; grow from real-world misses.



## Roadmap: sprints and verification

Phase 1 is the funded sprint — its value does not depend on anything after the gate.
Every sprint ends runnable; verification is a command or a number, never a feeling.

**Sprint exit ritual (drift control), applied at the end of every sprint:**

1. The demo command runs from a clean checkout.
2. Tests green; new behavior covered at the cheapest test layer that can catch its
  regressions.
3. Drift check: reread Positioning. If the sprint's output doesn't strengthen
  *owned/governed/measurable*, write down why it exists or cut it.
4. Append 3–5 lines to `WORKLOG.md`: what shipped, what it bought against the problem,
  what surprised. This is the raw material for the eventual case study.
5. From Sprint 3.3 onward: eval numbers recorded; any real-world miss since last sprint
  converted to a fixture.



### Phase 1 — prototype floor

**Sprint 0.1 — Config core** *(serves: governed)*
Deliverables: uv project skeleton; pydantic config schema; layered loader with
defaults → org → team (`extends`) → repo → CLI precedence; fixed per-field merge
semantics; basic `demerzel config show` printing effective config. The resolution
order must be designed with the directory-config layer in its place (repo → directory
→ CLI) even though directory cascade implementation lands in 3.1 — retrofitting a
layer into precedence later is the kind of rework this sprint exists to prevent.
Verification:

- Unit: full precedence matrix — each layer beats the one below on scalars; maps
deep-merge; `rules`/`ignore_paths` append; `focus`/`sections` replace.
- Unit: unknown key at any layer → hard error naming the file and key.
- Demo: `demerzel config show` against the example configs in this plan.

**Sprint 0.2 — Providers & auth** *(serves: owned)*
Deliverables: provider factory resolving `bedrock:`/`anthropic:`/`openai:`/`litellm:`
URIs; per-provider `auth` modes (ambient, assume-role, API key env) with the Bedrock
assume-role flow (STS → Session → BedrockModel) and credential refresh implemented
first; tier resolution; `demerzel doctor`.
Verification:

- Unit (mock): URI parsing, tier remap, allowed-model bounds.
- Live: `doctor` passes with a real assumed role against Bedrock AND with an Anthropic
API key — both auth modes proven before any pipeline exists.
- Failure UX (asserted in tests): bad role ARN, missing env key, unavailable model
each exit 2 with an actionable one-line error.

**M0 reached** = both auth flows verified live. Already beats hand-rolled boto3 glue.

**Sprint 1.1 — Diff & render, no models** *(serves: measurable)*
Deliverables: unified diff parser; hunk context extraction; ignore-path filtering;
`Finding`/`RunReport` pydantic models; markdown + JSON renderers; sidecar schema v0.
Verification:

- Golden: fixture diffs (multi-file, renames, binary, monorepo paths) → parsed
structures; fixed finding sets → rendered markdown.
- Unit: sidecar validates against its own published JSON schema.
- Demo: `demerzel review --diff fixture.diff --dry-run` prints what would be reviewed
and what's ignored — inspectable without spending a token.

**Sprint 1.2 — First agent** *(serves: owned + measurable)*
Deliverables: `engine/` module (the only Strands import); single standard-tier reviewer
with structured output; cost meter wrapping every model call; gather → review → compose
wired end to end.
Verification:

- Mock-model: full pipeline runs deterministically; structured output parsed; sidecar
totals correct.
- Live smoke: a real diff through Bedrock; sidecar shows tokens + USD; exit codes
0/1/2 behave per contract.
- Dogfood starts: Demerzel reviews its own commits from this sprint forward.

**M1 reached** = a usable tool; can be pointed at any PR by hand.

**Sprint 2.1 — Prompt slots** *(serves: governed)*
Deliverables: slot composer (identity → principles → tone → output contract → team
behavior → guardrails); tone presets + free text; output config derives the contract
slot; `extra_instructions` wired.
Verification:

- Snapshot: composed prompts across a config matrix, including a hostile-override
fixture ("ignore all security issues") showing guardrails bound it.
- Unit: slot-order invariant — guardrails provably last regardless of config.
- Demo: `demerzel config show --prompt` prints the composed system prompt, so teams
can see exactly what their overrides do.

**Sprint 2.2 — Governance** *(serves: governed — the differentiator)*
Deliverables: org `locked` keys; bounded knobs (budgets lower-only, tier remaps within
the allowed-model list); org→team repo-pattern mapping; per-key provenance recorded in
the sidecar.
Verification:

- Unit: locked-key violation → exit 2 naming key, layer, and file; budget raise
rejected; disallowed model rejected.
- Snapshot: sidecar provenance for the full example config stack.
- Drift note: this sprint *is* the moat layer; anything cut here weakens the pitch.

**M2 reached** = overrides live and governed. The margin over PR-Agent now exists in
code, not just in this document.

**Sprint 3.1 — Plan stage & parallelism** *(serves: measurable — the cost half)*
Deliverables: fast-tier planner bucketing files by risk; skip logic for trivial
changes; parallel per-file review; per-file config resolution (path-scoped overrides
and nested directory configs go live here, plus ownership-manifest → team-config
mapping and `unowned` marking in provenance).
Verification:

- Mock: lockfile-only diff → zero review-tier calls; mixed diff → correct routing.
- Unit: monolith fixture — a PR spanning three directory configs resolves three team
  layers; an unclaimed path falls back to org defaults and is marked `unowned`.
- Measured: wall-clock on a 10-file fixture vs sequential — record the number.
- Live: same real diff with planning on/off — record the cost delta.

**Sprint 3.2 — Adversarial verify & budgets** *(serves: measurable — the accuracy half)*
Deliverables: verifier pass (fast tier) actively falsifying each finding;
disagreement escalates once to standard tier; budget cap with graceful degrade that
reports partial coverage honestly.
Verification:

- Mock: kill path, escalation path, and budget-exhaustion path each deterministic.
- Live mini-eval: seeded-bug fixtures — verifier kills planted false findings without
killing seeded true bugs.

**Sprint 3.3 — Eval suite v1 & rules** *(serves: measurable — the proof)*
Deliverables: ~20 labeled fixture diffs with seeded bugs across categories;
`demerzel eval` runner emitting precision/recall/cost/latency; named rules (additive,
findings cite rule id); `demerzel config resolve --path` with provenance.
Verification:

- Baseline eval report committed to the repo.
- Ablations recorded as numbers: verifier on/off (precision delta), tier routing
remapped (cost delta). These two numbers are the thesis, proven or broken.
- Unit: rule-to-path matching; rule-id citation in findings.
- Standing rule begins: every real-world miss becomes a fixture, checked at each
sprint exit.

**M3 reached** = speed/accuracy/cost thesis has numbers, not adjectives.

**Sprint 4.1 — Contract freeze & packaging** *(serves: owned — embeddable)*
Deliverables: CLI contract v1 frozen (flags, exit codes, versioned sidecar schema);
stdin diff; remote org-config fetcher schemes (`https://` incl. git raw endpoints,
`s3://`; `file://` exists from 0.1); wheel + container builds, signed, with SBOM
(see Distribution model — supply-chain hygiene lands here, not later).
Verification:

- Contract tests pinned to the frozen spec — any breaking change fails CI.
- Clean-room install: from artifact + credentials alone, `doctor` and `review` run in
a fresh container.
- **Precondition for 4.2: the IP question (Open questions) is resolved.** Keep the
personal clean-room boundary explicit before any private integration work.

**Sprint 4.2 — Side-by-side** *(serves: the story)*
Deliverables: Demerzel wired as an alternate review engine; representative review
runs through both engines; comparison writeup from `WORKLOG.md` + eval data; sample
teams or fixtures exercising `.demerzel.yml` overrides.
Verification: the validation gate itself, below.

**M4 reached** = prototype adoption is measurable.

### The validation gate

Phase 2 work starts only when all of these hold, measured over a representative
evaluation period — the same numbers become the design-partner pitch:

1. **Quality**: beats or matches a baseline engine on the side-by-side
   (precision at equal-or-better recall), with verifier kill-rate showing real
   false-positive reduction, not finding suppression.
2. **Adoption**: sample teams or fixtures use `.demerzel.yml` overrides voluntarily —
   proof the governance layer is wanted, not tolerated.
   Stretch signal: a team asks to run a *non-review* agent on the harness's shared
   layers — the paved road being chosen unprompted.
3. **Economics**: per-MR cost and latency numbers an external buyer would accept,
   with cost provenance to show for it.
4. **Story**: the case study is writable without spin.

If the gate fails, the project still produced a better review engine; stop there.

### Phase 2 — external options (validation-gated)

Held at milestone granularity deliberately: sprint-mapping these before gate data
exists would be planning theater. They get the Phase 1 treatment when the numbers
justify it.

- **M5 — SCM adapters.** GitHub PR / GitLab MR comment posting for external
consumers, comment-update-in-place on re-runs. GitLab first — the target buyer
(SaaS-prohibited, self-hosted-heavy) skews GitLab. *Proves: generic embeddable story.*
- **M6 — Productization for pilots.** Versioned private distribution (container +
wheel), install/upgrade docs an external platform team can follow unassisted,
license terms, the benchmark-as-sales-artifact flow (`demerzel eval` against the
customer's models in their account). One or two design-partner pilots.
*Proves: someone outside will deploy and pay.*
- **M7 — CI triage vertical.** Log ingestion, failure classification, root-cause,
remediation suggestions. Reuses everything below the pipeline layer; doubles the
story from "review tool" to "pipeline agent platform" for renewal conversations.

### Distribution model (non-OSS, decided)

- **Source-available proprietary.** Readable source under a commercial annual
  per-org license. No obfuscation, no compilation: the buyer's security team must
  review source before it runs in-tenancy, and readable source *is* the audit-grade
  positioning. The license restricts use and redistribution, not reading; the moat
  is eval data, governance design, and the update stream — never the bits.
- **No phone-home, ever.** Consistent with "no demerzel-hosted anything" and
  air-gapped buyers. Renewal nudge = an offline signed license file validated by
  `doctor`, which warns loudly on expiry but never hard-stops a pipeline. Contract
  is the enforcement; do not build license-key plumbing.
- **Channel by phase**: internal registry (M4, already in 4.1) → direct delivery of
  signed artifacts that pilots import into their own registries (M6) → managed
  private registry with per-customer revocable tokens at scale. Container image is
  a first-class artifact: `image: …/demerzel:x.y` is the natural CI install.
- **Supply-chain hygiene over DRM**: signed artifacts (cosign/attestations), SBOM,
  pinned deps, stated CVE policy — lands in Sprint 4.1 with packaging, cheap there
  and expensive to retrofit.
- **Renewal logic**: the artifact decays (model IDs churn, price tables stale,
  prompts retune per model generation, eval suite grows). The subscription is
  updates + support + benchmark tooling; model churn is the recurring demand driver.



## For the implementing agent: unknowns, decisions, stop points

Read the whole plan before coding. If you find a contradiction between sections, or
between this plan and observed reality (library APIs, tool behavior), surface it
before working around it — the plan is the contract, and silent workarounds rot it.

**Decide-and-document (your call — make it, record it in `WORKLOG.md` and in schema
docstrings, don't stop):**

- Exact severity scale and ordering (e.g. `info < warning < error`; `never` as a
  threshold sentinel) — the plan uses these values but never formalizes the scale.
- The `categories` remap grammar (`style: ignore` is shown; the full syntax is yours).
- CLI run-override grammar (e.g. `--set review.behavior.max_findings=5`).
- Internal module APIs, file layouts within the prescribed tree, test organization.
- Model pricing source for the cost meter: a static, versioned price table in-repo is
  acceptable at first — note staleness risk in its docstring (Sprint 1.2 concern).

**Stop and ask Daniel (hard stop points):**

1. **Live credentials** — Sprint 0.2's live `doctor` verification needs a real Bedrock
   role ARN/region and an Anthropic API key. Everything mock-verifiable must be green
   *before* asking. Never fake or skip a live verification and report it as done.
2. **Contract semantics** — any change to exit-code meanings, sidecar schema
   philosophy, or config precedence order. These are frozen concepts even before the
   M4 contract freeze.
3. **Isolation-rule conflicts** — if something seems to require `strands` outside
   `engine/` or `boto3` outside the Bedrock provider/s3 fetcher, stop; the design has
   a seam for it and forcing it is how the asset hierarchy dies.
4. **New dependencies** beyond the named toolchain (typer, pydantic, jinja2, pytest,
   ruff, strands, boto3) — propose with a one-line justification first.
5. **Strands API mismatch** — versions are pinned at first use; if the pinned API
   contradicts this plan's assumptions (e.g. `structured_output_model=`,
   `boto_session=`), report the mismatch rather than improvising around it.
6. **Scope pull** — anything that implements ahead of the current sprint (directory
   cascade before 3.1, governance enforcement before 2.2, engine beyond the doctor
   stub before 1.2). The sprint map is the pace, deliberately.

**Known unknowns (documented, not blocking; do not design against guesses):**

- The initial caller invocation shape — captured before M4, shapes the contract freeze.
- Azure-native provider timing — decided at the gate (LiteLLM bridges until then).
- Ownership-manifest format for the monolith mapping — examples arrive with a pilot
  consumer; Sprint 3.1 designs against a fixture, not a guess.

## Open questions (defaults chosen, flag if wrong)

- **Name**: using the workspace name `demerzel`. Rename is trivial; must happen before
any external pilot.
- **Python 3.12+ / uv / typer / pydantic / jinja2 / pytest / ruff** as the toolchain.
- **Repo config filename**: `.demerzel.yml`.
- **The initial caller invocation shape** will shape the M4 contract — worth capturing
before M4, not before M1.
- **Azure surface timing.** LiteLLM bridges Azure OpenAI/Vertex from day one; a native
Azure provider (Entra/managed-identity auth) is justified only by a concrete Phase 2
buyer. Decide at the gate, informed by design-partner conversations.
- **IP ownership.** This is built as a personal clean-room project. Keep the boundary
between personal product work and any private integration work explicit before external
pilots. This is the single biggest non-technical risk to the returns story.



## Risks

- **Verifier over-kills real findings** → track kill-rate in evals from M3; tune with
labeled fixtures before trusting it.
- **Override abuse** ("ignore all security findings") → guardrail slot ordering plus
org-level `locked` keys and bounded knobs, enforced from M2.
- **Bedrock cross-region/model availability drift** → `demerzel doctor` catches this at
pipeline start rather than mid-review.
- **Strands API churn** (pre-2.0 framework) → engine isolation rule; pin versions;
replay tests catch behavioral drift on upgrade.
- **Model progress absorbs the loop** (tiering/verification margins shrink as models
improve and cheapen) → asset hierarchy: keep investment in layers 1–4; be ready to
swap the engine for a lab CLI rather than defend the loop.
- **AWS sherlocks the category** (AgentCore/Q Developer ship "governed agents on
Bedrock") → sell where AWS is structurally weak: GitLab/self-hosted SCM shops,
cross-provider orgs, service depth with design partners; keep the eval data moat.
- **Eval rot** — the moat decays if fixtures stall at the initial ~20 → make
miss-to-fixture conversion part of normal operation from M3; if the eval suite isn't
growing, the external claims are vibes and the gate cannot be honestly passed.
- **Free alternative erosion** (PR-Agent et al. add governance features) → the margin
must be re-verified at the gate, not assumed from this document.

