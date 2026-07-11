#!/usr/bin/env bash
# Live smoke test: one real review against Bedrock.
# Manual caller-mode check. Verifies the Strands runner, effort passthrough, and model
# availability per tier. Needs real credentials.
#
# Credentials:
#   - Set AWS_REGION to the region where the Anthropic models are enabled.
#   - Either export base creds + AWS_ROLE_ARN (this script assumes the role), or have
#     an already-resolved session in the environment / an AWS profile.
#
# Usage:
#   AWS_REGION=us-east-1 AWS_ROLE_ARN=arn:aws:iam::ACCT:role/demerzel-bedrock \
#     scripts/smoke_bedrock.sh [path-to-diff-file]
set -euo pipefail

SCRIPT_DIR_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

: "${AWS_REGION:?set AWS_REGION to the Bedrock-enabled region}"

# Assume the sandbox role if an ARN is provided (mirrors CI OIDC->STS).
if [[ -n "${AWS_ROLE_ARN:-}" ]]; then
  echo "Assuming role $AWS_ROLE_ARN ..."
  CREDS_JSON="$(aws sts assume-role \
    --role-arn "$AWS_ROLE_ARN" \
    --role-session-name "demerzel-smoke-$(date +%s)" \
    --query 'Credentials' --output json)"
  AWS_ACCESS_KEY_ID="$(echo "$CREDS_JSON" | python3 -c 'import sys,json;print(json.load(sys.stdin)["AccessKeyId"])')"
  AWS_SECRET_ACCESS_KEY="$(echo "$CREDS_JSON" | python3 -c 'import sys,json;print(json.load(sys.stdin)["SecretAccessKey"])')"
  AWS_SESSION_TOKEN="$(echo "$CREDS_JSON" | python3 -c 'import sys,json;print(json.load(sys.stdin)["SessionToken"])')"
  export AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN
fi

echo "Caller identity:"; aws sts get-caller-identity || { echo "no valid credentials"; exit 1; }

CONFIG="$SCRIPT_DIR_ROOT/examples/agents/configs/demerzel-review.json"
BASE_DIR="$SCRIPT_DIR_ROOT/examples"
PROMPT_PATH="$WORK/prompt.md"
DIFF_FILE="${1:-}"

{
  echo "Review this diff and report any issues. Keep it short."
  echo '```diff'
  if [[ -n "$DIFF_FILE" && -f "$DIFF_FILE" ]]; then
    cat "$DIFF_FILE"
  else
    cat <<'EOF'
--- a/util.py
+++ b/util.py
@@ -1,3 +1,4 @@
 def divide(a, b):
-    return a / b
+    # TODO: handle b == 0
+    return a / b
EOF
  fi
  echo '```'
} > "$PROMPT_PATH"

DIFF_SIZE="$(wc -c < "$PROMPT_PATH" | tr -d ' ')"

# Point the budget preflight at a nonexistent budget so it degrades to a warning
# rather than requiring a real AWS Budgets resource for the smoke.
export DEMERZEL_BUDGET_NAME="${DEMERZEL_BUDGET_NAME:-does-not-exist-smoke}"
export DEMERZEL_RUNNER="${DEMERZEL_RUNNER:-strands}"

cd "$SCRIPT_DIR_ROOT"
uv run python3 -m demerzel.harness.bedrock \
    --config "$CONFIG" \
    --prompt-file "$PROMPT_PATH" \
    --output "$WORK/review.md" \
    --trace "$WORK/trace.jsonl" \
    --base-dir "$BASE_DIR" \
    --repo "acme/core" \
    --diff-size "$DIFF_SIZE" \
    --system-prompt-file "$WORK/system_prompt.md" \
    --artifacts-dir "$WORK/artifacts" \
    --raw-trace "$WORK/raw.jsonl"
HARNESS_EXIT=$?

echo "=== exit: $HARNESS_EXIT ==="
echo "=== review ==="; cat "$WORK/review.md"
echo "=== manifest ==="; cat "$WORK/artifacts/run_manifest.json"
test "$HARNESS_EXIT" -eq 0 && test -s "$WORK/review.md" && echo "LIVE SMOKE OK" || { echo "LIVE SMOKE FAILED"; exit 1; }
