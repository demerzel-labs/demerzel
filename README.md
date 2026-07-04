# Demerzel

Demerzel drops governed AI agents into the CI pipelines you already have — running in
your cloud, on your model contracts, with audit-grade control over behavior, quality,
and cost. It is a pipeline-first agent harness: code review first, CI triage second,
built as a private prototype with an external-shaped design. Private preview.

This repository is at **Sprint 0.1 — Config core**: the uv project skeleton, the
pydantic configuration schema, the layered loader/merger
(`defaults → org → team → repo → directory → cli`), and `demerzel config show`. No
model calls, no cloud code, no network — pure config resolution.

## Install

```bash
uv sync
```

## Demo

Resolve the effective configuration for the bundled example repo, layering an org
config and its `extends`-linked team config:

```bash
uv run demerzel config show --repo-root examples/repo --org-config examples/org/demerzel.yml
```

Add `--set dotted.path=value` (repeatable) to inject a CLI override layer, or
`--format json` for machine-readable output.

## Development

```bash
uv run pytest        # unit + integration tests
uv run ruff check .  # lint
uv run ruff format . # format
```

## Docs

Architecture, roadmap, and design rationale live in `PLAN.md`. Deeper `docs/` content
lands in later sprints.
