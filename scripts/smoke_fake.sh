#!/usr/bin/env bash
# Offline smoke test for the sample CI invocation. The fake runner needs no AWS or
# network access and verifies the CLI contract end to end.
set -euo pipefail

SCRIPT_DIR_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

CONFIG="$SCRIPT_DIR_ROOT/examples/agents/configs/prototype-review.json"
PROMPT_PATH="$WORK/prompt.md"
RAW_REVIEW_FILE="$WORK/review.md"
TRACE_FILE="$WORK/trace.jsonl"
RESULTS_DIR="$WORK"
JIRA_KEY="SMOKE-1"
REPO="acme/core"
DIFF_SIZE=2000
HARNESS_LOG="$WORK/harness.log"

# base-dir points at the examples tree so relative prompt paths resolve.
BASE_DIR="$SCRIPT_DIR_ROOT/examples"

printf 'Please review this diff:\n' > "$PROMPT_PATH"
head -c "$DIFF_SIZE" /dev/zero | tr '\0' 'x' >> "$PROMPT_PATH"

export PROTOTYPE_RUNNER=fake
export PROTOTYPE_BUDGET_NAME="does-not-exist-smoke"  # preflight -> check_failed -> proceed

cd "$SCRIPT_DIR_ROOT"
uv run python3 -m prototype.harness.bedrock \
    --config "$CONFIG" \
    --prompt-file "$PROMPT_PATH" \
    --output "$RAW_REVIEW_FILE" \
    --trace "$TRACE_FILE" \
    --base-dir "$BASE_DIR" \
    --repo "$REPO" \
    --diff-size "${DIFF_SIZE:-0}" \
    --system-prompt-file "${RESULTS_DIR}/${JIRA_KEY}_system_prompt.md" \
    2>"$HARNESS_LOG"
HARNESS_EXIT=$?

echo "=== harness exit: $HARNESS_EXIT ==="
echo "=== stderr log ==="; cat "$HARNESS_LOG"
echo "=== review output ==="; cat "$RAW_REVIEW_FILE"
echo "=== system prompt artifact (head) ==="; head -12 "${RESULTS_DIR}/${JIRA_KEY}_system_prompt.md"
echo "=== trace events ==="; cut -c1-200 "$TRACE_FILE"
echo "=== manifest ==="; cat "$(dirname "$TRACE_FILE")/run_manifest.json"

test "$HARNESS_EXIT" -eq 0 && test -s "$RAW_REVIEW_FILE" && echo "SMOKE OK" || { echo "SMOKE FAILED"; exit 1; }
