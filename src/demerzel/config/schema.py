"""Pydantic v2 schemas for Demerzel configuration (Sprint 0.1).

Design decisions baked into these models (made by the orchestrator; enforced or
merely carried per the notes below):

* **Severity scale** — ``info < warning < error`` (see :data:`SEVERITY_ORDER`
  and :func:`severity_at_least`). ``never`` is a *threshold-only* sentinel
  (report nothing / never fail); it is NOT a valid rule severity. Hence rule
  ``severity`` uses :class:`Severity` while thresholds use :data:`Threshold`.
* **Categories remap grammar** — ``review.behavior.categories`` maps a category
  name to a :data:`CategoryAction`: ``ignore`` drops that category's findings;
  a severity name (``info``/``warning``/``error``) remaps them to that severity.
  Categories deep-merge across layers (map semantics).
* **extra="forbid" everywhere** — unknown keys are hard errors so config drift
  surfaces in CI, not in silently-ignored behavior. Every field is defaulted so
  partial layer files validate on their own before merging.
* **Governance keys are parsed, not enforced** — ``locked``/``bounds``/``teams``
  and ``models.allowed`` are validated for *type* only and carried through to the
  effective config. Enforcement (locked-key rejection, budget bounds, allowed-model
  restriction) lands in Sprint 2.2; dotted-path existence validation for
  locked/bounds keys is deliberately deferred to 2.2 as well.
* **Model URI strings** (e.g. ``bedrock:us.anthropic.claude-sonnet-4-5``) are plain
  strings in 0.1 — URI parsing arrives in Sprint 0.2.
"""

from __future__ import annotations

from enum import StrEnum
from typing import Literal

from pydantic import BaseModel, ConfigDict, Field, model_validator


class Severity(StrEnum):
    """Rule/finding severity. Ordered ``info < warning < error``.

    ``never`` is intentionally absent — it is a threshold sentinel only
    (see :data:`Threshold`), never a valid rule severity.
    """

    info = "info"
    warning = "warning"
    error = "error"


#: Total order over :class:`Severity`, low to high. Used by threshold comparisons
#: in later sprints (report/fail gates); defined here so the scale lives with the enum.
SEVERITY_ORDER: dict[str, int] = {
    Severity.info.value: 0,
    Severity.warning.value: 1,
    Severity.error.value: 2,
}


def severity_at_least(value: str, threshold: str) -> bool:
    """Return True if ``value`` is at or above ``threshold`` on the ``info <
    warning < error`` scale.

    ``threshold`` may be the ``never`` sentinel, in which case nothing ever
    meets it (returns False) — this is how ``report_threshold``/``fail_threshold``
    of ``never`` mean "report nothing" / "never fail". ``value`` must be a real
    severity, never ``never``.
    """

    if threshold == "never":
        return False
    return SEVERITY_ORDER[value] >= SEVERITY_ORDER[threshold]


#: Threshold values for ``report_threshold`` / ``fail_threshold``. Adds the
#: ``never`` sentinel to the severity scale (report nothing / never fail).
Threshold = Literal["info", "warning", "error", "never"]

#: A per-category action in ``review.behavior.categories``: ``ignore`` drops the
#: category's findings; a severity name remaps them to that severity.
CategoryAction = Literal["ignore", "info", "warning", "error"]


class _Strict(BaseModel):
    """Base model: forbids unknown keys so drift is a hard error."""

    model_config = ConfigDict(extra="forbid")


class OutputConfig(_Strict):
    """``review.output`` — how findings are rendered.

    Reused (partially) inside ``overrides[*].output``; defaults let a partial
    override carry only the keys it sets.
    """

    format: Literal["markdown", "json", "sarif"] = "markdown"
    template: str = "default"
    sections: list[str] = Field(default_factory=lambda: ["summary", "findings"])


class BehaviorConfig(_Strict):
    """``review.behavior`` — reporting/gating knobs.

    Reused (partially) inside ``overrides[*].behavior``. ``categories`` maps a
    category name to a :data:`CategoryAction` (deep-merged across layers).
    """

    focus: list[str] = Field(default_factory=list)
    ignore_paths: list[str] = Field(default_factory=list)
    report_threshold: Threshold = "info"
    fail_threshold: Threshold = "error"
    categories: dict[str, CategoryAction] = Field(default_factory=dict)
    max_findings: int = 20


class ModelsConfig(_Strict):
    """``models`` for repo/team layers: tier routing per stage and tier->URI map.

    ``routing`` keys are pipeline stages (``plan``/``review``/``verify``); values
    are tiers (``fast``/``standard``/``deep``). ``tiers`` maps a tier to a concrete
    model URI string. Both maps deep-merge across layers.
    """

    routing: dict[Literal["plan", "review", "verify"], Literal["fast", "standard", "deep"]] = Field(
        default_factory=dict
    )
    tiers: dict[Literal["fast", "standard", "deep"], str] = Field(default_factory=dict)


class OrgModelsConfig(ModelsConfig):
    """``models`` for org/effective layers: adds the governance ``allowed`` list.

    ``allowed`` restricts which model URIs tier remaps may use. Parsed and carried
    through in 0.1; the restriction is ENFORCED in Sprint 2.2.
    """

    allowed: list[str] = Field(default_factory=list)


class RuleConfig(_Strict):
    """A named, additive review rule. Findings cite the rule ``id``.

    ``id`` and ``instruction`` are required; ``severity`` defaults to ``warning``
    and cannot be ``never`` (that is a threshold sentinel, not a rule severity).
    """

    id: str
    paths: list[str] = Field(default_factory=list)
    severity: Severity = Severity.warning
    instruction: str


class OverrideEntry(_Strict):
    """A path-scoped override (ESLint-``overrides`` style).

    ``behavior``/``output``/``models``/``tone`` sit *directly* under the entry
    (not under a nested ``review:``). ``paths`` is required and non-empty. All
    other fields are optional and, in the effective-config dump, are serialized
    ``exclude_unset`` so an override appears exactly as authored — defaults are
    never materialized inside an override entry.

    Path-scoped *application* (resolving a file's effective config) is Sprint 3.1;
    here entries are only parsed, merged (APPEND across layers), and round-tripped.
    """

    paths: list[str] = Field(min_length=1)
    tone: str | None = None
    output: OutputConfig | None = None
    behavior: BehaviorConfig | None = None
    models: ModelsConfig | None = None


class ReviewConfig(_Strict):
    """The ``review`` block. ``tone`` is free text; ``concise``/``friendly``/
    ``formal`` are documented presets but any string is accepted."""

    tone: str = "concise"
    output: OutputConfig = Field(default_factory=OutputConfig)
    behavior: BehaviorConfig = Field(default_factory=BehaviorConfig)
    rules: list[RuleConfig] = Field(default_factory=list)
    overrides: list[OverrideEntry] = Field(default_factory=list)


class BudgetConfig(_Strict):
    """``budget`` — hard per-run cost cap. ``max_cost_usd`` default is provisional
    until the Sprint 1.2 cost meter exists."""

    max_cost_usd: float = 1.00


class BedrockProvider(_Strict):
    """AWS Bedrock provider settings. ``assume_role`` auth requires
    ``assume_role_arn`` (validated here); the assume-role flow itself is Sprint 0.2."""

    region: str = "us-east-1"
    auth: Literal["ambient", "assume_role"] = "ambient"
    assume_role_arn: str | None = None

    @model_validator(mode="after")
    def _require_arn_for_assume_role(self) -> BedrockProvider:
        if self.auth == "assume_role" and not self.assume_role_arn:
            raise ValueError("bedrock auth 'assume_role' requires 'assume_role_arn'")
        return self


class AnthropicProvider(_Strict):
    """Anthropic API-key provider settings."""

    auth: Literal["api_key"] = "api_key"
    api_key_env: str | None = None


class OpenAIProvider(_Strict):
    """OpenAI API-key provider settings."""

    auth: Literal["api_key"] = "api_key"
    api_key_env: str | None = None


class LitellmProvider(_Strict):
    """LiteLLM bridge provider settings (Azure OpenAI / Vertex / self-hosted)."""

    api_key_env: str | None = None


class ProvidersConfig(_Strict):
    """``providers`` — provider selection and per-provider auth settings."""

    default: str = "bedrock"
    bedrock: BedrockProvider = Field(default_factory=BedrockProvider)
    anthropic: AnthropicProvider | None = None
    openai: OpenAIProvider | None = None
    litellm: LitellmProvider | None = None


class BoundSpec(_Strict):
    """A governance bound: lower layers may move a value within ``[min, max]``.

    Type-only in 0.1; enforcement (e.g. "budgets may lower, never raise") is
    Sprint 2.2. Dotted-path existence of the bounded key is not validated in 0.1.
    """

    min: float | None = None
    max: float | None = None


class TeamMapping(_Strict):
    """An org ``teams`` entry mapping repo patterns to a team config.

    Parsed and carried through in 0.1; the pattern->team-config resolution for
    unmigrated repos is enforced in Sprint 2.2.
    """

    match: list[str] = Field(default_factory=list)
    extends: str


class RepoConfig(_Strict):
    """Schema for repo, team, directory, CLI, and harness-default layers
    (the ``.demerzel.yml`` shape).

    ``extends`` opts a repo into a single team layer; team configs that themselves
    declare ``extends`` are rejected by the loader (no ``extends`` chaining). This
    schema carries NO governance keys — ``locked``/``bounds``/``teams``/
    ``models.allowed`` are org-only, so setting them at a repo/team layer is an
    unknown-key error.
    """

    version: Literal[1] = 1
    extends: str | None = None
    review: ReviewConfig = Field(default_factory=ReviewConfig)
    models: ModelsConfig = Field(default_factory=ModelsConfig)
    budget: BudgetConfig = Field(default_factory=BudgetConfig)
    providers: ProvidersConfig = Field(default_factory=ProvidersConfig)


class OrgConfig(_Strict):
    """Schema for the org layer: the regular body PLUS governance keys.

    The org layer contributes values (``review``/``models``/``budget``/
    ``providers``) like any other layer AND declares governance. Org configs have
    NO ``extends`` (setting one is an unknown-key error). Governance keys are
    type-validated and carried through; enforcement is Sprint 2.2.
    """

    version: Literal[1] = 1
    review: ReviewConfig = Field(default_factory=ReviewConfig)
    models: OrgModelsConfig = Field(default_factory=OrgModelsConfig)
    budget: BudgetConfig = Field(default_factory=BudgetConfig)
    providers: ProvidersConfig = Field(default_factory=ProvidersConfig)
    locked: list[str] = Field(default_factory=list)
    bounds: dict[str, BoundSpec] = Field(default_factory=dict)
    teams: list[TeamMapping] = Field(default_factory=list)


class EffectiveConfig(OrgConfig):
    """The fully-merged, validated configuration produced by the loader.

    Identical in shape to :class:`OrgConfig` (the value body plus carried-through
    governance) with all defaults materialized. Round-trips to YAML/JSON for
    ``demerzel config show``; use :func:`demerzel.config.loader.effective_to_dict`
    to serialize so override entries dump as authored.
    """
