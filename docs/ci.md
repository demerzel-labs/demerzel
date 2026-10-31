# Continuous integration

CI checks each pull request and each push to `main`.
A manual run starts all CI jobs.
The Monday schedule starts only `dependencies`.
It checks new advisories against the current lockfile.

## Required checks

| Check name | Checks |
| --- | --- |
| `test (ubuntu-latest)` | Formatting, Clippy, workspace tests, and benchmark probe tests |
| `test (macos-latest)` | The same checks on macOS |
| `gates` | Rustdoc, release build, release tests, and binary size on Linux |
| `quality` | CI helper tests, workflow syntax, and local Markdown links and anchors |
| `dependencies` | Dependency advisories, licenses, and sources |
| `PR metadata` | Pull request title, commit subjects, and required body sections |

The release tests use the full workspace suite with optimization enabled.
The release binary must remain below 25,000,000 bytes.
CI does not compare timing measurements from shared runners.
Use the reference machines in [bench-platforms.md](bench-platforms.md) for performance claims.

Local link checks run without network access.
They include Markdown anchors.
External links do not block a pull request.

## Documentation-only pull requests

The `quality` job checks the complete pull request diff against its merge base.
It omits the Rust checks only when every changed path appears in
[the documentation list](../scripts/ci/rust_scope.py).
The list covers the README, agent instructions, plans, contribution guide, and header image.
It also includes other documentation that builds and tests do not read.

Changes outside this list run all checks.
This includes new paths, code, dependencies, build settings, and CI files.
`docs/config.md` also runs all checks because a Rust test reads it.
Keep build and test inputs out of the documentation list.
Renames check both the old path and the new path.
An empty diff or failed detection runs the Rust checks.

The workflow still checks pull request metadata, documentation links, workflow syntax, and dependencies.
The Rust jobs wait for `quality` and omit checks only after it succeeds.
The test jobs keep both platform check names and skip their build steps.
Each test job records the reason in its summary.
The `gates` job skips as a whole.
These results satisfy required checks without leaving them pending.
A failed `quality` job still blocks merge when branch protection requires it.

A documentation commit in a pull request with code changes still runs the Rust checks.
Pushes to `main` and manual runs always run all checks.
The Monday schedule still runs only dependency checks.

## CI evidence

GitHub stores each job status and log on the workflow run.
Use the run link when you cite a CI result.
Include the tested revision with the result.
Check each job log for the exact command and output.

Pull request runs normally test a temporary merge commit.
Its SHA can differ from the pull request head SHA.
Use the tested checkout SHA when you cite these results.

A failed command fails its workflow step.
Passing CI does not prove correctness.
Test output is not coverage data.
Durations from shared runners are not performance benchmarks.

Job logs contain command output.
Do not add credentials, real session files, or private data to fixtures or output.

## Pull request metadata

The metadata check runs when a pull request opens, reopens, changes, or receives a title or body edit.
It fetches the current title, body, and commit list from the repository API.
It does not execute their contents.
These edits do not restart the Rust jobs.

The check validates the subject format in [contributing.md](contributing.md#commit-subjects).
It checks each commit in the pull request, including merge commits.
It does not inspect the base history.
To update a branch, merge `main` locally with a subject that follows the convention.
The default **Update branch** merge subject fails this check.
The commit list must be complete and must match the current head SHA.
The API returns at most 250 commits. Split larger changes into smaller pull requests.
If metadata changes during the fetch, run the check again.

The body needs a summary and these sections, in order:

1. `## Decisions`
2. `## Verify`
3. `## Review`
4. `## Contract growth`

The summary can use opening prose or `## Summary`.
Empty sections and placeholder-only sections fail.
HTML comments do not count as content.
Use `Contract growth: none` when no public contract changes.

The check validates structure, not the truth of claims.
Reviewers must assess evidence, judgment, scope, and review results.
They must also check imperative wording and the final merge subject.

Dependabot uses the same rules.
A maintainer must complete its pull request body before merge.
An edit starts the metadata check again.

## Dependency policy

[deny.toml](../deny.toml) defines the allowed licenses and sources.
The check includes development dependencies.
It rejects known vulnerabilities, yanked releases, and unapproved sources or licenses.
It also rejects dependencies that match an unmaintained advisory.
It checks unsound-code advisories across direct and transitive dependencies.
Duplicate dependency versions do not fail this check.

Review each dependency alert.
Prefer a compatible update that fixes the issue.
Document any necessary exception, its scope, and its removal condition in the pull request.
Do not add a broad exception solely to pass CI.

Workflow actions use full commit SHAs.
The tool installer checks release checksums and disables fallback installation.
Dependabot proposes weekly Cargo and action updates.
Review those changes before merge.
The checker versions and archive hashes require separate updates.

## Local verification

Use the tool versions in [.github/workflows/ci.yml](../.github/workflows/ci.yml).
The metadata checker requires Python 3.11 or newer.
Run the relevant commands before you submit changes:

```bash
python3 -m unittest discover -s scripts/ci -p 'test_*.py'
python3 -m unittest discover -s bench -p 'test_*.py'
actionlint
git ls-files -z '*.md' | xargs -0 lychee --offline --include-fragments --no-progress --
cargo deny --locked check advisories licenses sources
cargo test --workspace --release --locked
```

The link command checks tracked files.
Stage new documentation files before you run it.
See [contributing.md](contributing.md#local-checks) for the other Rust checks.

## Branch protection

`main` has no required-check protection yet.
Passing workflow runs alone do not prevent a merge.
`PR metadata` is a separate workflow check.

Before the first packaged release:

1. Enable branch protection or a ruleset for `main`.
2. Require pull requests and all six check names in the table above.
3. Require the branch to be current before merge.
4. Disable routine bypasses.
5. Confirm that a failed check blocks merge.
6. Check the final merge subject against the commit convention.

Do not enable a merge queue without adding and checking the required `merge_group` workflow events.
See [the required-check guidance](https://docs.github.com/en/pull-requests/how-tos/merge-and-close-pull-requests/troubleshooting-required-status-checks).
