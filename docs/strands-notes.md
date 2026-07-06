# Strands API notes (validation spike)

Pinned version: **strands-agents 1.45.0** (Python 3.11 venv; supports 3.10+).
Recorded 2026-07-04 from `scripts/_spike_strands*.py` (since deleted); thinking/effort
wire shapes verified against the Anthropic + AWS Bedrock docs 2026-07-05 (see
"Thinking & effort" below). Update this file on every `strands-agents` bump — it is
the contract the runner (`runner/strands.py`) depends on, so an upgrade that changes
any item below is a reviewed diff, not a surprise.

## Verdict

Every load-bearing assumption in PLAN.md sec 4 holds, and several things the plan
expected to hand-roll are native. Net: the runner gets *smaller*.

## Confirmed API surface

### Model + config
- `from strands.models import BedrockModel`
- `BedrockModel(*, boto_session=None, boto_client_config=None, region_name=None,
  endpoint_url=None, **BedrockConfig)`.
- `boto_session` accepts a custom `boto3.Session` → the assume-role flow works
  without hand-rolling boto3. `boto_client_config` (botocore `Config`) → adaptive
  retries + connect/read timeouts.
- `BedrockConfig` keys we use: `model_id`, `max_tokens`, `additional_request_fields`
  (→ `output_config.effort` and the thinking config go here; maps verbatim to Converse
  `additionalModelRequestFields`), `cache_config` (**native prompt caching** — no
  manual cache-point splicing). NOTE: `cache_prompt` is **deprecated in 1.45.0**
  (warns per run); we use `CacheConfig(strategy="auto")`, which auto-injects a cache
  point at the last user message for Anthropic models (conversation-prefix caching,
  5-min TTL default). `temperature`/`top_p` exist but the harness never sets them —
  strands only sends configured keys, and Opus 4.7/4.8, Sonnet 5, and Fable/Mythos 5
  reject non-default temperature/top_p/top_k with a 400.

### Agent + invocation
- `from strands import Agent`
- `Agent(model=<Model|str>, tools=[...], system_prompt=..., hooks=[...],
  conversation_manager=..., retry_strategy=..., ...)`.
- Sync call: `result = agent(prompt, *, limits=Limits(...))`.
  Async: `invoke_async`, `stream_async`.
- `Model` is injectable — a scripted subclass implementing `stream()` (async gen of
  `StreamEvent`), `structured_output`, `update_config`, `get_config` is all a
  hermetic loop test needs. **This is how we test the runner with no AWS.**

### Turn / token governors — NATIVE
- `from strands.types.agent import Limits` → `Limits(turns=int, output_tokens=int,
  total_tokens=int)`, passed per call: `agent(prompt, limits=Limits(turns=n))`.
- Checked at the top of each loop iteration; tools from the previous turn finish
  before a cap fires and `agent.messages` stays reinvokable.
- `stopReason` includes `limit_turns`, `limit_output_tokens`, `limit_total_tokens`.
- **Mapping:** `compute_max_turns()` (workload scaling) → `Limits(turns=...)`.
  Replaces the plan's "feed the event-loop cycle limit" hand-waving.

### Retry / throttling — NATIVE
- `from strands import ModelRetryStrategy`
- `ModelRetryStrategy(max_attempts=6, initial_delay=4, max_delay=240)` — exponential
  backoff on `ModelThrottledException`. Pass via `Agent(retry_strategy=...)`.
- Retries surface on `AfterModelCallEvent.retry` (bool) for tracing.
- **Mapping:** replaces the plan's "explicit retry layer with backoff+jitter." We
  configure it and trace it; we don't implement it.

### Usage incl. cache tokens — NATIVE
- `Usage` TypedDict: `inputTokens`, `outputTokens`, `totalTokens`,
  `cacheReadInputTokens`, `cacheWriteInputTokens`.
- Per-call: `MetadataEvent.usage` in the stream.
- Totals: `result.metrics` is `EventLoopMetrics` with `accumulated_usage`,
  `cycle_count`, `cycle_durations`, `tool_metrics`, `accumulated_metrics` (latency).
- **Mapping:** `CostMeter` reads per-call usage in an `AfterModelCallEvent` hook and
  reconciles totals from `result.metrics` at the end. `cacheReadInputTokens` proves
  caching hit (PLAN sec 4).

### Hooks — NATIVE, this is the observability + governor spine
- `from strands.hooks import (BeforeModelCallEvent, AfterModelCallEvent,
  BeforeToolCallEvent, AfterToolCallEvent, MessageAddedEvent, HookProvider, ...)`
- Register: `agent.add_hook(provider)` / `Agent(hooks=[...])` /
  `registry.add_callback(EventType, cb, order=0.0)`.
- Event fields that matter:
  - `AfterModelCallEvent`: `agent, stop_response, exception, retry` → feed CostMeter,
    trace each turn, detect retries.
  - `BeforeModelCallEvent`: `agent, projected_input_tokens, cancel (bool|str)` →
    cooperative timeout / hard cost-cap abort point.
  - `BeforeToolCallEvent`: `agent, selected_tool, tool_use, cancel_tool (bool|str)`.
  - `AfterToolCallEvent`: `agent, tool_use, result, exception` → trace tool calls.

### Cost-ceiling governor mapping (the "produce final review NOW" behavior)
Native idiom, no custom loop:
- Track cumulative cost in an `AfterModelCallEvent` hook.
- **Soft stop** (crossed ceiling): set `BeforeToolCallEvent.cancel_tool =
  "COST BUDGET REACHED - produce your final review now with what you have."` The
  string becomes the tool result the model receives, so it stops calling tools and
  writes its final answer. Fires once.
- **Hard stop** (past grace factor): set `BeforeModelCallEvent.cancel = True` to abort
  the next model call; extract best-effort final text from `agent.messages`.
- **Wall-clock `timeout_s`**: a time governor hook checks elapsed on
  `Before{Model,Tool}CallEvent` and cancels at the next boundary (cooperative,
  same as `Limits`); botocore read-timeout guards a single stuck call.

### Telemetry (optional OTel)
- `from strands.telemetry import StrandsTelemetry` →
  `setup_otlp_exporter()` / `setup_console_exporter()` / `setup_meter()`.
- Enable only when `OTEL_EXPORTER_OTLP_ENDPOINT` is set — off by default, no egress.

### Tools
- `from strands import tool` → `@tool` decorator wraps our hardened bash function
  (`name`, `description`, `inputSchema`). We keep our own gating (sec 9); we do NOT
  use Strands' built-in shell.
- Tool result type: `ToolResult{content: [{text|json|...}], status: 'success'|'error',
  toolUseId}`.

### Structured output (used by the quality gate)
- `Agent(structured_output_model=PydanticModel)` / `__call__(structured_output_model=)`;
  `result.structured_output`. Used by the post-review quality gate
  (`StrandsRunner.run_structured` -> `gate.py` GateResult); the main review output
  stays markdown.

## Not adopted (with reason)
- **`strands.Sandbox` / `PosixShellSandbox`**: Strands ships a generic shell sandbox
  (`execute(command, timeout, cwd, env)`). We keep our own explicit allowlist/deny/
  confinement bash tool (sec 9) because the security gates must be reviewable in our
  code, not delegated to a framework abstraction. Revisit `PosixShellSandbox` only if
  we want it as an OS-level confinement tier (it needs `execute_streaming`).
- **`conversation_manager` compression/summarizing**: default (null/sliding) is fine;
  context management for review runs is bounded by diff size + turn caps already.

## Thinking & effort (verified against Anthropic/AWS docs 2026-07-05)

Previously "open"; now confirmed against the AWS Bedrock adaptive/extended-thinking
pages and the Anthropic effort/adaptive-thinking docs. `_model_kwargs` implements this
mapping; `_ADAPTIVE_THINKING_MODELS` in `runner/strands.py` is the model split.

- **Effort**: `{"output_config": {"effort": "low|medium|high|xhigh|max"}}` inside
  `additionalModelRequestFields`. It must be a *separate* `output_config` object —
  effort inside `thinking` is a ValidationException. Supported on Fable/Mythos 5,
  Opus 4.8/4.7/4.6/4.5, Sonnet 5/4.6; **not Haiku 4.5** (harness skips it there).
  `high` is the API default; effort works with or without a thinking block and also
  damps tool-call spend.
- **Thinking, adaptive models** (Opus 4.8/4.7/4.6, Sonnet 5/4.6, Fable/Mythos 5):
  `{"thinking": {"type": "adaptive"}}`, depth controlled by effort. Manual
  `{"type": "enabled", "budget_tokens": N}` is a **400** on Opus 4.8/4.7, Sonnet 5,
  Fable/Mythos 5, and deprecated on Opus/Sonnet 4.6. The harness maps the config knob
  (`inference.thinking.enabled`) to adaptive on these models and drops `budget_tokens`.
- **Thinking, legacy manual models** (Haiku 4.5, Opus 4.5, Sonnet 4.5, older):
  `{"type": "enabled", "budget_tokens": N}` with `budget_tokens < max_tokens` required.
  Without a budget the harness skips the block rather than send an invalid request.
- **max_tokens counts thinking + response** on adaptive models (Opus 4.8 supports up
  to 128k output); docs recommend generous budgets when thinking is on — routing.json
  sets review at 32k.
- **Model IDs verified real** (AWS model cards): `us.anthropic.claude-opus-4-8` (US
  geo inference ID), `us.anthropic.claude-sonnet-4-6`,
  `us.anthropic.claude-haiku-4-5-20251001-v1:0`.
- **Pricing verified** (AWS Marketplace / Anthropic announcements, 2026-07): Opus 4.8
  $5/$25 per MTok, Sonnet 4.6 $3/$15, Haiku 4.5 $1/$5, cache read ~0.1x input — all
  matching `pricing.py`.
- The gate's structured-output call strips the thinking config entirely
  (`run_structured`) — it is a single cheap hygiene pass.

## Open (needs live Bedrock, not resolvable offline)
- Per-account enablement of the routing.json model IDs in the sandbox (note: newly
  launched models can show AUTHORIZED/AVAILABLE while runtime TPM quota is still
  unprovisioned — an AccessDeniedException at runtime despite entitlement means an AWS
  support ticket, not an IAM bug). Covered by `smoke_bedrock.sh`.
- End-to-end acceptance of the assembled request shapes against Bedrock
  (`smoke_bedrock.sh` — the shapes are doc-verified above, the smoke is the proof).
