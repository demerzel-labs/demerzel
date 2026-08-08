# M1 Implementation Plan — Dogfoodable Core

This plan turns the M1 milestone of [`plan.md`](plan.md) into ordered,
verifiable steps.

## Status (2026-08-08)

All 7 steps are done. The exit criteria hold. Workspace state after
step 7: 140 tests, `cargo fmt` and
`cargo clippy --workspace --all-targets -- -D warnings` clean,
release binary at 7.0 MB.

First bench numbers (2026-08-08, Ryzen 9 5900X; raw data and the
machine spec sit in `bench/results/`):

- Boot (`--version`): 1.0 ms mean over 50 runs.
- Time to interactive (PTY probe, median of 50): 1.2 ms warm, 4.4 ms
  cold with verified page-cache eviction. The probe's own spawn
  floor, 0.55 ms, sits inside both numbers.
- Idle REPL memory (median of 10 launches): 516 KiB anonymous — the
  stable own-heap number — and 3.5 MiB PSS, which also counts clean
  mapped binary text.
- Per-turn latency against the local mock (mean): 140 µs text only,
  302 µs with one read dispatch, over a 42 µs transport floor. The
  harness itself adds roughly 97 µs to a text turn.

Notes for the next session:

- M2 is next: TUI, `anthropic` adapter, `dmz login`, layered config,
  `doctor`, slash commands. Read [`plan.md`](plan.md) for the scope
  and write an M2 plan first.
- The PTY probe and the idle-memory driver give each run a private
  `XDG_DATA_HOME` with a synthetic auth store. A bench run never
  reads the real login and never touches the network.
- The mock server has a looping mode (`MockServer::start_looping`)
  for unbounded benchmark runs. That mode records no requests.
- The criterion bench (`crates/demerzel/benches/turn.rs`) builds the
  engine per iteration outside the timed span. The number includes
  the localhost round-trip to the mock and the session appends.
- The REPL prints `demerzel ready` as its first line, on stderr; the
  constant is `demerzel::READY_MARKER`. The PTY probe waits for it
  on the merged stream.
- The bash tool kills its whole process group after every call, so a
  command cannot leave a background server running. This is the
  accepted M1 posture (PR #11). The background-task decision is an
  open question in [`plan.md`](plan.md).
- `AGENTS.md` holds the pull-request gate and the source rules that
  bind every step.

## Goal

After M1, Demerzel can run a real multi-turn coding session in this repository
against the ChatGPT-subscription Codex endpoint. The loop, the tools, and the session
record work. The TUI, the Anthropic adapter, and the ecosystem features come
later.

## Exit criteria

M1 is done when all of these hold:

1. A multi-turn coding session in this repo completes against the Codex
   endpoint with ChatGPT-subscription credentials.
2. Hermetic loop tests pass against the mock server. No network, no model.
3. The boot benchmark and the memory benchmark produce first real numbers, and
   the raw data sits in `bench/results/`.
4. A prompt-prefix test proves that the prefix stays byte-identical across
   turns.
5. `cargo fmt`, `cargo clippy`, and `cargo test` are clean.

## Scope

In: provider trait with capabilities, `openai-codex` adapter with streaming
(the Responses API over the ChatGPT subscription), OAuth credential import
and refresh, core tools (read, hash-anchored edit, create, bash, grep, glob),
approval modes, agent loop, JSONL sessions with resume, one-shot mode,
plain-terminal REPL, mock server, first bench numbers.

Out (deferred to the milestone in parentheses): TUI (M2), `anthropic` adapter
and API-key credentials (M2), `dmz login` PKCE flow (M2), layered config
files (M2), `doctor` (M2), slash commands (M2), skills, hooks, MCP,
subagents, memory (M3), compaction (M4), `openai-compat` adapter for local
endpoints and gateways (by demand).

## Workspace after M1

```text
crates/
  demerzel/            # bin: CLI, REPL, wiring (lib target: run wiring)
  demerzel-core/       # domain types, provider contract, loop, sessions, approval policy
  demerzel-providers/  # openai-codex adapter; re-exports the contract
  demerzel-tools/      # read, edit, create, bash, grep, glob
  demerzel-mock/       # mock OpenAI-compatible server (dev/test/bench only)
bench/                 # boot + memory runners, PTY probe, criterion harness
```

`demerzel-mock` gets its own crate because it has two consumers: the hermetic
loop tests and the Layer-2 benchmarks. It never links into the shipped binary.

## Design

### Domain types (demerzel-core)

The harness owns its own message model. Adapters translate to and from it.

- `Message { role, content: Vec<ContentBlock> }`
- `ContentBlock`: `Text`, `ToolUse { id, name, input }`,
  `ToolResult { tool_use_id, content, is_error }`,
  `Reasoning { summary, sealed }` — `sealed` is an optional provider-tagged
  opaque payload
- `Usage { input_tokens, output_tokens, cache_read_tokens }`
- `StopReason`: `EndTurn`, `ToolUse`, `MaxTokens`, `Aborted`

### Provider trait (demerzel-core; re-exported by demerzel-providers)

```text
trait Provider {
    fn capabilities(&self) -> Capabilities;
    async fn stream(&self, request: Request) -> Result<EventStream>;
}
```

Stream events: `TextDelta`, `ReasoningDelta`, `ToolCallStart`,
`ToolCallDelta`, `ToolCallEnd`, `ReasoningDone`, `Usage`,
`Done(StopReason)`. `ReasoningDelta` streams the readable reasoning
summary. `ReasoningDone` carries the sealed payload. `ToolCallEnd`
carries the complete parsed tool input.

`Capabilities` declares: streaming, tool calls, parallel tool calls, usage in
stream, cache control, sealed reasoning. The harness reads the flags and
degrades features. It never assumes a capability. The trait must already fit
the M2 Anthropic adapter: content blocks, cache-control hooks, and usage
fields map one-to-one to the Messages API. The `Reasoning` block covers both
shapes: OpenAI sealed reasoning items and Anthropic thinking blocks with
signatures.

### openai-codex adapter

The first adapter speaks the Responses API against the ChatGPT-subscription
Codex endpoint. The open-source Codex CLI client is the contract reference.
Step 2 checks request and event shapes against that source and records live
fixtures. The backend is not a public API; the parser must ignore unknown
events and fields.

- POST `{base_url}/responses` with SSE streaming. Default base URL:
  `https://chatgpt.com/backend-api/codex`.
- Auth: `Authorization: Bearer <access_token>` plus the ChatGPT account-id
  header. No API key. API-key auth arrives in M2 with the Anthropic adapter.
- Always `store: false`. The endpoint returns reasoning items with sealed
  payloads. The adapter keeps them and sends them back on later turns for
  same-provider continuity. It never sends them to a different provider.
- Tool definitions serialize as function tools. Streamed tool-call arguments
  assemble per output item.
- One shared `reqwest` client with rustls. Connections persist across turns
  (loop-efficiency checklist).
- On 401: refresh the token once, retry once, then fail with a clear message.
- Retry with backoff on 429, 5xx, and network errors. Fail fast on other 4xx.

### Credentials

M1 does not implement a login flow. It reuses the Codex CLI login:

1. If the dmz auth store (`$XDG_DATA_HOME/demerzel/auth.json`, mode 0600)
   holds valid credentials, use them.
2. Else, if `~/.codex/auth.json` exists, import it into the dmz auth store.
3. Else, exit with a message: log in with the Codex CLI, or wait for
   `dmz login` (M2).

dmz refreshes tokens with the standard OAuth refresh flow. It writes updates
to its own store only, never to the Codex CLI file. Model and endpoint
overrides stay available for tests: `--model`, `--base-url`,
`DEMERZEL_MODEL`, `DEMERZEL_BASE_URL`. The mock server accepts any token.

### Tools (demerzel-tools)

Common trait: name, description, JSON-Schema input, structured result.
Output truncation adds explicit markers. Errors return to the model as
`ToolResult { is_error: true }` with a message the model can act on.

- **read** — returns a `[path#tag]` snapshot header and numbered lines
  (`42:line text`). The tag is a short hash of the whole file. Prior art:
  oh-my-pi Hashline.
- **edit** — addresses lines by number and validates the snapshot tag from
  the latest read. A stale tag returns an error that lists the current
  lines near the edit target. Line numbers are unique, so no anchor is
  ambiguous. The response returns the new tag and the renumbered changed
  lines, so chained edits need no fresh read.
- **create** — writes a new file. It fails when the file exists, so an
  overwrite is always an explicit read plus edit. Prior art: omp keeps
  file creation out of the Hashline edit language.
- **bash** — real subprocess. Constructed environment (PATH, HOME, TERM,
  LANG; no ambient credentials). Per-call timeout. Captured stdout and
  stderr with truncation markers. Exit code in the result. The whole
  process group ends with the call, on every exit path.
- **grep** — in-process, `grep-searcher`/`grep-regex` crates. No fork-exec.
- **glob** — in-process, `globset` + `ignore` crates. Respects `.gitignore`.

### Approval policy (demerzel-core)

Three modes: `read-only`, `ask` (interactive default), `auto`. The policy
gates the write and execute tools (edit, create, bash). The engine checks
the policy before every dispatch, keyed on the access class the tool host
declares, so a composite tool host cannot bypass the gate. Policy lives
in the harness, never in the prompt. Approval decisions go into the
session log as non-prompt events with their call ids, so the record stays
auditable and the prefix stays stable.

Non-interactive runs (one-shot) default to `read-only`. `--auto` enables
writes.

### Loop (demerzel-core)

1. Append the user message to history.
2. Assemble the prompt: fixed system prompt, then history. No volatile state.
3. Stream the model response to the renderer.
4. If the response contains tool calls: check policy, run the tools
   sequentially, append the results, go to step 2.
5. Stop on `EndTurn`, on the turn limit, or on user abort (Ctrl-C). Abort
   cancels the in-flight request and leaves the session valid.

### Sessions (demerzel-core)

- One JSONL file per session:
  `$XDG_DATA_HOME/demerzel/projects/<project-id>/sessions/<ulid>.jsonl`.
- First record: metadata (schema version, cwd, model, timestamps). Then one
  record per event: messages, tool results, approvals, usage.
- Append-only. The file is the source of truth for resume and replay, and it
  is the fixture format for the mock server.
- Plaintext and self-contained. The session layer must pass five checks:
  inspect, export, replay, audit, delete (spec source: the Earendil
  session-portability tests). Delete means: remove the file, nothing remains.
- Sealed reasoning payloads go into the record next to their readable
  summary. A sealed item is never the only carrier of meaning.
- The harness writes no secrets into the file. Auth tokens live only in the
  auth store. `dmz -c` loads the latest session for the project and rebuilds
  history.

### CLI and REPL (demerzel bin)

Flag surface for M1:

```text
dmz                        # interactive REPL
dmz "prompt"               # one-shot, prints answer, exits
dmz -c                     # continue the last session in this project
dmz --model NAME           # model override
dmz --base-url URL         # endpoint override
dmz --auto | --read-only   # approval mode override
dmz --version
```

The REPL is a plain terminal loop: line editor, streamed tokens, one status
line per tool call, approval prompts (yes / always / no). It prints a ready
marker on start;
the PTY boot probe waits for that marker. No ratatui in M1.

Decisions that bind the M1 REPL (landed in step 6):

- One dedicated thread owns the line editor and stdin. The REPL loop
  and the approval prompts request lines over a channel, so no two
  readers ever hold stdin.
- Ctrl-C during a turn aborts the turn by a drop of the turn future.
  Ctrl-C at the main prompt clears the prompt. Ctrl-C at an approval
  prompt stops the whole turn, and every unrun call gets an error
  result. The session file stays valid across aborts; the next turn
  repairs a cut tool call.
- The approval prompt takes yes, always, or no. An `always` answer
  grants the tool name for the rest of the run. The grant does not
  survive a resume; per-input grants are an M2 decision.
- Sessions open on the first submitted prompt, so an idle boot reads
  and writes no session file.

### Mock server and tests (demerzel-mock)

The mock server implements `/responses` and replays canned Responses-API SSE
transcripts, keyed by request order. Deterministic, no network variance, no
auth. One replay script serves a 401 first to test the refresh path.

Test list:

- Adapter conformance: request serialization, SSE parsing, tool-call
  assembly, reasoning round-trip, usage capture, retry behavior.
- Credentials: import from `~/.codex/auth.json`, refresh on 401, no write to
  the Codex CLI file.
- Hermetic loop: a multi-turn read → edit → bash scenario; stale-anchor
  recovery; tool errors surfaced to the model; abort mid-stream; resume
  rebuilds identical history.
- Prefix stability: the assembled prompt prefix is byte-identical across
  turns, and approval state never appears in it.
- Tools: anchor stability, stale-edit rejection, bash timeout and
  environment construction (assert no inherited secret variables), grep and
  glob against fixtures.

### Benchmarks

- Extend `bench/boot/` with the PTY time-to-interactive probe (spawn in a
  pseudo-terminal, wait for the ready marker), cold and warm.
- Run the hyperfine and PSS suites against the real binary. Commit the first
  results with the machine spec to `bench/results/`.
- Add a criterion benchmark: harness-added latency per turn against the mock
  server.
- iai-callgrind comes with CI in M5. Note it in `bench/README.md`.

## Dependencies

Each new dependency must pass a `cargo-bloat` check against the < 25 MB
binary gate. Dev-only crates are exempt.

| Crate | Purpose |
|---|---|
| tokio | async runtime |
| reqwest (rustls, no default features) | HTTP client |
| serde, serde_json | domain types, JSONL, tool schemas |
| base64 | JWT claim decode (ChatGPT account id) |
| clap | CLI |
| rustyline or reedline | REPL line editing (decide in step 6) |
| grep-searcher, grep-regex, globset, ignore | in-process search tools |
| libc (unix only) | bash process-group kill on timeout |
| thiserror, anyhow | errors |
| axum (dev-only) | mock server |
| criterion (dev-only) | Layer-2 micro-benchmarks |

SSE parsing is hand-rolled. The format is small and a dependency is not
justified.

## Build order

Each step is one PR with its own verify line.

1. **Workspace split and domain types.** New crates `demerzel-core`,
   `demerzel-providers`, `demerzel-tools`. Message model, provider trait,
   capabilities. *Verify: build, unit tests, clippy.*
   **Done — PR #5, merged 2026-08-02.**
2. **Mock server, openai-codex adapter, credentials.** Responses-API SSE
   streaming, tool-call assembly, retries, auth import and refresh.
   *Verify: adapter conformance tests against the mock; one live smoke call
   with imported Codex CLI credentials.*
   **Done — PR #8, merged 2026-08-02.**
3. **Read-only tools.** Tool trait, registry, read with hash tags, grep,
   glob. *Verify: unit tests against fixtures.*
   **Done — PR #10, merged 2026-08-02.** Step 4 revised the read output
   to the snapshot-tag format; see the resolved decision below.
4. **Write tools and policy.** Hash-anchored edit, bash sandbox, approval
   modes. *Verify: stale-anchor tests, timeout test, environment test.*
   **Done — PR #11, merged 2026-08-02.** Also landed: the `create` tool,
   write confinement to the project root, and the tool-type move into
   `demerzel-core` that gives the step-5 loop its seam.
5. **Loop and sessions.** Turn engine, JSONL store, resume, one-shot mode.
   *Verify: hermetic loop tests, prefix-stability test, resume test.*
   **Done — PR #14.** Also landed: the provider-contract move into
   `demerzel-core`, the `ToolHost` seam, and a live one-shot dogfood
   run against the Codex endpoint.
6. **REPL and CLI.** Flags, credential resolution, streaming render,
   approval prompts, ready marker. *Verify: a real dogfood session in this
   repo against the Codex endpoint; Ctrl-C behavior.*
   **Done — PR #15.** Also landed: the `Runtime` wiring seam, the
   clap flag surface with `--model` and `--base-url`, and the
   line-editor decision below.
7. **Bench pass.** PTY probe, first hyperfine and PSS numbers, criterion
   turn benchmark. *Verify: raw JSON and machine spec committed to
   `bench/results/`.*
   **Done — PR #16.** Also landed: cold and warm probe modes, the
   idle-session memory driver, the mock server's looping mode, and
   the machine-spec recorder.

## Decisions to make during M1

- Refresh strategy for imported tokens — resolved in step 2. When the Codex
  CLI file holds a newer `last_refresh`, dmz re-imports it instead of
  spending its own refresh token. Otherwise dmz runs the OAuth refresh and
  saves to its own store only. This keeps the two programs from invalidating
  each other while the Codex CLI stays in daily use.
- Edit addressing — resolved in step 4 from the Hashline source. omp
  pairs one snapshot tag for the whole file with plain line numbers; it
  does not tag single lines. Demerzel follows that scheme. The tag is
  the FNV-1a 64-bit hash of the file content, folded to 24 bits, six
  hex characters, inline code, no new dependency. Line numbers are
  unique, so no edit target is ambiguous. One stale-tag error covers
  every drift case, and the edit response re-grounds the model with the
  new tag and the renumbered lines. This supersedes the step-3 per-line
  tags, which could not address a duplicated line without a second
  round trip.
- rustyline vs reedline — resolved in step 6 by measurement. Cost over
  the 6.1 MB step-5 binary: reedline 0.49 adds 506 KB, rustyline 18
  with default features adds 352 KB, rustyline 18 without default
  features adds 308 KB. Demerzel uses rustyline without default
  features and in-memory history. File-backed history can come with
  layered config (M2).
- tokio runtime flavor — resolved in step 7. The harness stays on
  `current_thread`. The Layer-2 numbers put the harness-added cost per
  turn far below one millisecond, and nothing contends for the loop
  thread. Revisit only if a future bench shows a need.
- Sequential vs parallel tool execution — kept sequential. The Layer-2
  numbers put the harness's per-call overhead far below real tool
  runtimes, so parallel dispatch buys nothing at this layer. Revisit
  when Layer-3 workloads show tool-bound turns.

## Risks

- **Unstable backend contract.** The Codex endpoint is not a public API.
  OpenAI can change it without notice. Mitigation: pin to the Codex CLI
  source as the reference, keep recorded fixtures, ignore unknown events and
  fields.
- **Shared refresh token.** After import, dmz and the Codex CLI hold the
  same refresh token. Rotation can invalidate one side. Mitigation: the
  step-2 decision above; test against the real flow early.
- **Binary-size creep.** clap and reqwest are the heavy dependencies.
  Mitigation: `cargo-bloat` check in every PR.
- **Duplicate-line anchors.** An ambiguous anchor can edit the wrong line.
  Mitigation: the constraint in the edit design; property tests in step 4.
- **Abort cleanup.** A cancelled stream must not corrupt the session file.
  Mitigation: append-only writes are atomic per record; the abort test
  covers it.
