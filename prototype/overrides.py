"""Override resolution: repo profiles + team layer (PLAN.md sec 7).

INI (`overrides.cfg`, stdlib configparser). Existing repo-profile entries stay valid;
this adds repo globs, profile inheritance, routing pins, team profiles, per-repo
ownership maps, and CODEOWNERS ingestion.

Detection chain for a PR: changed file paths -> team attribution (explicit map first,
then CODEOWNERS) -> team profiles (additive: context + instructions + a bounded model
pin). Teams are the floor; there is no per-individual granularity.

This module decides *what* applies (profile, teams, pins, which files/contexts).
config.py reads the referenced files and assembles the actual system prompt text.
"""

from __future__ import annotations

import configparser
import fnmatch
import os
from dataclasses import dataclass, field
from pathlib import Path

from prototype.routing import Pin

# Most-capable first: when a diff touches multiple teams, the most capable pinned
# model wins. Aliases not listed rank last (a full model id pinned by a team is
# treated as lowest precedence for the "highest tier wins" tiebreak).
DEFAULT_TIER_ORDER = ("fable", "opus", "sonnet", "haiku")

_PROFILE_KEYS = {"system_prompt", "context", "config", "model", "effort", "extends"}
_TEAM_KEYS = {"context", "instructions", "model", "effort"}
_CODEOWNERS_LOCATIONS = ("CODEOWNERS", ".gitlab/CODEOWNERS", "docs/CODEOWNERS")


class OverrideError(ValueError):
    """Malformed overrides.cfg — raised at startup so CI fails before spending tokens."""


@dataclass
class Profile:
    name: str
    system_prompt: str | None = None  # path (rel base_dir); replaces default section
    context: list[str] = field(default_factory=list)  # paths, appended
    config: str | None = None  # path to a full agent-config replacement
    model: str | None = None
    effort: str | None = None
    extends: str | None = None


@dataclass
class Team:
    name: str
    context: list[str] = field(default_factory=list)
    instructions: str | None = None
    model: str | None = None
    effort: str | None = None


@dataclass
class OverrideResolution:
    profile: Profile | None = None
    matched_teams: list[str] = field(default_factory=list)
    teams: list[Team] = field(default_factory=list)
    file_attribution: dict[str, list[str]] = field(default_factory=dict)
    attribution_source: dict[str, str] = field(default_factory=dict)  # file -> map|codeowners
    unowned_files: list[str] = field(default_factory=list)
    config_path: str | None = None
    pin: Pin | None = None
    system_prompt_path: str | None = None
    context_sources: list[str] = field(default_factory=list)
    sources: list[str] = field(default_factory=list)  # every contributing path

    def to_trace(self) -> dict:
        return {
            "profile": self.profile.name if self.profile else None,
            "matched_teams": self.matched_teams,
            "file_attribution": self.file_attribution,
            "attribution_source": self.attribution_source,
            "unowned_files": self.unowned_files,
            "config_path": self.config_path,
            "pin": None
            if self.pin is None
            else {"model": self.pin.model, "source": self.pin.source},
            "sources": self.sources,
        }


def _split_csv(value: str) -> list[str]:
    return [p.strip() for p in value.split(",") if p.strip()]


class OverridesConfig:
    """Parsed overrides.cfg. Structural key validation happens at load; cross-reference
    and file-existence validation happens in `validate(base_dir)`."""

    def __init__(self):
        self.profiles: dict[str, Profile] = {}
        # (repo-or-glob, profile-name) in file order
        self.repos: list[tuple[str, str]] = []
        self.teams: dict[str, Team] = {}
        # (repo-glob, [(pathglob, team)])
        self.team_maps: list[tuple[str, list[tuple[str, str]]]] = []
        self.codeowners_enabled: bool = False
        self.codeowners_map: dict[str, str] = {}  # owner-handle -> team

    # ---- loading ----------------------------------------------------------

    @classmethod
    def load(cls, path: str | os.PathLike) -> OverridesConfig:
        p = Path(path)
        if not p.exists():
            raise OverrideError(f"overrides file not found: {p}")
        parser = configparser.ConfigParser(interpolation=None)
        parser.optionxform = str  # preserve case for repos/paths/owner handles
        try:
            parser.read(p, encoding="utf-8")
        except configparser.Error as e:
            raise OverrideError(f"overrides.cfg parse error: {e}") from e

        cfg = cls()
        for section in parser.sections():
            items = dict(parser.items(section))
            if section.startswith("profile:"):
                cfg._load_profile(section[len("profile:") :], items)
            elif section == "repos":
                for repo, profile_name in items.items():
                    cfg.repos.append((repo, profile_name.strip()))
            elif section.startswith("team:"):
                cfg._load_team(section[len("team:") :], items)
            elif section.startswith("teams:"):
                repo_glob = section[len("teams:") :]
                mappings = [(pg, team.strip()) for pg, team in items.items()]
                cfg.team_maps.append((repo_glob, mappings))
            elif section == "codeowners":
                cfg._load_codeowners(items)
            else:
                raise OverrideError(f"unknown section [{section}] in overrides.cfg")
        return cfg

    def _load_profile(self, name: str, items: dict) -> None:
        unknown = set(items) - _PROFILE_KEYS
        if unknown:
            raise OverrideError(f"[profile:{name}] has unknown keys: {sorted(unknown)}")
        self.profiles[name] = Profile(
            name=name,
            system_prompt=items.get("system_prompt"),
            context=_split_csv(items.get("context", "")),
            config=items.get("config"),
            model=items.get("model"),
            effort=items.get("effort"),
            extends=items.get("extends"),
        )

    def _load_team(self, name: str, items: dict) -> None:
        unknown = set(items) - _TEAM_KEYS
        if unknown:
            raise OverrideError(f"[team:{name}] has unknown keys: {sorted(unknown)}")
        self.teams[name] = Team(
            name=name,
            context=_split_csv(items.get("context", "")),
            instructions=items.get("instructions"),
            model=items.get("model"),
            effort=items.get("effort"),
        )

    def _load_codeowners(self, items: dict) -> None:
        enabled = items.pop("enabled", "false").strip().lower() in {"1", "true", "yes"}
        self.codeowners_enabled = enabled
        for owner, team in items.items():
            self.codeowners_map[owner] = team.strip()

    # ---- validation -------------------------------------------------------

    def validate(self, base_dir: str | os.PathLike | None = None) -> None:
        """Structural validation only — unknown sections/keys are caught at load, and
        this checks cross-references (repo->profile, team maps->teams, codeowners->teams)
        and inheritance cycles. Referenced *files* are NOT checked here: a missing
        context/prompt file must degrade gracefully (skip + log) at assembly time, never
        fail the review (overrides are best-effort enhancement). `base_dir` is accepted
        for backward compatibility and unused."""
        # repos -> known profiles
        for repo, profile_name in self.repos:
            if profile_name not in self.profiles:
                raise OverrideError(f"[repos] maps {repo!r} to unknown profile {profile_name!r}")
        # extends targets exist + no cycles
        for name in self.profiles:
            self._resolve_chain(name)  # raises on cycle / missing
        # team maps -> known teams
        for repo_glob, mappings in self.team_maps:
            for pathglob, team in mappings:
                if team not in self.teams:
                    raise OverrideError(
                        f"[teams:{repo_glob}] maps {pathglob!r} to unknown team {team!r}"
                    )
        # codeowners map -> known teams
        for owner, team in self.codeowners_map.items():
            if team not in self.teams:
                raise OverrideError(f"[codeowners] maps {owner!r} to unknown team {team!r}")

    def _resolve_chain(self, name: str) -> list[str]:
        chain: list[str] = []
        seen: set[str] = set()
        cur: str | None = name
        while cur is not None:
            if cur in seen:
                raise OverrideError(f"profile inheritance cycle at {cur!r}")
            if cur not in self.profiles:
                raise OverrideError(f"profile {name!r} extends unknown profile {cur!r}")
            seen.add(cur)
            chain.append(cur)
            cur = self.profiles[cur].extends
        return chain

    # ---- resolution -------------------------------------------------------

    def resolve_profile(self, repo: str) -> Profile | None:
        """Exact repo key first, then globs in file order. Inheritance merged."""
        match = None
        for pattern, profile_name in self.repos:
            if pattern == repo:
                match = profile_name
                break
        if match is None:
            for pattern, profile_name in self.repos:
                if fnmatch.fnmatch(repo, pattern):
                    match = profile_name
                    break
        if match is None:
            return None
        return self._merged_profile(match)

    def _merged_profile(self, name: str) -> Profile:
        # Walk parent -> child so child wins on scalars and context appends in order.
        chain = list(reversed(self._resolve_chain(name)))
        merged = Profile(name=name)
        for link in chain:
            p = self.profiles[link]
            if p.system_prompt is not None:
                merged.system_prompt = p.system_prompt
            if p.config is not None:
                merged.config = p.config
            if p.model is not None:
                merged.model = p.model
            if p.effort is not None:
                merged.effort = p.effort
            merged.context.extend(p.context)
        return merged

    def _maps_for_repo(self, repo: str) -> list[tuple[str, str]]:
        """Ownership entries applying to a repo: exact section first, then globs in
        file order; entries keep their in-section order."""
        exact: list[tuple[str, str]] = []
        globbed: list[tuple[str, str]] = []
        for repo_glob, mappings in self.team_maps:
            if repo_glob == repo:
                exact.extend(mappings)
            elif fnmatch.fnmatch(repo, repo_glob):
                globbed.extend(mappings)
        return exact + globbed

    def attribute(
        self, repo: str, changed_files: list[str], repo_root: str | os.PathLike | None
    ) -> tuple[dict[str, list[str]], dict[str, str], list[str]]:
        """Return (file->teams, file->source, unowned). Explicit map wins per file;
        CODEOWNERS fills the rest when enabled and repo_root is available."""
        entries = self._maps_for_repo(repo)
        codeowners = None
        if self.codeowners_enabled and repo_root is not None:
            codeowners = _load_codeowners_file(repo_root)

        attribution: dict[str, list[str]] = {}
        source: dict[str, str] = {}
        unowned: list[str] = []

        for f in changed_files:
            claimed = _first_map_match(f, entries)
            if claimed is not None:
                attribution[f] = [claimed]
                source[f] = "map"
                continue
            if codeowners is not None:
                owners = codeowners.owners_for(f)
                teams = _dedupe(
                    [self.codeowners_map[o] for o in owners if o in self.codeowners_map]
                )
                if teams:
                    attribution[f] = teams
                    source[f] = "codeowners"
                    continue
            unowned.append(f)
        return attribution, source, unowned

    def resolve(
        self,
        repo: str,
        *,
        changed_files: list[str] | None = None,
        repo_root: str | os.PathLike | None = None,
        tier_order: tuple[str, ...] = DEFAULT_TIER_ORDER,
    ) -> OverrideResolution:
        changed_files = changed_files or []
        profile = self.resolve_profile(repo)
        attribution, source, unowned = self.attribute(repo, changed_files, repo_root)

        matched_teams = _dedupe([t for teams in attribution.values() for t in teams])
        team_objs = [self.teams[t] for t in matched_teams if t in self.teams]

        res = OverrideResolution(
            profile=profile,
            matched_teams=matched_teams,
            teams=team_objs,
            file_attribution=attribution,
            attribution_source=source,
            unowned_files=unowned,
        )

        sources: list[str] = []
        if profile:
            res.system_prompt_path = profile.system_prompt
            res.config_path = profile.config
            res.context_sources.extend(profile.context)
            if profile.system_prompt:
                sources.append(profile.system_prompt)
            sources.extend(profile.context)
        for team in team_objs:
            res.context_sources.extend(team.context)
            sources.extend(team.context)
        res.sources = sources

        res.pin = self._resolve_pin(profile, team_objs, tier_order)
        return res

    def _resolve_pin(
        self, profile: Profile | None, teams: list[Team], tier_order: tuple[str, ...]
    ) -> Pin | None:
        if profile and profile.model:
            return Pin(model=profile.model, effort=profile.effort, source="profile")
        # highest-tier team model wins
        best: tuple[int, Team] | None = None
        for team in teams:
            if not team.model:
                continue
            rank = tier_order.index(team.model) if team.model in tier_order else len(tier_order)
            if best is None or rank < best[0]:
                best = (rank, team)
        if best is not None:
            t = best[1]
            return Pin(model=t.model, effort=t.effort, source=f"team:{t.name}")
        return None


def _first_map_match(path: str, entries: list[tuple[str, str]]) -> str | None:
    for pathglob, team in entries:
        if fnmatch.fnmatch(path, pathglob):
            return team
    return None


def _dedupe(items: list[str]) -> list[str]:
    seen: set[str] = set()
    out: list[str] = []
    for x in items:
        if x not in seen:
            seen.add(x)
            out.append(x)
    return out


# ---- CODEOWNERS -----------------------------------------------------------


class CodeOwners:
    """Minimal GitLab-style CODEOWNERS matcher: last matching pattern wins. Section
    headers are ignored (all entries pooled). Pattern matching is an approximation of
    gitignore/GitLab semantics via fnmatch — good enough for path->owner attribution;
    documented as approximate."""

    def __init__(self, entries: list[tuple[str, list[str]]]):
        self.entries = entries  # (pattern, owners) in file order

    def owners_for(self, path: str) -> list[str]:
        winner: list[str] | None = None
        for pattern, owners in self.entries:
            if _codeowners_match(pattern, path):
                winner = owners  # last match wins
        return winner or []


def _load_codeowners_file(repo_root: str | os.PathLike) -> CodeOwners | None:
    root = Path(repo_root)
    for loc in _CODEOWNERS_LOCATIONS:
        f = root / loc
        if f.exists():
            return _parse_codeowners(f.read_text(encoding="utf-8"))
    return CodeOwners([])


def _parse_codeowners(text: str) -> CodeOwners:
    entries: list[tuple[str, list[str]]] = []
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("[") or line.startswith("^["):
            continue  # section header
        parts = line.split()
        if len(parts) < 2:
            continue  # pattern with no owners -> unowned by design
        pattern, owners = parts[0], parts[1:]
        entries.append((pattern, owners))
    return CodeOwners(entries)


def _codeowners_match(pattern: str, path: str) -> bool:
    pat = pattern.strip()
    if not pat:
        return False
    anchored = pat.startswith("/")
    if anchored:
        pat = pat[1:]
    if pat.endswith("/"):
        pat = pat + "**"
    # Basename-style (no slash, unanchored): match at any depth.
    if "/" not in pat.replace("*", "") and not anchored:
        return (
            fnmatch.fnmatch(path, pat)
            or fnmatch.fnmatch(path, "*/" + pat)
            or fnmatch.fnmatch(os.path.basename(path), pat)
        )
    if anchored:
        return fnmatch.fnmatch(path, pat) or fnmatch.fnmatch(path, pat.rstrip("*") + "**")
    return fnmatch.fnmatch(path, pat) or fnmatch.fnmatch(path, "*/" + pat)
