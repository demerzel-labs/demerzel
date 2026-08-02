# AGENTS.md — Repository Rules

Rules for AI coding agents that work in this repository. `CLAUDE.md` is a
symlink to this file. Edit `AGENTS.md` only; the symlink keeps both names
current.

## Orientation

- Demerzel is an open-source terminal coding agent written in Rust.
- Read [`docs/plan.md`](docs/plan.md) for the product plan and milestones.
- Read [`docs/m1-plan.md`](docs/m1-plan.md) for the current milestone.

## Documentation placement

- All documentation lands in `docs/` at the repo root.
- `README.md` at the repo root is the standard entry point. It stays at the
  root.
- A directory can keep a `README.md` that documents only that directory.
  Example: `bench/README.md`.

## Writing standard: Simplified Technical English

All public-facing technical content follows the principles of ASD-STE100
(https://www.asd-ste100.org/). This covers `README.md`, every file in
`docs/`, directory READMEs, CLI help text, error messages, and tool
descriptions.

The rules:

1. Use one word with one meaning. Pick one verb for one action and reuse it.
2. Write in the active voice.
3. Use simple tenses only.
4. Write one instruction per sentence.
5. Keep instructions at 20 words or fewer. Keep descriptions at 25 words or
   fewer.
6. Stack at most three words as a noun cluster.
7. Keep the subject, the verb, and the article explicit.
8. Use a list for three or more steps or conditions.
9. Keep necessary technical terms. Define a term once if it is not common
   English.

Check each documentation change against these rules before you commit it.
Code, code comments, and identifiers are exempt.
