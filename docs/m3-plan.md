# M3 Implementation Plan — Ecosystem

Pull request numbers recorded before the 2026-09-04 migration refer to the
[original private archive](history.md#historical-pull-requests).

This plan turns the M3 milestone of [`plan.md`](plan.md#m3--ecosystem) into
ordered, verifiable steps. It follows the shape of
[`m2.5-plan.md`](m2.5-plan.md). Each step is one pull request.

## Next-session handoff

**Checked 2026-09-06:** M0 through M2.5 are complete, with both close-outs.
This plan is step 1 of M3. No M3 code exists yet.

Step 2 landed on `main` on 2026-09-22. Its dogfood receipt landed on
2026-09-26; see the step's build notes. Start the next work in this
order:

1. Build the general Responses adapter as step 3. Add its row to
   `adapter_rules`.
2. Follow the remaining steps in order. Each step names its dependencies.
   Until Daniel restores pull requests, commit each step to `main` under
   the [`AGENTS.md`](../AGENTS.md) rules for direct commits.

Steps 8a to 8c add [judgments](#judgments), the milestone's
differentiator. Read that design before step 5, because `EngineParts`
and `ProviderHost` carry the judge.

Read [`config.md`](config.md) before you change config behavior. Read
the [trust rules](#trust-rules-for-extension-content) before you add a
key or a file that a project can supply.

The [tracked items](#tracked-separately) are not prerequisites for any
step. Do not fold them into a step without a decision in this document.

## Status (2026-09-06)

Step 1 merged in [PR #18](https://github.com/demerzel-labs/demerzel/pull/18).
Step 3 adds general Responses support after the Chat Completions adapter.
The 2026-09-22 revision adds steps 8a to 8c for judgments.
The M2.5 close-out
([`m2.5-closeout.md`](m2.5-closeout.md)) recorded the receipts on both
reference machines and the decisions that carry into M3:

- Five provider capability flags have no consumer. Each flag without a
  consumer at the M3 close-out is removed.
- Layout-cache retention and the row-count browse anchor stay through M3.
- Mac system-cold timing stays deferred.

## Goal

After M3, Demerzel supports Chat Completions and Responses through
configured provider instances. New model ids need config changes only.
Adapters contain protocol changes and vendor differences. A model role
names a chain of provider instances, and a session records
which one answered. A fast judge answers typed questions about the loop.
Code turns its probabilities into records, warnings, notes, approvals, and
denials. Project files extend the agent: instructions, skills,
custom commands, hooks, MCP servers, and subagent definitions. Demerzel
finds the config that other agents left in a repository and uses it with
zero edits. The agent remembers facts per project and proposes changes
to a personality profile that only the user edits. A background command
can outlive its tool call. At the exit, the evaluation runner has its
foundation: a Harbor adapter, versioned manifests, evidence bundles, and
a hermetic smoke subset that guards every step from step 8a on.

## Exit criteria

M3 is done when all of these hold:

1. Both compatible adapters complete live tool turns and pass hermetic
   tests against fixtures from independent protocol implementations.
   Step 2 tests Chat Completions against a local server. Step 3 tests
   Responses against a hosted server, including session resume.
   Each receipt names the server, model, protocol, and tested revision.
2. A role chain falls back to its second target in a hermetic test and
   in one dogfood session. The session file names the target that
   answered each call.
3. One real session in this repository exercises a skill, a custom
   command, a hook, and an MCP server.
4. A fixture repository with `.claude/` and `.cursor/` config yields its
   skills, commands, and rules with zero edits, in a test and in a
   dogfood session on a real repository.
5. A fact saved in one session comes back in a fresh session inside the
   recall budget, in a test and in a dogfood session.
6. A subagent returns a schema-valid result in a dogfood session. A
   test covers the validation retry loop and every limit.
7. The profile workflow applies a proposal only through `/profile accept`.
   Tests cover rejection, acceptance, and the 8 KiB profile limit.
8. A background command started in one tool call answers a later call
   and dies at session exit, in a test.
9. The smoke subset runs hermetically in CI. The Harbor adapter runs one
   task end to end. The bank index lists 10 to 20 tasks, and the
   close-out records the count and each task's source step. Manifests
   and bundles validate against their schemas.
10. Each capability flag without a consumer is removed. The alignment
    review is folded into the plan docs. The metrics receipts from both
    reference machines are committed. Warm boot and idle memory stay
    within 10% of each machine's M2.5 receipts, measured with no hooks,
    no MCP servers, no judge instance, an empty memory store, and the
    default discovery sources. A crossing needs one confirmation run and
    a documented
    cause, as M2.5 did. The stripped binary stays inside the 15 MB
    working budget, with a `cargo-bloat` receipt for each dependency
    that grows it more than 10%. `cargo fmt`, `cargo clippy`,
    `cargo test`, and `cargo doc` are clean.
11. A judge instance records every built-in check in dogfood sessions.
    Approval judgments pass the step 8b gate. A rule denies a call in a
    test and in a dogfood session. The calibration report for the
    compatible backend is in this plan.

## Scope

In:

- the `openai-compat` and `openai-responses` adapters, `kind`, and
  named provider instances;
- model roles with fallback chains, session model identity, and one
  runtime call that opens a session with its route;
- judgments: the judge contract, the `typesafe` backend, a backend over
  a compatible instance, the built-in checks, approval judgments, and
  rules in plain language;
- prompt composition with fixed segment positions;
- project and user instruction files, skills, custom slash commands,
  and discovery of Claude Code, Cursor, and Codex CLI config;
- background commands with output and stop tools;
- the hermetic smoke runner, early, as the regression gate;
- project memory in SQLite with save, search, forget, and bounded recall;
- the personality profile with an explicit acceptance flow;
- subprocess hooks with JSON contracts and a trust gate for project
  definitions;
- an MCP client over stdio with deferred tool discovery;
- in-process subagents with schema-validated results;
- the evaluation foundation named in the exit criteria;
- the close-out: flag removal, alignment review, receipts.

Out, with the milestone that owns each item:

- compaction and its distillation into memory (M4);
- the public benchmark baseline and campaigns (M4);
- bounded layout retention and bulk card expansion (M4);
- managed local inference (post-M5);
- in-process scripting, with Lua as the named candidate (deferred until
  a per-keystroke extension point exists);
- MCP over Streamable HTTP, MCP resources, prompts, and sampling (by
  demand, after the stdio client proves the seam);
- parallel tool execution and parallel subagents (revisit with the
  Layer-2 numbers, as [`plan.md`](plan.md#tools-in-process-where-it-counts)
  says);
- import of another product's OAuth tokens: a client id binds those
  tokens. API keys and the Codex CLI file stay the import set, and the
  first-run screen is unchanged. This closes the M2.5 deferral of
  cross-harness credential import;
- discovery of Pi and oh-my-pi layouts, which [`plan.md`](plan.md#ecosystem-and-customizability)
  names. The shared skills layout under `.agents/skills/` covers the
  case they share. A request that names a current layout adds a row to
  the discovery table (by demand);
- a worktree per subagent (a follow-up after sequential subagents
  prove the contract);
- a per-server approval grant for MCP tools (a dogfood decision, see
  the MCP design);
- local embeddings for memory (only if the Layer-4 recall measurement
  shows lexical recall failing);
- vendor-hosted tools and server-managed conversation state (by demand,
  when a concrete use case justifies a separate contract change);
- routing each turn to a role by judgment (M4, after a paired result
  shows no loss);
- judgment ranking for skills, tools, and memory recall, and judgment
  grading in the evaluation runner (by evidence, after the M3 checks
  prove the contract);
- judges over `anthropic`, `openai-codex`, and `openai-responses`
  instances. Their protocols return no log probabilities;
- user edits to the built-in check questions (by demand).

## Workspace after M3

One new crate and one new Python directory:

```text
crates/
  demerzel/            # bin: CLI, wiring, prompt composition, discovery,
                       #      hooks, judgment checks and rules,
                       #      subagents, background tasks
  demerzel-core/       # + lexical (BM25), Route, ProviderHost, Judge,
                       #   LoopHost, ToolContext, EngineParts
  demerzel-providers/  # + openai_compat, openai_responses, instance config,
                       #   the typesafe and logprob judges
  demerzel-tools/      # + task tools, the skill tool, the mcp module
  demerzel-tui/        # + agent cards, the /profile and /tasks notes
  demerzel-memory/     # SQLite fact store, memory tools, recall (new)
  demerzel-mock/       # + Chat Completions, Responses, and judge routes,
                       #   MCP echo server, standalone smoke-runner binary
bench/
  eval/                # manifests, bundles, oracle, Harbor adapter,
                       #   smoke runner, public hermetic tasks (new)
```

`demerzel-memory` is an exception to the second-consumer rule in
[`plan.md`](plan.md#architecture), recorded here: it isolates
`rusqlite`, which bundles a C library. The providers crate takes the
tools crate as a development dependency, so a store inside the tools
crate would compile SQLite into every provider test. The MCP client is
a module of `demerzel-tools`: the bridge implements `Tool`, the crate
already spawns subprocesses for `bash`, and the module adds no
dependency, so the same development dependency costs the provider
tests nothing new. It becomes a crate when Streamable HTTP or a second
consumer arrives. The lexical index lives in `demerzel-core` because two crates
consume it: the tools crate for discovery and the memory crate for
facts. The binary crate keeps subagents, hooks, judgment checks, and
discovery because each one wires providers, tools, config, and sessions
together.

## Design

### Named provider instances

A `[providers.<name>]` table names one instance. `kind` names the
adapter: `openai-codex`, `anthropic`, `openai-compat`, or
`openai-responses`. A table without `kind` keeps today's meaning:
the name is the adapter id. The two shipped
adapters stay implicit instances, so an existing config file resolves as
before.

```toml
[providers.xai]
kind = "openai-compat"
base_url = "https://api.x.ai/v1"
model = "grok-5"
api_key_env = "XAI_API_KEY"
context_window = 256000

[providers.local]
kind = "openai-compat"
base_url = "http://127.0.0.1:11434/v1"
model = "qwen3-coder"
```

Rules:

- Both compatible adapters require `base_url` and `model`. The run
  stops at boot with a clear error when either is missing. The adapter
  has no default endpoint and no model catalog. A model id is a string
  the config names and the server accepts or refuses.
- `context_window` is the only window source for a compatible instance. A
  missing key shows the count with no share. The lookup keys on the
  instance's key first and on the shipped catalog by `kind` and model
  second.
- Credentials resolve in this order:
  1. The variable that `api_key_env` names. A named variable that is
     unset or empty stops the turn and names the variable; it does not
     fall through.
  2. The auth store section for the instance (step 4).
  3. None. An instance without a key sends no `Authorization` header.
     A local server needs none.
- An adapter with credentials of its own, a login or a key, serves its
  implicit instance and each named instance at its default endpoint. A
  named instance with a `base_url` of its own names its key in
  `api_key_env`, or the run stops at boot. The adapter's key never
  reaches an endpoint that the user did not name it for. Decided at
  step 2 (2026-09-22), after the review found the leak.
- Credentials resolve at the first turn, not at boot, as today. The
  provider host holds one lazy slot per configured instance; a failed
  build is not kept, so a login that lands between two prompts takes
  effect at the next one.
- `--base-url` and `DEMERZEL_BASE_URL` apply to the head target's
  instance for the run, as they follow a provider switch today.
- Trust: only flags, the environment, and the user file set `kind`,
  `base_url`, and `api_key_env`. A project file can name an instance
  the user file defines and can set `model`, `effort`, `context_window`,
  and `thinking` under the existing rules. It cannot define an instance
  or pick a credential.
- Step 4 adds `dmz login <instance>` for both compatible adapters.
  It prompts for a key with echo off and checks it
  with `GET {base_url}/models`. A 401 refuses the key. Any other
  failure warns and stores the key, because many compatible servers do
  not serve that route. The auth store gains a `keys` section, file
  version 3. Older builds keep unknown sections through a save, so an
  M2.5 binary reads the file. The benchmark auth fixture stays valid
  under version 3, so the first-run screen does not block the boot
  probe.
- The `thinking` key, `anthropic` kind only: `adaptive`, the default, or
  `off`. `adaptive` sends adaptive thinking and the resolved effort.
  `off` sends `thinking: {"type": "disabled"}` and omits
  `output_config.effort`, including after `/effort`.
  The command keeps the session's effort for other targets and explains
  that this instance omits it. `doctor` shows the effective request shape.
  This covers models without adaptive thinking or effort support;
  models that require thinking can still refuse `off`.
  The [thinking](https://platform.claude.com/docs/en/build-with-claude/thinking)
  and [effort](https://platform.claude.com/docs/en/build-with-claude/effort#compatibility)
  contracts define these fields. No per-model capability flag is needed.
- `doctor` lists each instance with its kind, endpoint, model, and
  credential state. Its secret scanner allows `api_key_env`, because the
  value is a variable name.

### Chat Completions adapter

The wire contract is the Chat Completions streaming shape. The adapter
sends `model`, `messages`, `stream: true`, `stream_options` with
`include_usage`, and `tools` when the request has any. It sends
`max_tokens` only when the request sets `max_output_tokens`. It sends
`reasoning_effort` only when config, a flag, the environment, or
`/effort` sets the level. The default level goes to the shipped
adapters, except an `anthropic` instance with thinking off.
A compatible endpoint gets no implicit effort, because many servers
refuse fields they do not know. The config resolver already
tracks the origin of each setting, so this rule needs no new key.

Stream mapping:

- `choices[].delta.content` maps to `TextDelta`.
- `delta.reasoning_content` and `delta.reasoning` map to
  `ReasoningDelta`. The segment closes with a `ReasoningDone` that
  carries readable text and a `SealedPayload`. Its data preserves each
  original field name and its complete text. The payload records the
  adapter, instance, and model; `sealed` means opaque state, not encryption.
- `delta.tool_calls[]` accumulates by `index`. `ToolCallStart` waits
  for the call's first argument text, because a local server can
  re-send a grown `id` and `function.name` before its arguments. Each
  repeat before the start overwrites the last. Each argument fragment
  emits `ToolCallDelta`. The terminal chunk emits one `ToolCallEnd` per
  accumulated call, whatever `finish_reason` says.
- `finish_reason` maps `tool_calls` to `ToolUse`, `length` to
  `MaxTokens`, and `content_filter` to `Refusal`. `stop` maps to
  `ToolUse` when the reply sent a call and to `EndTurn` otherwise. A
  gateway can repeat the finish chunk: the first reason holds, and the
  call flag lasts for the whole reply. `error`, `insufficient_system_resource`, `abort`, and
  `aborted` fail the call. `delta.refusal` maps to `TextDelta` and ends
  the turn with `Refusal`.
- Usage arrives in a chunk with empty `choices`, or on the last chunk
  with a choice. The adapter maps `prompt_tokens`,
  `completion_tokens`, and `prompt_tokens_details.cached_tokens`, with
  `prompt_cache_hit_tokens` as the cache fallback.
  `data: [DONE]` ends the stream. A stream that ends without `[DONE]`
  is a protocol error, as with the two shipped adapters. An `error`
  payload fails the call.
- Tool results go out as `role: tool` messages, one per result, with
  `tool_call_id`. Matching reasoning payloads restore the original fields
  on their assistant messages, including after resume. They never come
  from a shortened display summary. The
  [DeepSeek contract](https://api-docs.deepseek.com/guides/thinking_mode/)
  requires this replay on subsequent requests with tools.

Step 2 adds optional `model` and `instance` fields to `SealedPayload`.
The compat adapter records both. Replay requires a matching adapter and
matching values for every identity field present in the payload.
Step 5 applies this rule to the shipped adapters and model switching.

Capabilities: `tool_calls` true and the other fields at their defaults.
The close-out decision below removes the fields without a consumer.
Retries mirror the shipped adapters: backoff on 429, 5xx, and network
errors, fail fast on other 4xx, no refresh on 401.

Session identity: `meta.provider` becomes the instance name. The two
shipped adapters keep their spelling, because instance name and adapter
id are equal there. `meta` gains `kind`, optional: the adapter id
behind the instance. A file without it reads as before. This is a
redefinition of an existing field and an added field; step 2 declares
both. `SessionMeta::now` and `SessionStore::create` take one
`SessionOpen` struct in place of their five parameters, so steps 17 and
18 add fields without touching a signature. The mock server gains
`POST /v1/chat/completions` and fixture builders next to the Codex and
Anthropic ones.

### General Responses API adapter

Step 3 adds `kind = "openai-responses"`. It sends requests to
`{base_url}/responses` with the configured model and optional bearer key.
The instance, credential, context-window, and trust rules above apply.
Codex OAuth, account headers, and subscription defaults stay in
`openai-codex`. A protocol change must not require vendor branches in
the engine, tools, memory, or interface.

The request sends `model`, `instructions`, `input`, `stream: true`, and
`store: false`. It includes the full local conversation on each turn.
It does not depend on `previous_response_id` or server conversation state.
Tool definitions use the Responses function shape with `strict: false`.
Tool results use `function_call_output` and preserve `call_id`.
`max_output_tokens` appears only when the request sets it.
`reasoning.effort` appears only when the user sets effort.
The adapter adds no implicit effort or summary setting. Optional vendor
fields require an observed need and an adapter or instance contract.

Text, tool calls, reasoning, and usage map to existing neutral events.
Both reasoning text and reasoning-summary deltas can supply display text.
The adapter requests `reasoning.encrypted_content` and preserves complete
reasoning output items in `SealedPayload`. Replay checks adapter,
instance, and model identity, and keeps each item's original position.
Display text never replaces opaque state.
Local text and tool history remain usable across provider switches;
foreign reasoning payloads stay off the wire.

`response.completed` ends the response. Refusal content maps to display
text and `StopReason::Refusal`, even when the response completes normally.
The tests cover `response.refusal.delta` and `response.refusal.done`.
`response.incomplete`
maps known reasons to the matching stop reason, including output limits
and refusal. Unknown reasons fail the call. `response.failed`, a top-level
`error`, or a missing terminal event fails the call.
The adapter never treats a transport close as successful completion.
It never executes incomplete tool calls. Wire item ids and `call_id`
remain distinct during assembly and dispatch.
Unknown optional fields and events do not break a valid tool turn.
Required fields and terminal failures still receive explicit checks.
Retries follow the existing error classes and must not replay emitted events.

Protocol code is shared with `openai-codex` only where fixtures prove
the behavior matches. Codex regression tests remain in the gate.
The new adapter uses the existing HTTP and streaming dependencies.

The wire references are the [Responses streaming events](https://developers.openai.com/api/reference/resources/responses/streaming-events),
the [function-calling contract](https://developers.openai.com/api/docs/guides/function-calling),
and the [reasoning contract](https://developers.openai.com/api/docs/guides/reasoning).
The [xAI Responses guide](https://docs.x.ai/developers/model-capabilities/text/generate-text)
and [reasoning guide](https://docs.x.ai/developers/model-capabilities/text/reasoning)
define the second implementation's fixtures.

### Provider compatibility checks

Steps 2 and 3 use sanitized fixtures from independent implementations.
Chat Completions covers a hosted provider and a local server. Responses
covers the public OpenAI and xAI contracts. Live checks cover a local
Chat Completions tool turn and a hosted Responses tool turn with resume.
CI replays the fixtures without credentials or network calls.

Each fixture records its source protocol and capture or documentation
revision. New model ids, extra optional fields, fragmented tool calls,
and terminal failures receive tests. A provider regression adds a fixture
before the adapter fix. Compatibility receipts describe the tested subset;
they do not claim every vendor feature works.

Steps 5 and 6 test provider switches and fallback across protocols.
Tools, approvals, memory access, and local history keep their harness
contracts. Opaque reasoning replay follows the identity rules above.
The close-out checks these boundaries and the current upstream contracts.

### Model routing: roles, chains, and session identity

A role names a chain of targets. Each target is an instance name and an
optional model. The engine tries the chain in order at each model call.

```toml
[roles.default]
chain = [
  { provider = "anthropic", model = "claude-sonnet-5" },
  { provider = "xai" },
]

[roles.fast]
chain = [{ provider = "local" }]
```

Rules:

- `default` is the role of the main loop. Subagents name other roles.
  A role that names an unknown instance stops the run at boot.
- A target carries no effort. The session effort applies to every
  target, under the existing precedence and trust rules. A user who
  wants a low-effort variant defines a second instance.
- Trust: a project file can define a role only over instances the user
  file or the shipped defaults define. It cannot raise cost through a
  role, because a target carries no effort and no limit.
- The top-level `provider` and `model` keys define the head target of
  `default` when no `[roles.default]` table exists. A `--provider` or
  `--model` flag, or its variable, replaces the head target and keeps
  the tail. `/model <model>` sets the head target's model.
  `/model <provider> <model>` sets the head target's instance and
  model. Two tokens, because model ids can hold `/` and `:`. `/model`
  alone shows the chain.
- The chain advances only on a failure before the first stream event
  reaches the sink: an authentication, network, server, rate-limit, or
  invalid-request error after the adapter's own retries. A failure
  after a delta reached the screen surfaces as today. Falling back then
  would repeat visible text.
- The engine emits `ModelStart { provider, model }` once per attempt,
  before the call, so the progress line and the status name the target
  in use. The `usage` record gains `provider` and `model`, optional, so
  the file names the target that answered each call. The `meta` record
  keeps the head target.
- The engine reads capabilities from the target it is about to call,
  not from a snapshot at construction. `set_model`, `set_effort`, and
  `set_approval` write their `meta` record from the route's head
  target: instance name, kind, and model.
- Sealed reasoning is bound to its adapter, instance, and model.
  Step 5 records the step-2 identity fields on the shipped adapters.
  Replay checks every recorded identity field, as the compat design states.
  `set_model` stops dropping payloads from the history: the wire filter
  makes the drop unnecessary, and a switch back restores continuity. A
  file from before the field replays as before, so the M2 gap closes
  for files written from step 5 on.
- Resume adopts the head target from the session's last `meta` record
  when no flag or variable names a provider or model. The tail comes
  from config. A config file key does not override the session's head:
  a session keeps its model the way it keeps its history. Effort and
  approval still resume from config, as [`config.md`](config.md) says,
  because neither binds to the history. When the recorded instance no
  longer exists in config, the run falls back to config with a notice.
  `RunConfig` carries the origin of `provider` and `model`, so the
  runtime can tell a flag from a config default.

One call opens a session with its route. `Runtime::open(resume,
settings, first_prompt)` replaces `open_session` and `engine`. It
resolves the route, composes the system prompt, opens or resumes the
file, and returns the engine with the replay for display. The M2 note
asked for one routing struct instead of two widened signatures; this is
that fold. `SessionSettings` carries a `Route` in place of `model`.
`Engine::new` takes one `EngineParts` struct: the provider host, the
tool host, the policy, the config, the session, and the history. Later
seams add fields, not parameters. `ProviderHost` maps an instance name
to a provider through one lazy slot per instance, so a reference lives
as long as the host and a failed build still retries.

### Judgments

Decided 2026-09-22, as the milestone's differentiator. The product
reasoning is in [`plan.md`](plan.md#judgments-a-fast-model-beside-the-main-model).
A judge is a fast model that answers typed questions about the loop's
state with probabilities. It writes no text and calls no tools. Code
applies the thresholds and picks the action.

#### The judge contract

`demerzel-core` gains a `Judge` trait beside `Provider`: `id` and
`ask`. A request holds one state value and a map of questions by id. A
reply holds one answer for each id, the model id that answered, and
usage. The question types:

| Type | Question input | Answer |
|---|---|---|
| `YesNo` | instructions, optional criteria for yes and no | the probability of yes |
| `Choice` | instructions, 2 to 255 named options | the chosen option and a probability for each option |
| `Score` | instructions, 2 to 10 ordered levels | a probability for each level and their weighted position |

`Judge` is a second model contract, not a `Provider` variant. A judge
does not stream, does not call tools, and returns no text. Each loop
event sends at most one request: every question at that event goes into
it. Question ids stay in code. The question text carries the full
meaning.

`ProviderHost` builds judges in its lazy instance slots. A `typesafe`
instance serves only as a judge. An `openai-compat` instance can serve
as a provider and as a judge. A role that names a `typesafe` instance
stops the run at boot. A judge setting that names an instance of another
kind also stops the run at boot.

#### Backends

- **`kind = "typesafe"`, step 8a.** The reference backend. The wire
  contract is the published [System One API](https://docs.typesafe.ai/api):
  `POST {base_url}/v1/systemone` with a bearer key, `model`, `state`,
  and `questions`. `YesNo` maps to the wire type `noul`. The default
  `base_url` is `https://api.typesafe.ai`. The default model is the
  alias `jev-latest`. An alias can move to a new version, so a tuned
  config pins a version id. The reply's `model` field names the version
  that answered. `doctor` notes an alias when a check acts above `log`.
  Retries back off on 429, 529, 5xx, and network errors, and honor
  `retry-after`. 401 and 422 fail fast. Every retry stays inside the
  request timeout. `dmz login <instance>` checks a key with
  `GET {base_url}/v1/models`, as step 4 does for the compatible
  adapters. The step checks these fields against the API reference at
  build time and records any drift in this plan.
- **Log probabilities over an `openai-compat` instance, step 8c.** Each
  question becomes one Chat Completions request with `max_tokens: 1`,
  `logprobs: true`, and `top_logprobs: 20`. Options and levels carry
  letter labels. The answer is the probability mass on each label,
  normalized over the labels. A reply with less than half of its mass
  on the labels fails that question. Requests for one event run with at
  most 8 in flight. A server that returns no log probabilities gets a
  `doctor` notice. The framing text is not contract. This backend makes
  no calibration claim. Step 8c measures it against the reference
  backend and reports the result.
- **The mock.** `demerzel-mock` gains the System One route with answers
  scripted by question id.

The `anthropic`, `openai-codex`, and `openai-responses` kinds cannot
serve as judges: their protocols return no log probabilities.

#### Configuration

```toml
[providers.judge]
kind = "typesafe"
model = "jev-1.13.0"
api_key_env = "TYPESAFE_API_KEY"

[judgments]
instance = "judge"
checks = { outcome = "warn", progress = "warn", claim = "log", approval = "act" }
max_calls = 500

[[judgments.rules]]
event = "tool_before"
tool = "bash"
ask = "Does this command change files under src/auth/?"
threshold = 0.8
action = "deny"
reason = "src/auth/ is off limits in this repository."
```

- `judgments.instance` names the judge. Without it, the harness sends
  no judgment request and writes no record.
- `judgments.checks` sets one action for each built-in check. Each check
  defaults to `log` when an instance exists.
- `judgments.max_calls` caps requests for one session run. The default
  is 500. At the cap, checks stop with one notice, and approval falls
  back to the policy.
- `judgments.rules` holds rules in plain language. A rule asks one
  `YesNo` question at one event. `tool` limits a rule to one tool name.
  The rule fires when the probability of yes reaches `threshold`, 0.8 by
  default. `reason` defaults to the question.
- The request timeout is 2 seconds, and the thresholds of the built-in
  checks are constants. Each becomes a key only when records show that
  users need another value.

#### Actions

| Action | Effect | Valid for |
|---|---|---|
| `off` | The check does not run. | built-in checks |
| `log` | The `judgment` record keeps the answers. Nothing else changes. | all |
| `warn` | As `log`. A `Judgment` event shows the finding on the tool card or the turn. | all |
| `note` | As `warn`. The finding joins the tool result that the model reads next. | `tool_after` checks and rules |
| `act` | The approval check changes the approval question, as described below. | `approval` |
| `deny` | The call does not run. The reason becomes its error result. | `tool_before` rules |

A note joins the stored tool result, so a resume sends the same bytes.
A deny writes an `approval` record with `decision` `deny` and `answer`
`judge`, in the shape that hooks use.

#### The built-in checks

Code filters each event before it builds a request:

| Check | Event | Question | Runs when |
|---|---|---|---|
| `outcome` | `tool_after` | Did the call fail, or return something other than its purpose, although it reports success? | an `Execute` call returns without its error flag |
| `progress` | `tool_after` | Does the call repeat an earlier failed action without a material change? | the turn holds an earlier failed call of the same tool |
| `claim` | `turn_end` | Does the final message claim a result that the turn's tool evidence does not show? | a turn with at least one tool call completes |
| `approval` | `tool_before` | Two `YesNo` questions: is the call destructive or hard to undo; is it inside the user's request? | a `Write` or `Execute` call reaches the approval policy |

The fire threshold for `outcome`, `progress`, and `claim` starts at 0.8.
The question wording is not contract. Tests check actions and
thresholds, not wording.

#### Approval judgments

The approval check can move a call one step. It never replaces the
policy:

1. In `read-only` mode, nothing changes. The policy refuses writes before
   any judgment.
2. In `ask` mode, the check runs only for a call the policy would ask
   about. At `act`, the judge can remove the question. It removes it
   when the probability of destructive is at most 0.05 and the
   probability of in scope is at least 0.9. The `approval` record then has `decision`
   `allow` and `answer` `judge`. Other calls show the question as today,
   with the judge's concern on the card.
3. In `auto` mode, at `act`, a call with a probability of destructive
   of at least 0.5 becomes a question. A run without an approval prompt
   refuses the call and gives the reason.
4. A judge approval covers one call. It creates no session grant.
5. A timeout, an error, or the call cap leaves the policy's decision
   unchanged.

At `log`, the check runs in shadow. The policy and the user decide. The
records pair each judge answer with the user's answer, and step 8b uses
these pairs as its labeled set. The starting thresholds above change
only with that evidence.

#### The loop seam

`demerzel-core` gains one `LoopHost` seam on `EngineParts`. The engine
calls it at `turn_start`, `tool_before`, `tool_after`, and `turn_end`.
Judgments implement it in step 8a. Hooks implement it in step 15, in
place of a separate hook seam. At `tool_before`, the order is:

1. hooks;
2. rules;
3. the approval policy with the approval check.

A deny at any stage ends the call. At `tool_before`, the host gets the
policy's decision and can return a changed one. The engine allows these
changes only: ask to allow, allow to ask, and any decision to deny. A
host cannot turn a refusal into a run.

Checks and rules also run on subagent calls. Their records go to the
child session file.

#### The state

Each check builds its state in code from the session:

- the user prompt of the current turn, capped at 4 KiB;
- the tool name and its input, capped at 4 KiB;
- the tail of the tool output, capped at 8 KiB;
- the names and outcomes of the turn's last 10 calls;
- for `claim`, the final message, capped at 4 KiB.

A request holds at most 16 KiB of state. The builder cuts the output
tail first. The state never holds the system prompt, the tool
definitions, or earlier turns. Judgments never enter the prompt: the
cached prefix stays byte-stable.

#### Latency and cost

- Checks at `log` and `warn` at `tool_after` run at the same time as the
  next model call. They add no turn latency.
- A `note` check holds the next model call until its answer or the
  timeout.
- Rules and the approval check hold the dispatch until the answer or the
  timeout.
- Code filters skip a request when no check applies. A turn without tool
  calls sends at most one request, for `claim`.
- The judge builds at the first check. Boot and idle memory stay
  unchanged.

#### Records

The session file gains a `judgment` record for each request. It holds:

- the event and the call id, when a tool call exists;
- the checks and the rules, with each rule's source file;
- the instance and the model id from the reply;
- the answers by question id, and the action taken;
- the state's byte count and SHA-256;
- the start time, the duration, and the usage.

A replay can apply new thresholds to the recorded answers without a new
call. The state hash shows when the state builder changed.

#### Trust and egress

A judge receives tool input and output, as the main model does. The
egress rule in [`plan.md`](plan.md#non-goals) names the judge instance.
Only the user file, flags, and the environment can turn judgments on.
`doctor` names the judge instance, each check's action, and each rule
with its source. A project rule sends state to the user's judge and
counts against the user's cap. A project rule cannot approve a call. A
rule runs no code, so the hook trust gate does not apply.

Every check starts at `log`. A default action rises only when a plan
records the check's precision at its threshold on labeled records.
Approval at `act` needs the step 8b gate.

#### Naming

The `typesafe` kind names the service it calls, as the `anthropic` kind
does. The judgment code uses Demerzel's own terms: judge, check, rule,
`YesNo`, `Choice`, and `Score`. Step 8a adds the reference backend to
[`prior-art.md`](prior-art.md).

### Prompt composition

The system prompt is fixed for the life of a session run. The runtime
composes it once, at the first prompt, and the engine sends the same
bytes on every call. Volatile state never enters it. A resumed session
composes again, so its prompt can differ from the one its history ran
under; the file's `prompt_hash` from step 18 records each.

The segments, in this order:

1. The base prompt. The harness default, or the whole content of the
   file `prompt.base_file` names. A replaced base prompt is sent as is.
   The harness does not merge or repair it. Only the user file can set
   the key.
2. The personality profile: `$XDG_CONFIG_HOME/demerzel/personality.md`,
   capped at 8 KiB.
3. Project instructions: the user file
   `$XDG_CONFIG_HOME/demerzel/AGENTS.md`, then the project's `AGENTS.md`,
   or `CLAUDE.md` when no `AGENTS.md` exists, then the discovered
   instruction files. Each file is capped at 32 KiB with a marker, and
   the segment at 64 KiB. Two paths that resolve to one file count once,
   so a `CLAUDE.md` symlink to `AGENTS.md` does not repeat.
4. The skill index: one line per skill, name and description, each line
   capped at 200 bytes, the segment at 8 KiB. When the segment
   overflows, skills drop from the lowest precedence level first, and
   `doctor` names the dropped skills. Bodies load through the `skill`
   tool.
5. Memory recall: the facts the first prompt retrieves, inside
   `memory.recall_tokens`. Last, so a session's recall changes only the
   tail of the prefix. Tools and the segments above stay byte-stable
   across sessions.

Each segment starts with a fixed heading, so a reader of the trace can
see the boundaries. The order and the caps are contract. The heading
words are not: tests check order and caps, not wording. Token budgets
use bytes divided by four.

### Instructions, skills, commands, and discovery

Every extension file is plain text. Frontmatter is YAML between `---`
fences, as the [Agent Skills format](https://agentskills.io/specification)
requires. Parsing covers quoted scalars, folded and literal text, inline
and block lists, and nested metadata. Step 10 selects a maintained parser
with compatibility fixtures and a size receipt. It rejects malformed
frontmatter and duplicate keys with a `doctor` notice; it never guesses
activation flags from an unreadable value. The parser bounds input size
and nesting and disables custom tags and alias expansion.

Native paths, project then user:

| Kind | Project | User |
|---|---|---|
| Instructions | `AGENTS.md`, else `CLAUDE.md` | `$XDG_CONFIG_HOME/demerzel/AGENTS.md` |
| Skills | `.demerzel/skills/<name>/SKILL.md` | `$XDG_CONFIG_HOME/demerzel/skills/<name>/SKILL.md` |
| Commands | `.demerzel/commands/<name>.md` | `$XDG_CONFIG_HOME/demerzel/commands/<name>.md` |
| Agents | `.demerzel/agents/<name>.md` | `$XDG_CONFIG_HOME/demerzel/agents/<name>.md` |

Discovered paths, when `discovery` lists the source:

| Source | Instructions | Skills | Commands | Agents | MCP servers |
|---|---|---|---|---|---|
| `claude` | `CLAUDE.md` | `.claude/skills/`, `~/.claude/skills/` | `.claude/commands/`, `~/.claude/commands/` | `.claude/agents/`, `~/.claude/agents/` | `.mcp.json` |
| `cursor` | `.cursor/rules/*.mdc`, `.cursorrules` | — | — | — | `.cursor/mcp.json` |
| `codex` | `~/.codex/AGENTS.md` | — | — | — | `~/.codex/config.toml` |
| `agents` | — | `.agents/skills/`, `~/.agents/skills/` | — | — | — |

The `agents` source is the shared skills layout that more than one
agent reads. The default is every source. `discovery = []` turns
discovery off. This table is the decision. Step 12 builds it, and
[`config.md`](config.md) becomes the list of record. A path or field
found wrong at build time is corrected in both places, with a note in
this plan; nothing drops out in silence.

Trust: a project file can remove sources from `discovery`. It cannot
add one, so a repository cannot switch a user's `discovery = []` back on
and pull the user's own files into the prompt.

Compatibility limits:

- A Cursor rule joins the instructions only with `alwaysApply: true`.
  Manual, relevance-selected, and glob-scoped rules stay out with a
  `doctor` notice: M3 has no matching activation mechanism.
  An absent glob does not make a rule unconditional. The plain
  `.cursorrules` file remains an unconditional legacy instruction file.
  See the [rule types](https://cursor.com/docs/rules#rule-anatomy).
- A skill without `name` uses its directory name. A missing description
  uses the first body paragraph, capped at 200 bytes.
  `disable-model-invocation: true` removes the skill from the model index
  and blocks the `skill` tool; explicit `/skill` invocation still works.
  `user-invocable: false` disables explicit invocation and completion.
  These restrictions combine when both are set. The
  [invocation controls](https://code.claude.com/docs/en/skills#control-who-invokes-a-skill)
  keep their meaning. Other fields grant no permission; unsupported
  execution settings produce a `doctor` notice.
- A command body is a prompt template. `$ARGUMENTS` expands to the text
  after the command name. A `description` line feeds `/help` and the
  completion popup.
- A discovered agent definition maps `description`, `tools`, and the
  body. A `model` line maps to a role when a role of that name exists,
  and warns otherwise. Step 12 finds and lists agent definitions; step
  17 parses and uses them.
- Hooks in another tool's settings never run. Their contracts differ,
  and a translation would run commands the user did not write for this
  harness.

Precedence: the first match wins, in this order:

1. Built-in names.
2. The project's native files.
3. The project's discovered files.
4. The user's native files.
5. The user's discovered files.

`doctor` lists each shadowed name with both paths. A project name that
shadows a user-native name also shows a one-line notice at the moment of
use, because it redirects the user's own keystroke. A custom command
cannot take a built-in name. `/help` and the popup list custom commands
after the built-in set, with their descriptions.

Invocation: `/<command> [args]` runs a custom command as a user turn
with the expanded body. `/skill <name> [args]` puts the skill body and
the arguments into a user turn. The model loads a skill through the
`skill` tool, input `{ "name": string }`, read-only access. The tool
returns the body and the skill's file list.

Credit and naming: the formats above come from other products. Step 10
adds their rows to [`prior-art.md`](prior-art.md), with each license.
Step 12 adds a Source-rules exception to `AGENTS.md` for the discovery
module and its config surface, on the same wire-contract argument the
Codex adapter uses: the module names the tools whose files it reads,
and the file layouts are the contract.

### Trust rules for extension content

Two classes of project content:

1. Prompt content: instructions, skills, commands, and agent definitions.
   The harness loads them without a gate. They cannot raise privilege,
   because the approval policy lives in the harness and every tool call
   still passes it. The
   [existing trust rules](config.md#trust-rules) for a project file hold.
2. Executable definitions: hooks and MCP servers. A project file, or a
   discovered file, can define them. They run only after the user
   allows them once for that project. The grant is keyed by the
   canonical project path and a SHA-256 of the definition set. An edit
   to any definition asks again.

A grant trusts the commands and the code they load, including later
edits to referenced project scripts. The definition hash does not pin
that code. Both the trust question and `dmz trust` state this scope.
Revoke the grant before running an untrusted revision of that code.
Trusted hooks run independently of tool approval; this grant is not a
sandbox or an integrity check on script contents.

The grant lives in `$XDG_DATA_HOME/demerzel/trust.json`. `dmz trust`
prints the project's definitions and their state. `dmz trust --allow`
grants; `dmz trust --revoke` removes the grant. An interactive session
with pending definitions asks once at the first prompt, through the
approval channel, before the first turn. A one-shot run never asks: it
skips pending definitions and prints one notice. The user file's own
hooks and servers need no grant: the user wrote them.

Every new config key follows one of these trust shapes, stated in its
design section and written into [`config.md`](config.md) by the step
that adds it:

| Key | Project file may |
|---|---|
| `providers.<name>.kind`, `.base_url`, `.api_key_env`, `prompt.base_file` | not set it |
| `providers.<name>.thinking` | set it |
| `roles.<name>.chain` | name user-defined instances only |
| `discovery` | remove sources, not add |
| `memory.enabled` | turn off, not on |
| `memory.recall_tokens`, `subagents.max_model_calls`, `subagents.timeout_seconds`, `judgments.max_calls` | lower, not raise |
| `hooks`, `mcp.<name>` | define, behind the grant |
| `judgments.instance` | not set it |
| `judgments.checks` | turn a check off, not on or up |
| `judgments.rules` | define, without a grant; a rule cannot approve |

Harness-owned stores under `$XDG_DATA_HOME` are outside the project-root
write confinement: the memory store, the proposals file, and the task
logs. A tool that writes one declares `Write` access, so the approval
mode still gates it.

### Background tasks

The bash process group ends with each call today. M3 adds one input:
`background: true`. The command starts in its own process group and
the call returns at once with a task id and the log path. The
background path drops the call's group guard and `kill_on_drop`; the
run's task list owns the kill instead. Output goes to
`$XDG_DATA_HOME/demerzel/projects/<project-id>/tasks/<task-id>.log`,
capped at 8 MiB with a final marker line.

Two tools read and stop tasks: `task_output`, input `{ "id": string,
"lines": integer? }`, read-only, returns the last lines, 100 by
default, at most 32 KiB. `task_stop`, input `{ "id": string }`, execute
access, sends `SIGTERM`, waits two seconds, then `SIGKILL`. `/tasks`
lists the run's tasks with their state and log paths.

A task belongs to the run that started it. Every exit path the harness
controls stops its tasks the same way `task_stop` does. A `SIGKILL` of
the harness cannot; a later run does not adopt orphans, and their logs
stay until the user removes them. Subagents share the parent's task
list. This closes the background-task open question: the mechanism is a
detached group with a log file, not a job control layer.

### Project memory

`demerzel-memory` owns a SQLite file per project:
`$XDG_DATA_HOME/demerzel/projects/<project-id>/memory.sqlite`, next to
the sessions. One table holds facts: id, text, tags, session id,
created time, and source (`tool`, `user`, or `compaction` in M4). The
store opens in WAL mode with a busy timeout. Concurrent sessions share
the store; writes serialize and report an error if the timeout expires.
`forget` deletes the row.

Three tools: `memory_save`, input `{ "text": string, "tags": [string]? }`,
write access; `memory_search`, input `{ "query": string, "limit":
integer? }`, read-only; `memory_forget`, input `{ "id": string }`, write
access. A fact is at most 2 KiB. A project holds at most 10,000 facts;
a save past the cap fails with a message. `/remember <text>` saves a
fact with source `user`. `/memory` lists the newest facts;
`/memory search <query>` and `/memory forget <id>` do what they say.

Retrieval is lexical. `demerzel_core::lexical` holds a BM25 index over
tokens: lowercase, split on non-alphanumerics and on case changes, with
`k1 = 1.2` and `b = 0.75`. The session builds the index from the table
at open, in memory. A committed save or forget updates the local index.
Before an explicit search, the store checks for commits from other
connections and refreshes a stale index. The search observes commits
completed before it starts; SQLite change tracking can drive the refresh.
Auto-recall stays fixed for the run. Tool discovery uses the same index
type. Embeddings stay out until the Layer-4 recall measurement says
lexical recall fails.

Auto-recall runs once per session run, at the first prompt. The query is
the first prompt. The facts that fit `memory.recall_tokens`, default
1,000, join the system prompt as its last segment, newest first among
equal scores. The session file gains a `recall` record with the fact ids
and the byte count, so a replay can measure recall precision.
`memory.enabled = false` turns the tools and the recall off.

### The personality profile

One file: `$XDG_CONFIG_HOME/demerzel/personality.md`. The user reads and
edits it. It joins the prompt as segment 2. The open question asked
whether the profile is one file or layered; the answer is one file.
Project-specific workflow belongs in the project's instructions, which
already layer.

The profile workflow separates proposals from accepted changes.
The `profile_propose` tool takes `{ "text": string }` and declares write
access. It appends the proposal, session id, and date to
`$XDG_DATA_HOME/demerzel/profile-proposals.jsonl`. The interface shows a notice.
In `ask` mode the
approval question shows the text before the proposal lands. The tool
takes no session grant: an `always` answer covers that call alone, so
every proposal shows its text. `/profile`
shows the file path and the pending proposals. `/profile accept <n>`
appends the proposal under a dated heading. Acceptance serializes with
other accepts and checks the latest file plus the addition against 8 KiB.
The addition includes its dated heading.
An overflow leaves both the file and proposal unchanged and asks the
user to edit the profile. Accepted text takes effect on the next run,
because the current prompt stays fixed. `/profile reject <n>` drops
the proposal. No other profile command or memory tool writes the file;
the existing unsandboxed `bash` tool remains outside this workflow guarantee.

### Hooks

A hook is a subprocess at a loop event. It reads one JSON object on
stdin and can write one on stdout. Config:

```toml
[[hooks]]
event = "tool_before"
tool = "bash"
run = "scripts/check-command.sh"
timeout_seconds = 10
```

Events and their input fields, beside the fixed `event`, `session`, and
`project` fields:

| Event | When | Input | Output |
|---|---|---|---|
| `session_start` | the session opens, at the first prompt | `resumed` | — |
| `turn_start` | a user turn starts | `prompt` | `{ "context": string }` appends text to the user message |
| `tool_before` | before a tool call dispatches | `tool`, `input` | `{ "decision": "deny", "reason": string }` blocks the call |
| `tool_after` | after a tool call returns | `tool`, `input`, `output`, `is_error` | — |
| `turn_end` | a user turn ends | `outcome` | — |
| `session_end` | the run ends | `outcome` | — |

Exit code 0 continues. Exit code 2 denies a `tool_before` call with
stderr as the reason the model sees. Any other exit code, a timeout, or
unreadable output logs a warning and continues: a broken hook must not
strand a session. The child gets the bash tool's constructed
environment plus `DEMERZEL_SESSION` and `DEMERZEL_PROJECT`; the tools
crate exports its environment builder for this. The default timeout is
10 seconds. A `tool_before` hook's `context` output is ignored: only
the user message may grow, and only at `turn_start`.

Hooks implement the `LoopHost` seam that step 8a adds to
`EngineParts` (see [the loop seam](#the-loop-seam)). The engine fires
the four turn and tool events; the
runtime fires `session_start` at `Runtime::open` and `session_end` on
the run's exit path, because the engine sees neither boundary. The
binary crate implements hooks with subprocess spawns. At `tool_before`,
hooks run before judgment rules and the approval policy. The engine
records each run as a
`hook` session record: event, hook index, exit code, decision, reason,
and duration. A denied call keeps the existing `approval` record shape:
`decision` is `deny` and `answer` takes the new value `hook`. The model
sees the hook's reason in its error result. Project hooks pass the
trust gate above.

### The MCP client and deferred tool discovery

The `mcp` module of `demerzel-tools` speaks the stdio transport: one
JSON-RPC message per line on the child's stdin and stdout, stderr
captured to the debug trace. Config:

```toml
[mcp.docs]
command = "uvx"
args = ["docs-server"]
env = { DOCS_ROOT = "./docs" }
always = ["docs_search"]
```

Protocol, checked against the
[specification](https://modelcontextprotocol.io/specification/2026-07-28)
on 2026-09-06: the 2026-07-28 revision carries the protocol version and
the client capabilities in `_meta` on every request, and
`server/discover` reports a server's versions. Revisions up to
2025-11-25 open with an `initialize` handshake. The client is dual-era,
the behavior the specification's stdio page recommends:

1. Send `server/discover` with a 2-second timeout of its own.
2. A `DiscoverResult` or a recognized modern error means a modern
   server. Pick a version from its list and continue.
3. Any other error, or the timeout, means a legacy server. Send
   `initialize` with the newest legacy version and accept the version
   the server returns.

The probe's timeout sits inside the 10-second startup budget, so a
legacy server still has eight seconds to answer `initialize`. The step
re-checks the method names and the `_meta` fields against the
specification at build time and records any drift in this plan.

The client lists tools through `tools/list` with its cursor, calls them
through `tools/call`, joins text content items into the result, and
marks every other content type with one line. `isError` maps to an
error result. Dropping a call sends `notifications/cancelled`. A server
starts in its own process group, as a `bash` command does. Shutdown
closes stdin, waits two seconds, then sends `SIGTERM` and `SIGKILL` to
the group, so a launcher's grandchild dies too. A server that exits on
its own restarts once per session.

Servers start at session open, concurrently, with a 10-second startup
timeout each. A server that fails is reported once and retried at the
next turn start. Nothing starts at boot.

Tool bridge: each server tool becomes a `Tool` with the server's
`inputSchema`. Its name derives from `<server>__<tool>`, encoded into
`[A-Za-z0-9_-]` and limited to 64 bytes, including any hash suffix.
The mapping is deterministic and unique across core and server tools.
It retains the original server and tool names for dispatch; registration
refuses any unresolved collision. The
[MCP alphabet](https://modelcontextprotocol.io/specification/2026-07-28/server/tools#tool-names)
permits dots, which the
[provider alphabet](https://platform.claude.com/docs/en/agents-and-tools/tool-use/define-tools#specifying-client-tools)
does not. Server tools declare `Execute` access because they run code
in another process with arbitrary arguments. The M2 rule holds: an
`always` answer covers the exact input. In `ask` mode, a server tool
asks once per distinct input. A
per-server grant that covers a tool by name is a dogfood decision,
listed under Scope. The `always` config list only names the tools the
prompt advertises from the start; it grants nothing. Server annotations
stay untrusted, as the specification requires, so no annotation lowers
the class.

Deferred discovery: the prompt carries the core tools, the `always`
tools, and one `search_tools` tool, input `{ "query": string, "limit":
integer? }`, read-only. The search ranks every server tool by BM25 over
name, description, and property names. The default limit is 5 and the
cap is 10; each hit names its required parameters. From the next model
call, the found tools' definitions join the advertised set, appended
after the core tools in discovery order. The bridge holds the found set
behind interior mutability, per session, cleared by `/new`. The engine
takes the host's definitions again after every tool batch; equal bytes
keep the prefix stable, and a grown set changes it once. `/mcp` shows
each server, its state, and its tool count. `doctor` shows the same.

### Subagents

The decision: in-process. A subagent is a second `Engine` on the same
runtime, the same provider host, and the same tool implementations. The
reasons: the marginal-session memory gate is 15 MB PSS on Linux, a
second process of this binary would pay the whole idle baseline again,
and an in-process engine shares the HTTP client, the config, and the
tools at no extra cost. A subprocess buys isolation that the approval
policy already provides. Step 17 measures the in-process PSS delta
during a subagent turn as a sanity check on the choice; the marginal
gate itself measures a second process and stays provisional.

The `agent` tool, read-only access, input:

```json
{
  "name": "reviewer",
  "task": "Review the diff for correctness.",
  "result_schema": { "type": "object", "properties": { "findings": { "type": "array" } }, "required": ["findings"] },
  "role": "fast",
  "tools": ["read", "grep", "glob"]
}
```

`name` picks a definition from the agent files above; the body becomes
the subagent's instructions and its frontmatter supplies defaults for
`role` and `tools`. The subagent's system prompt is the parent's
segments 1 to 3, then the definition body: no skill index and no
recall. It is fixed for the subagent's run, and the child file records
its own `prompt_hash` from step 18 on. `task` is the subagent's first
prompt.
`result_schema` is optional; without it the result is the text. The
subagent's registry is built for the call: the named tools and
`submit_result`, input `{ "result": any }`. The only way to finish is
`submit_result`. The harness validates the value against the schema
with a built-in validator for `type`, `properties`, `required`,
`items`, `enum`, and `additionalProperties`. A failure returns the
validator's message to the subagent as an error result, so the model
repairs it. The parent's tool result is the validated JSON.

The seam: `ToolHost::run_approved` gains a `ToolContext` argument that
carries the parent's event sink, its approver, and its policy for the
call's duration. The subagent host is built before the registry and
shared by the runtime and the `agent` tool, so no back-reference into
the runtime exists. The subagent starts from a copy of the parent's
policy, mode and grants; the grants it collects merge back when it
ends.

Routing: `role` picks the chain; the default is the parent's route.
Approval: the subagent runs under the parent's mode. In `ask` mode its
questions go through the parent's approver, with the agent's name on
the card. In `read-only` mode the subagent holds read tools only.
Cancellation: the parent's Ctrl-C drops the `agent` future, which drops
the nested engine and its running tool; the existing drop contract
kills the tool's process group. Limits: depth 1, so a subagent has no
`agent` tool; one subagent at a time; `subagents.max_model_calls`,
default 32, its own counter apart from the parent's turn;
`subagents.timeout_seconds`, default 600; the result at most 64 KiB.
Workspace: the same project root and the same tools; sequential
execution rules out concurrent writes.

The subagent writes its own session file. Its `meta` record gains
`parent`: the parent session id and the call id. Before the child runs,
the parent appends an `agent_child` record with `call_id` and `session_id`.
The parent's file also holds the call, the result, and the `tool` record.
`SessionStore::latest` excludes child sessions, including for the exit hint.
An explicit `--session` naming a child stops with a message that names
its parent; M3 does not resume children as standalone conversations.
The engine emits
`AgentStart { id, name }` and `AgentEnd { id }` around the nested run;
the nested engine's events pass through between them. The interface
folds them into one agent card, collapsed by default, with a live line
and Ctrl-O expansion. Usage events pass through too, so the status
totals count the subagent's spend.

### The evaluation foundation

`bench/eval/` is Python, next to the probes. The session file is the
trace: schema 1 with the M3 fields. Pieces:

- `smoke.py`: the hermetic runner, early. Each task is a fixture
  repository, a scripted model transcript, and an oracle. The runner
  starts the standalone mock binary, points an `openai-compat` instance
  at it, runs the one-shot CLI, and checks the oracle. No model, no
  network. CI runs it in the `gates` job after the release build. The
  mock replays by request order, so a prompt change does not break a
  task; a harness change that alters the final state does.
- `manifest.py`: the candidate envelope and the scenario, each a JSON
  document with `schema: 1`. The candidate names the commit, the
  binary's SHA-256, the provider instance and model, the judge instance
  and model with each check's action, `prompt_hash`,
  `tools_hash`, the environment image, the retry and context policy,
  and the budget. The scenario names the base image, the seed, the
  allowed tools, the oracle command, the policy assertions, the slice
  labels, and an optional fault profile.
- `bundle.py`: reads a session file and writes the evidence bundle:
  the typed trace, usage totals with cache parts, judge usage apart from
  model usage, the judgment records, tool steps with durations, turn
  outcomes, wall time, the final-state diff, the oracle
  result, and an infrastructure-failure class. Memory and I/O come from
  the runner's process probe, not from the trace.
  It follows `agent_child` records and checks each child's `meta.parent`,
  including failed or cancelled children. Call ids are qualified by
  session; each usage
  record contributes once. Parent tool duration includes child time,
  so nested durations do not add to total wall time. A missing child
  trace marks the bundle incomplete rather than silently reducing totals.
- `oracle.py`: the state-oracle interface. An oracle is a command run in
  the final state; exit 0 passes. Artifacts are the final-state `git
  diff` and named files the scenario lists.
- `harbor_agent.py`: a `BaseInstalledAgent` for Harbor. `install` puts
  the release binary in the container. `run` executes one-shot
  `demerzel --auto` with the instruction. `populate_context_post_run`
  reads the session file into Harbor's trajectory format. The step
  checks the format's current name and fields against the Harbor docs
  before it lands. One Terminal-Bench task runs through it locally;
  this answers the open question about a shim.

Step 18 records `meta.prompt_hash` and the initial `meta.tools_hash`
on each open or resume. A `tool_set` record stores the canonical
advertised definitions and their hash before the first model call and
whenever they change. Each record applies until the next one in that
session, including calls without usage. The bundle preserves these records
for parent and child sessions. Equal definitions create no duplicate
record. This captures deferred discovery and approval-mode changes without
storing every schema on every call. Harbor conversion uses the same
joined trace as the bundle.

Private material stays out of the public tree. The bank is a directory
outside this repository; `DEMERZEL_EVAL_BANK` names its path. Whether
that directory is a private repository or a local folder is the
owner's choice and not a step. Each task there is a reproduced dogfood
failure in the same layout, and the bank keeps an index file with each
task's source step. The public tree holds the schemas, the runner, and
the 3 to 5 smoke tasks, each a sanitized synthetic reproduction with no
private data. This redefines the smoke subset that
[`plan.md`](plan.md#layer-3--agentic-capability-harness--model) once
called a subset of the private bank; this plan's pull request edited
that line.

## Contract growth, proposed

This table lists the growth the steps propose. Each step's pull request
declares its own share. This documentation pull request grows no
contract.

| Family | Proposed growth |
|---|---|
| Provider trait | The methods are unchanged: `id`, `capabilities`, `stream`. Step 19 removes each `Capabilities` field without a consumer; the struct stays. That is a breaking change to a public type in the provider contract. Step 8a adds the `Judge` trait beside it, with `id` and `ask`. The judge is a second model contract; step 8a adds it to the `AGENTS.md` contract list. |
| Engine events | `ModelStart { provider, model }` (step 6); `Judgment { call_id, source, action, text }` (8b); `AgentStart { id, name }` and `AgentEnd { id }` (step 17). |
| Tool names and schemas | `bash` gains `background` (step 9); new tools `task_output`, `task_stop` (9), `skill` (10), `memory_save`, `memory_search`, `memory_forget` (13), `profile_propose` (14), `search_tools` and `<server>__<tool>` (16), `agent` and `submit_result` (17). |
| Config keys | `providers.<name>.kind`, `.api_key_env`, and instance names as `provider` values (2); `kind = "openai-responses"` as a new value (3); `.thinking` (4); `[roles.<name>] chain` (6); `[prompt] base_file` (7); `kind = "typesafe"` as a new value, `[judgments] instance`, `checks` with `off` and `log`, `max_calls` (8a); the `warn`, `note`, and `act` actions and `[[judgments.rules]] event`, `tool`, `ask`, `threshold`, `action`, `reason` (8b); `openai-compat` instances as `judgments.instance` (8c); `discovery` (12); `[memory] enabled`, `recall_tokens` (13); `[[hooks]] event`, `tool`, `run`, `timeout_seconds` (15); `[mcp.<name>] command`, `args`, `env`, `always` (16); `[subagents] max_model_calls`, `timeout_seconds` (17). The environment gains `DEMERZEL_EVAL_BANK` (8), a variable and not a key, like `DEMERZEL_LOG`. |
| Session file | `meta.provider` redefined as the instance name, `meta.kind`, and optional `SealedPayload.model` and `.instance` added; `SealedPayload` becomes non-exhaustive (2); shipped adapters populate payload identity (5); `usage.provider`, `usage.model` (6); the `judgment` record (8a); the `answer` value `judge` (8b); the `recall` record (13); the `hook` record and the `answer` value `hook` (15); `meta.parent` and the `agent_child` record (17); `meta.prompt_hash`, `meta.tools_hash`, and the `tool_set` record (18). The schema version stays 1. |
| CLI | `--provider` and `DEMERZEL_PROVIDER` take instance names (2); `login <instance>` for both compatible adapters (4); `/model <provider> <model>` (6); `login <instance>` for a `typesafe` instance (8a); `/judgments` (8b); `/skill` (10); custom commands (11); `/tasks` (9); `/remember`, `/memory` (13); `/profile` (14); `trust` subcommand (15); `/mcp` (16); `-c` excludes children and `--session` refuses a child id (17). |

Crate API changes that are not in the six families: `SessionOpen`
replaces the parameters of `SessionMeta::now` and `SessionStore::create`
(step 2); `Runtime::open` replaces `open_session` and `engine`,
`Engine::new` takes `EngineParts`, and core gains `Route` and
`ProviderHost` (5); `RunConfig` carries the route origin (5); core gains
`Judge`, its request and reply types, and `LoopHost` on `EngineParts`
(8a), `lexical` (13), and `ToolContext` on
`ToolHost::run_approved` (17); hooks implement `LoopHost` (15); the tools
crate exports its environment builder (15). Each step names its share.

## Dependencies

Each new dependency passes a `cargo-bloat` check against the 15 MB
working budget and the 25 MB CI limit. A dependency that grows the
binary more than 10% lands with a receipt naming what the bytes buy.
Dev-only crates are exempt.

| Crate | Purpose | Note |
|---|---|---|
| rusqlite (`bundled`) | the memory store | MIT; SQLite is public domain. Expected to cross the 10% line: the receipt is required. |
| YAML parser, selected in step 10 | extension frontmatter | Maintained, bounded parsing; the compatibility fixtures and size receipt decide the library. |

Not added, with the reason:

- The unmaintained `serde_yaml` crate. Step 10 selects a maintained
  alternative under the repository's dependency policy.
- A JSON Schema crate. The subagent result validator covers the
  keywords above in a few hundred lines. MCP servers validate their own
  inputs.
- An MCP SDK crate. The stdio client is one framing rule and a handful
  of methods; the hand-rolled SSE parser and loopback listener set the
  precedent. Step 16 measures the official Rust SDK's size before it
  decides, and records the receipt.
- A search-engine crate. BM25 over short documents is a small module.
- A client crate for the judge API. No Rust client exists, and the
  request is one JSON `POST` over the existing HTTP client.

## Build order

Each step is one pull request, except the close-out sequence. Each
block names the outcome, the scope, the dependencies, the crates, the
contract changes, the compatibility rules, the verification, the
completion rule, and the measurements. Each step's dogfood records every
failure as a bank candidate under `DEMERZEL_EVAL_BANK`, so step 18
assembles the bank rather than invents it.

### 1. M3 plan

This document, merged in PR #18. PR #19 added step 3 and
renumbered the later steps. The 2026-09-22 revision adds steps 8a to 8c
for judgments. Letters keep the later step numbers stable.
*Verify: review.*

### 2. `openai-compat` and named provider instances

- **Outcome:** a user points Demerzel at any Chat Completions endpoint
  with a `base_url`, a model id, and an optional key from the
  environment, and runs a tool session against it.
- **Scope:** the adapter; `kind`; instance resolution in config and
  `doctor`, with the secret-scanner allowance; `api_key_env`; the
  provider host with one lazy slot per instance; `SessionOpen`;
  `meta.provider` as the instance name and `meta.kind`; payload identity
  and lossless compat reasoning replay; the mock route
  and fixtures; the `--base-url` rule; the trust lines and the
  [`config.md`](config.md) update.
- **Depends on:** nothing.
- **Crates:** `demerzel-providers`, `demerzel`, `demerzel-mock`,
  `demerzel-core`.
- **Contracts:** provider trait: `SealedPayload`, which travels in
  `StreamEvent::ReasoningDone`, becomes non-exhaustive with `new`,
  `with_identity`, and `replays_to`; engine events none; tools none;
  config `providers.<name>.kind`, `.api_key_env`, and `provider` takes
  instance names and the `openai-compat` value; session
  `meta.provider` redefined, `meta.kind`, `SealedPayload.model`, and
  `SealedPayload.instance` added; CLI `--provider` and
  `DEMERZEL_PROVIDER` take instance names.
- **Compatibility:** a table without `kind` keeps its meaning. The two
  shipped adapters stay implicit instances, and their `meta.provider`
  spelling is unchanged. Old session files read.
- **Verify:** hermetic adapter tests against the mock: a text turn, a
  tool call streamed in fragments, two calls in one reply, a reply with
  `reasoning_content`, a usage chunk, a stream without `[DONE]`,
  `length`, `content_filter`, 401, 429 with `retry-after`, and 5xx. A
  strict fixture refuses a tool continuation without the original reasoning
  fields. The same continuation passes after resume; a different instance
  or model receives no foreign reasoning payload. A
  test shows `reasoning_effort` absent at the default level and present
  after `/effort`. Config tests resolve a named instance, refuse a
  compat instance without `base_url` or `model`, and reject `kind` and
  `api_key_env` in a project file. A `doctor` test lists instances and
  raises no secret warning on `api_key_env`. Dogfood: a tool session
  against a local compatible server on the Linux reference machine.
  Fixtures cover a hosted provider and a local implementation, including
  a new model id and unknown optional response fields.
- **Done when:** the dogfood completes a tool turn and every test above
  passes.
- **Measure:** the turn bench gains `turn_text_compat`; boot and idle are
  unchanged because the provider builds at the first turn.
- **Build notes (2026-09-22):**
  - The runtime keeps its one lazy provider slot, because a run uses
    one instance until routing arrives. Step 5's `ProviderHost` adds one
    slot per instance.
  - `demerzel_providers::adapter_rules` states each adapter's rules
    once: a required endpoint, implicit effort, a key variable, and
    credentials of its own. The boot check, `doctor`, the first-run
    screen, and the effort rule read it. Step 3 adds one row.
  - The effort rule and the first-run check read the head instance.
    Step 5 must apply them per route target, from the target's rules.
  - A level counts as set for an adapter without implicit effort only
    from a flag, a variable, the instance's own table, or `/effort`. A
    top-level `effort` belongs to no instance. An adapter without a
    default model takes a top-level `model` only from a layer that
    names the instance.
  - `config::Instance` and `config::instance_problems` serve `doctor`
    and the boot check. Step 6 needs every chain target resolved under
    the same trust rules: add one `resolve_instance` then, and let
    `Settings` hold the head's result.
  - `api_key_env` also serves an `anthropic` instance.
  - A switch record starts from the session's last meta record, so the
    fields that steps 17 and 18 add stay on each record.
  - The fixtures reproduce the documented streams of six
    implementations, with the source and the read date in each file.
  - Crate API: `EngineConfig` becomes non-exhaustive and gains
    `instance`; `RunConfig` gains `kind`, `api_key_env`, and
    `effort_set`; `SessionSettings` gains `effort_set`,
    `implicit_effort`, and `request_effort`; `Settings.model` becomes
    optional and `Settings` gains `kind`, `api_key_env`, `effort_set`,
    and `problems`; `PROVIDER_IDS` lists adapter ids, the `kind`
    values; `Session::meta` returns the last meta record.
- **Dogfood receipt (2026-09-26):** the release build of `f4b7900` ran
  on the Linux reference machine against Ollama 0.34.4. The models were
  `qwen3:8b` and `qwen3-8b-16k`, a derived model with `num_ctx` 16384.
  The instance had no key. Results:
  - A one-shot prompt called `read` and completed its turn. The next
    request sent the `reasoning` field back on the assistant message.
  - The full screen ran two turns in `ask` mode with `read`, `edit`, and
    `bash` calls. The turns covered approval, denial, an interrupt, and
    an `edit` input that the tool refused.
  - `-c` resumed the session. The first request sent `reasoning` on all
    9 earlier assistant messages. A resume with the other model sent it
    on none of 10.
  - The session file names `local` in `meta.provider` and
    `openai-compat` in `meta.kind`. Each payload names its instance and
    its model.
- **Dogfood findings:**
  - Ollama loaded `qwen3:8b` with a context of 4,096 tokens. One reply
    overran it with 6,304 tokens, and the model lost its task. The
    derived model ran the rest of the dogfood. Demerzel cannot detect
    this limit, so `context_window` must match the server's context.
  - All 6 `edit` calls put literal `\n` escapes into multi-line content,
    also after a correction. The request and the tool schema hold no
    such escape, so the model causes the failure. It is a bank candidate
    for step 8.

### 3. General Responses API adapter

- **Outcome:** a named instance runs a coding session through the
  Responses API with an API key and a configurable endpoint.
- **Scope:** `openai-responses`; instance resolution and `doctor`;
  the request and stream mappings above; lossless reasoning replay;
  the mock route and compatibility fixtures; the `config.md` update.
  Shared protocol code stays private to `demerzel-providers` and needs
  matching fixtures before extraction. Codex authentication stays separate.
- **Depends on:** step 2.
- **Crates:** `demerzel-providers`, `demerzel`, and `demerzel-mock`.
- **Contracts:** provider trait none; engine events none; tools none;
  config adds the `openai-responses` value to `providers.<name>.kind`;
  session uses the existing identity fields and opaque payload; CLI none.
- **Compatibility:** existing instances and sessions keep their behavior.
  Model ids remain config values. The adapter preserves local text and
  tool history without server storage or Codex credentials.
- **Verify:** sanitized fixtures cover the public OpenAI and xAI
  Responses contracts. Tests cover text, usage, fragmented tool calls,
  optional tool inputs, and distinct item and call ids. A strict fixture
  requires the original reasoning items on a tool continuation and resume.
  A different instance or model receives no foreign payload. Tests cover
  completed refusals, incomplete, failed, error, and truncated streams;
  partial tool calls never execute. Extra optional fields and new model ids pass.
  Effort stays absent by default. A retry never duplicates emitted events.
  Instance tests cover required endpoint/model, credential isolation,
  and `doctor`. Existing Codex tests pass after any shared-code change.
  Dogfood: a hosted API-key instance completes a tool turn and resumes
  its session, with the provider and model in the receipt.
- **Done when:** the tests pass and the hosted tool/resume receipt lands.
- **Measure:** the turn bench gains `turn_text_responses`; boot and idle
  keep lazy provider construction. The existing size budget applies.

### 4. Instance login and the thinking key

- **Outcome:** a user stores a key for either compatible adapter without
  an environment variable. An `anthropic` instance can turn adaptive
  thinking and effort fields off.
- **Scope:** `dmz login <instance>`; the `keys` auth section, file
  version 3; the benchmark auth fixture under version 3; `thinking`;
  the [`config.md`](config.md) update.
- **Depends on:** steps 2 and 3.
- **Crates:** `demerzel-providers`, `demerzel`.
- **Contracts:** provider trait none; engine events none; tools none;
  config `providers.<name>.thinking`; session none; CLI
  `login <instance>`.
- **Compatibility:** an M2.5 binary reads a version-3 store. A store
  without the section behaves as today.
- **Verify:** each compatible adapter stores a key after a mock `/models` check,
  refuses it on 401, and stores it with a warning on 404. A config test
  resolves `thinking` and sends the request shape. A strict fixture accepts
  disabled thinking and refuses adaptive thinking or `output_config.effort`.
  An `off` instance completes its tool turn, including after `/effort`.
  The command notice and `doctor` expose the omission. The boot probe runs
  against the updated fixture.
- **Done when:** the tests pass and the boot probe runs.
- **Measure:** none.

### 5. One runtime call and model identity

- **Outcome:** a resumed session keeps its own model, new payloads retain
  their identity across model switches, and the runtime opens through one call.
- **Scope:** `Route` and `ProviderHost` in core; `EngineParts`;
  `Runtime::open`; `SessionSettings.route`; the route origin on
  `RunConfig`; the step-2 payload identity fields and the wire filter in each
  adapter; the resume rule; `set_model` without the payload drop; the
  `config.md` line on what resumes from the file.
- **Depends on:** steps 2 and 3.
- **Crates:** `demerzel-core`, `demerzel-providers`, `demerzel`,
  `demerzel-tui` (the open path).
- **Contracts:** provider trait none; engine events none; tools none;
  config none; session the shipped adapters populate payload identity;
  CLI none.
- **Compatibility:** old payloads without identity fields replay as before.
  Their original instance and model cannot be recovered reliably.
- **Verify:** tests: a resume adopts the file's head target and a flag
  overrides it; a recorded instance missing from config falls back
  with a notice; a payload from another model stays off the wire; a
  switch back restores same-model payloads; the M2.5 replay-on-resume
  PTY scenario still passes. Cross-protocol tests preserve text and tool
  history while filtering foreign reasoning. Dogfood: `/model` to a
  second model and back, then resume.
- **Done when:** the tests pass and the dogfood resumes on the session's
  model.
- **Measure:** the turn bench stays flat.

### 6. Roles and fallback chains

- **Outcome:** a failed instance falls back to the next in its chain,
  and the session file names the target that answered.
- **Scope:** `[roles.<name>] chain`; the chain loop with the
  first-event rule; per-attempt capabilities; `ModelStart`;
  `usage.provider` and `usage.model`; `/model <provider> <model>`; the
  target on the status and the progress line; the trust line and the
  `config.md` update.
- **Depends on:** step 5.
- **Crates:** `demerzel-core`, `demerzel`, `demerzel-tui`.
- **Contracts:** provider trait none; engine events `ModelStart`; tools
  none; config `roles.<name>.chain`; session `usage.provider`,
  `usage.model`; CLI `/model <provider> <model>`.
- **Compatibility:** no `[roles]` table means the top-level keys define
  the head target, as today.
- **Verify:** tests: the chain falls to the second target on an
  authentication, network, or invalid-request failure before the first
  event; no fallback after a text delta; a `usage` record names the
  answering target; a project role over an undefined instance is
  refused; `/model` with two tokens switches the instance. Dogfood: two
  instances, the first with a bad key; the status names the second.
  A cross-protocol chain runs through the same tools and approval policy.
- **Done when:** exit criterion 2 holds.
- **Measure:** the turn bench stays flat: one route lookup per call.

### 7. Prompt composition and instruction files

- **Outcome:** `AGENTS.md` and the user's instruction file shape every
  session; a user can replace the base prompt; the profile file joins
  the prompt.
- **Scope:** the composer in the binary crate with the five segment
  positions; segments 1 to 3; the personality file read;
  `prompt.base_file`; the caps; the first-prompt argument of
  `Runtime::open`; the `config.md` update.
- **Depends on:** step 5.
- **Crates:** `demerzel`.
- **Contracts:** provider trait none; engine events none; tools none;
  config `prompt.base_file`; session none; CLI none.
- **Compatibility:** a project without instruction files sends the base
  prompt alone, as today. A project file cannot set `prompt.base_file`.
- **Verify:** tests: the segment order; byte-equal prompts across two
  sessions with equal inputs; a replaced base prompt sent as is; the
  file and segment caps with their markers; one symlinked file counted
  once. Dogfood: this repository's `AGENTS.md` shows in the trace at
  `trace` level; one cache-hit replay shows the second session's first
  call reading the prefix from cache.
- **Done when:** the tests pass and the replay shows the stable prefix.
- **Measure:** the composer runs once per session; the cache-hit replay
  is the measurement.

### 8. The hermetic smoke runner

- **Outcome:** CI checks the harness end to end against replayed
  transcripts, before the ecosystem steps land.
- **Scope:** the standalone mock binary; `smoke.py`; the 3 to 5 smoke
  tasks; the `gates` CI step and the [`ci.md`](ci.md) update; the bank
  layout and index format; `DEMERZEL_EVAL_BANK`.
- **Depends on:** step 2.
- **Crates:** `demerzel-mock` (the binary). The rest is Python.
- **Contracts:** provider trait none; engine events none; tools none;
  config none, with the `DEMERZEL_EVAL_BANK` variable; session none;
  CLI none.
- **Compatibility:** none; the runner is outside the binary.
- **Verify:** the smoke subset passes in CI on the release build; a
  task with a broken oracle fails the run; the bank index validates.
- **Done when:** the smoke part of exit criterion 9 holds.
- **Measure:** the smoke subset's CI duration is logged, not gated.

### 8a. The judge contract and logged checks

- **Outcome:** a configured judge answers the built-in checks in every
  session, and the session file records each answer. The loop's
  behavior does not change yet.
- **Scope:** `Judge` and its request and reply types in core; `LoopHost`
  on `EngineParts`; the `typesafe` kind with its retries; judge slots in
  `ProviderHost`; `[judgments] instance`, `checks` at `off` and `log`,
  and `max_calls`; the state builder and its caps; the four built-in
  checks at `log`, with approval in shadow; the `judgment` record;
  `doctor`; `dmz login` for a `typesafe` instance; the mock route and
  API fixtures; the [`prior-art.md`](prior-art.md) row; the `AGENTS.md`
  contract list; the trust lines and the [`config.md`](config.md) update.
- **Depends on:** steps 4 (instance login), 5 (`EngineParts` and
  `ProviderHost`), and 8 (the smoke runner).
- **Crates:** `demerzel-core`, `demerzel-providers`, `demerzel`,
  `demerzel-mock`.
- **Contracts:** provider trait none, and the new `Judge` trait beside
  it; engine events none; tools none; config `kind = "typesafe"`,
  `judgments.instance`, `.checks`, `.max_calls`; session the `judgment`
  record; CLI `login <instance>` accepts a `typesafe` instance.
- **Compatibility:** without `[judgments]`, the harness sends no judgment
  request and writes no record. Old binaries skip the `judgment` record.
- **Verify:** hermetic tests against the mock:
  - one request carries every question of one event;
  - each answer type maps to the reply;
  - 401, 422, 429 with `retry-after`, 529, and a timeout;
  - a `log` check changes no event, no tool result, and no request byte
    to the main model;
  - the approval shadow leaves the policy's decision unchanged;
  - the state caps and the cut order;
  - the call cap stops checks with one notice;
  - a project file cannot set `instance` or raise a check;
  - a role that names a `typesafe` instance stops at boot;
  - `doctor` lists the judge and notes an alias model id.

  Fixtures from the API reference cover each answer type, an unknown
  optional field, and a pinned and an alias model id. A smoke task runs
  with a scripted judge. Dogfood: real sessions in this repository with
  every check at `log`.
- **Done when:** the tests pass, and the dogfood records hold at least
  200 asks with both the judge's answer and the user's answer.
- **Measure:** judge latency, p50 and p90, from the records on both
  reference machines. The turn bench gains `turn_text_judged`, with a
  mock judge at `log`; it stays within noise of `turn_text_compat`.
  Boot and idle stay unchanged.

### 8b. Judgment actions and rules

- **Outcome:** the judge changes the loop. It warns, adds notes, skips
  questions for safe calls, adds questions for risky calls in `auto`
  mode, and enforces rules in plain language.
- **Scope:** the `warn`, `note`, `act`, and `deny` actions; the approval
  rules for `ask` and `auto`; `[[judgments.rules]]`; the `Judgment`
  engine event; the finding on the tool card and on the approval
  question; `/judgments`; the `approval` answer value `judge`; the risky
  fixture set under `bench/eval/`; the gate report; the `config.md`
  update.
- **Depends on:** step 8a and its dogfood records.
- **Crates:** `demerzel-core`, `demerzel`, `demerzel-tui`.
- **Contracts:** provider trait none; engine events `Judgment`; tools
  none; config the `warn`, `note`, and `act` actions and
  `judgments.rules`; session `answer` gains `judge`; CLI `/judgments`.
- **Compatibility:** checks at `log` behave as in step 8a. Every default
  stays at `log`.
- **Verify:** tests with a scripted judge:
  - `warn` emits the event;
  - `note` appends to the next tool result and to nothing else;
  - an `act` approval runs a call without the question and grants
    nothing for the session;
  - a judge approval never applies in `read-only` mode;
  - `auto` mode asks about a call rated destructive, and a run without
    an approval prompt refuses it;
  - a timeout leaves the policy's decision unchanged;
  - a rule denies a call with its reason, and both records name it;
  - a project rule cannot approve;
  - the engine refuses a host change from deny to allow;
  - subagent calls pass the same checks.

  A live gate runs on the reference backend. The gate needs no approval
  across a risky set of at least 100 calls. It also needs no shadow
  approval of a call that the user denied in the step 8a records. The report gives each check's
  precision at its threshold. Dogfood: a session in `ask` mode with
  `approval = "act"`, and a rule that refuses edits under a protected
  path.
- **Done when:** the tests pass, the live gate passes, and the report is
  in this plan.
- **Measure:** approval questions per session with and without `act`;
  the dispatch latency that rules and approval add, p50 and p90.

### 8c. A judge over a compatible instance

- **Outcome:** a user without the reference service runs judgments on a
  local or hosted Chat Completions server that returns log
  probabilities.
- **Scope:** the log-probability judge in `demerzel-providers`;
  `openai-compat` instances as `judgments.instance`; the question
  framing for each type; at most 8 requests in flight; the `doctor`
  notice for a server without log probabilities; the mock fixtures; the
  calibration report.
- **Depends on:** steps 2 and 8b (the labeled set and the risky set).
- **Crates:** `demerzel-providers`, `demerzel`, `demerzel-mock`.
- **Contracts:** provider trait none; engine events none; tools none;
  config `judgments.instance` accepts an `openai-compat` instance;
  session none; CLI none.
- **Compatibility:** the reference backend keeps its behavior.
- **Verify:** tests: `YesNo`, `Choice`, and `Score` answers from fixture
  log probabilities, normalized over the labels; a reply with too little
  mass on the labels fails the question and leaves the policy's decision
  unchanged; a server without log probabilities gets the `doctor`
  notice; `doctor` warns when `approval = "act"` names this backend.
  Live: a local server on the Linux reference machine answers the
  labeled set and the risky set.
- **Done when:** the tests pass and the calibration report is in this
  plan. The report compares reliability and the approval gate with the
  reference backend.
- **Measure:** latency per event on the local server; the calibration
  report.

### 9. Background tasks

- **Outcome:** a session can start a server, use it, and stop it.
- **Scope:** `background` on `bash`; the task list; the log file and its
  cap; `task_output`; `task_stop`; `/tasks`; cleanup on every exit path
  the harness controls.
- **Depends on:** nothing beyond `main`; ordered here for value.
- **Crates:** `demerzel-tools`, `demerzel` (the task list, `/tasks`,
  exit cleanup).
- **Contracts:** provider trait none; engine events none; tools `bash`
  gains `background`, new `task_output`, `task_stop`; config none;
  session none; CLI `/tasks`.
- **Compatibility:** a call without `background` behaves as today.
- **Verify:** tests: a background server answers a later `bash` call;
  `task_output` returns the tail; `task_stop` ends the group; the run's
  exit kills a live task; a task that ends on its own reports its exit
  status; the log cap. Dogfood: a dev server in this repository.
- **Done when:** exit criterion 8 holds.
- **Measure:** none; the path is off the turn bench.

### 10. Skills

- **Outcome:** a skill in the project or the user directory is one line
  in the prompt and one tool call away.
- **Scope:** the frontmatter parser; native skill paths; the skill
  index segment with its caps and drop order; the `skill` tool;
  `/skill`; invocation restrictions; the parser dependency and size receipt;
  the [`prior-art.md`](prior-art.md) rows for the borrowed
  formats.
- **Depends on:** step 7.
- **Crates:** `demerzel`, `demerzel-tools` (the `skill` tool).
- **Contracts:** provider trait none; engine events none; tools `skill`;
  config none; session none; CLI `/skill`.
- **Compatibility:** a project without skills sends no index segment.
- **Verify:** tests: folded and literal descriptions, quoted scalars,
  block and inline lists, nested metadata, malformed input, duplicate keys,
  and parser limits; missing-name defaults; the invocation restrictions
  alone and together; the index
  caps and the drop order with the `doctor` note; the `skill` tool on a
  known and an unknown name; `/skill` with arguments. Dogfood: one
  skill from the user's shared skills directory runs in a session.
- **Done when:** the tests pass and the dogfood skill runs.
- **Measure:** the YAML parser's size receipt.

### 11. Custom commands

- **Outcome:** a Markdown file becomes a slash command with completion
  and help.
- **Scope:** native command paths; `$ARGUMENTS`; the registry's second
  table; `/help` and the popup; the built-in name rule.
- **Depends on:** step 10 (the parser).
- **Crates:** `demerzel`, `demerzel-tui` (the popup takes a longer list).
- **Contracts:** provider trait none; engine events none; tools none;
  config none; session none; CLI `/<command>`.
- **Compatibility:** built-in command names stay reserved.
- **Verify:** tests: `$ARGUMENTS`; a built-in name is refused; `/help`
  lists custom commands after the built-in set; a REPL parity test over
  one custom command. Dogfood: one custom command in this repository.
- **Done when:** the command part of exit criterion 3 holds.
- **Measure:** none.

### 12. Config discovery

- **Outcome:** a repository's Claude Code, Cursor, and Codex CLI
  instruction, skill, command, and agent files work with zero edits.
- **Scope:** the discovery table for instructions, skills, commands,
  and agent definitions; `discovery`; precedence and shadowing with the
  `doctor` list and the use-time notice; the `AGENTS.md` Source-rules
  exception; the `config.md` update. MCP definition discovery waits for
  step 16.
- **Depends on:** steps 10 and 11.
- **Crates:** `demerzel`.
- **Contracts:** provider trait none; engine events none; tools none;
  config `discovery`; session none; CLI none.
- **Compatibility:** a repository without any of the paths sees no
  change.
- **Verify:** tests: precedence and shadowing across the five levels; a
  fixture repository with `.claude/`, `.cursor/`, and `.agents/` content
  yields its supported items with zero edits; Cursor's unconditional,
  glob-scoped, relevance-selected, and manual rules keep their activation
  limits; an imported user-only skill rejects model invocation;
  `doctor` names skipped content; `discovery = []` finds nothing; a project file
  cannot add a source. Dogfood: a real repository with another agent's
  config.
- **Done when:** exit criterion 4 holds.
- **Measure:** discovery walks fixed paths at session open, no
  recursion beyond the named directories; the first-prompt cost is
  logged at `info`.

### 13. Project memory

- **Outcome:** the agent saves facts, searches them, and recalls them in
  later sessions without a second brain built by hand.
- **Scope:** `demerzel_core::lexical`; the `demerzel-memory` crate; the
  three tools; `/remember` and `/memory`; auto-recall as segment 5;
  the `recall` record; `memory.enabled` and `memory.recall_tokens`
  with their trust lines; the `config.md` update.
- **Depends on:** step 7.
- **Crates:** `demerzel-core`, `demerzel-memory` (new), `demerzel`.
- **Contracts:** provider trait none; engine events none; tools
  `memory_save`, `memory_search`, `memory_forget`; config
  `memory.enabled`, `memory.recall_tokens`; session the `recall`
  record; CLI `/remember`, `/memory`.
- **Compatibility:** a project without a store gets one at the first
  save. Old binaries skip the `recall` record.
- **Verify:** tests: save in one session and search in another; recall
  inside the budget and last in the prompt; BM25 ranking on a fixed
  corpus; the fact cap and the size cap; two stores open at once;
  local and cross-session save/forget become visible to explicit searches
  without restart, while the composed recall stays unchanged;
  `enabled = false` advertises no memory tool; a project file cannot
  raise `recall_tokens`. Dogfood: save a fact about this repository,
  and a fresh session recalls it.
- **Done when:** exit criterion 5 holds.
- **Measure:** the `cargo-bloat` receipt for `rusqlite`; idle memory
  is unchanged because the store opens at the first prompt; the
  session-open cost with 10,000 facts, logged at `info`, under 50 ms on
  the Linux reference machine.

### 14. Personality proposals

- **Outcome:** the agent proposes profile changes; the user accepts or
  rejects them; the file never changes otherwise.
- **Scope:** `profile_propose`; the proposals file; `/profile` with
  `accept` and `reject`; the notice.
- **Depends on:** step 7.
- **Crates:** `demerzel`, `demerzel-tools`.
- **Contracts:** provider trait none; engine events none; tools
  `profile_propose`; config none; session none; CLI `/profile`.
- **Compatibility:** no profile file means an empty segment.
- **Verify:** tests: a proposal lands pending; `accept` appends under a
  dated heading; `reject` drops it; the file's bytes are equal on every
  other profile path; acceptance at and beyond 8 KiB, including headings;
  overflow leaves the proposal pending; accepted text joins the next run;
  `read-only` advertises no proposal tool; a second proposal
  asks again after an `always` answer. Dogfood: one
  accepted proposal in a real session.
- **Done when:** exit criterion 7 holds.
- **Measure:** none.

### 15. Hooks and the trust gate

- **Outcome:** a user runs a script at loop events, blocks a tool call
  from it, and adds context to a turn; project hooks run only after an
  explicit grant.
- **Scope:** hooks as a `LoopHost` implementation, before the judgment
  rules; the subprocess runner with the exported environment builder;
  the six events and
  their JSON; `[[hooks]]`; the `hook` record and the `answer` value;
  `trust.json`; `dmz trust`; the first-prompt question; the one-shot
  skip; the `config.md` update.
- **Depends on:** steps 7 and 8a (`LoopHost`).
- **Crates:** `demerzel-core`, `demerzel-tools`, `demerzel`,
  `demerzel-tui` (the trust question through the approval channel).
- **Contracts:** provider trait none; engine events none; tools none;
  config `hooks[]`; session the `hook` record, `answer` gains `hook`;
  CLI `trust`.
- **Compatibility:** no hooks means no spawn and no record.
- **Verify:** tests: a deny blocks the call and both records name the
  hook; exit 2 with stderr as the reason; a timeout continues; the
  constructed environment; `context` reaches the user message at
  `turn_start` only; a project hook stays off without a grant; an
  edited definition asks again; a one-shot run skips with one notice.
  The trust question states that referenced code can change; a script-only
  edit keeps the grant, while revocation blocks the next run. A hook
  deny ends the call before any judgment rule runs.
  Dogfood: a `tool_before` hook in this repository that refuses
  `git push`.
- **Done when:** the hook part of exit criterion 3 holds.
- **Measure:** the turn bench gains a workload with a no-op
  `tool_before` hook, so the fork-exec cost per tool call is a number
  on both reference machines.

### 16. The MCP client and deferred discovery

- **Outcome:** a configured MCP server's tools are one search away, and
  a repository's `.mcp.json` works after one grant.
- **Scope:** the `mcp` module of `demerzel-tools`; the dual-era stdio
  client; the tool bridge; `search_tools`; the engine's definitions
  refresh after each batch; `[mcp.<name>]`; MCP definition discovery
  for the `claude`, `cursor`, and `codex` sources; `/mcp`; `doctor`;
  the mock crate's echo server; the `config.md` update.
- **Depends on:** steps 13 (`lexical`) and 15 (the trust gate).
- **Crates:** `demerzel-tools`, `demerzel-core` (the refresh),
  `demerzel`, `demerzel-mock`.
- **Contracts:** provider trait none; engine events none; tools
  `search_tools`, `<server>__<tool>`; config `mcp.<name>.command`,
  `.args`, `.env`, `.always`; session none; CLI `/mcp`.
- **Compatibility:** no servers means no change to the prompt. The
  refresh after a batch keeps equal bytes when nothing changed.
- **Verify:** tests against the echo server in both eras: the probe and
  its timeout, listing with a cursor, a call round trip, `isError`, a
  non-text content item, cancellation on drop, the shutdown order, one
  restart, the startup timeout. Name tests cover dots, truncation,
  encoding collisions, core-name collisions, and original-name dispatch.
  A search test ranks a fixed catalog. A
  loop test shows a found tool advertised from the next call, the
  prefix equal before it, and the set cleared by `/new`. A policy test
  drops server tools in `read-only` and asks per input in `ask`; an
  `always` list advertises only policy-allowed tools and grants nothing.
  A discovery test reads a
  `.mcp.json` and stays off without a grant. The
  size receipt for the SDK decision. Dogfood: a real server in this
  repository.
- **Done when:** the MCP part of exit criterion 3 holds.
- **Measure:** the session-open cost with one server, logged at `info`;
  the `cargo-bloat` receipt.

### 17. Subagents

- **Outcome:** the agent delegates a bounded task and gets a typed
  result back, no prose parsing.
- **Scope:** `ToolContext` on `run_approved`; the shared subagent host;
  the `agent` tool; `submit_result`; the validator; the native agent
  paths, and agent definition parsing and defaults; the subagent's
  prompt; `[subagents]` with its trust lines;
  `AgentStart` and `AgentEnd`; the child session file and
  `meta.parent` and `agent_child`; latest-session filtering and child-resume refusal;
  the agent card; the `config.md` update.
- **Depends on:** steps 6 and 12.
- **Crates:** `demerzel` (the subagent runner), `demerzel-core`
  (`ToolContext`, events, session records), `demerzel-tui` (the card),
  `demerzel-tools` (the validator next to input parsing).
- **Contracts:** provider trait none; engine events `AgentStart`,
  `AgentEnd`; tools `agent`, `submit_result`; config
  `subagents.max_model_calls`, `subagents.timeout_seconds`; session
  `meta.parent`, `agent_child`; CLI `-c` excludes children and `--session`
  refuses them.
- **Compatibility:** the interfaces treat an unknown event as they do
  today; both learn the two events in this step.
- **Verify:** tests: a valid result returns; an invalid one loops once
  and returns; the call limit, the timeout, and the size cap each end
  the run with an error result; depth 1 holds; Ctrl-C drops the nested
  engine and kills its running tool; an `ask` question reaches the
  parent's approver; a grant made inside the subagent holds in the
  parent afterwards; `read-only` hands the subagent read tools only;
  parent and child files record their link before execution;
  a project file cannot raise the
  limits; after a child and a completed parent, `-c` resumes the parent;
  the exit hint names the parent; an explicit child id reports its parent.
  Dogfood: a review delegated with a findings schema.
- **Done when:** exit criterion 6 holds.
- **Measure:** the in-process PSS delta during a subagent turn on the
  Linux reference machine, as a sanity check on the in-process choice;
  the render bench with an agent card on screen.

### 18. Manifests, bundles, the oracle, and the Harbor adapter

- **Outcome:** a task runs through Harbor with Demerzel as the agent,
  and every run leaves a validated evidence bundle.
- **Scope:** `manifest.py`, `bundle.py`, `oracle.py`, `harbor_agent.py`;
  `meta.prompt_hash`, `meta.tools_hash`, and `tool_set` records;
  joined parent and child evidence; the process probe for
  memory and I/O; one local Harbor run; the bank at 10 to 20 tasks.
- **Depends on:** steps 8, 8b, 16, and 17; every dogfood before it for the bank.
- **Crates:** `demerzel-core` (hashes and tool-set records). The rest is Python.
- **Contracts:** provider trait none; engine events none; tools none;
  config none; session `meta.prompt_hash`, `meta.tools_hash`, `tool_set`;
  CLI none.
- **Compatibility:** old files without the hashes bundle with the fields
  absent.
- **Verify:** tests: a bundle from a real session file; manifests
  validate and a bad one fails; an oracle passes and fails; child traces
  and usage appear exactly once, including cancellation and duplicate local
  call ids; a missing child marks incomplete evidence; discovery changes
  the recorded tool set before the next call; unchanged definitions add
  no record; resume, approval changes, and child calls retain their sets.
  Dogfood:
  one Terminal-Bench task through the Harbor adapter with a live model,
  its bundle committed with a private-data review.
- **Done when:** exit criterion 9 holds.
- **Measure:** none beyond the bundle's own fields.

### 19. Close-out sequence

Three pull requests, in order:

1. Remove each capability flag without a consumer.
2. The receipts, one pull request per reference machine when the
   machines are not both at hand: metrics, warm boot, and idle memory,
   under the configuration exit criterion 10 names; render and turn
   only when a step above moved them.
3. `m3-closeout.md` with the alignment review, the contract inventory,
   the dependency list with every size receipt linked, the folds into
   the plan docs, and the M4 handoff.

*Depends on: every step. Done when: exit criterion 10 holds.*

## Decisions

Made now, from the code and the recorded evidence:

- **Two general protocols in separate steps.** Chat Completions lands
  in step 2; general Responses lands in step 3. Each uses named
  instances and the neutral provider contract. Vendor differences stay
  inside adapters, with fixtures from independent implementations.
- **Local conversation state for Responses.** The harness keeps the
  history and opaque reasoning items. Server storage is not required.
  Codex authentication and subscription defaults remain separate.
- **Named instances over per-model flags.** The `kind` key plus
  per-instance request keys carry every per-model difference the
  harness has met. Capability stays on the adapter.
- **Explicit effort for compat.** The adapter sends `reasoning_effort`
  only for a level the user set. The resolver's origin tracking makes
  this deterministic without a key.
- **No effort on a route target.** A second instance covers the case,
  and the existing effort ceiling stays the only one.
- **Fallback before the first event only.** A repeated delta is worse
  than a visible error.
- **Resume keeps the session's head target.** Flags and variables
  override; config files do not. New payloads record instance and model;
  legacy payloads without identity retain their old replay behavior.
- **One `Runtime::open` and one `EngineParts`.** The first prompt is an
  argument, so recall and composition need no engine seam. Later seams
  add fields, not parameters.
- **Prompt order fixed, recall last.** Everything above the recall
  segment stays byte-stable across sessions.
- **Compatible YAML frontmatter.** A maintained parser must pass the
  import fixtures and the size check. Activation restrictions survive import.
- **The discovery table is the decision.** A wrong path is corrected in
  place, never dropped in silence.
- **Discovered hooks never run.** Prompt content crosses tools;
  commands do not.
- **Trust by definition hash.** A grant covers one definition set in one
  project and the code it loads, including future script edits.
  A definition edit asks again; the trust question states this distinction.
- **Every new key has a trust shape.** The table above is the checklist
  for each step's `config.md` change.
- **Harness stores under `$XDG_DATA_HOME` sit outside the project-root
  confinement.** The tools that write them declare `Write`.
- **Background tasks as detached groups.** One input on `bash`, two
  tools, cleanup at exit. No job control.
- **The smoke runner lands early, from public fixtures.** Sanitized
  synthetic tasks replace the private-subset wording in `plan.md`, so
  CI carries no private data and the gate guards steps 8a to 18.
- **One new crate.** `demerzel-memory` isolates SQLite, an exception to
  the second-consumer rule recorded above. The MCP client is a module
  of the tools crate until a second consumer arrives.
- **BM25 in core, SQLite for durability.** One index type serves memory
  and tool discovery; the store gives provenance, WAL concurrency, and
  delete by id. Explicit searches refresh after commits; auto-recall stays fixed.
- **Personality changes require acceptance in the profile workflow.**
  Acceptance respects the 8 KiB prompt limit and affects the next run.
  The proposal tool declares `Write` because it appends to a store.
- **Hooks fail open, except a deny.** A broken hook logs and continues.
- **Dual-era MCP client, stdio only.** Checked against the 2026-07-28
  specification. The probe is the specification's own recommendation,
  and the legacy handshake is the path most servers in use need.
- **MCP tools are `Execute` with per-input grants.** The M2 rule holds;
  a per-server name-scoped grant waits for dogfood evidence.
- **Definitions refresh after each batch.** Cheaper and simpler than a
  change flag on tool output.
- **Subagents in process, depth 1, sequential, through `ToolContext`.**
  The parent lends its sink, approver, and policy for the call.
  Child sessions stay out of normal resume and join the evaluation evidence.
- **`submit_result` is the only exit.** Typed results need a typed
  door.
- **The evaluation runner is Python.** Parent and child session files
  carry the trace, including changes to advertised tool definitions.
  No new binary surface for evaluation.
- **Build order by value and by gate.** The smoke runner comes first
  among the non-feature steps; background tasks and memory land before
  hooks, MCP, and subagents.
- **Credit rows before the code.** Step 10 adds the prior-art rows for
  the borrowed formats; step 12 adds the Source-rules exception for the
  discovery module.
- **Judgments are the differentiator.** Decided 2026-09-22. A fast
  judge answers typed questions. Code owns the thresholds, the actions,
  and the policy.
- **A judge is a second model contract.** It returns probabilities, not
  text or tool calls, so it does not fit `Provider`.
- **The reference backend first, a compatible backend second.** Any
  backend that returns probabilities can serve. Step 8c keeps the
  feature usable without the reference service.
- **Log first, act on evidence.** Every check starts at `log`. A default
  rises only with a recorded precision result.
- **The judge moves a call one step.** It can remove a question or add
  one. It never overrides `read-only` mode, a hook, or a rule, and it
  grants nothing for the session.
- **Judgments stay out of the prompt.** A note joins one tool result;
  the prefix stays byte-stable.
- **One loop seam for hooks and judgments.** `LoopHost` serves both.
  Hooks run first, then rules, then the policy.
- **Thresholds and the timeout are constants in M3.** A key waits for
  records that show a user need.
- **Lettered steps for the revision.** Steps 8a to 8c follow the smoke
  runner, so the smoke subset guards them. The later numbers stay
  stable.

## Reconciled decisions

Older decisions checked against the current roadmap:

1. The M2 step-6 note named per-model capability flags as M3 work. The
   2026-08-26 obsolescence decision put capability on the adapter and
   never on a model id. Named instances resolve it: the `thinking` key
   on an `anthropic` instance carries the request shape that the note
   wanted a flag for. No per-model flag lands.
2. The M2 close-out kept five unread capability flags for M3. This plan
   names no consumer for `streaming`, `parallel_tool_calls`,
   `usage_in_stream`, `cache_control`, or `sealed_reasoning`. The
   sealed-payload filter keys on the payload, not on a flag. Step 19
   removes each flag that still has no consumer; the expected result is
   `tool_calls` alone. The struct stays.
3. The model-catalog exception from M2.5 step 8e covers the two shipped
   adapters. The compat adapter has no catalog; `context_window` is its
   only window source.
4. The background-task question named M2 or M3. Step 9 decides it.
5. The subagent-transport question asked footprint benches to decide.
   The design decides from the gate and the shared-state argument, and
   step 17 measures to confirm.
6. The personality-format and prompt-format questions close with the
   designs above.
7. `plan.md` described the smoke subset as a subset of the private
   bank. This plan redefines it as public sanitized fixtures and edits
   that line; step 8 builds it.
8. Compaction split, the crates.io name check, and the local-model
   track keep their milestones.

## Tracked separately

These items stay outside the build order. None is a prerequisite for a
step:

- The Codex `prompt_cache_key` proposal from the
  [M2 decision record](m2-plan.md#decisions-to-make-during-m2). It
  changes the provider contract and lands in its own pull request.
  Steps 5 and 6 must not absorb it.
- The size-policy reconciliation: plans allow receipt-backed budget
  exceptions, CI enforces 25,000,000 bytes. The M2.5 handoff left the
  gate unchanged. Step 13's receipt is the next data point, not the
  decision.
- [Required-check protection](ci.md#before-public-release) before a
  public release.
- The two M4 follow-ups: bounded layout retention and bulk card
  expansion, after the public benchmark baseline.
- Mac system-cold timing, deferred by decision.

## Measurements

M3 does not repeat the M2.5 pass. The table lists the receipts a step
commits; each step's block names its other checks.

| Step | Measurement |
|---|---|
| 2 | `turn_text_compat` in the turn bench |
| 3 | `turn_text_responses` in the turn bench |
| 7 | one cache-hit replay for the stable prefix |
| 8a | judge latency, p50 and p90, on both reference machines; `turn_text_judged` in the turn bench |
| 8b | the approval gate report; approval questions per session with and without `act`; the dispatch latency of rules and approval |
| 8c | the calibration report against the reference backend; latency per event on a local server |
| 10 | the YAML parser size receipt |
| 13 | the `rusqlite` size receipt; session-open cost with 10,000 facts |
| 15 | a turn-bench workload with a no-op `tool_before` hook |
| 16 | session-open cost with one server; the SDK size receipt |
| 17 | the in-process PSS delta of a subagent turn; the render bench with an agent card |
| 19 | warm boot, idle memory, and metrics on both reference machines; render and turn only when a step above moved them |

The gates hold as [`plan.md`](plan.md#targets) states them. The plain
interface keeps its M2 behavior and gains the new commands through the
shared registry, with parity tests over the commands, not the look.

## Risks

- **Compatible endpoints differ and change.** Servers reject fields they
  do not know and omit fields the adapter expects. Mitigation: the minimal request
  body, the explicit-effort rule, fixtures from independent implementations,
  and local and hosted tool-session receipts. Parsers ignore unknown
  optional fields and events but validate required data and terminal status.
  A field a server refuses becomes a per-instance key only with a
  dogfood receipt. An upstream regression adds a fixture and an adapter
  fix; protocol selection stays explicit. The close-out checks current
  protocol support and deprecations against the primary documentation.
- **Prefix churn.** Discovery, recall, and deferred tools can each move
  the prompt. Mitigation: fixed positions, recall last, definitions
  appended in discovery order, and the cache-hit replay at step 7.
- **Binary growth.** SQLite is the largest addition since the TUI.
  Mitigation: the receipt rule and the working budget. SQLite and the
  YAML parser are the planned runtime dependency additions.
- **MCP protocol drift.** The specification changed its lifecycle in
  2026-07-28. Mitigation: the dual-era client, the echo server in both
  eras, the build-time re-check, and a parser that ignores unknown
  fields.
- **Trust surface.** Project files now carry commands. Mitigation: the
  gate, the hash, the key table, and no translation of another tool's
  hooks.
- **Subagent cost.** A nested engine can spend a session's budget.
  Mitigation: the call limit, the timeout, depth 1, and the usage
  events on the status.
- **A hosted judge.** The reference backend is a paid, closed service.
  Its price, limits, or model can change, and an alias can move.
  Mitigation: the judge contract, the compatible backend in step 8c,
  pinned model ids in tuned configs, the answering model id in every
  record, and the fallback to the policy on any failure.
- **False approvals.** A wrong `act` approval runs a harmful command.
  Mitigation: `act` is opt-in; the step 8b gate; `read-only` mode,
  hooks, and rules stay above it; one approval covers one call.
- **Judgment latency.** A check on the critical path slows the loop.
  Mitigation: `log` and `warn` checks run beside the model call; one
  request for each event; code filters; the 2-second timeout.
- **Data egress.** A judge receives tool input and output. Mitigation:
  only the user file turns judgments on; the state caps; `doctor` names
  the judge.
- **Calibration drift.** A new judge version moves answers. Mitigation:
  records name the answering version; the gate report runs again when a
  pinned id changes.
- **Scope creep.** Nineteen numbered steps and three lettered steps
  define the scope. A feature outside the build order waits for the M4
  plan.

## Close-out

Run these at the end of the milestone, before the M4 plan lands:

1. Remove each capability flag without a consumer, in one pull request.
2. The alignment review over the whole repository, two lenses:
   architecture against [`plan.md`](plan.md), and simplification. Fold
   the findings into the plan docs and the vault notes, not a standalone
   review file. `m3-closeout.md` records the evidence and the contract
   inventory, as [`m2.5-closeout.md`](m2.5-closeout.md) does.
3. The metrics receipts, one per reference machine, compared with the
   M2.5 receipts from the same machine. The Linux and Mac warm-boot and
   idle receipts, within 10% of M2.5, under the configuration exit
   criterion 10 names, with one confirmation run on a crossing. A
   machine that is unreachable lands its receipt as a follow-up commit.
4. The dependency list checked against the table above, with every
   size receipt linked.
5. The next-session handoff rewritten for the M4 plan.
