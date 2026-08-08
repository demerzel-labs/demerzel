#!/usr/bin/env bash
# Layer 1 boot benchmark: pure binary start via `--version`.
# `pty_probe.py` next to this script measures time-to-interactive.
set -euo pipefail
cd "$(dirname "$0")/../.."

command -v hyperfine >/dev/null 2>&1 || {
    echo "hyperfine is required (Arch: pacman -S hyperfine)" >&2
    exit 1
}

BIN="${BIN:-target/release/demerzel}"
[ -x "$BIN" ] || {
    echo "no binary at $BIN — run: cargo build --release" >&2
    exit 1
}

OUT="bench/results/boot-$(date +%F)-$(git rev-parse --short HEAD).json"
# -N: no shell between hyperfine and the binary; the command is
# sub-5 ms and shell startup would blur it.
hyperfine -N --warmup 5 --runs "${RUNS:-50}" --export-json "$OUT" "$BIN --version"
echo "raw data: $OUT"
