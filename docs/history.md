# Repository history

Demerzel moved to a new repository on 2026-09-04.
The [active repository](https://github.com/demerzel-labs/demerzel) contains sanitized development history.
The [original archive](https://github.com/demerzel-labs/demerzel-private-archive) remains private.
The migration did not change the current Rust source or benchmark receipts.

## Development history

The migration preserves all 243 original commits, their parent relationships,
and their author and committer dates.
The commit IDs changed after the removal of personal email metadata and
private integration notes.
Historical prototypes use neutral names.
The earlier Python prototype was independently authored.
It remains in the ancestry of the Rust implementation.

The initial repository contains these references:

- `main` preserves 238 original commits and adds the migration documentation.
- `archive/legacy-config-core-2026-07-04` preserves five commits from a separate
  configuration prototype.

New development continues from `main`.
The original archive also keeps the old branch names and GitHub records.

## Commit IDs and benchmark receipts

The [commit map](history-commit-map.tsv) contains one row for each original commit.
The first column contains the original ID.
The second column contains the sanitized ID.
A short ID matches the start of the original ID.

Use the sanitized ID with `git show` or `git checkout`.
The map covers all 46 benchmark receipts present at the migration.
Receipt filenames, recorded IDs, and measurements remain unchanged.
The map identifies the corresponding source; it does not represent a new benchmark run.

## Historical pull requests

Pull request numbers recorded before the migration refer to the original private archive.
The new repository starts its own issue and pull request numbering.
Historical commit messages and milestone documents retain the original numbers.

## Existing checkouts

Clone the active repository again before resuming work in an old checkout.
Save any local work separately first.
Do not merge or push commits from the original archive into the active repository.
Use the commit map to find equivalent commits in the sanitized history.
