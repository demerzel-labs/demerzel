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
  CLI client is the contract reference.
- **M2: `anthropic`** — native Messages API with prompt-cache control, and
  the first API-key credential path (Anthropic supports no subscription
  OAuth for third-party tools). The provider trait is written in M1 with
  this adapter in mind so wiring it is mechanical, not architectural.
- Later, by demand: `openai-compat` (Chat Completions against a configurable
  base URL — Ollama, vLLM, LM Studio, llama.cpp, and most gateways; the
  widest door for an open-source audience), Gemini, Bedrock.

Adapters declare capabilities (tool calling, streaming, caching, usage fields);
the harness degrades features per capability instead of assuming them. Model
roles (`default`, `fast`, `plan`, …) map to provider+model pairs in config, with
fallback chains — per-role routing is config, not code.

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
- **Theming** for the TUI.
- Deferred until proven necessary: a WASM plugin ABI. Subprocess hooks + MCP
  cover the extension surface without bloating the binary.

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

- **Terminal-Bench 2.0** (89 Docker tasks, the field standard) as the primary
  suite.
- **Artificial Analysis Coding Agent Index** (scores model+harness stacks) as
  the external third-party target once public.
- **Fixed-model A/B**: the same model driven through Demerzel and through
  Claude Code / pi / omp isolates the harness contribution (Harness-Bench
  methodology).
- Cheap smoke: aider-polyglot-style task set for fast iteration.

### Layer 4 — loop efficiency

- Prompt-cache hit rate (cache-read tokens / input tokens) on replayed
  sessions — proves prefix discipline.
- Tokens per completed task on the Layer-3 suites — cost per successful task.
- Memory recall precision on replayed sessions with known relevant facts —
  gates the optional move from BM25 to embeddings.

### Targets

Honest ordering: first beat the incumbent class, then chase jcode with
profiling receipts.

| Metric | Gate (M4 report) | Bar (jcode) |
|---|---|---|
| Boot to interactive | < 50 ms | 14 ms |
| Single session PSS | < 40 MB | 27.8 MB |
| Marginal session PSS | < 15 MB | ~9.9 MB |
| Stripped binary | < 25 MB | — |

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
**Verify:** TUI session parity with the REPL; cache-hit rate measured on a
replayed session; config precedence tests.

### M3 — Ecosystem

Skills, hooks, MCP client with deferred discovery, subagents with typed
results, competitor-config auto-discovery, theming, project memory (store,
tools, auto-recall), and the personality profile. **Verify:** a skill, a hook,
and an MCP server exercised in a real session; discovery picks up a repo's
existing `.claude`/`.cursor` config with zero edits; a fact saved in one
session comes back in a fresh session inside the token budget.

### M4 — Optimization pass and first benchmark report

Profile against Layer 1/2 gates; land prefix stability, compaction with
distillation into memory, connection reuse. Publish the first full report: pinned competitor versions, machine spec,
raw data, all four layers. **Verify:** every gate in the targets table met or
the miss documented with a profile trace.

### M5 — Open-source launch

Docs, contribution guide, CI (fmt, clippy, test, iai-callgrind gate),
cargo-dist static releases (musl), `dmz` hardlink in installers, README with
the benchmark report. **Verify:** clean-machine install to working session in
under two minutes with each of: ChatGPT subscription, Anthropic key.

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
- Whether Terminal-Bench harness adapters accept a third-party agent cleanly or
  need a shim (investigate during M3).
- Name collision hygiene: crates.io names (`demerzel` availability) checked
  before M5.
