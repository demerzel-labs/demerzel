# Configuration

Demerzel resolves one **effective configuration** by folding a fixed chain of
layers, low to high precedence, then validating the merged result. This page
covers the layer model, the merge rules, the full schema, org governance, and
where a config file can live.

## The layer model

Six layers participate, in this precedence order (later beats earlier):

| Layer | Where it comes from |
|---|---|
| `defaults` | Baked into the harness — no file. |
| `org` | `--org-config <location>`. |
| `team` | The repo config's `extends:` key, resolved relative to the directory of the file that declares it. A team config cannot itself declare `extends` — chaining is rejected as a hard error. |
| `repo` | `<repo-root>/.demerzel.yml`, if present. Its absence is fine. |
| `directory` | Reserved for a nearest-ancestor `.demerzel.yml` cascade inside a repo. The slot exists in the precedence chain today but always resolves to zero layers — it lands in a future sprint. |
| `cli` | `--set dotted.path=value`, repeatable. |

Every layer is validated against its schema *before* merging, with unknown
keys forbidden (`extra="forbid"`). An unknown key at any layer is a hard
error naming the source file (or `<cli>`) and the dotted key, e.g.:

```
error: Unknown config key 'foo' in /path/to/.demerzel.yml
```

The org layer and the repo/team/cli layers use different schemas: the org
layer additionally accepts `locked`, `bounds`, `teams`, and `models.allowed`
(see [Org configuration & governance](#org-configuration--governance)) and
cannot declare `extends`. Setting a governance key at a repo, team, or CLI
layer is an unknown-key error.

## Merge semantics

Three rules, fixed per field — never guessed at merge time:

- **Scalars replace.** A higher layer's value for a plain field (strings,
  numbers, `tone`, `max_findings`, ...) fully replaces the lower layer's value.
- **Maps deep-merge.** Nested objects merge key by key, recursively, with the
  higher layer winning on collision. This covers `review.behavior.categories`,
  `models.routing`, `models.tiers`, and `bounds`.
- **Lists follow a per-field policy.** Most lists replace; a few named lists
  append (lower layers' items first, then higher layers'):

| Field | Policy |
|---|---|
| `review.rules` | append |
| `review.behavior.ignore_paths` | append |
| `review.overrides` | append |
| `review.behavior.focus` | replace |
| `review.output.sections` | replace |
| any other list | replace |

### Worked example

The team layer (`examples/org/teams/payments.yml`) sets
`ignore_paths: ["**/generated/**"]` and a rule `no-plaintext-secrets`. The
repo layer (`examples/repo/.demerzel.yml`) sets
`ignore_paths: ["vendor/**", "*.lock"]` and a rule `no-foo-client`. Both
fields are on the append policy, so `demerzel config show` reports the
concatenation, team then repo:

```yaml
review:
  behavior:
    ignore_paths: ["**/generated/**", "vendor/**", "*.lock"]
  rules:
    - id: no-plaintext-secrets
      ...
    - id: no-foo-client
      ...
```

Compare `review.output.sections`, which is on the replace policy: the harness
default is `[summary, findings]`, but the repo layer sets
`[summary, findings, nitpicks]`, and that list — not a concatenation — is what
ends up in the effective config.

## Schema reference

Every layer file is a mapping with `version: 1` at the top. Defaults below are
the harness-default values from `defaults.py`; a partial file only needs to
set the keys it wants to override — every field has a default, so partial
layers validate on their own before merging.

### Top level

| Key | Type | Default | Meaning |
|---|---|---|---|
| `version` | `1` (literal) | `1` | Schema version; only `1` is valid. |
| `extends` | `str \| null` | `null` | Repo/team layers only. Path or URL to a team config to opt into. Org configs cannot set this. |

### `review.tone`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `review.tone` | `str` | `"concise"` | Reviewer tone. `concise`, `friendly`, and `formal` are documented presets; any free-text string is accepted. |

### `review.output`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `review.output.format` | `markdown \| json \| sarif` | `markdown` | Render format for findings. |
| `review.output.template` | `str` | `"default"` | Built-in template name or a repo-relative path to a Jinja template. |
| `review.output.sections` | `list[str]` | `[summary, findings]` | Which sections to render. List policy: replace. |

### `review.behavior`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `review.behavior.focus` | `list[str]` | `[]` | Areas to emphasize (e.g. `security`, `correctness`). List policy: replace. |
| `review.behavior.ignore_paths` | `list[str]` | `[]` | Glob patterns excluded from review. List policy: append. |
| `review.behavior.report_threshold` | severity or `never` | `"info"` | Minimum severity that gets reported. |
| `review.behavior.fail_threshold` | severity or `never` | `"error"` | Minimum severity that fails the run (exit 1, in a later sprint). |
| `review.behavior.categories` | `dict[str, action]` | `{}` | Per-category remap; see [Categories grammar](#categories-grammar) below. Deep-merges across layers. |
| `review.behavior.max_findings` | `int` | `20` | Cap on reported findings. |

#### Severity scale

Rule severities and the `categories` remap use `Severity`: `info < warning <
error`, a strict total order. `never` is **not** a valid severity — it is a
threshold-only sentinel accepted only by `report_threshold` and
`fail_threshold`, meaning "report nothing" / "never fail" respectively. Using
`never` as a rule's `severity` is a validation error.

#### Categories grammar

`review.behavior.categories` maps a category name to an action:

| Action | Effect |
|---|---|
| `ignore` | Drop that category's findings entirely. |
| `info` / `warning` / `error` | Remap that category's findings to the given severity. |

Example: `categories: { style: ignore }` drops all `style` findings.

### `review.rules[]`

Named, additive rules. List policy: append (rules from every layer that sets
them all survive, in layer order).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `id` | `str` | required | Rule identifier; findings cite it. |
| `paths` | `list[str]` | `[]` | Glob patterns the rule applies to. |
| `severity` | severity (not `never`) | `"warning"` | Severity for findings from this rule. |
| `instruction` | `str` | required | Free-text instruction describing what to flag. |

### `review.overrides[]`

Path-scoped overrides (ESLint-`overrides` style): `paths` plus any of `tone`,
`output`, `behavior`, `models`, applied directly on the entry (not nested
under a `review:` key). List policy: append.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `paths` | `list[str]` (non-empty) | required | Paths this override applies to. |
| `tone` | `str \| null` | `null` | Override tone for matching paths. |
| `output` | `OutputConfig \| null` | `null` | Override output settings for matching paths. |
| `behavior` | `BehaviorConfig \| null` | `null` | Override behavior settings for matching paths. |
| `models` | `ModelsConfig \| null` | `null` | Override model routing/tiers for matching paths. |

An override is dumped in `demerzel config show` exactly as authored — unset
fields are not filled in with defaults, unlike the rest of the effective
config.

### `models`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `models.routing` | `dict[stage, tier]` | `{plan: fast, review: standard, verify: fast}` | Which tier (`fast`/`standard`/`deep`) each pipeline stage (`plan`/`review`/`verify`) uses. Deep-merges across layers. |
| `models.tiers` | `dict[tier, str]` | `{fast: bedrock:us.amazon.nova-lite-v1:0, standard: bedrock:us.anthropic.claude-sonnet-4-5, deep: bedrock:us.anthropic.claude-opus-4-1}` | Maps a tier name to a concrete model URI string. Deep-merges across layers. Model URIs are plain strings in this sprint; URI parsing/validation arrives later. |
| `models.allowed` | `list[str]` | `[]` | Org layer only. See [governance](#org-configuration--governance). |

### `budget`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `budget.max_cost_usd` | `float` | `1.00` | Hard per-run cost cap. Provisional until a real cost meter exists. |

### `providers`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `providers.default` | `str` | `"bedrock"` | Default provider name. |
| `providers.bedrock.region` | `str` | `"us-east-1"` | AWS region. |
| `providers.bedrock.auth` | `ambient \| assume_role` | `"ambient"` | Credential mode. `assume_role` requires `assume_role_arn` — setting `auth: assume_role` without it is a validation error. |
| `providers.bedrock.assume_role_arn` | `str \| null` | `null` | IAM role ARN, required when `auth` is `assume_role`. |
| `providers.anthropic.api_key_env` | `str \| null` | `null` | Env var holding the Anthropic API key (`auth` is fixed to `api_key`). |
| `providers.openai.api_key_env` | `str \| null` | `null` | Env var holding the OpenAI API key (`auth` is fixed to `api_key`). |
| `providers.litellm.api_key_env` | `str \| null` | `null` | Env var for the LiteLLM bridge (Azure OpenAI, Vertex, self-hosted). |

`anthropic`, `openai`, and `litellm` are `null` unless a layer sets them.

## Org configuration & governance

The org layer is a normal value-contributing layer — everything in
[Schema reference](#schema-reference) applies to it too — plus four extra
keys that only the org layer may set:

| Key | Type | Meaning |
|---|---|---|
| `locked` | `list[str]` | Dotted config paths that lower layers should not be able to change. |
| `bounds` | `dict[str, {min, max}]` | Dotted config paths with a numeric range lower layers should stay within. |
| `teams` | `list[{match, extends}]` | Repo-pattern-to-team-config mappings, for repos that haven't opted in via their own `extends:`. |
| `models.allowed` | `list[str]` | Model URIs (or glob patterns like `bedrock:*`) that tier remaps should be restricted to. |

Be clear about what "should" means here: in this sprint, `locked`, `bounds`,
`teams`, and `models.allowed` are **validated for type and carried through**
into the effective config — you'll see them in `demerzel config show` output
— but nothing enforces them yet. A lower layer can currently set a `locked`
key, exceed a `bounds` range, or pick a model outside `models.allowed`
without error. Enforcement is on the roadmap, not in this release.

The same honesty applies to `review.overrides`: they are parsed, validated,
and merged (append policy) today, and round-trip through `config show`
exactly as authored. Actually *applying* an override to a matching file's
effective config is part of the review pipeline, which does not exist yet.

## Config locations

`--org-config` and a repo's `extends:` both accept a **location**: a bare
filesystem path, or a `scheme://` URL.

| Scheme | Status |
|---|---|
| bare path | Supported. Relative paths resolve against the current working directory (`--org-config`) or the directory of the file declaring `extends:` (team configs). |
| `file://` | Supported; equivalent to a bare local path. |
| `https://` | Recognized but rejected with a clear error — remote fetching is not implemented yet. |
| `s3://` | Recognized but rejected with a clear error — remote fetching is not implemented yet. |
| anything else | Rejected as an unknown scheme. |

The `https://`/`s3://` error names the location and says fetching isn't
supported yet, rather than failing as a generic "file not found":

```
error: https:// config locations are not supported yet (arrives in Sprint 4.1): https://example.com/demerzel.yml
```
