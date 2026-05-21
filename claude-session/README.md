# claude-session

A session registry and CLI for navigating to running Claude Code sessions from anywhere - Hammerspoon, scripts, or the terminal.

## How it works

Claude Code hooks (`SessionStart` / `SessionEnd`) write and delete a small JSON record per session in `~/.claude/active-sessions/`. The `claude-session` CLI reads that registry and can navigate to any session via WezTerm + Zellij.

## CLI

```
claude-session sessions list           # list all active sessions as JSON
claude-session sessions get <id>       # get one session record
claude-session sessions switch <id>    # navigate to the session
```

Each operation maps to a REST endpoint if an HTTP layer is added later:

| CLI | REST |
|-----|------|
| `sessions list` | `GET /sessions` |
| `sessions get <id>` | `GET /sessions/:id` |
| `sessions switch <id>` | `POST /sessions/:id/switch` |

## Session record schema

```json
{
  "session_id": "abc123",
  "claude_session_name": "lanes",
  "zellij_session": "lanes",
  "zellij_pane_id": 0,
  "wezterm_tab_id": 5,
  "cwd": "/Users/bmiller/src/projects/lanes",
  "started_at": "2026-05-21T22:00:00Z"
}
```

`zellij_pane_id` is the numeric `$ZELLIJ_PANE_ID` from the hook environment. `wezterm_tab_id` is resolved at hook time by running `wezterm cli list` and matching `$ZELLIJ_SESSION_NAME` in the tab title.

## Installation

The hooks are already symlinked and `claude-session` is in `~/.local/bin`. For a fresh machine:

```bash
# Symlink hooks
ln -sf $(pwd)/hooks/session-start.sh ~/.claude/hooks/session-start.sh
ln -sf $(pwd)/hooks/session-end.sh ~/.claude/hooks/session-end.sh

# Symlink CLI
ln -sf $(pwd)/bin/claude-session ~/.local/bin/claude-session

# Add hooks to ~/.claude/settings.json (see hooks section below)
```

Add to `~/.claude/settings.json` under `"hooks"`:

```json
"SessionStart": [
  { "matcher": "", "hooks": [{ "type": "command", "command": "~/.claude/hooks/session-start.sh" }] }
],
"SessionEnd": [
  { "matcher": "", "hooks": [{ "type": "command", "command": "~/.claude/hooks/session-end.sh" }] }
]
```

## Hammerspoon integration

Hammerspoon calls the CLI via `hs.task` - it does no session logic itself:

```lua
-- Show a picker of all active Claude sessions
hs.hotkey.bind({"cmd", "ctrl"}, "c", function()
  hs.task.new("/Users/bmiller/.local/bin/claude-session", function(_, stdout, _)
    local sessions = hs.json.decode(stdout)
    if not sessions or #sessions == 0 then return end
    local choices = {}
    for _, s in ipairs(sessions) do
      table.insert(choices, { text = s.claude_session_name, subText = s.cwd, id = s.session_id })
    end
    local chooser = hs.chooser.new(function(choice)
      if not choice then return end
      hs.task.new("/Users/bmiller/.local/bin/claude-session", nil, {"sessions", "switch", choice.id}):start()
    end)
    chooser:choices(choices)
    chooser:show()
  end, {"sessions", "list"}):start()
end)
```

## Known limitations

- **Stale sessions:** If Claude crashes, `SessionEnd` won't fire and the registry file remains. Mitigation not yet implemented - manually delete `~/.claude/active-sessions/<id>.json` or add a startup cleanup step.
- **Multiple Claude sessions in one lane:** `sessions switch` navigates to whichever session's record exists; if multiple exist for the same Zellij session, pick by `session_id` or add a `--latest` flag.
- **Zellij pane navigation:** `focus-pane-with-id` with the numeric pane ID is unverified in practice - may need to fall back to `go-to-tab-name` if it doesn't work.
