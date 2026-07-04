# CLI reference

Sprint 0.1 has one command: `demerzel config show`. `review`, `doctor`, and
`eval` arrive in later sprints.

## `demerzel config show`

Resolves and prints the effective configuration for a repo.

| Option | Type | Default | Meaning |
|---|---|---|---|
| `--repo-root` | path | `.` | Repo root. The repo config is `<repo-root>/.demerzel.yml`; its absence is fine. The path must exist and be a directory, or the command fails before doing anything else. |
| `--org-config` | location | none | Org config location — a path, or `file://`. Relative paths resolve against the current working directory. See [Configuration: config locations](configuration.md#config-locations). |
| `--set` | `KEY=VALUE` | none | CLI override, repeatable. See [The `--set` grammar](#the---set-grammar) below. |
| `--format` | `yaml` \| `json` | `yaml` | Output format. |

### The `--set` grammar

Each `--set` value is `dotted.path=value`. The part after `=` is parsed with
`yaml.safe_load`, so:

- `--set review.behavior.max_findings=5` sets an integer.
- `--set providers.default=anthropic` sets a string.
- `--set 'review.behavior.focus=[security, correctness]'` sets a list, using
  YAML flow-list syntax.

Quote the whole `KEY=VALUE` pair when the value contains spaces — that's for
your shell, not for YAML: `--set 'review.tone=make it snappy'` sets the
string `make it snappy`; without the quotes, the shell splits it into extra
positional arguments and the command fails.

`extends` cannot be set with `--set` — declare it in the repo config instead.
Trying anyway is a hard error:

```
error: 'extends' cannot be set from the CLI; declare it in the repo config
```

The CLI layer is validated with the same unknown-key rules as any other
layer, so `--set some.unknown.key=1` fails naming `<cli>` as the source.

### Output

In `yaml` format (the default), the first line is a comment listing every
layer that participated, in precedence order, followed by the merged config
as YAML:

```
# layers: defaults -> org (...) -> team (...) -> repo (...)
version: 1
...
```

In `json` format, output is pure JSON — no layer-header comment, no other
text on stdout.

### Exit codes

| Code | Meaning |
|---|---|
| `0` | Success. |
| `2` | Configuration or usage error (bad `--repo-root`, unknown key, bad `--set` syntax, unsupported config location, and so on). Config errors print as `error: <message>` on stderr; option-level usage errors (e.g. a nonexistent `--repo-root`) print the standard usage message instead. |

Exit `1` is reserved for review findings at or above `fail_threshold` — that
behavior arrives with the review pipeline in a later sprint.

## Examples

Resolve the full example stack (defaults, org, team via `extends`, repo):

```bash
demerzel config show --repo-root examples/repo --org-config examples/org/demerzel.yml
```

Same, but inject a CLI override and emit JSON:

```bash
demerzel config show --repo-root examples/repo --set review.behavior.max_findings=5 --format json
```

Resolve a repo with no org config — just defaults plus the repo file:

```bash
demerzel config show --repo-root .
```

Point at a not-yet-supported remote org config, to see the exit-2 error path:

```bash
demerzel config show --repo-root examples/repo --org-config https://example.com/demerzel.yml
```
