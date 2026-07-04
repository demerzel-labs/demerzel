"""The ``demerzel`` CLI (Sprint 0.1).

Only ``demerzel config show`` exists. ``review``/``doctor``/``eval`` arrive in later
sprints and are deliberately NOT stubbed here. Config mistakes never surface as a
traceback: a :class:`ConfigError` is printed as ``error: <message>`` on stderr and
the process exits ``2`` (harness error, per the plan's exit-code contract).
"""

from __future__ import annotations

import json
import sys
from enum import StrEnum
from pathlib import Path

import typer
import yaml

from .config import ConfigError, effective_to_dict, layer_header, load_effective_config

app = typer.Typer(
    name="demerzel",
    help="Governed AI agents for your existing CI. (Sprint 0.1: config core.)",
    no_args_is_help=True,
    add_completion=False,
)
config_app = typer.Typer(help="Inspect layered configuration.", no_args_is_help=True)
app.add_typer(config_app, name="config")


class OutputFormat(StrEnum):
    """Output format for ``config show``."""

    yaml = "yaml"
    json = "json"


@config_app.command("show")
def config_show(
    repo_root: Path = typer.Option(
        Path("."),
        "--repo-root",
        exists=True,
        file_okay=False,
        help="Repo root; repo config is <repo-root>/.demerzel.yml (its absence is fine).",
    ),
    org_config: str | None = typer.Option(
        None, "--org-config", help="Org config location (path or file://; relative to CWD)."
    ),
    set_overrides: list[str] = typer.Option(
        [], "--set", help="Override a dotted.path=value (repeatable; value parsed as YAML)."
    ),
    output_format: OutputFormat = typer.Option(
        OutputFormat.yaml, "--format", help="Output format."
    ),
) -> None:
    """Print the fully-resolved effective configuration.

    In ``yaml`` mode the output is prefixed with a ``# layers: ...`` comment listing
    the layers that participated, in order. ``json`` mode emits pure JSON only.
    """

    try:
        config, layers = load_effective_config(
            repo_root=repo_root, org_config=org_config, cli_overrides=set_overrides
        )
    except ConfigError as exc:
        typer.echo(f"error: {exc}", err=True)
        raise typer.Exit(2) from exc

    data = effective_to_dict(config)
    if output_format is OutputFormat.json:
        typer.echo(json.dumps(data, indent=2))
    else:
        typer.echo(layer_header(layers))
        typer.echo(yaml.safe_dump(data, sort_keys=False, default_flow_style=False).rstrip())


def main() -> None:
    """Console-script entry point wrapper."""

    app()


if __name__ == "__main__":
    sys.exit(app())
