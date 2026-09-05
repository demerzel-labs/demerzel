# Contributing

Demerzel is a pre-alpha project. Small, focused pull requests are
easiest to review.

## Before you start

1. Read [`README.md`](../README.md).
2. Read [`plan.md`](plan.md).
3. Read the active milestone plan.
4. Check [`config.md`](config.md) before you change config behavior.
5. Read [`AGENTS.md`](../AGENTS.md) for the full repository rules.

Each plan step is one pull request. Create the branch from `main`.
Use the name `agent/<step-name>`.

## AI assistance

AI assistance is welcome. You are responsible for each change that you submit.
Understand the behavior, risks, and tradeoffs before you submit work.

Review the full diff. Verify generated code and claims against source
material and actual outcomes.

Model approval and passing CI do not replace review.

## Local checks

Run these before you open a pull request:

```bash
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo test --workspace
```

Run benchmark scripts only when the change can affect startup,
memory, render cost, or harness overhead. See
[`bench/README.md`](../bench/README.md).

## Evidence

Give accurate, reproducible verification in the pull request.
Include commands, observed outcomes, and relevant environment.
Clearly list checks that you did not run.
State each limit that affects the result.
Run checks against the submitted version.
Repeat affected checks after fixes.

For a behavior fix, include failure reproduction and regression coverage.
For a performance claim, include benchmark evidence.

Tests must exercise public contracts.
Do not change expected behavior solely to make a test pass.
Do not weaken checks only to make CI pass.

## Documentation

Keep public docs in `docs/`, except for the root `README.md` and
directory-specific `README.md` files.

Use short, direct sentences. Use active voice. Keep one instruction
in each sentence. Define a project term the first time you use it.

When a milestone plan is complete, keep its receipts, dates, and
decisions unless they are wrong or unsafe to publish. Label stale
implementation notes as historical when that keeps the context clear.

## Clean pull requests

Keep each pull request focused.
Do not mix unrelated refactors with the requested change.
Do not paste agent transcripts or unreviewed summaries into the pull request.
Explain material tradeoffs in the pull request body.
Follow the review rules in [`AGENTS.md`](../AGENTS.md) when code changes.

## Pull request body

Use these sections:

- Summary
- Decisions
- Verify
- Review
- Contract growth

Under `Contract growth`, name each change to provider traits,
engine events, tool names and schemas, config keys, session files,
CLI flags, or CLI subcommands. Write `Contract growth: none` when
there is no change.

## Safety

Do not put credentials, tokens, private endpoints, hostnames,
personal paths, or personal identifiers in a pull request.

Session files and debug logs can contain prompts, tool output, file
contents, and model text. Review them before you share them in an
issue or a pull request.

The approval mode is not an operating-system sandbox. It controls
which tools Demerzel offers and when it asks before a tool runs.
