#!/usr/bin/env python3
"""Milestone metrics receipt. Records the size and the contract
surface of the workspace, so growth between milestones is a trend
line, not a code read.

The counts are crude and stable on purpose: `pub` items include impl
methods and test helpers, CLI names are struct fields rather than
flag spellings. The trend matters, not the absolute. Change a
counting rule only with a note in the receipt, or the trend breaks.
"""

import os
import re
import sys
import tomllib

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import harness

REPO_ROOT = harness.REPO_ROOT
CRATES_DIR = os.path.join(REPO_ROOT, "crates")

PUB_ITEM = re.compile(r"^\s*pub (?:fn|struct|enum|trait|const|type|mod) ")


def rust_files(root: str) -> list[str]:
    found = []
    for dirpath, _, names in os.walk(root):
        found.extend(os.path.join(dirpath, n) for n in names if n.endswith(".rs"))
    return sorted(found)


def read(path: str) -> str:
    try:
        with open(path) as f:
            return f.read()
    except OSError:
        return ""


def crate_counts() -> dict[str, dict[str, int]]:
    counts = {}
    for crate in sorted(os.listdir(CRATES_DIR)):
        src = os.path.join(CRATES_DIR, crate, "src")
        if not os.path.isdir(src):
            continue
        lines = 0
        pub_items = 0
        for path in rust_files(src):
            text = read(path)
            lines += text.count("\n")
            pub_items += sum(1 for line in text.splitlines() if PUB_ITEM.match(line))
        counts[crate] = {"lines": lines, "pub_items": pub_items}
    return counts


def external_deps() -> list[str]:
    """Direct workspace dependencies, path deps excluded."""
    with open(os.path.join(REPO_ROOT, "Cargo.toml"), "rb") as f:
        manifest = tomllib.load(f)
    deps = manifest.get("workspace", {}).get("dependencies", {})
    return sorted(
        name
        for name, spec in deps.items()
        if not (isinstance(spec, dict) and "path" in spec)
    )


def block_after(text: str, anchor: str) -> str:
    """The brace-delimited block that starts at `anchor`."""
    start = text.find(anchor)
    if start < 0:
        return ""
    depth = 0
    for i in range(start, len(text)):
        if text[i] == "{":
            depth += 1
        elif text[i] == "}":
            depth -= 1
            if depth == 0:
                return text[start:i]
    return ""


def enum_variants(path: str, enum_name: str) -> list[str]:
    # The anchor matches `enum X` and `pub enum X` both.
    block = block_after(read(path), f"enum {enum_name}")
    return re.findall(r"^    ([A-Z]\w*)", block, re.MULTILINE)


def trait_methods(path: str, trait_name: str) -> list[str]:
    block = block_after(read(path), f"pub trait {trait_name}")
    return re.findall(r"fn (\w+)", block)


def struct_fields(path: str, struct_name: str) -> list[str]:
    block = block_after(read(path), f"struct {struct_name} {{")
    return re.findall(r"^    (?:pub )?(\w+):", block, re.MULTILINE)


def registered_tools() -> list[str]:
    text = read(os.path.join(CRATES_DIR, "demerzel-tools", "src", "lib.rs"))
    return [name.lower() for name in re.findall(r"register\(Box::new\((\w+)Tool::new", text)]


def config_keys() -> list[str]:
    """Match-arm keys in the config parser. Empty before the parser
    exists at the measured rev."""
    text = read(os.path.join(CRATES_DIR, "demerzel", "src", "config.rs"))
    return sorted(set(re.findall(r'^\s*"([a-z_]+)" =>', text, re.MULTILINE)))


def contract_inventory() -> dict:
    core = os.path.join(CRATES_DIR, "demerzel-core", "src")
    main_rs = os.path.join(CRATES_DIR, "demerzel", "src", "main.rs")
    cli_fields = [f for f in struct_fields(main_rs, "Cli") if f != "command"]
    return {
        "provider_trait": trait_methods(os.path.join(core, "provider.rs"), "Provider"),
        "engine_events": enum_variants(os.path.join(core, "engine.rs"), "EngineEvent"),
        "tools": registered_tools(),
        "config_keys": config_keys(),
        "cli_fields": cli_fields,
        "cli_subcommands": enum_variants(main_rs, "Command"),
    }


def main() -> int:
    binary = os.path.join(REPO_ROOT, "target", "release", "demerzel")
    if not os.path.exists(binary):
        print("build the release binary first: cargo build --release", file=sys.stderr)
        return 1

    crates = crate_counts()
    deps = external_deps()
    inventory = contract_inventory()
    report = harness.base_report("metrics", binary)
    report.update(
        {
            "crates": crates,
            "lines_total": sum(c["lines"] for c in crates.values()),
            "pub_items_total": sum(c["pub_items"] for c in crates.values()),
            "external_deps": deps,
            "external_dep_count": len(deps),
            "binary_bytes": os.path.getsize(binary),
            "contracts": inventory,
            "contract_counts": {name: len(items) for name, items in inventory.items()},
        }
    )

    out = os.path.join(
        REPO_ROOT,
        "bench",
        "results",
        f"metrics-{report['date']}-{report['git_rev']}-{report['os']}.json",
    )
    harness.write_report(out, report)
    print(f"lines {report['lines_total']}, pub items {report['pub_items_total']}, "
          f"deps {report['external_dep_count']}, binary {report['binary_bytes']} bytes")
    print(f"raw data: {os.path.relpath(out, REPO_ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
