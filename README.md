# Demerzel

Demerzel is a self-hosted Python agent harness for developer workflows and governed
enterprise automation. It puts a stable layer for tools, context, policy, sessions,
budgets, traces, and evaluation around a Strands agent loop, with model connectivity
supplied by the user or operator.

The current implementation is the automated code-review loop migrated from a personal
clean-room prototype on 2026-07-10. It provides an observable, budgeted, routed Strands
loop on Amazon Bedrock. Direct prompt invocation, sessions, and OpenAI, Anthropic, and
local provider adapters are planned and not implemented yet.

The product direction, invocation contract, provider sequence, security rules, and build
order live in [`PLAN.md`](PLAN.md). Its appendix preserves the imported implementation
record so the existing caller-mode contract and code references remain traceable.
Historical integration notes remain archived under [`docs/`](docs/index.md).

## Install

```bash
uv sync
```

## Test

The whole suite is hermetic and uses no AWS or network access:

```bash
uv run pytest          # unit + loop + CLI e2e
uv run ruff check .    # lint
uv run ruff format .   # format
```

## Current caller-mode invocation

```bash
python3 -m demerzel.harness.bedrock \
    --config  examples/agents/configs/demerzel-review.json \
    --prompt-file /tmp/prompt.md \
    --output  /tmp/review.md \
    --trace   /tmp/trace.jsonl \
    --base-dir examples \
    --repo    acme/core \
    --diff-size 12000 \
    --system-prompt-file /tmp/system_prompt.md
# exit 0 = ok, 1 = fatal error, 2 = monthly budget exceeded
```

See [`PLAN.md`](PLAN.md), Appendix A §1, for the current flag, exit-code, and
environment-variable contract.

Live runs need personal AWS credentials and a monthly budget named
`demerzel-bedrock-monthly` (or `DEMERZEL_BUDGET_NAME`). A failed budget lookup logs a
warning and proceeds with the per-run `DEMERZEL_MAX_COST` ceiling still active.
