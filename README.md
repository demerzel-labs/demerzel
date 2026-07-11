# Demerzel

Demerzel is Daniel's personal Python agent harness. Its current baseline is an
independent automated code-review loop migrated from a personal prototype on
2026-07-10. It provides an observable, budgeted, routed agent loop on Amazon Bedrock.

This migration renamed the repository, Python package, prompt identity, environment
variables, scripts, examples, and documentation. It did not add an interactive session
or generic prompt command. The current interface remains the caller-mode code-review
contract below. The next planning phase will define the personal harness invocation
model and capability expansion.

The imported implementation record lives in [`PLAN.md`](PLAN.md). Historical
integration notes remain archived under [`docs/`](docs/index.md).

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

See [`PLAN.md`](PLAN.md) §1 for the current flag, exit-code, and environment-variable
contract.

Live runs need personal AWS credentials and a monthly budget named
`demerzel-bedrock-monthly` (or `DEMERZEL_BUDGET_NAME`). A failed budget lookup logs a
warning and proceeds with the per-run `DEMERZEL_MAX_COST` ceiling still active.
