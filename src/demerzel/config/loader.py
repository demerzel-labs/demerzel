"""The layered config loader: build the chain, validate each layer, merge, resolve.

Pipeline for :func:`load_effective_config`:

1. Assemble the frozen layer chain ``defaults -> org -> team -> repo -> directory
   -> cli`` (skipping absent optional layers).
2. Validate each layer's raw dict against its schema with ``extra="forbid"`` BEFORE
   merging, raising a single :class:`ConfigError` that names the source file (or
   ``<cli>``) and the full dotted key path on the first violation.
3. Fold the raw dicts per the fixed merge policy (see :mod:`.merge`).
4. Validate the merged dict into :class:`EffectiveConfig` — the one place defaults
   are materialized — and return it with the participating layers.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import yaml
from pydantic import BaseModel, ValidationError

from .defaults import default_layer_raw
from .errors import ConfigError
from .fetch import fetch
from .merge import Layer, directory_layers, fold_layers
from .schema import EffectiveConfig, OrgConfig, RepoConfig

REPO_CONFIG_FILENAME = ".demerzel.yml"
CLI_SOURCE = "<cli>"


def _loc_to_dotted(loc: tuple[Any, ...]) -> str:
    """Translate a pydantic error ``loc`` tuple to a dotted key path.

    Integer elements are list indices attached to the preceding key, e.g.
    ``("review", "rules", 0, "severity")`` -> ``review.rules[0].severity``.
    """

    parts: list[str] = []
    for item in loc:
        if isinstance(item, int):
            if parts:
                parts[-1] = f"{parts[-1]}[{item}]"
            else:
                parts.append(f"[{item}]")
        else:
            parts.append(str(item))
    return ".".join(parts)


def _raise_from_validation(exc: ValidationError, source: str) -> None:
    """Convert a pydantic :class:`ValidationError` into a :class:`ConfigError`
    naming ``source`` and the first offending dotted key."""

    error = exc.errors()[0]
    key = _loc_to_dotted(tuple(error["loc"]))
    if error["type"] == "extra_forbidden":
        raise ConfigError(
            f"Unknown config key {key!r} in {source}", source=source, key=key
        ) from exc
    message = error.get("msg", "invalid value")
    raise ConfigError(
        f"Invalid config value for {key!r} in {source}: {message}", source=source, key=key
    ) from exc


def _validate_layer(raw: dict[str, Any], model: type[BaseModel], source: str) -> None:
    """Validate a layer's raw dict against ``model`` (extra=forbid) as a gate.

    The validated model is discarded — merging uses the raw dict to preserve
    "explicitly set" semantics. Only the validation side effect (error raising)
    matters here.
    """

    try:
        model.model_validate(raw)
    except ValidationError as exc:
        _raise_from_validation(exc, source)


def _parse_yaml(text: str, source: str) -> dict[str, Any]:
    """Parse YAML text into a mapping, or raise :class:`ConfigError`."""

    try:
        data = yaml.safe_load(text)
    except yaml.YAMLError as exc:
        raise ConfigError(f"invalid YAML in {source}: {exc}", source=source) from exc
    if data is None:
        return {}
    if not isinstance(data, dict):
        raise ConfigError(
            f"config in {source} must be a mapping, got {type(data).__name__}", source=source
        )
    return data


def _set_dotted(root: dict[str, Any], keys: list[str], value: Any) -> None:
    """Set ``value`` at the nested ``keys`` path in ``root``, creating dicts."""

    node = root
    for key in keys[:-1]:
        existing = node.get(key)
        if not isinstance(existing, dict):
            existing = {}
            node[key] = existing
        node = existing
    node[keys[-1]] = value


def build_cli_dict(overrides: list[str]) -> dict[str, Any]:
    """Build the CLI layer's raw dict from repeatable ``dotted.path=value`` pairs.

    Each value is parsed with ``yaml.safe_load`` so ``true``, ``5``, ``0.5``,
    ``[a, b]`` (flow list) and quoted strings all work. A pair missing ``=`` or
    with an empty key is a usage error (:class:`ConfigError`, exit 2). The result
    is merged and validated like any other layer, so unknown keys error naming
    ``<cli>``.
    """

    root: dict[str, Any] = {}
    for pair in overrides:
        if "=" not in pair:
            raise ConfigError(
                f"invalid --set override (expected KEY=VALUE): {pair!r}", source=CLI_SOURCE
            )
        key, _, value_str = pair.partition("=")
        if not key:
            raise ConfigError(f"invalid --set override (empty key): {pair!r}", source=CLI_SOURCE)
        value = yaml.safe_load(value_str)
        _set_dotted(root, key.split("."), value)
    return root


def _load_team(location: str, base_dir: Path) -> Layer:
    """Load a team config referenced by a repo's ``extends:``.

    Team configs use the repo schema. A team config that itself declares
    ``extends`` is rejected — ``extends`` chaining is not supported.
    """

    result = fetch(location, base_dir=base_dir)
    raw = _parse_yaml(result.text, result.source)
    _validate_layer(raw, RepoConfig, result.source)
    if raw.get("extends") is not None:
        raise ConfigError(
            f"extends chaining is not supported: team config {result.source} declares 'extends'",
            source=result.source,
            key="extends",
        )
    raw.pop("extends", None)
    return Layer("team", result.source, raw)


def build_layers(
    repo_root: Path,
    org_config: str | None,
    cli_overrides: list[str],
) -> list[Layer]:
    """Assemble and validate the frozen layer chain (without merging).

    ``org_config`` is a location string resolved relative to CWD when relative;
    a repo's ``extends:`` resolves relative to the repo file's directory.
    """

    layers: list[Layer] = [Layer("defaults", None, default_layer_raw())]

    if org_config:
        result = fetch(org_config, base_dir=Path.cwd())
        raw = _parse_yaml(result.text, result.source)
        _validate_layer(raw, OrgConfig, result.source)
        layers.append(Layer("org", result.source, raw))

    repo_file = repo_root / REPO_CONFIG_FILENAME
    if repo_file.is_file():
        raw = _parse_yaml(repo_file.read_text(), str(repo_file))
        _validate_layer(raw, RepoConfig, str(repo_file))
        extends = raw.pop("extends", None)
        if extends is not None:
            layers.append(_load_team(extends, base_dir=repo_file.parent))
        layers.append(Layer("repo", str(repo_file), raw))

    layers.extend(directory_layers(repo_root, repo_root))

    if cli_overrides:
        cli_raw = build_cli_dict(cli_overrides)
        if "extends" in cli_raw:
            raise ConfigError(
                "'extends' cannot be set from the CLI; declare it in the repo config",
                source=CLI_SOURCE,
                key="extends",
            )
        _validate_layer(cli_raw, RepoConfig, CLI_SOURCE)
        count = len(cli_overrides)
        layers.append(Layer("cli", f"{count} override{'s' if count != 1 else ''}", cli_raw))

    return layers


def resolve(layers: list[Layer]) -> EffectiveConfig:
    """Fold ``layers`` and validate the merged dict into :class:`EffectiveConfig`.

    A merge that produces an invalid combination (e.g. a type conflict only visible
    once layers combine) raises :class:`ConfigError` attributed to ``<merged>``.
    """

    merged = fold_layers(layers)
    try:
        return EffectiveConfig.model_validate(merged)
    except ValidationError as exc:
        _raise_from_validation(exc, "<merged>")


def load_effective_config(
    repo_root: Path,
    org_config: str | None = None,
    cli_overrides: list[str] | None = None,
) -> tuple[EffectiveConfig, list[Layer]]:
    """Load, validate, and merge the full layer chain.

    Returns the fully-resolved :class:`EffectiveConfig` and the ordered list of
    participating :class:`Layer` objects (for the ``config show`` header).
    """

    layers = build_layers(repo_root, org_config, cli_overrides or [])
    return resolve(layers), layers


def layer_header(layers: list[Layer]) -> str:
    """Render the ``# layers: a -> b -> c`` header line for ``config show``."""

    return "# layers: " + " -> ".join(layer.label() for layer in layers)


def effective_to_dict(config: EffectiveConfig) -> dict[str, Any]:
    """Serialize the effective config to a plain dict for YAML/JSON output.

    Defaults are materialized (fully-resolved config) EXCEPT inside
    ``review.overrides`` entries, which are dumped ``exclude_unset`` so each
    override appears exactly as authored rather than with defaults filled in.
    """

    data = config.model_dump(mode="json")
    data["review"]["overrides"] = [
        entry.model_dump(mode="json", exclude_unset=True) for entry in config.review.overrides
    ]
    return data
