# Worklog

One entry per sprint exit, 3–5 lines each: what shipped, what it bought against
the problem, what surprised. Decide-and-document decisions get a short block
under the entry that made them (they also live in schema docstrings).

## Sprint 0.1 — Config core (2026-07-03)

Shipped the config core: pydantic schemas covering both PLAN.md example configs,
the frozen layer chain (defaults → org → team → repo → directory → cli) with the
directory slot reserved for 3.1, fixed per-field merge semantics, a scheme-based
fetcher (file:// only), and `demerzel config show`. 63 tests; unknown keys at any
layer hard-error naming file and key — the governance posture now exists in code.
Surprise: the toolchain list omitted a YAML parser entirely; PyYAML approved by
Daniel and added. Docs/ started same sprint (index, getting-started,
configuration reference, CLI reference), all examples verified against the CLI.

Decisions (decide-and-document authority):

- Severity scale `info < warning < error`; `never` is a threshold-only sentinel
  (valid for `report_threshold`/`fail_threshold`, rejected as a rule severity).
- Categories grammar: `category name → ignore | info | warning | error`
  (ignore drops, severity name remaps); categories deep-merge as a map.
- CLI override grammar: repeatable `--set dotted.path=value`, value parsed via
  `yaml.safe_load`; `extends` is rejected at the CLI layer.
- List merge policy table: `rules`/`ignore_paths`/`overrides` append,
  `focus`/`sections` replace, any other list replaces (documented default).
- Team configs may not chain `extends` (hard error) until a real need appears.
