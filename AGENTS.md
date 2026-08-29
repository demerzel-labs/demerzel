# AGENTS.md — Repository Rules

Rules for AI coding agents that work in this repository. `CLAUDE.md` is a
symlink to this file. Edit `AGENTS.md` only; the symlink keeps both names
current.

## Orientation

- Demerzel is an open-source terminal coding agent written in Rust.
- Read [`docs/plan.md`](docs/plan.md) for the product plan and milestones.
- Read [`docs/m2.5-plan.md`](docs/m2.5-plan.md) for the current
  milestone. [`docs/m1-plan.md`](docs/m1-plan.md) and
  [`docs/m2-plan.md`](docs/m2-plan.md) record the completed
  milestones; the M2 close-out runs before M2.5 step 2.

## Build workflow

- Each plan step is one pull request. Branch `agent/<step-name>` off
  `main`.
- Write commit subjects in the imperative. Do not add AI attribution.
- Before you open a pull request, run these checks and make them pass:
  1. `cargo fmt`
  2. `cargo clippy --workspace --all-targets -- -D warnings`
  3. `cargo test --workspace`
- Publication check, before every pull request. Read the full diff.
  Reject content that must not become public:
  1. Other products named in source code, code comments, or READMEs
     (see Source rules).
  2. Credentials or secret-shaped strings. A test value must be
     clearly fake.
  3. Personal identifiers of the project's own people: usernames,
     email addresses, hostnames, and absolute paths under a home
     directory. The owner's name in `LICENSE` and in this file is
     deliberate and stays.
  4. Endpoints, tokens, or identifiers that are not already
     published. A value counts as published when an open-source
     client states it.
  Remove each hit, or move the content to its approved place. Do
  this before the pull request opens. A full-tree sweep with the
  same list ran on 2026-08-12, so the diff scope covers everything
  after that date.
- The final gate before every pull request that changes code: dispatch
  adversarial review subagents (Opus 5, high reasoning effort) over the
  full change set.
  Use three lenses: performance and memory, API and architecture
  stability, correctness. Fix the material findings. Verify the fixes
  with one more review pass. Record the review results in the pull
  request body.
- The pull request body keeps this layout: a summary, then
  `Decisions`, `Verify`, `Review`, and `Contract growth`. Before the
  pull request opens, one subagent edits the draft for clear
  presentation. The reference is the Google developer documentation
  style guide (https://developers.google.com/style). The pass keeps
  every decision, number, finding, and fix. Word count is not the
  target. Apply the edited text.
- Test discipline, checked by the correctness lens: tests exercise
  public contracts. Do not pin incidental output, such as report
  wording, unless a plan doc names that output as contract.
- Declare contract growth in every pull request body, under a
  `Contract growth` heading. The load-bearing contracts:
  1. The provider trait.
  2. Engine events.
  3. Tool names and input schemas.
  4. Config keys.
  5. The session file schema.
  6. The CLI flags and subcommands.
  List each addition or change to one of these. Write
  `Contract growth: none` when there is none. Daniel reviews this
  section closely; the review gate owns the implementation.
- Optimize for performance, low memory use, clean stable APIs, and an
  architecture that stays clear as the project grows. Do not trade
  customizability away for these goals.
- Stop after you open the pull request. Daniel reviews and merges.

## Milestone close-out

Run these two steps at the end of every milestone, before the next
milestone plan lands:

- An alignment review over the whole repository, not a diff. Two
  lenses: architecture — the crates and contracts still match
  [`docs/plan.md`](docs/plan.md) — and simplification — speculative
  abstraction, dead config surface, single-use indirection. Fold the
  findings into the plan docs.
- The metrics receipts, one per reference machine (Linux and macOS,
  see [`docs/bench-platforms.md`](docs/bench-platforms.md)): build
  the release binary, run `bench/metrics.py`, and commit the JSON it
  writes to `bench/results/`. Compare each against the previous
  milestone's receipt from the same machine in the alignment review.
  When one machine is unreachable, its receipt lands as a follow-up
  commit and does not block the close-out.

## Source rules

- Demerzel credits its design inspirations openly. All prior-art
  credit lives in `docs/`. [`docs/prior-art.md`](docs/prior-art.md)
  names each source and the license that applies. Demerzel
  reimplements ideas; it does not copy code from other projects.
- Keep code, code comments, and READMEs in Demerzel's own terms. Do
  not describe a feature through another product's name. Such a
  reference goes stale and hides the contract the code must state.
- Exception: these sites name the Codex CLI:
  `crates/demerzel-providers/src/openai_codex/`,
  `crates/demerzel-providers/src/auth.rs`,
  `crates/demerzel-providers/src/oauth.rs`, and the callers and
  examples that wire the credential import. That client is the
  wire-contract reference for the endpoint, the login flow, and the
  source of imported logins. The word Codex alone names the endpoint
  and the provider, and the word ChatGPT names the subscription a
  login signs into; neither is a product name under this rule.

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
