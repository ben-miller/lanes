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

`~/.config/lanes/lanes.toml` is the global config - a plain preferences file living alongside the context directories, same as e.g. Zellij's own `~/.config/zellij/config.kdl` sits beside its `layouts/`. It declares which drivers to run and the order contexts should sort in:

```toml
drivers = ["zellij", "claude"]
order = ["sheetwork-sandbox", "lanes-dev", "infra"]
```

If `drivers` is omitted, all built-in drivers run. A context missing from `order` still works at runtime (it sorts alphabetically after the listed ones), but `lanes doctor` flags it.

`~/.config/lanes/layout-templates/default.toml` (same `[[targets]]` shape as a context's own `layout.toml` below) supplies window-placement defaults applied to every context, unconditionally. A context's own `layout.toml` can also opt into one named `layout-templates/<name>.toml` via `layout_template = "<name>"`, cascading in between. Each level overrides the previous one CSS-style: an entry for a given driver overrides the matching one from the level below rather than duplicating it; anything it doesn't mention still comes from there. See `DECISIONS.md` for why this is single-inheritance (no mixins) and why contexts/templates live in separate directories.

### Contexts

Each context is a directory under `~/.config/lanes/contexts/<id>/`. The directory name *is* the identity - there's no separate id field anywhere. Everything inside is a facet file, and every facet is optional:

```
~/.config/lanes/
  lanes.toml           # global config - see above
  layout-templates/
    default.toml       # window-placement defaults, applied to every context
    dev.toml           # a named template a context can opt into via
                        # layout_template = "dev"
  contexts/
    job-hunting/
      lane.toml         # switcher-UI view: display name, active/cyclable
      tab.toml          # Zellij session name + repo/cwd scope
      layout.toml       # window-placement targets (non-Boards) - overrides
                         # default.toml (and an opted-into template, if any)
      board.toml        # the Boards projection (a single `driver = "boards"` target)
    lanes-wm/
      lane.toml
      tab.toml
      # no layout.toml or board.toml - this context doesn't need them
```

A context with only `tab.toml` gets a real Zellij session without ever showing up in the Lanes Switch dashboard - useful for background contexts you don't want cluttering the switcher. A missing or malformed facet file never breaks the rest of the context (or the rest of the config); it just falls back to its default and (if malformed) prints a warning.

Example `lane.toml`:

```toml
name = "Job Hunting"
active = true
```

Example `tab.toml`:

```toml
# Whether the WezTerm tab should be open - ztabs' own concern (see the
# infra repo's `ztabs` command), independent of lane.toml's `active`
# (switcher visibility). Defaults to true when absent. The two can
# legitimately disagree: a context can be hidden from the switcher while
# still having an open terminal tab, or vice versa.
active = true

[[scope]]
kind = "repo"
path = "/Users/me/src/job-hunting"

[[scope]]
kind = "zellij_session"
session = "job-hunting"
```

Example `layout.toml` (same shape whether this is a context's own `contexts/<id>/layout.toml`, a `layout-templates/<name>.toml`, or `layout-templates/default.toml` - see the "Contexts" section above for the cascade):

```toml
[[targets]]
driver = "obsidian"
vault = "job-hunting"
monitor = "lg-left"
position = { cols = 3, col = [1, 2] }
```

### Target drivers

Each target's `driver` picks one of these; fields beyond it are driver-specific:

| driver | fields | mechanism |
|---|---|---|
| `wezterm` | `session` (optional, defaults to the lane's own terminal session) | `wezterm cli activate-tab` |
| `zellij` | `session` (optional), `pane` (optional) | zellij's own action API |
| `obsidian` | `vault` | `open 'obsidian://open?vault=<vault>'`, placed via lanes-wm's focused-window selector |
| `boards` | `project` | `appctl` socket `open_project` on the running instance (dev or installed), placed by pid; falls back to `open 'boards://open?project=<project>'` - matches a folder name under Boards' own data root exactly, case-sensitive |
| `sourcetree` | `repo` | `cd <repo> && stree` |
| `vscode` | `folder` | `code -r <folder>` (reuse window) |
| `app` | `name`, `bundle_id` (optional) | `open -a <name>`; `bundle_id` unlocks placement, otherwise activate-only |

`boards`'s instance choice is `appctl`'s own default policy: one live `dev-*`
instance wins, else the `installed` copy, else fall back to `open
'boards://...'` (which cold-launches the installed app, as before). This
means a `just dev` Boards build now works with Lanes without
`just install-dev` first - no separate config needed to pick it up.

A placed target (`monitor` set) only raises its app if `raise = true` - it's correctly repositioned via lanes-wm's focused-window selector either way, so stealing focus is opt-in (needed for a "peek" app whose whole point is to be seen immediately, e.g. a Trello board glance-view).

A target can minimize instead of place - `minimized = true`, no `monitor`/`position` (mutually exclusive with them). Good for a window an app insists on keeping open but that has no actual use, e.g. Wispr Flow's:

```toml
[[targets]]
driver = "app"
name = "Wispr Flow"
bundle_id = "com.electron.wispr-flow.accessibility-mac-app"
minimized = true
```

At most one target per lane can be marked `primary = true` - the one thing a lane switch should end up visually looking at, overriding the terminal's usual claim to that. Granularity (which pane, which vault note, ...) is never primary's own concern - it's whatever that driver's own fields already express (Zellij's `pane`, say), so marking something primary never needs new fields of its own:

```toml
[[targets]]
driver = "obsidian"
vault = "Planning"
primary = true
```

`lanes doctor` fails if a lane ends up with more than one target marked primary - never guessed, never silently resolved.

Example `board.toml` (a single target, not an array - the Boards projection for this context):

```toml
driver = "boards"
project = "job-hunting"
```

### Drivers

| Name | What it reads | Requires |
|---|---|---|
| `zellij` | Sessions, tabs, pane commands, cwds | `zellij` on PATH |
| `claude` | Active Claude Code sessions, AI titles, state | `~/.claude/active-sessions/` registry |
| `brotab` | Firefox tabs | `bt` CLI + browser extension |
| `shell` | Long-running commands that finished / failed | shell hook installed (see below) - **opt-in**, not run unless listed |

`lanes doctor` checks each configured driver and reports what's working and what isn't.

### `shell` driver - long-running command signal

Surfaces a chip when a foreground shell command finishes (or fails) after
running longer than a threshold - "your build/test/deploy is done, go
look." Opt in by adding `shell` to the `drivers` list.

The detection is a shell hook (the shell-specific half of this driver;
everything in Rust is shell-agnostic). Install it with one line in
`~/.config/fish/config.fish`:

```fish
lanes shell-init fish | source
```

The hook writes one JSON record per pane to
`~/.local/state/lanes/shell/<session>--<pane>.json` when a command exits
after running longer than `LANES_SHELL_LATCH_SECS` (default 30). A
command killed by a signal (Ctrl-C'd dev server) writes nothing; a fast
command writes nothing. The `shell` driver reads those records, reconciles
them against live Zellij panes, and emits a `command` / `done` or
`command` / `failed` signal - visible in the dashboard, clickable to jump
to the pane, and one-way dismissable in edit mode (or from the click
overlay). Running the next command in that pane also clears it.

Failure-isolated: a panic in the driver, a malformed record, or a missing
state dir degrades to "no command chips this refresh," never a broken
snapshot. `lanes doctor` reports whether the hook is installed and
records are landing.

Phase 2 (see `PLAN-shell-signal.md`) will add a live `running` chip and
`bash`/`zsh` hooks.

## lensd

Lanes' rule set for the lensd daemon, `lanes.dl`, lives centrally in
`~/src/dev-infra/rules/` (it combines Lanes' contexts with Zellij, WezTerm,
Claude, git and appctl facts, so it belongs to no single project; see
`~/src/dev-infra/PLAN-lensd-facts.md`); lensd's config lists that directory
and serves it on
`~/.local/state/lensd/lanes.sock`. It exposes `lanes_session`,
`lanes_session_running`, `lanes_claude` and `lanes_command` rows, among
others.

This is the only path left that turns facts into lane signals.
`gather_lanes()` - a second, independently-drifting implementation that did
the same thing without lensd, by shelling out to git/zellij/wezterm and
reading the claude/shell registry files directly - is gone; it was the
confirmed root cause of at least two bugs needing duplicate fixes. `lanes
snapshot`/`lanes signals` and Lanes Switch's dashboard all go through
`lensd::snapshot_from_mirror` now, which means `lanes snapshot`/`lanes
signals` require a live lensd connection - they used to work standalone.

`src/lensd.rs` holds the client. `lensd::Client::start(path, on_change)` keeps a
`Mirror` of the rows current on a background thread, reconnects with backoff
(100ms doubling to 5s), bumps `version()` on every change and calls
`on_change`. `current()` is `None` while lensd is not running: there is no
stale data and no fallback.

`lensd::snapshot_from_mirror(cfg, mirror)` builds a `LanewiseSnapshot` from the
live rows plus Lanes' lane config. Layout (lane order, scope) comes from the
config; panes and signals come from the `lanes_*` relations in
`~/src/dev-infra/rules/lanes.dl`, and each lane's repo watch is derived there too, via
`lane_watch_repo` (a second, non-colliding name for the same `watch_repo`
control relation `~/src/dev-infra/rules/repos.dl` uses for repos no lane declares
- see lensd's own README for why they can't share one relation name).
Policy stays in Lanes: the shared `correct_signals` pass sets
`cyclable`, `visible`, `lifecycle` and the Awaiting-to-Ready upgrade, and
dismissals and latched command state still come from `state.kdl`.
`SessionMissing` is "session exists but `lanes_tab` has no WezTerm tab for
it". Panes are ordered top to bottom, then left to right. Gap: cycling and tab
activation still use the cached tab id, so `cyclable` (live tab) can disagree
with what activation does.

**Stale build chips** (`build` kind): `lanes_build_changed` /
`lanes_build_rebuilt` in `lanes.dl` join lensd's `appctl` facts (running
app instances and their build identity, `~/src/dev-infra`) to the lane
whose repo is the app's own. The repo facet then shows `build stale ·
<app> <instance> · <changed sources>` when a source the running binary was
built from has changed on disk (including a newer dev-infra rolled out
into its `Cargo.lock`), or `build relaunch · <app>
<instance>` when the exe was rebuilt or reinstalled since launch. Warning
urgency, always visible. See `../PLAN-build-chip.md`.

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

Every lane/session switch, and every lensd-backed refresh (the CLI's `snapshot`/`signals`, and Lanes Switch's dashboard), writes microsecond-timestamped lines to `~/.local/state/lanes/perf.log` - the full keystroke -> tab change -> Lanes Switch UI update timeline:

1. `keystroke` - the hypo+J/K hotkey firing, logged from Hammerspoon itself (`hs-profiles/lanesswitch.lua` in infra) before it spawns anything - the only point that captures everything upstream of the `lanes` binary itself (hs.task dispatch, process fork/exec, `bash -l`). In practice that upstream overhead turned out to be negligible (~10-20ms, measured directly) - the real gap between `keystroke` and `switch.trigger` was `cycle_claude_session()`'s own work (see `cycle.*` below), not anything before the binary started.
2. `cycle.enumerated` / `.filtered` / `.ambiguous_sessions` / `.positions_resolved` / `.target_resolved` - only logged for `sessions next`/`prev` (cycling, not a direct `sessions switch`): enumerating live Claude sessions, filtering to reachable lanes, deciding which Zellij sessions actually need a `list-panes` call to break a same-lane tie (`sessions_needing_pane_positions`), running those calls, then picking the cycle target. This used to run one `list-panes` call per distinct live Zellij session unconditionally and sequentially, ahead of every single switch - `.positions_resolved`'s jump from `.ambiguous_sessions` is what that now-restricted, now-concurrent cost looks like.
3. `switch.trigger` / `switch.optimistic_notify` - cursor/lane state being written + the UI socket-notified.
4. `switch.tab_activated` / `switch.pane_focused` / `switch.complete` - the actual tab change: WezTerm's `activate-tab` and Zellij's `focus-pane-id`, logged separately so a slow switch shows which of the two is responsible, not just their total.
5. `ui.socket_received` / `ui.emitted_lane_changed` / `ui.rendered_lane_changed` - Lanes Switch's socket listener receiving and re-emitting the change, and the frontend actually applying it (the fast highlight-only path, not a full refresh).

Full-refresh cost is broken out too (`ui.get_snapshot.start`/`.done`,
`ui.refresh.start`/`.done`) - `ui.get_snapshot` wraps
`lensd::snapshot_from_mirror`, which reads from the already-live `Mirror`
kept current by a background thread rather than making any subprocess
calls of its own, so this is now cheap. (It used to be the expensive path,
back when it ran one `list-panes` per running Zellij session plus one `git
status` per repo on every single refresh - `gather_lanes()`'s own
subprocess-batching, since replaced entirely by lensd's live facts.) A
switch's own highlight update (`switch.trigger` -> `ui.rendered_lane_changed`)
lands in single-digit milliseconds over the direct socket path, independent
of the refresh this section measures, which runs afterward, triggered by
the `state.kdl` write every switch makes.

```
lanes diagnostics status       # on/off
lanes diagnostics on/off       # toggle logging (on by default)
lanes diagnostics logs [-f]    # tail perf.log
```

## Lanes Switch window state

`~/.local/state/lanes/state.kdl` is the single source of truth for Lanes Switch's window state - `switch-pinned`, `edit-mode`, `show-inactive`, plus the focused lane/session cursor. The CLI only ever writes this file; the running Tauri app reacts to it two ways:

- **Direct socket** (`~/.local/state/lanes/switch.sock`) - the low-latency path for `show`/`hide`/`edit-mode:`/lane-changed, used when the app is already running. `lanes show-switch`/`hide-switch`/`toggle-edit-mode`/`sessions next --show` all ping this.
- **`state.kdl` fs-watcher** - the fallback for when the app wasn't running to catch the socket message. It syncs the tray checkboxes and the dashboard to whatever the file now says.

Two rules keep the two paths from fighting each other:

1. **Writes are atomic** (`state::atomic_write` - write a sibling `state.kdl.tmp`, then `rename(2)`). The fs-watcher reads the file the instant it sees a change event; a plain truncate-then-write left a window where it read a half-written (or momentarily empty) file, parsed nothing, and acted on every field's default value. That's what made `edit-mode` flicker `false` then `true` again mid-switch, and Lanes Switch's window jump back up on the bounce.
2. **Only deliberate actions raise the window.** Enabling edit mode from hypo+E or the tray checkbox brings the window to the front (the shortcut is meant to work from anywhere); the fs-watcher path never does. Otherwise a switch - which rewrites `state.kdl` - would drag the window back up right after the Control-Option release hid it.

The hypo+J/K window auto-hides on Control-Option release regardless of edit mode (only `switch-pinned` keeps it up). Hammerspoon (`hs-profiles/lanesswitch.lua` in infra) owns that release watcher and always sends `hide-switch`.

**Non-edit mode is the cycle list.** The dashboard only renders signal chips that hypo+J/K would actually land on - i.e. `signal.cyclable` (a live Claude session, in an active + reachable lane, not individually excluded). Git chips (pending commit, non-default branch), lanes chips (session missing), and disabled Claude sessions are hidden until you turn on edit mode, which is where you manage what's in the cycle. An active lane whose chips are all hidden still shows as a row (a valid plain-focus click target), labelled "no signals".

## Building and installing

```bash
cd lanes-cli
cargo build --release
ln -sf $(pwd)/target/release/lanes ~/.local/bin/lanes
```

## Architecture

The library (`src/lib.rs`) exposes a single `gather() -> Snapshot` entry point. Drivers (`src/drivers/`) each implement an `enumerate() -> Vec<Observed>` function. After all drivers run, a correlation pass annotates resources with cross-driver associations (e.g. linking a Claude session to its Zellij session and lane).

The binary (`src/main.rs`) is a thin CLI over the library. Future consumers (a dashboard, a switcher, a daemon) call `gather()` directly.

### Spawning fire-and-forget children: always `spawn_detached`

Any launched app or raised window (Firefox, `open -a <app>`, …) that `lanes` doesn't wait on must go through `spawn_detached` in `lib.rs`, never a bare `.spawn()`. `Command`'s default stdio is `inherit()` - fine when you run `lanes` interactively, but Hammerspoon invokes it via `hs.task.new`, which owns a pipe around `lanes`' stdout/stderr to capture output. A long-lived child that inherits those fds (a GUI app that outlives `lanes` for the rest of the session) holds the pipe's write end open indefinitely; `lanes` and bash both exit, but the pipe never sees EOF. `hs.task`'s internals eventually do a synchronous drain-to-EOF read on **Hammerspoon's main thread**, which then hangs forever - every hotkey stops responding, with no crash and no log line anywhere to explain it (confirmed via `sample <hammerspoon-pid>`, which showed the main thread parked in `-[NSConcreteFileHandle readDataOfLength:]`). `spawn_detached` redirects all three streams to `/dev/null` so the child never shares our fds.
