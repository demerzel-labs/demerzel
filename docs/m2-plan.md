# M2 Implementation Plan — Interface and Second Provider

This plan turns the M2 milestone of [`plan.md`](plan.md) into ordered,
verifiable steps. It follows the shape of [`m1-plan.md`](m1-plan.md).

## Status (2026-08-22)

M1 is complete (PR #16). Steps 1 to 6 are merged: the plan (PR #17),
layered config and `doctor` (PR #18) with the platform statement
(PR #19), the `anthropic` adapter (PR #20), `dmz login` (PR #23), the
TUI foundation (PR #24), and TUI interaction (PR #25). CI hardening
landed between steps (PR #26). Step 7, the measurement pass, is
merged (PR #27): the render-throughput bench, the cache-hit
measurement for both providers, the turn record that closes the
measurement seams, and the receipts in `bench/results/`. The build
order is complete. The close-out — the alignment review and the
metrics receipt — is next. Then M2.5, the interface build-out
([`m2.5-plan.md`](m2.5-plan.md)), runs before M3; theming moves
there from M3.

Exit criteria, checked at step 7:

1. TUI parity with the REPL — the step-6 dogfood (PR #25). Done.
2. A live multi-turn tool session against the Anthropic API — step 3.
   Done.
3. `dmz login` against the real endpoint — step 4. Done.
4. The cache-hit rate on a replayed session for both providers — step
   7, numbers below. Done.
5. Config precedence tests — step 2. Done.
6. Hermetic adapter tests for both providers — steps 3 and 5. Done.
7. `cargo fmt`, `cargo clippy`, and `cargo test` clean; the Layer-1
   benches inside the gates after the TUI — the step-5 and step-6
   receipts. Done.

Step-7 receipts (2026-08-22, Ryzen 9 5900X; raw data and the machine
spec sit in `bench/results/`):

- Render, headless 120x40 screen over 200 history lines: one redraw
  188 µs (criterion's typical estimate); 64 KiB of text in 16-byte
  deltas ingests in 246 µs (254 MiB/s); the same stream with a frame
  every 64 deltas takes 13.2 ms for 65 changed frames, 4,900 frames
  per second. The stream number is frame-bound. At the 16 ms frame
  cap one frame costs about 1% of its budget, and the cost does not
  move with the history length: only the visible tail wraps.
- Turn latency against the local mock, rerun because the turn record
  adds one append per turn: `turn_text` 140 µs, `transport_floor`
  42 µs, `turn_read_tool` 298 µs — level with the 2026-08-08 receipt
  (140 / 42 / 302 µs). The append sits inside the noise.
- Prompt-cache hit rate, `anthropic` (`claude-sonnet-5`), two fresh
  sessions of six user turns, the full tool set advertised: 87% and
  90% of all input tokens read from cache, 97% and 98% on the last
  call. Every warm call read the previous call's whole prompt:
  `cache_read` equals the previous `input` minus 2 on 25 of 25 warm
  calls. The second session's first call read 2,054 of its 2,056
  tokens from the first session's cache: the static prefix — system
  prompt, tool definitions, first prompt — is byte-stable across
  sessions. An earlier run in this step with the read-only tool set
  had a 980-token static prefix, under the model's 1,024-token
  minimum cacheable length, and read nothing across sessions; the
  full tool block is the prefix a real session sends.
- Prompt-cache hit rate, `openai-codex` (`gpt-5.6-sol`), same script:
  17% and 44%. No call read the previous call's whole prompt, the
  second session's first call read nothing, and reads arrived late,
  in 128-token steps, and fell back to zero — the last call of the
  first session read 0 of 11,283 tokens. The adapter sends no
  `prompt_cache_key`. See the step-7 decision below.

## Goal

After M2, Demerzel runs interactive sessions in a full-screen TUI. A
second adapter speaks the Anthropic Messages API with an API key.
`dmz login` replaces the Codex CLI import for the ChatGPT subscription.
Configuration comes from layered files and flags. The plain REPL from
M1 stays as the fallback interface.

## Exit criteria

M2 is done when all of these hold:

1. A TUI session in this repo reaches parity with the REPL: streamed
   text and reasoning, tool status lines, edit previews, approval
   prompts, resume, and the Ctrl-C rules at every point.
2. A live multi-turn session with tools completes against the Anthropic
   API with an API-key credential.
3. `dmz login` completes the PKCE flow against the real endpoint. The
   stored credentials drive a session without the Codex CLI import.
4. The prompt-cache hit rate is measured on a replayed session for both
   providers. The raw data sits in `bench/results/`.
5. Config precedence tests pass: flags beat environment, environment
   beats project file, project file beats user file, user file beats
   defaults.
6. Hermetic adapter tests pass against the mock server for both
   providers. No network, no model.
7. `cargo fmt`, `cargo clippy`, and `cargo test` are clean. The Layer-1
   benches stay inside the M4 gates after the TUI lands.

## Scope

In: ratatui TUI (streaming render, edit previews, approval prompts,
status bar, scrollback), `anthropic` adapter with prompt caching and
sealed thinking, API-key credentials, `dmz login` (PKCE for the ChatGPT
subscription; key entry for Anthropic), layered TOML config, slash
commands (`/help`, `/model`, `/new` plus the M1 `/exit`), `doctor`,
file-backed REPL history, cache-hit measurement.

Out (deferred to the milestone in parentheses): skills, hooks, MCP,
subagents, competitor-config discovery, memory, personality (M3),
theming (M2.5), per-role model routing and fallback chains (M3, with
subagents),
compaction (M4), `openai-compat` adapter (post-M5, phase 1 of the
local-model track in [`plan.md`](plan.md)), background-task
mechanism (decision revisited at the end of M2; see the open question
in [`plan.md`](plan.md)).

## Workspace after M2

```text
crates/
  demerzel/            # bin: CLI, REPL, wiring, config resolution
  demerzel-core/       # domain types, provider contract, loop, sessions, policy
  demerzel-providers/  # openai-codex + anthropic adapters
  demerzel-tools/      # read, edit, create, bash, grep, glob
  demerzel-tui/        # ratatui interface (new)
  demerzel-mock/       # mock server: /responses + /v1/messages routes
bench/                 # + render-throughput bench, cache-hit measurement
```

## Design

### Layered config (demerzel bin)

TOML, three layers plus defaults. Highest wins:

1. Command-line flags, then their environment variables.
2. Project file: `.demerzel/config.toml` at the project root. The
   project root is the working directory of the run — the M1 rule
   for tools and sessions. There is no upward search.
3. User file: `$XDG_CONFIG_HOME/demerzel/config.toml`.
4. Built-in defaults.

M2 keys, all optional. The live reference for every key is
[`config.md`](config.md).

```toml
provider = "openai-codex"        # or "anthropic"
model = "gpt-5.6-sol"
approval = "ask"                 # read-only | ask | auto
history_file = true              # file-backed REPL/TUI input history

[providers.openai-codex]
base_url = "https://chatgpt.com/backend-api/codex"
model = "gpt-5.6-sol"

[providers.anthropic]
base_url = "https://api.anthropic.com"
model = "claude-sonnet-5"
```

Rules:

- Resolution happens once at boot into the existing `RunConfig`. The
  engine and the tools never read config files.
- Unknown keys warn on stderr and do not stop the run. This keeps old
  binaries usable on newer config files.
- A `[providers.<id>]` table sets per-provider endpoint and model. The
  top-level `provider` key picks the active one. Role tables come in
  M3; the shape above stays valid when they land.
- In one layer, a `[providers.<id>]` value is more specific than the
  top-level `model`, so it wins. Across layers, the higher layer wins.
- Trust rule (step-2 review decision): a project file travels with the
  repository, so it cannot raise privilege. It cannot raise the
  approval mode above the user setting, it cannot set `base_url`, and
  it cannot turn the history file on when the user file turns it off.
  Ignored keys warn on stderr and in `doctor`. A project `provider`
  key can still pick among the endpoints the user file defines;
  step 3 revisits that edge with the second provider.
- A one-shot run clamps `approval = "ask"` to read-only. There is
  nobody to ask.
- No secrets in config files. Credentials live in the auth store only.
  `doctor` warns when it finds a key-shaped value in a config file.

### `doctor` (demerzel bin)

`dmz doctor` reports presence, not values:

- Which config files exist, and which layer set each active key.
- Which provider credentials exist in the auth store, with expiry
  state for OAuth tokens. Never the token text.
- Tool preconditions: bash resolves, the project root is writable,
  the session directory is writable.
- Exit code 0 when a session can start; 1 otherwise.

### `anthropic` adapter (demerzel-providers)

Native Messages API with SSE streaming. The M1 provider contract fits
it by design; the adapter is translation, not architecture.

- POST `{base_url}/v1/messages` with `stream: true`. Auth header:
  `x-api-key`. Version header pinned per the API reference.
- Credential order: `ANTHROPIC_API_KEY` environment variable, then the
  `anthropic` section of the auth store.
- Content mapping: `Text`, `ToolUse`, `ToolResult` map one-to-one.
  Thinking blocks map to `Reasoning { summary, sealed }`; the
  signature travels in `sealed` and returns on later turns for
  same-provider continuity, exactly like the Codex sealed payloads.
- Prompt caching: `cache_control` breakpoints at fixed positions — the
  end of the system prompt, the end of the tool definitions, and the
  latest history tail. Positions are fixed so the prefix stays stable.
  `Usage.cache_read_tokens` maps from `cache_read_input_tokens`.
- Capabilities: streaming, tool calls, parallel tool calls declared
  false in M2 (execution stays sequential), usage in stream, cache
  control, sealed reasoning.
- Retries mirror the codex adapter: backoff on 429, on 5xx, and on
  network errors; fail fast on other 4xx. On 401 there is no refresh —
  the error tells the user to check the key.
- The parser ignores unknown events and fields.

The mock server gains a `/v1/messages` route that replays scripted
Messages-API SSE transcripts, plus fixture builders next to the codex
ones.

### `dmz login` (demerzel bin)

Two forms, one command:

- `dmz login` — PKCE authorization-code flow for the ChatGPT
  subscription. The Codex CLI login flow is the wire-contract
  reference, and `CLIENT_ID` is already in the codebase. Steps:
  1. Start a loopback listener on `127.0.0.1`, on the first free
     registered redirect port: 1455, then 1457. The authorization
     server accepts the redirect only on these ports (step-4
     decision), so an ephemeral port cannot receive it. Tests bind
     port 0.
  2. Open the browser to the authorization URL with an S256 challenge.
     When the browser does not open, print the URL.
  3. Receive the code on the loopback listener, exchange it for
     tokens, and write them to the dmz auth store with mode 0600.
  4. When no browser and no loopback are available, accept a pasted
     redirect URL on stdin as the fallback.
- `dmz login anthropic` — prompt for an API key with echo off, verify
  it with one cheap API call, and store it with mode 0600.

The loopback listener handles one request with a hand-rolled HTTP
reply. The flow needs no HTTP-server dependency, the same call the
M1 plan made for hand-rolled SSE parsing.

### TUI (demerzel-tui)

ratatui + crossterm, alternate screen, a new crate as planned in
[`plan.md`](plan.md). The bin crate picks the interface: TUI when
stdout is a terminal, REPL under `--plain`, one-shot with a prompt
argument.

Layout:

- Transcript pane: streamed text, dim reasoning summaries, one status
  line per tool call, edit previews rendered as diffs from the edit
  tool input (the `ToolRun` engine event already carries the full
  input).
- Input line at the bottom with history. The same reader-ownership
  rule as the REPL: one owner for input events, requests over a
  channel.
- Status bar: model, approval mode, session id, token usage.
- Approval prompt: inline yes / always / no with the M1 grant
  semantics. Ctrl-C rules carry over unchanged from the REPL
  (documented in [`m1-plan.md`](m1-plan.md)).

Contracts:

- The TUI prints the `demerzel ready` marker on stderr before it
  enters the alternate screen. The PTY probe keeps working and gains
  a TUI scenario.
- The renderer consumes the existing `EngineEvent` stream. The engine
  does not change for the TUI.
- Theming is M3. The M2 palette is the terminal's own colors.

### Slash commands (demerzel bin)

One registry serves the REPL and the TUI. M2 commands: `/help`,
`/model <name>` (switches the session model), `/new` (starts a fresh
session), `/exit` and `/quit` from M1. The registry maps a name to a
handler; M3 file-based custom commands extend the same table. Unknown
commands keep the M1 error shape.

### Cache-hit measurement (bench)

A Layer-4 slice, pulled forward because it is the M2 exit criterion:

- Replay a recorded session against each live provider twice. Report
  `cache_read_tokens / input_tokens` per turn and per session.
  Step-7 shape: `bench/cache/replay.py` replays the user side of one
  session, a fixed list of six user turns over this repository. It
  runs them through the real engine
  (`crates/demerzel/examples/cache_replay.rs`) as two fresh sessions,
  one after the other. The model side is live, so the two sessions
  differ in what the model reads and how many calls it makes. The
  numbers that compare across milestones are the per-call ratios and
  the first call of the second session, not the session totals. The
  engine advertises the full tool set in `ask` mode with no approver,
  the prefix a real session sends; only the read tools run. The
  prompt list carries no machine paths. `input_tokens` counts the
  cached parts for both providers, so the ratio compares across them.
- Commit the raw JSON to `bench/results/` with the provenance
  envelope.
- Add a render-throughput bench for the TUI (Layer 2). Step-7 shape:
  `crates/demerzel-tui/benches/render.rs` drives the transcript and
  the frame renderer through a headless screen over a synthetic delta
  stream in process. The screen is `demerzel_tui::bench::Screen`,
  hidden from the docs and not interface contract. The number is the
  layout and the buffer diff of the live interface, minus the
  terminal write; the mock transport stays outside, so it is render
  cost alone. Three benches: one redraw, a 64 KiB text stream with no
  frames, and the stream with a frame per 64 deltas.
- Measurement seams (evaluation-contract prework, see
  [`plan.md`](plan.md)): engine events and session records keep
  usage, cost, cache, tool-step, termination, environment, and
  final-outcome fields. Step 7 checked the fields and added the two
  that were missing — the turn record and the cache-write count; the
  inventory is a step-7 decision below. The full evaluation runner
  waits for the M3 exit and does not delay M2.

## Dependencies

Each new dependency must pass a `cargo-bloat` check against the
< 25 MB binary gate. Dev-only crates are exempt.

| Crate | Purpose |
|---|---|
| ratatui (no default features where possible) | TUI |
| crossterm | terminal backend, input events |
| toml | config files |
| sha2 | PKCE S256 challenge |
| getrandom | PKCE verifier bytes |

Not added: an HTTP-server crate (the loopback listener is hand-rolled),
a browser-opener crate (`xdg-open`/`open` via `std::process::Command`,
with the URL printed as fallback).

## Build order

Each step is one PR with its own verify line.

1. **M2 plan.** This document. *Verify: review.*
2. **Layered config and `doctor`.** TOML resolution into `RunConfig`,
   precedence tests, `doctor`, file-backed history. *Verify: config
   precedence tests; `doctor` against a machine with and without
   credentials.*
3. **`anthropic` adapter.** Messages API streaming, tool calls,
   thinking round-trip, cache breakpoints, API-key path, mock route,
   conformance tests. *Verify: hermetic adapter tests; one live smoke
   call with a real key.*
4. **`dmz login`.** PKCE flow, loopback listener, pasted-URL fallback,
   `dmz login anthropic`. *Verify: a real login on this machine drives
   a session without the Codex CLI file.*
5. **TUI foundation.** `demerzel-tui` crate, event loop, streaming
   render, input line, ready marker, `--plain` fallback. *Verify: a
   scripted PTY session against the mock; boot and idle benches stay
   inside the gates.*
6. **TUI interaction.** Edit previews, approval prompts, slash-command
   registry shared with the REPL, status bar. Step-5 decision: both
   interfaces open the session on the first prompt, so an idle boot
   reads no session file. The status bar therefore names a resumed
   session from the first prompt on, not at boot. *Verify: a real
   dogfood session in this repo through the TUI; Ctrl-C at every
   point.*
7. **Measurement pass.** Cache-hit rate on replayed sessions for both
   providers, render-throughput bench, results committed. *Verify: raw
   JSON in `bench/results/`; exit criteria checked off.*

## Decisions to make during M2

- Anthropic default model — decided in step 3. The live `/v1/models`
  list confirms `claude-sonnet-5` as a served id; it is the adapter's
  default. Adaptive thinking with summarized display is the default
  request shape.
- Cross-provider model keys — decided in step 3. Two rules:
  1. Each adapter exports its own default model. When no layer sets a
     model, the active provider's default applies.
  2. A top-level `model` key applies when its layer names the active
     provider or names no provider. When its layer names a different
     provider, the resolver skips the key: an explicit pair stays
     paired. A bare `model` key follows the active provider by
     design, and the `--model` flag and `DEMERZEL_MODEL` always win;
     a mismatch there fails at the API with a clear unknown-model
     error. Pin models in `[providers.<id>]` tables to scope them
     explicitly.
- Trust-rule edge (step-2 review) — revisited in step 3, kept as is.
  A project `provider` key picks among the endpoints the user file
  defines. The user wrote those endpoints and stored those
  credentials, so the pick is the same class of choice as a model
  name. Endpoints and credentials still come only from flags, the
  environment, and the user file. The `--base-url` flag and
  `DEMERZEL_BASE_URL` stay provider-agnostic and follow a provider
  switch; they are the user's own input, outside the trust rule.
- Per-input approval grants (deferred from M1) — decided in step 6.
  An `always` answer scopes by access class. On an execute tool the
  grant covers only the exact input, so one `always` on bash cannot
  approve every later bash command — the injection surface a step-3
  dogfood review flagged. On a write tool the grant keeps the tool
  name as its scope: every edit input differs, so a per-input grant
  there would never match again. `/help` states the rule.
- `/model` and the session file — decided in step 6. A switch
  appends a meta record as an audit trail; a resume still takes its
  model from config, because config cannot tell a default from an
  explicit choice yet. A real switch drops the sealed reasoning
  payloads from the in-memory history — a sealed payload is bound to
  the model that produced it. Known gap, present since M1: a resume
  with a different model still replays the file's sealed payloads.
  The M3 fix is a model field on the sealed payload, which also lets
  a resume adopt the session's own model.
- `/model` to a model outside the request shape — observed in the
  step-6 live dogfood. The `anthropic` adapter requests adaptive
  thinking by default, and a model without it rejects every turn
  with the API's own 400 message; `/model` back restores the
  session. This matches the step-3 rule for `--model`: an unknown or
  mismatched name fails at the API with a clear error. Per-model
  capability flags are M3 work, with the role tables.
- One model seam for M3 — noted in step 6 review. The interfaces own
  the session model, so `Runtime::open_session` and `Runtime::engine`
  both take it. Per-role routing in M3 must fold these into one
  call with a routing struct, not widen both signatures again.
- Measurement seams — checked in step 7. Where each field lives:
  1. Usage and cache: the `ModelUsage` engine event and the `usage`
     session record. `input_tokens` is the whole prompt;
     `cache_read_tokens` and `cache_creation_tokens` (new in step 7)
     are the parts read from the cache and written into it. The
     `anthropic` adapter had folded the write count into the input
     total, which hid the cost split.
  2. Tool steps: the `ToolStart`, `ToolRun`, and `ToolEnd` events;
     the `tool_use` and `tool_result` blocks in the messages; the
     `approval` record with the call id for every policy decision.
  3. Environment: the meta record — project directory, provider,
     model, creation time, schema version.
  4. Termination and the per-turn outcome: the `turn` record, new in
     step 7. The engine appends it when a user turn returns:
     `outcome` is `completed`, `max_tokens`, `refused`, `call_limit`,
     or `aborted` (`TurnOutcome::as_str`), or `error` with the
     provider's message cut at 1 KiB. A dropped turn future, a
     process exit mid-turn, and a session write failure write none,
     so a user turn without one was cut off. Readers skip the record,
     so old binaries read new files. The file carries no clock beyond
     the meta record's creation time, so wall time per turn stays the
     runner's to measure, as it does for the cache bench.
  5. Cost: derived from the three usage parts and the model the meta
     record names. The harness carries no price table; prices change
     faster than releases, and the evaluation runner owns pricing.
  6. Final outcome: the task's final state belongs to the runner's
     state oracle. The harness's final outcome per turn is the turn
     record.
  The check is a test over a real session file
  (`the_session_file_carries_the_measurement_seams`).
- Codex prompt-cache key — found in step 7, open. The `openai-codex`
  adapter sends no `prompt_cache_key`. On the same six-turn script
  the endpoint served 17% and 44% of the input from cache against
  87% and 90% for the `anthropic` adapter, and no Codex call read its
  predecessor's whole prompt. The Codex CLI sends a per-conversation
  key, and the endpoint's documented routing uses it. Proposed as its
  own pull request, the first item of the M4 prefix-stability work,
  pulled forward because the receipt is in hand:
  1. Add an optional cache key to the neutral `Request`. The struct
     is non-exhaustive, so the field is additive.
  2. Let the engine fill it with the session id.
  3. Send it as `prompt_cache_key` from the `openai-codex` adapter.
  4. Rerun `bench/cache/replay.py --provider openai-codex`.
  It touches the provider contract, so it does not ride in step 7.
- File-backed history default. The key exists in step 2; decide the
  default (on or off) after dogfood. Step 2 ships with `on` until
  then.
- The REPL's long-term place once the TUI reaches parity — decided
  in the M2.5 plan: `--plain` stays supported as the low-dependency
  path and keeps its M2 behavior; presentation work is TUI-only.
- Background tasks: revisit the open question at the end of M2 with
  TUI and config in hand; the mechanism itself is expected in M3.

## Risks

- **TUI regresses the Layer-1 numbers.** ratatui and crossterm add
  code and startup work. Mitigation: the step-5 verify line runs the
  boot and idle benches; lazy TUI construction; `cargo-bloat` check.
- **Messages-API contract drift.** Thinking-block and cache semantics
  change with model generations. Mitigation: capability flags, pinned
  version header, recorded fixtures, parser ignores unknown fields.
- **PKCE against a backend not built for third parties.** Same risk
  class as the Codex endpoint in M1. Mitigation: the Codex CLI login
  as the reference, the pasted-URL fallback, recorded fixtures for the
  token exchange.
- **Config sprawl.** Every key added in M2 is contract surface.
  Mitigation: the minimal key set above; unknown-key warnings instead
  of errors keep later additions non-breaking.
- **Two interfaces drift apart.** REPL and TUI must keep the same
  semantics. Mitigation: both consume the same `EngineEvent` stream
  and the same command registry; the parity dogfood in step 6.
