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

## Documentation

Keep public docs in `docs/`, except for the root `README.md` and
directory-specific `README.md` files.

Use short, direct sentences. Use active voice. Keep one instruction
in each sentence. Define a project term the first time you use it.

When a milestone plan is complete, keep its receipts, dates, and
decisions unless they are wrong or unsafe to publish. Label stale
implementation notes as historical when that keeps the context clear.

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

## Commit subjects

Use this format for each new commit subject:

```text
<type>(area): short descriptive title
```

The type and area are required. Use lowercase for both.
Choose the type that describes the change:

| Type | Change |
| --- | --- |
| `feat` | New behavior |
| `fix` | Bug fix |
| `docs` | Documentation |
| `style` | Formatting without a behavior change |
| `refactor` | Code structure without a behavior change |
| `perf` | Performance improvement |
| `test` | Tests |
| `build` | Build system or dependencies |
| `ci` | Continuous integration |
| `chore` | Repository maintenance |
| `revert` | Reversal of an earlier commit |

The area names the affected part of the project, such as `tui`,
`providers`, `docs`, or `repo`.
Write a short title that states the concrete change.
Use the imperative, such as `add`, `fix`, or `remove`.
Do not add a trailing period or AI attribution.

Examples:

```text
feat(tui): add a session picker
fix(providers): handle an expired login
docs(repo): define commit subject conventions
```

Use the same format for pull request titles.
Use the same format for squash and merge commit subjects.
This rule applies to new commits. Leave existing history unchanged.

## Safety

Do not put credentials, tokens, private endpoints, hostnames,
personal paths, or personal identifiers in a pull request.

Session files and debug logs can contain prompts, tool output, file
contents, and model text. Review them before you share them in an
issue or a pull request.

The approval mode is not an operating-system sandbox. It controls
which tools Demerzel offers and when it asks before a tool runs.
