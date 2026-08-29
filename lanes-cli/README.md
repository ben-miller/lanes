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

## Diagnostics

Every lane/session switch, and every `gather_lanes()` refresh (the CLI's `snapshot`/`signals`, and Lanes Switch's dashboard), writes microsecond-timestamped lines to `~/.local/state/lanes/perf.log` - the switch-latency timeline: when a switch is triggered (`switch.trigger`), when cursor/lane state is optimistically written and the UI socket-notified (`switch.optimistic_notify`), when the actual WezTerm/Zellij switch finishes (`switch.complete`), when Lanes Switch's socket listener receives and re-emits it (`ui.socket_received`/`ui.emitted_lane_changed`), and when the frontend actually applies it (`ui.rendered_lane_changed`). Full-refresh cost is broken out too (`gather_lanes.start`/`.subprocess_batch_done`/`.done`, `ui.get_snapshot.start`/`.done`, `ui.refresh.start`/`.done`) - this is the expensive path (one `list-panes` per running Zellij session, plus one `git status` per repo, run concurrently but each an external process round-trip), and the one most likely to explain a noticeably laggy switch. Measured cost: `zellij action list-panes` alone runs ~120-170ms per session even sequentially (vs. ~10ms for a trivial action like `current-tab-info`) - that floor is inherent to the action itself, not something request flags change, and 5 concurrent sessions only partially parallelize against each other rather than all finishing in that same ~150ms. A switch's own highlight update (`switch.trigger` -> `ui.rendered_lane_changed`) is unaffected by any of this and lands in single-digit milliseconds over the direct socket path; the refresh this section measures is what runs afterward, triggered by the `state.kdl` write every switch makes.

```
lanes diagnostics status       # on/off
lanes diagnostics on/off       # toggle logging (on by default)
lanes diagnostics logs [-f]    # tail perf.log
```

## Building and installing

```bash
cd lanes-cli
cargo build --release
ln -sf $(pwd)/target/release/lanes ~/.local/bin/lanes
```

## Architecture

The library (`src/lib.rs`) exposes a single `gather() -> Snapshot` entry point. Drivers (`src/drivers/`) each implement an `enumerate() -> Vec<Observed>` function. After all drivers run, a correlation pass annotates resources with cross-driver associations (e.g. linking a Claude session to its Zellij session and lane).

The binary (`src/main.rs`) is a thin CLI over the library. Future consumers (a dashboard, a switcher, a daemon) call `gather()` directly.
