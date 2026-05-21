#!/usr/bin/env bash
set -euo pipefail

export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"

STATE="$1"
PAYLOAD=$(cat)
SESSION_ID=$(echo "$PAYLOAD" | jq -r '.session_id')
FILE="$HOME/.claude/active-sessions/$SESSION_ID.json"

[[ -f "$FILE" ]] || exit 0

tmp=$(mktemp)
jq --arg state "$STATE" '. + {state: $state}' "$FILE" > "$tmp" && mv "$tmp" "$FILE"
