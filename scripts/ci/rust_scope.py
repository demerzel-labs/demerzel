"""Select Rust checks from the complete pull request diff."""

import os
import subprocess
import sys


# Keep build and test inputs out of this set, including docs/config.md.
DOC_ONLY_PATHS = {
    "README.md",
    "AGENTS.md",
    "CLAUDE.md",
    "bench/README.md",
    "docs/assets/readme-header.png",
    "docs/bench-platforms.md",
    "docs/ci.md",
    "docs/contributing.md",
    "docs/history-commit-map.tsv",
    "docs/history.md",
    "docs/m1-plan.md",
    "docs/m2-plan.md",
    "docs/m2.5-plan.md",
    "docs/plan.md",
    "docs/prior-art.md",
}


def requires_rust(base: str, head: str) -> bool:
    changed = subprocess.check_output(
        [
            "git", "diff", "--name-only", "--no-renames", "-z",
            f"{base}...{head}", "--",
        ]
    )
    return not changed or any(
        os.fsdecode(path) not in DOC_ONLY_PATHS
        for path in changed.rstrip(b"\0").split(b"\0")
    )


if __name__ == "__main__":
    print(f"rust={str(requires_rust(*sys.argv[1:])).lower()}")
