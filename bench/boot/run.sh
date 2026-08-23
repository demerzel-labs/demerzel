#!/usr/bin/env bash
# Layer 1 boot benchmark: pure binary start via `--version`.
# `pty_probe.py` next to this script measures time-to-interactive.
set -euo pipefail
cd "$(dirname "$0")/../.."

command -v hyperfine >/dev/null 2>&1 || {
    echo "hyperfine is required (Arch: pacman -S hyperfine; macOS: brew install hyperfine)" >&2
    exit 1
}

BIN="${BIN:-target/release/demerzel}"
[ -x "$BIN" ] || {
    echo "no binary at $BIN — run: cargo build --release" >&2
    exit 1
}

SLUG="$(python3 -c 'import sys; sys.path.insert(0, "bench"); import harness; print(harness.os_slug())')"
OUT="bench/results/boot-$(date +%F)-$(git rev-parse --short HEAD)-$SLUG.json"
# -N: no shell between hyperfine and the binary; the command is
# sub-5 ms and shell startup would blur it.
hyperfine -N --warmup 5 --runs "${RUNS:-50}" --export-json "$OUT" "$BIN --version"
# Wrap the raw export in the shared provenance envelope.
python3 - "$OUT" "$BIN" <<'EOF'
import json
import os
import sys

sys.path.insert(0, os.path.join(os.getcwd(), "bench"))
import harness

out, bin_path = sys.argv[1], sys.argv[2]
with open(out) as f:
    raw = json.load(f)
report = harness.base_report("boot-version", bin_path)
report["hyperfine"] = raw
harness.write_report(out, report)
EOF
echo "raw data: $OUT"
