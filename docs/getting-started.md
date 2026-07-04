# Getting started

## Requirements

- Python 3.12+
- [uv](https://docs.astral.sh/uv/)

## Install

```bash
uv sync
```

This installs Demerzel and its dependencies (typer, pydantic, pyyaml, jinja2)
into a project-local virtual environment.

## Run the bundled demo

The repo ships example configs under `examples/`: an org config, a team config
it links to, and a repo config that opts into that team. Resolve the effective
configuration across all of them:

```bash
uv run demerzel config show --repo-root examples/repo --org-config examples/org/demerzel.yml
```

Output starts with a comment naming every layer that participated, in
precedence order, followed by the merged config as YAML:

```yaml
# layers: defaults -> org (.../examples/org/demerzel.yml) -> team (.../examples/org/teams/payments.yml) -> repo (examples/repo/.demerzel.yml)
version: 1
review:
  tone: concise
  output:
    format: markdown
    template: default
    sections:
    - summary
    - findings
    - nitpicks
  behavior:
    focus:
    - security
    - correctness
    ...
```

`tone: concise` and the `nitpicks` section come from the repo layer (see
`examples/repo/.demerzel.yml`); `focus` comes from the team layer it
`extends:`. See [Configuration](configuration.md) for why each value landed
where it did.

## Your own repo

Write a `.demerzel.yml` at the root of your repo:

```yaml
version: 1
review:
  tone: friendly
  behavior:
    focus: [security]
```

Then resolve it — no org config needed, defaults fill in everything else:

```bash
uv run demerzel config show --repo-root .
```

The layer header now reads `defaults -> repo (.demerzel.yml)`, and the merged
output shows `tone: friendly` and `focus: [security]` coming from your file
while every other field (models, budget, providers, `report_threshold`, and so
on) falls back to the harness defaults. That fallback-and-override behavior is
the whole config system — see [Configuration](configuration.md) for the full
layer model and schema.
