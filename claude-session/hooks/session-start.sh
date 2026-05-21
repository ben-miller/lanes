#!/usr/bin/env bash
set -euo pipefail

export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"

PAYLOAD=$(cat)
SESSION_ID=$(echo "$PAYLOAD" | jq -r '.session_id')
CWD=$(echo "$PAYLOAD" | jq -r '.cwd')

REGISTRY_DIR="$HOME/.claude/active-sessions"
mkdir -p "$REGISTRY_DIR"

# Find the live WezTerm socket - $WEZTERM_UNIX_SOCKET may be stale if WezTerm restarted
WEZTERM_SOCKET=$(ls "$HOME/.local/share/wezterm/gui-sock-"* 2>/dev/null | sort -t- -k3 -n | tail -1 || true)

# Resolve WezTerm tab ID by matching Zellij session name in the tab title ("session | tab-name")
WEZTERM_TAB_ID="null"
if [[ -n "${WEZTERM_SOCKET:-}" && -n "${ZELLIJ_SESSION_NAME:-}" ]]; then
    TAB_ID=$(WEZTERM_UNIX_SOCKET="$WEZTERM_SOCKET" wezterm cli list 2>/dev/null \
        | grep "${ZELLIJ_SESSION_NAME} |" \
        | awk '{print $2}' \
        | head -1 || true)
    [[ -n "${TAB_ID:-}" ]] && WEZTERM_TAB_ID="$TAB_ID"
fi

jq -n \
    --arg session_id "$SESSION_ID" \
    --arg claude_session_name "${ZELLIJ_SESSION_NAME:-}" \
    --arg zellij_session "${ZELLIJ_SESSION_NAME:-}" \
    --argjson zellij_pane_id "${ZELLIJ_PANE_ID:-null}" \
    --argjson wezterm_tab_id "$WEZTERM_TAB_ID" \
    --arg cwd "$CWD" \
    --arg started_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    '{
        session_id: $session_id,
        claude_session_name: $claude_session_name,
        zellij_session: $zellij_session,
        zellij_pane_id: $zellij_pane_id,
        wezterm_tab_id: $wezterm_tab_id,
        cwd: $cwd,
        started_at: $started_at
    }' > "$REGISTRY_DIR/$SESSION_ID.json"
