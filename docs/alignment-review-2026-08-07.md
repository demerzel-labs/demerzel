# Alignment Review — 2026-08-07

This review checks the plan and the merged code against the four
project goals: performance, minimalism, extensibility, and a harness
that lets frontier models do their best work. Scope: `main` after
PR #12, with steps 1–4 of M1 merged.

Result: the project holds all four goals. Three deliberate
restrictions need review in later milestones. See the watch list.

## Performance

- The bench program stays a product feature. The jcode numbers stay
  the bar. The gates stay: boot under 50 ms, single-session PSS under
  40 MB, stripped binary under 25 MB.
- The code matches the plan. Grep and glob run in-process. The SSE
  parser is hand-rolled. reqwest uses rustls without default features.
- The release profile uses thin LTO, one codegen unit, and strip.
- The stub binary is 340 KB.

## Minimalism

- The workspace holds about 20 source files in five crates.
- A crate splits only when a second consumer exists.
- The mock server is dev-only. It never links into the shipped binary.
- M1 has no TUI dependency. The WASM plugin ABI stays deferred.
- Each dependency passes a cargo-bloat check against the 25 MB gate.

## Extensibility

The seams for later milestones exist in the merged code:

- The provider trait declares capabilities. No adapter is privileged.
  The `Reasoning` block covers OpenAI sealed items and Anthropic
  thinking blocks.
- The `Tool` trait borrows names and descriptions, so runtime-defined
  MCP tools (M3) fit without change.
- `ApprovalRequest` carries the full call input. Per-tool rules and
  path checks can land later without a signature break.
- Config can replace the system prompt completely (decision of
  2026-08-02).

## Model experience

The harness corrects the model instead of blocking it:

- Every tool failure returns to the model as an error message it can
  act on. A stale edit returns the new tag and the renumbered lines.
  An unknown tool returns the available tool names.
- Policy lives in the harness, never in the prompt. The prompt prefix
  stays stable.
- A read-only session drops the write tools. The model never sees a
  tool it cannot use.
- Read tools can leave the project root. Write tools stay inside it.
  Bash runs as a real subprocess with a constructed environment.
- Defaults: bash timeout 120 s with a 600 s model-set maximum, file
  cap 10 MB, truncation always marked.

## Watch list

Three restrictions are deliberate today and need review later:

1. **Bash kills its process group after every call.** A command
   cannot leave a server running. The model cannot start a server in
   one call and test it in the next. Plan a background-task story in
   M2 or M3.
2. **Write confinement has no escape hatch.** Write tools stay inside
   the project root, with no config override. The confinement is a
   consistency guardrail, not a sandbox: bash in `auto` mode can
   write anywhere. Do not present it as a security boundary. Add a
   config override when a real session needs one.
3. **Tool execution is sequential.** Frontier models use parallel
   tool calls heavily. The capability flag exists in the provider
   trait. Revisit after the Layer-2 numbers and the first dogfood
   sessions.
