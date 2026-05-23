# lanes

A context manager for your working environment. Observes your running tools (Zellij sessions, Claude sessions, browser tabs) and gathers them into a typed snapshot. The foundation for lane switching, context capture, and environment reset.

## Commands

```
lanes doctor          Check that all configured drivers are available
lanes snapshot        Dump the current environment snapshot as JSON
lanes sessions list   List active Claude sessions
lanes sessions get <id>  Get a single Claude session by ID
```

## Configuration

`~/.config/lanes/registry.toml` is the single config file. It declares which drivers to run and defines named lanes.

```toml
drivers = ["zellij", "claude"]

[[lanes]]
name = "sheetwork"
zellij_session = "sheetwork"
position = 0

[[lanes]]
name = "lanes dev"
zellij_session = "lanes"
position = 1
```

If `drivers` is omitted, all built-in drivers run.

### Drivers

| Name | What it reads | Requires |
|---|---|---|
| `zellij` | Sessions, tabs, pane commands, cwds | `zellij` on PATH |
| `claude` | Active Claude Code sessions, AI titles, state | `~/.claude/active-sessions/` registry |
| `brotab` | Firefox tabs | `bt` CLI + browser extension |

`lanes doctor` checks each configured driver and reports what's working and what isn't.

## Snapshot format

`lanes snapshot` outputs a JSON `Snapshot`:

```json
{
  "taken_at": "2026-05-23T12:00:00Z",
  "resources": [
    {
      "selector": { "kind": "terminal", "driver": "claude", "id": "<uuid>" },
      "locator": "<uuid>",
      "label": "AI-generated session title",
      "state": { "status": "busy", "detail": { "kind": "claude", "activity": "running" } },
      "cwd": "/Users/bmiller/src/projects/sheetwork",
      "extra": { "lane": "sheetwork", "zellij_session": "sheetwork" }
    }
  ]
}
```

Each resource has a `selector` (durable handle for re-finding it), a `locator` (ephemeral runtime ID, for display only), and `extra` annotations added by the correlation pass — including which lane it belongs to if one can be determined from the registry.

## Building and installing

```bash
cd lanes-cli
cargo build --release
ln -sf $(pwd)/target/release/lanes ~/.local/bin/lanes
```

## Architecture

The library (`src/lib.rs`) exposes a single `gather() -> Snapshot` entry point. Drivers (`src/drivers/`) each implement an `enumerate() -> Vec<Observed>` function. After all drivers run, a correlation pass annotates resources with cross-driver associations (e.g. linking a Claude session to its Zellij session and lane).

The binary (`src/main.rs`) is a thin CLI over the library. Future consumers (a dashboard, a switcher, a daemon) call `gather()` directly.
