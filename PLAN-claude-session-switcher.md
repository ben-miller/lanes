# Plan: Claude Session Switcher

Jump to any running Claude session via a global keyboard shortcut.

## How it works

Two pieces:

1. **Session registry** - Claude Code hooks write a small record to disk when a session starts, delete it when it ends. Each record contains enough location info to navigate to that pane.

2. **`claude-session` CLI** - A resource-oriented CLI for reading the registry and triggering navigation. Hammerspoon (or anything else) calls it. Designed so a REST HTTP layer could be added later with each CLI operation mapping 1:1 to an endpoint.

## What we proved works

- `wezterm cli activate-tab --tab-id N` switches WezTerm tabs reliably from Hammerspoon, as long as `$WEZTERM_UNIX_SOCKET` is set to the live socket path (`~/.local/share/wezterm/gui-sock-*`).
- `zellij --session <name> action go-to-tab-name <tab>` navigates within a Zellij session from outside. This is the key primitive - `go-to-tab` is a server-side action and works with `--session`. `switch-session` is client-side and does not.

## Session registry

**Location:** `~/.claude/active-sessions/` - one JSON file per active Claude session, named `<session_id>.json`.

**Contents:**
```json
{
  "session_id": "abc123",
  "claude_session_name": "sheetwork",
  "zellij_session": "sheetwork",
  "zellij_pane_id": 0,
  "wezterm_tab_id": 5,
  "cwd": "/Users/bmiller/src/projects/sheetwork",
  "started_at": "2026-05-18T22:00:00Z"
}
```

`$ZELLIJ_SESSION_NAME` and `$ZELLIJ_PANE_ID` are available to hooks since they inherit the terminal environment. `$ZELLIJ_PANE_ID` is a plain integer. `wezterm_tab_id` must be resolved at hook time by running `wezterm cli list` and matching `$ZELLIJ_SESSION_NAME` against the TITLE column - the title format is `<zellij-session> | <tab-name>`. `$WEZTERM_UNIX_SOCKET` may be stale if WezTerm restarted after the pane was opened, so resolve the live socket dynamically via `ls ~/.local/share/wezterm/gui-sock-* | sort -t- -k3 -n | tail -1`.

**Why a directory instead of a single file:** no concurrent write conflicts when multiple Claude sessions start at the same time.

## Hooks

Configure in `.claude/settings.json` (project-level) or `~/.claude/settings.json` (global):

```json
{
  "hooks": {
    "SessionStart": [
      {
        "type": "command",
        "command": "~/.claude/hooks/session-start.sh"
      }
    ],
    "SessionEnd": [
      {
        "type": "command",
        "command": "~/.claude/hooks/session-end.sh"
      }
    ]
  }
}
```

`session-start.sh` writes the registry file. `session-end.sh` deletes it by session ID (available in the hook payload as `session_id`).

The hook payload comes in via stdin as JSON and includes `session_id`, `cwd`, and the `source` field (`startup|resume|clear|compact`).

## CLI interface

The `claude-session` CLI follows Stripe's "resource commands" pattern: `claude-session sessions <operation>`. Each operation maps 1:1 to a REST endpoint for a future HTTP layer.

| CLI | REST | Description |
|-----|------|-------------|
| `claude-session sessions list` | `GET /sessions` | List all active sessions as JSON |
| `claude-session sessions get <id>` | `GET /sessions/:id` | Get a single session record |
| `claude-session sessions switch <id>` | `POST /sessions/:id/switch` | Navigate to the session (wezterm + zellij) |

`list` and `get` output JSON to stdout. `switch` performs the navigation side effects and exits 0 on success.

## Hammerspoon integration

Hammerspoon is a thin client - it calls the CLI via `hs.task` and does nothing smart itself. Example flow for a picker binding:

1. Call `claude-session sessions list`, parse JSON output
2. Show `hs.chooser` with session names
3. On selection, call `claude-session sessions switch <id>`

The existing Super+4-8 bindings are separate exploratory code for testing WezTerm/Zellij navigation primitives and are not part of this implementation.

## Open questions

- **Stale sessions:** If Claude crashes or the terminal is killed, `SessionEnd` won't fire and the registry file stays. Mitigation: validate the session is still alive by checking if the `claude` process with that session ID still exists before navigating. Clean up stale files on `SessionStart` (scan registry and remove records whose processes are gone).

- **`wezterm_tab_id` at hook time:** Resolved by running `wezterm cli list` with the live socket and matching `$ZELLIJ_SESSION_NAME` in the TITLE column. `$WEZTERM_UNIX_SOCKET` may be stale - use dynamic socket resolution (see registry section).

- **Multiple Claude sessions in one lane:** A lane could have more than one Claude session running in different Zellij panes. The registry handles this fine (multiple files with the same `zellij_session`), but the navigation step needs to pick one - probably most recently started.

- **Zellij pane navigation:** `zellij action focus-pane-with-id <pane-id>` takes a numeric ID. `$ZELLIJ_PANE_ID` is a plain integer so this should work directly - needs verification in practice.
