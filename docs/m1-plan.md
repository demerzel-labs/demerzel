# M1 Implementation Plan — Dogfoodable Core

This plan turns the M1 milestone of [`plan.md`](plan.md) into ordered,
verifiable steps. Status: draft, not reviewed.

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
and refresh, core tools (read, hash-anchored edit, bash, grep, glob),
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
  demerzel/            # bin: CLI, REPL, wiring
  demerzel-core/       # domain types, loop, sessions, approval policy
  demerzel-providers/  # provider trait + openai-codex adapter
  demerzel-tools/      # read, edit, bash, grep, glob
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

### Provider trait (demerzel-providers)

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
- **bash** — real subprocess. Constructed environment (PATH, HOME, TERM,
  LANG; no ambient credentials). Per-call timeout. Captured stdout and
  stderr with truncation markers. Exit code in the result.
- **grep** — in-process, `grep-searcher`/`grep-regex` crates. No fork-exec.
- **glob** — in-process, `globset` + `ignore` crates. Respects `.gitignore`.

### Approval policy (demerzel-core)

Three modes: `read-only`, `ask` (interactive default), `auto`. The policy
gates edit and bash before dispatch. Policy lives in the harness, never in
the prompt. Approval decisions go into the session log as non-prompt events,
so the record stays auditable and the prefix stays stable.

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
line per tool call, y/n approval prompts. It prints a ready marker on start;
the PTY boot probe waits for that marker. No ratatui in M1.

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
| ulid | session ids |
| etcetera or dirs | XDG paths |
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
2. **Mock server, openai-codex adapter, credentials.** Responses-API SSE
   streaming, tool-call assembly, retries, auth import and refresh.
   *Verify: adapter conformance tests against the mock; one live smoke call
   with imported Codex CLI credentials.*
3. **Read-only tools.** Tool trait, registry, read with hash tags, grep,
   glob. *Verify: unit tests against fixtures.*
4. **Write tools and policy.** Hash-anchored edit, bash sandbox, approval
   modes. *Verify: stale-anchor tests, timeout test, environment test.*
5. **Loop and sessions.** Turn engine, JSONL store, resume, one-shot mode.
   *Verify: hermetic loop tests, prefix-stability test, resume test.*
6. **REPL and CLI.** Flags, credential resolution, streaming render,
   approval prompts, ready marker. *Verify: a real dogfood session in this
   repo against the Codex endpoint; Ctrl-C behavior.*
7. **Bench pass.** PTY probe, first hyperfine and PSS numbers, criterion
   turn benchmark. *Verify: raw JSON and machine spec committed to
   `bench/results/`.*

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
- rustyline vs reedline. Decide in step 6 by binary-size cost.
- tokio runtime flavor. Start with `current_thread`; move to multi-thread
  only if a bench shows a need.
- Sequential vs parallel tool execution. Start sequential; revisit with
  Layer-2 numbers.

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
