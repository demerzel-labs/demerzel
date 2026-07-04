# Demerzel

Demerzel is a Python CLI and library that runs governed AI agents as a step inside
the CI pipelines you already have. It does not host anything and does not
orchestrate — the CI system does that, the same way it orchestrates any other
step. Demerzel runs in your cloud, on your model contracts, and resolves a single
effective configuration before anything else happens.

Three properties drive the design: agents are **owned** (no Demerzel-hosted
service; runs in your tenancy), **governed** (org and team policy applied through
config layers, not left to individual developer preference), and **measurable**
(review quality and cost reported as numbers, with an eval suite to back them —
arriving in later sprints).

## Current status

This repository is at **Sprint 0.1: configuration core**. What exists today:
the pydantic configuration schema, the layered config loader and merger, and
the `demerzel config show` command. There are no model calls, no cloud code,
and no network access anywhere in this sprint — it is pure config resolution.

The review pipeline, model providers, and governance enforcement (locked keys,
budget bounds, allowed-model restriction) arrive in later sprints. Governance
keys are already parsed and carried into the effective config, but nothing
enforces them yet — see [Configuration](configuration.md#org-configuration--governance)
for the details.

## Contents

- [Getting started](getting-started.md) — install Demerzel and run the bundled
  demo.
- [Configuration](configuration.md) — the layer model, merge semantics, and
  full schema reference.
- [CLI reference](cli.md) — every `demerzel config show` option, the `--set`
  grammar, and exit codes.
