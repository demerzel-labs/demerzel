"""Demerzel configuration core (Sprint 0.1).

Public surface: the :class:`EffectiveConfig` model, the :func:`load_effective_config`
entry point, the :class:`Layer` chain primitives, and :class:`ConfigError`.
"""

from __future__ import annotations

from .errors import ConfigError
from .loader import (
    build_cli_dict,
    build_layers,
    effective_to_dict,
    layer_header,
    load_effective_config,
    resolve,
)
from .merge import Layer, directory_layers, fold_layers
from .schema import EffectiveConfig, OrgConfig, RepoConfig, Severity

__all__ = [
    "ConfigError",
    "EffectiveConfig",
    "Layer",
    "OrgConfig",
    "RepoConfig",
    "Severity",
    "build_cli_dict",
    "build_layers",
    "directory_layers",
    "effective_to_dict",
    "fold_layers",
    "layer_header",
    "load_effective_config",
    "resolve",
]
