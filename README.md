# Prototype Harness

Observable, budgeted, routed Bedrock agent loop for automated code review. This is an
independent Python CI review-harness prototype with a small caller-mode interface.

Beyond talking to the LLM, the harness provides three things: **observability**,
**budget control**, and **intelligent routing**. Full design and rationale live in
[`PLAN.md`](PLAN.md); archived integration notes live in [`docs/HANDOFF.md`](docs/HANDOFF.md).

## Install

```bash
uv sync
```

## Test

The whole suite is hermetic — no AWS, no network:

```bash
uv run pytest          # unit + loop + CLI e2e
uv run ruff check .    # lint
uv run ruff format .   # format
```

## Invocation (compatibility contract)

```bash
python3 -m prototype.harness.bedrock \
    --config  agents/configs/prototype-review.json \
    --prompt-file /tmp/prompt.md \
    --output  /tmp/review.md \
    --trace   /tmp/trace.jsonl \
    --base-dir . \
    --repo    acme/core \
    --diff-size 12000 \
    --system-prompt-file /tmp/system_prompt.md
# exit 0 = ok, 1 = fatal error, 2 = monthly budget exceeded
```

See [`PLAN.md`](PLAN.md) §1 for the frozen flag/exit-code/env-var contract.
