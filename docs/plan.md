# Demerzel: Terminal Coding Agent — Product Plan

Demerzel is an open-source terminal coding agent written in Rust. One lightweight
binary runs an interactive agent loop in your terminal against any model provider
you point it at.

Three pillars, in order:

1. **Rich ecosystem** — skills, slash commands, hooks, MCP, subagents, and
   zero-migration adoption of config other agents already left in your repos.
2. **Customizability** — layered config, per-role model routing, theming,
   persistent memory and an agent personality tuned to how you work, and an
   unopinionated provider layer.
3. **Lightweight and fast** — jcode-class boot time and memory footprint, backed by
   a published, reproducible benchmark suite instead of a README claim.

License: MIT. Plan adopted 2026-07-31.

---

## Lineage and boundaries

This repository previously held a Python/Strands pipeline review harness. That
code, its tests, and its product contract are preserved on
`archive/python-pipeline-harness-2026-07-31` and do not constrain this plan.
Concepts carried forward: the append-only trace/session discipline, cost
awareness, and the bash-tool sandbox posture (credential-free subprocess
environment, secret scanning). Explicitly dropped: governance, organization
policy, budgets-as-refusal, and every enterprise-runner surface. Demerzel is a
personal developer tool.

Sibling boundaries: the Demerzel ADE (Rust/GPUI) is the cockpit that herds
agents; `daneel` is the ADE's thin terminal head over the fleet. Demerzel — this
repo — is the agent itself. The cockpit may attach to it like any other agent;
no code crosses.

## Command surface

The binary and brand are `demerzel`. Installers create `dmz` as a hardlink — the
blessed short command for daily use. The two names are the same program with the
same flags; docs use `dmz` in examples and `demerzel` in prose.

```text
dmz                       # interactive session in the current project
dmz "explain this error"  # one-shot prompt, prints answer, exits
dmz -c                    # continue the last session in this project
dmz --model gpt-…         # model/provider override for this invocation
dmz doctor                # providers, credentials, tools — presence, not values
```

Exact flag surface is designed in M1; the shape above is the contract:
zero-config first run when one provider credential or local endpoint exists,
one-shot and interactive from the same binary, sessions resumable.

---

## Architecture

Cargo workspace. M0 starts with a single crate and splits as seams appear —
crates are extracted when a second consumer exists, not before:

```text
crates/
  demerzel/        # bin: CLI, wiring          (exists from M0)
  demerzel-core/   # loop, sessions, compaction (split in M1)
  demerzel-providers/  # provider trait + adapters
  demerzel-tools/  # read, edit, create, bash, grep, glob
  demerzel-tui/    # ratatui interface
bench/             # benchmark suite (exists from M0, see below)
```

Runtime choices: tokio, reqwest with rustls (keeps musl static builds clean),
ratatui + crossterm for the TUI. A custom terminal renderer (jcode built its
own) is considered only if the bench suite shows ratatui as the bottleneck.

Platforms: Linux and macOS are the supported targets. Platform-specific
code stays POSIX, behind `cfg(unix)`. CI builds and tests both systems.
The `bench/` tooling measures on both platforms; the plan is
[`bench-platforms.md`](bench-platforms.md). Windows is out of scope
until a milestone names it.

### Edits: hash-anchored

File reads return numbered lines under a short snapshot tag of the whole
file. The edit tool addresses lines by number and sends the tag back. The
harness applies the edits only while the file still hashes to that tag; a
stale tag returns a precise error the model can recover from. Prior art:
oh-my-pi's Hashline (MIT), which reports large output-token savings and
eliminates the whitespace-mismatch failure class of search/replace diffs.

### Tools: in-process where it counts

Search and glob run in-process (`grep`/`globset` crates), not as spawned
subprocesses — no fork-exec cost per tool call. Bash runs as a real subprocess
with the carried-over posture: constructed environment (no ambient credentials),
per-call timeout, output truncation with markers, structured results. Approval
modes gate writes and command execution; policy lives in the harness, never in
the prompt (prompt-prefix stability, see efficiency checklist).

Decisions from the alignment review of 2026-08-07:

- Write tools stay inside the project root. The confinement is a
  consistency guardrail, not a sandbox: bash in `auto` mode can write
  anywhere. Do not present it as a security boundary. Add a config
  override when a real session needs one.
- The bash process group ends with each call, so a command cannot
  leave a server running. This is the accepted posture until the
  background-task decision (see open questions).
- Tool execution is sequential in M1. Frontier models use parallel
  tool calls heavily; revisit with the Layer-2 numbers.

### Providers: neutral by construction

The harness defines its own message, tool-call, and usage model; adapters
translate. No adapter is privileged.

- **M1: `openai-codex`** — the Responses API against the ChatGPT-subscription
  Codex endpoint, with OAuth credentials. No API key. The open-source Codex
  CLI client is the contract reference. Known risk: the endpoint is
  subscription-gated, not a public API. OpenAI permits forks of the Codex
  CLI but does not say whether third-party clients may use the ChatGPT
  OAuth path
  ([openai/codex#8338](https://github.com/openai/codex/discussions/8338)).
  OpenAI can close the path at any time. The `anthropic` adapter and the
  later `openai-compat` adapter keep the harness usable if that happens.
- **M2: `anthropic`** — native Messages API with prompt-cache control, and
  the first API-key credential path (Anthropic supports no subscription
  OAuth for third-party tools). The provider trait is written in M1 with
  this adapter in mind so wiring it is mechanical, not architectural.
- **M3: `openai-compat`** — Chat Completions against a configurable base
  URL. This is the door for every vendor and gateway that speaks the
  format: xAI, Gemini's compatible endpoint, DeepSeek, OpenRouter,
  Ollama, vLLM, LM Studio, llama.cpp. A new vendor is a `base_url`
  and a key in config, not an adapter. Moved here from the post-M5
  local track on 2026-08-26 (see the decisions below).
- **Post-M5: the local-model track** — a managed local engine behind
  the M3 adapter. See the post-M5 milestone.
- Later, by demand, and only for what a compatible endpoint cannot
  carry: Gemini, Bedrock.

Adapters declare capabilities (tool calling, streaming, caching, usage fields);
the harness degrades features per capability instead of assuming them. Model
roles (`default`, `fast`, `plan`, …) map to provider+model pairs in config, with
fallback chains — per-role routing is config, not code.

Decisions from the obsolescence review of 2026-08-26. The harness must
not go stale when a model or a vendor ships:

- No model catalog in code. A model id is a string the config names
  and the adapter sends; the server refuses an id it does not know.
  Capability lives on the adapter, never on a model id. The only model
  id an adapter carries is its default. One narrow exception, from
  M2.5 step 8e: an adapter carries a static table of known model ids
  and their context windows. A window is a budget number, not a
  capability. An unknown id shows the count with no share, and
  the `context_window` config key overrides a stale row.
- `openai-compat` lands in M3, first step, not post-M5. Until it
  lands, the harness is neutral in shape but has two doors; after it,
  a vendor with a compatible endpoint needs no code.
- `[providers.<name>]` gains `kind`, the adapter id, so one adapter
  serves many named instances (`[providers.xai] kind =
  "openai-compat"`). A table without `kind` keeps today's meaning: the
  name is the adapter id. This is config-key growth; the adapter PR
  carries it, this plan decides it.
- A vendor adapter exists only for what a compatible endpoint cannot
  carry: prompt-cache control, subscription OAuth, a native tool.
  xAI, Gemini, and Bedrock arrive through `openai-compat` first.

Decisions from the alignment review of 2026-08-23 (the M2 close-out):

- The crates, the contracts, and the seams match this plan; no
  simplification cut was needed.
- Watched surface: five of the six capability flags are declared by
  the adapters and read by nothing — only `tool_calls` branches the
  harness. The flags stay as the declared contract for M3 (parallel
  tool calls, degradation), but a flag still unread at the M3
  close-out gets removed.
- Contract growth M1 to M2, from the metrics receipts: the provider
  trait, the engine events, and the tool set are unchanged; config
  grew its first 8 keys; the CLI gained `plain` and `provider`.
  Lines 7,876 to 15,618; direct dependencies 18 to 23, all named in
  milestone plans.

### Sessions

Append-only JSONL, one file per session, project-scoped under XDG data. The
record is the source of truth for resume, compaction, and replay — and doubles
as the fixture format for the bench suite's deterministic transcripts. No
secrets in session files.

### Memory and personality

Memory is a first-class feature of the harness, not user-space scaffolding. A
user does not have to build a second brain for their agent by hand.

- **Project memory**: a per-project fact store under XDG data (SQLite). Each
  record holds one fact with provenance: session id and date. The agent gets
  explicit tools — save, search, forget.
- **Auto-recall**: at session start the harness injects the most relevant
  facts, inside a fixed token budget, at a fixed position in prompt assembly,
  so the stable-prefix discipline holds.
- **Lexical retrieval first**: BM25, the same engine as deferred tool
  discovery. Simple beats embeddings until a measurement says otherwise, and
  lexical retrieval protects the binary-size gate. Local embeddings are an
  optional upgrade, added only if Layer-4 benches show lexical recall failing.
- **Compaction feeds memory**: when a session compacts, durable facts distill
  into the store instead of vanishing with the truncated context. One
  mechanism serves context economy and long-term knowledge.
- **Personality**: a user-scoped profile that tunes the agent to one human's
  way of working — tone, workflow preferences (plan-first or dive-in,
  test-before-done, verbosity), and standing corrections. The profile is a
  plain file the user can read and edit. The agent proposes profile additions
  from in-session feedback; it never rewrites the profile silently. Project
  memory stays per repository; the personality travels with the user.

Prior art: jcode's semantic memory, oh-my-pi's Hindsight fact bank, and the
MEMORY.md pattern in Claude Code.

### Ecosystem and customizability

- **System prompt, fully customizable** (decided 2026-08-02): the harness
  owns a small base prompt, and config can replace it completely — the Pi
  model, not the Codex or Claude Code model. Extension layers (project
  instructions, skills, personality, memory recall) compose at fixed
  positions so the prompt prefix stays stable. A user who replaces the base
  prompt owns the consequences; the harness does not merge or repair it.
  The composition order is designed in M3.
- **Markdown-first extension**: project instructions (AGENTS.md), skills, and
  slash commands as plain files. **Auto-discovery of competitor config** —
  `.claude/`, `.cursor/`, CLAUDE.md, pi/omp files — so adopting Demerzel in an
  existing repo requires zero migration (omp proved this wedge).
- **Hooks** as subprocesses: any language, JSON on stdin/stdout, at defined loop
  events.
- **MCP client** with deferred tool discovery: core tools in the prompt, the
  catalog behind a BM25 search tool.
- **Subagents** returning typed, schema-validated results — no prose parsing.
- **Theming** for the TUI (M2.5).
- Deferred until proven necessary: an in-process scripting layer.
  Subprocess hooks + MCP cover the extension surface without bloating
  the binary, and hooks are not hot-path — a ~1 ms fork-exec at loop
  events is invisible. The trigger for in-process is a per-keystroke
  or per-frame extension point: the status line, keybindings, custom
  interface widgets. **Lua is the named candidate** (decided
  2026-08-23), ahead of a WASM ABI: an embedded Lua costs a few
  hundred KB inside the size budget, starts fast, and matches the
  terminal-tool culture this audience lives in (Neovim, wezterm).
  Config stays TOML either way: config must read back statically for
  `doctor` and the precedence display, so it never becomes a script.

### Loop-efficiency checklist

Standing requirements, tracked from M1 (lineage: the OpenAI Codex/ChatGPT loop
write-up):

- append-only history, stable prompt prefixes; volatile state (approvals, UI)
  never enters the prompt;
- prompt-cache discipline per provider, with cache-hit rate measured;
- deferred tool discovery;
- compaction with a measured token budget;
- persistent HTTP connections;
- code mode (model writes a script calling tools as functions) — later, once
  tool traffic justifies it.

---

## Benchmark program

Performance claims are a product feature, so they get product discipline. The
bar is jcode's published numbers: 14 ms boot, 27.8 MB single-session RAM,
~9.9 MB per additional session, against competitors at 383–3,436 ms and
140–387 MB. jcode measures PSS "on this Linux machine across 10 interactive PTY
launches" and publishes no harness, hardware spec, or raw data. Demerzel wins
the credibility fight by publishing all three.

### Rules

- The suite lives in `bench/`, versioned with the code. Anyone can rerun it.
- Competitor versions are pinned per report; the machine spec is disclosed.
- Raw data (JSON) is committed alongside every published table.
- Statistics, not anecdotes: warmups, ≥50 runs for timing, confidence
  intervals (hyperfine defaults).
- Same metric as the competition where comparison is claimed: PSS from
  `/proc/<pid>/smaps_rollup`, so numbers are comparable with jcode's.
- Two reference machines, one Linux and one macOS, each with its own
  receipts and trend line. Numbers never compare across operating
  systems; each OS compares against competitors measured on the same
  machine with the same metric. macOS memory uses physical footprint,
  the metric Apple's tools report. The platform plan is
  [`bench-platforms.md`](bench-platforms.md).

### Layer 1 — footprint and speed (the headline claims)

- **Boot**: hyperfine over `--version` (pure binary cost) and a PTY
  time-to-interactive probe (spawn in a pseudo-terminal, wait for the ready
  marker), cold and warm.
- **Memory**: PSS/USS sampled from `smaps_rollup` in scripted scenarios — idle
  single session, 10 concurrent sessions (reports marginal cost per session),
  session with a large transcript loaded. Peak via cgroup-v2 `memory.peak`.
- **Binary size**: stripped release binary; `cargo-bloat` breakdown to keep
  regressions attributable.

### Layer 2 — harness overhead (deterministic, CI-friendly)

A mock OpenAI-compatible server replays canned transcripts, so no model and no
network variance. Measures: harness-added latency per turn, tool-call round
trip (spawn bash, read, edit), streaming render throughput. criterion for
micro-benchmarks; iai-callgrind instruction counts as the CI regression gate
(deterministic, immune to runner noise).

### Layer 3 — agentic capability (harness + model)

- **Terminal-Bench 3.0**, pinned, as the headline terminal suite.
  **Terminal-Bench 2.1** stays as the longitudinal track for comparison
  across releases.
- **SWE-bench Verified** as the repository-repair track.
- **Fixed-model A/B**: the same model, budget, task versions, and
  environment driven through Demerzel, through a minimal scaffold, and
  through one reproducible production harness. The pairing isolates the
  harness contribution.
- Cheap smoke: a 3–5 task hermetic subset of the private task bank (see
  the evaluation contract) for per-PR iteration.
- **Artificial Analysis Coding Agent Index** stays an external
  reference, not release evidence.

### Layer 4 — reliability and efficiency

Layer 4 reports a performance–reliability curve, not one efficiency
score:

- `pass@1`, `pass@k`, and `pass^k` on repeated Layer-3 trials, with
  task-level confidence intervals.
- Accepted tasks and repeated-run success plotted against tokens, cost,
  wall time, context growth, and machine resources.
- Prompt-cache hit rate (cache-read tokens / input tokens) on replayed
  sessions — proves prefix discipline.
- Memory recall precision on replayed sessions with known relevant facts —
  gates the optional move from BM25 to embeddings.

### Evaluation contract

The evaluated candidate is model × harness × tools × environment ×
budget, not a bare model and not one transcript. The contract is
versioned with the code:

1. **Candidate envelope**: the Demerzel commit and binary, the provider
   and model snapshot, system-prompt and tool-schema hashes, the
   environment image, the retry and context policy, and the execution
   budget.
2. **Scenario**: clean setup from an immutable base, a deterministic
   seed, the allowed tools, a state oracle, policy assertions, slice
   labels, and an optional fault profile.
3. **Evidence bundle**: the typed model and tool trace, artifacts, the
   final-state diff, outcome assertions, token and tool cost, wall
   time, memory and I/O, and an infrastructure-failure classification.
4. **Grading order**: outcome first — inspect the final state, do not
   trust the agent's claim of success. Layers 1–2 stay deterministic.
   Trace graders serve diagnosis and mandatory approval paths.
   Calibrated rubric judges decide only where code and state cannot.
5. **Release gates**: a confirmed critical policy failure blocks.
   Capability changes use explicit non-inferiority margins. Cost,
   latency, and resource budgets warn or block separately — they never
   disappear into one scalar score. No harness-superiority claim
   without a same-model, same-budget paired result.
6. **Living suite**: reproduced dogfood failures graduate into a small
   per-PR regression bank. Nightly runs add repetitions, perturbations,
   and judge grades. Sealed and adversarial cases stay pre-release.

Build order across milestones: measurement seams land in M2, the runner
foundation at the M3 exit, the public baseline as the first M4
workstream, the published receipts as an M5 launch gate. The milestone
sections below carry the details.

### Targets

Honest ordering: first beat the incumbent class, then chase jcode with
profiling receipts. The jcode bar is a Linux-PSS claim and gates the
Linux track only. The macOS memory gates are provisional until the
first macOS receipt calibrates them
([`bench-platforms.md`](bench-platforms.md)).

| Metric | Linux gate | macOS gate | Bar (jcode, Linux) |
|---|---|---|---|
| Boot to interactive | < 50 ms | < 50 ms | 14 ms |
| Single session memory | PSS < 40 MB | footprint < 60 MB, provisional | 27.8 MB |
| Marginal session memory | PSS < 15 MB | footprint < 20 MB, provisional | ~9.9 MB |
| Stripped binary | 25 MB budget | 25 MB budget | — |

Binary size is a budget, not a wall (decided 2026-08-23). Function and
measured performance never get cut only to stay under a size number.
A change that grows the binary more than 10% lands with a
`cargo-bloat` receipt naming what the bytes buy; crossing a budget
needs the receipt plus a note in the active plan doc. The direction
stays lightweight: the smaller dependency at equal function, lazy
loading, measurement before addition.

---

## Milestones

### M0 — Fresh slate (this commit)

Plan, MIT license, workspace with a compiling `demerzel` binary, bench skeleton
(methodology doc, boot runner, PSS sampler). **Verify:** `cargo build --release`
succeeds; `demerzel --version` runs; bench scripts execute against the stub.

### M1 — Dogfoodable core

Agent loop with streaming, `openai-codex` provider (ChatGPT-subscription
credentials imported from the Codex CLI), core tools (read, hash-anchored
edit, create, bash, grep, glob), approval modes, plain-terminal REPL (no full TUI),
session JSONL with resume, one-shot mode. Provider trait + capability model
designed for the M2 Anthropic adapter. **Verify:** a real multi-turn coding
session in this repo against the Codex endpoint;
hermetic loop tests against the mock server; boot/memory benches produce first
real numbers.

### M2 — Interface and second provider

ratatui TUI (streaming, edit previews, approvals), `anthropic` adapter with
prompt caching and API-key credentials, `dmz login` for the ChatGPT
subscription, layered config (user, project, flags), slash commands, `doctor`.
Measurement seams for the evaluation contract: engine events and session
records keep usage, cost, cache, tool-step, termination, environment, and
final-outcome fields. The full evaluation runner does not delay M2.
**Verify:** TUI session parity with the REPL; cache-hit rate measured on a
replayed session; config precedence tests.

### M2.5 — Interface build-out

The full-screen interface becomes the primary surface. It renders
model output as formatted text with highlighted code. It shows
thinking blocks that open and close, tool cards with status, duration,
and output, and a live progress line. It takes multi-line input with
command completion. Colors come from a theme with config keys, and a
first-run screen greets a machine without credentials. The plain REPL
stays the low-dependency fallback with its M2 behavior. The Layer-1
gates and the render bench hold; the stripped binary stays under
15 MB. Plan: [`m2.5-plan.md`](m2.5-plan.md). **Verify:** a dogfood
session shows every element; a PTY scenario per element; the render
bench inside its gates.

### M3 — Ecosystem

The `openai-compat` adapter with named provider instances first, then
skills, hooks, MCP client with deferred discovery, subagents with typed
results, competitor-config auto-discovery, project memory (store,
tools, auto-recall), and the personality profile. At the M3 exit, the
evaluation runner foundation: the Harbor agent adapter, versioned candidate
and scenario manifests, the evidence-bundle serializer, artifact and
state-oracle interfaces, and a 10–20 task private bank built from reproduced
dogfood failures, with a 3–5 task hermetic smoke subset cheap enough for CI.
**Verify:** a session against a
compatible endpoint the mock server stands in for; a skill, a hook,
and an MCP server exercised in a real session; discovery picks up a repo's
existing `.claude`/`.cursor` config with zero edits; a fact saved in one
session comes back in a fresh session inside the token budget; the smoke
subset runs hermetically.

### M4 — Optimization pass and first benchmark report

The public benchmark baseline is the first workstream: pinned
Terminal-Bench 3.0 as the headline track, Terminal-Bench 2.1 as the
longitudinal track, SWE-bench Verified as the repository-repair track, and
fixed-model A/B pairs against a minimal scaffold and one reproducible
production harness — same model snapshot, effort, budgets, task versions,
and environment. Then profile against Layer 1/2 gates; land prefix
stability, compaction with distillation into memory, connection reuse.
Publish the first full report: pinned competitor versions, machine spec,
raw data, all four layers. **Verify:** every gate in the targets table met or
the miss documented with a profile trace.

### M5 — Open-source launch

Docs, contribution guide, the full CI gate (iai-callgrind on top of the
fmt–clippy–test matrix that landed after M2 step 2), cargo-dist releases —
static musl for Linux, x86_64 and aarch64 darwin for macOS — `dmz` hardlink
in installers, README with the benchmark report. The benchmark receipts are
a launch gate: committed manifests, raw results, traces, slice reports,
confidence intervals, cost, tokens, and infrastructure-error rates, under
the evaluation contract. **Verify:** clean-machine install to working session in
under two minutes with each of: ChatGPT subscription, Anthropic key.

### Post-M5 — Local models and benchmark expansion

The local-model track, in three phases. The engine stays outside the
Demerzel executable, so the <25 MB binary gate holds. "Offline" means no
network egress by default. Telemetry stays out; any future telemetry must
be opt-in.

1. **Local provider contract.** A local server is an ordinary
   `openai-compat` instance (the M3 adapter): explicit capabilities,
   model identity recorded in session files, the same tool schemas,
   approval policy, loop, and error taxonomy. Add a no-egress
   integration test and local-provider fixtures to the mock server.
2. **Managed sidecar.** `dmz --offline-model <model.gguf>`. Download a
   pinned, checksum-verified `llama.cpp` build only after explicit
   approval. Discover existing GGUF files and running servers before any
   download. Estimate weights-plus-KV-cache memory and show the
   assumptions before launch. Distinguish owned from attached servers;
   clean up only owned processes.
3. **Native engine experiment.** Consider in-process inference only if it
   beats the managed sidecar on measured stable-prefix reuse, tool-call
   validity, subagent batching, footprint, latency, and accepted-task
   rate. The likely default stays the sidecar.

Benchmark expansion, same placement: SWE-bench Pro and Multilingual, Aider
Polyglot as an edit-protocol diagnostic, METR-style time-horizon
stratification, and a Factorio long-horizon track. Same-model local/cloud
runs join the Layer-3 pairs, with raw receipts published like every other
claim.

---

## Non-goals

- Governance, organization policy, audit surfaces, budget refusal — gone with
  the archive, not deferred.
- Hosted anything: no gateway, no relay, no telemetry endpoint. Network egress
  is model providers and MCP servers the user configured, nothing else.
- IDE plugins, browser IDE, editor embedding (ACP support may ride the session
  API later; not a launch goal).
- A cockpit: fleet management belongs to the ADE.
- Benchmark claims ahead of the published suite — no number leaves this repo
  without `bench/` receipts.

## Open questions

- Background tasks: the bash process group ends with each call, so a
  session cannot keep a server running. Decide the mechanism in M2 or
  M3.
- Prompt/context format details: how skills and AGENTS.md compose into the
  system prompt while keeping the prefix stable.
- Compaction split: how much of a compacted session becomes memory facts and
  how much becomes summary text; measure both on replayed sessions.
- Personality profile format: one file or layered (user + per-project
  overrides); decide in M3 with real dogfood feedback.
- Subagent transport: in-process vs subprocess-per-agent; footprint benches
  decide.
- Whether the Harbor agent adapter accepts a third-party agent cleanly or
  needs a shim (build at the M3 exit; Terminal-Bench runs through Harbor).
- Name collision hygiene: crates.io names (`demerzel` availability) checked
  before M5.
