# Per-lane window/tab targeting: `[[targets]]`

## Repo boundary

This all lives in **lanes-cli**, not a new repo. The dividing line is the
same one already written into lanes-wm's own README: a separate repo is
justified when something has no lane vocabulary, is independently reusable,
and shares no code with lanes-cli - true of lanes-wm (bundle IDs, monitor
UUIDs, presets, no concept of "lane" anywhere in it), false of everything in
this doc (`[[targets]]`, `driver`, `launch`, monitor-handle resolution,
`focus_lane()` orchestration - all inherently lane-specific, meant for
nothing but lanes-cli).

More concretely: the new drivers (`wezterm`, `obsidian`, `sourcetree`,
`vscode`, `app`) aren't a new subsystem - they're more entries in the driver
system that already exists (`zellij`, `claude`, `brotab`, per the
`drivers = [...]` registry and the "Drivers, not facets" architecture).
They get enabled/disabled the same way, dispatch from the same
`focus_lane()` loop, read the same lane config. Splitting them into a
separate repo would invent a boundary between "old drivers" and "new
drivers" that doesn't correspond to anything real.

lanes-wm's role doesn't change: the new drivers shell out to it for
placement exactly the way `activate_wezterm_tab` already shells out to
`wezterm cli` and `activate_window_facet` already shells out to `hs`.

## Summary

Replace the current `[[windows]]` (path/zone, dead syntax, never wired to
anything real) and the `[[scope]]` Terminal facet's hardcoded WezTerm/Zellij
activation with one array, `[[targets]]`, used for every app a lane cares
about focusing and/or placing.

Every entry has the same shape: a `driver` name, that driver's own
identifying fields, and an optional trailing `monitor` + `position` for
window placement (omit both to just activate/focus with no geometry change -
this is how WezTerm/Zellij pane-focus fit in, since they have no "position").

```toml
[[targets]]
driver = "<driver-name>"
# ...driver-specific fields...
monitor = "main"        # optional
position = "left-half"  # optional, requires monitor
```

`monitor` is a handle resolved against `[monitors.<handle>]` in the global
`~/.config/lanes.toml` (a flat file, sibling to the `lanes/` directory - not
`~/.config/lanes/lanes.toml` as an earlier draft of this doc said). It
already exists with `lg-left`/`lg-right` entries; `main` (the built-in
display) has been added alongside them. `position` is one of lanes-wm's
five presets (`full`/`left-half`/`right-half`/`top-half`/`bottom-half`).

## Drivers (v1)

Each driver is a small, fixed chunk of Rust in lanes-cli that turns its
fields into whatever needs to actually run - a shell command, an AX call via
`lanes-wm apply`, or both. Config authors never write a shell command or a
runtime-only ID (tab ID, window number, chat UUID) by hand.

| driver | fields | mechanism |
|---|---|---|
| `wezterm` | `session` | `wezterm cli activate-tab --tab-id <id>` - id resolved via lanes-cli's existing cached-tab-id lookup per session (was `activate_wezterm_tab`) |
| `zellij` | `session`, `pane` (optional) | zellij's own action API (was `focus_zellij_pane`/`first_pane_id`) |
| `obsidian` | `vault` | `open 'obsidian://open?vault=<vault>'`, then place via lanes-wm's focused-window selector (see below) |
| `sourcetree` | `repo` | `cd <repo> && stree` |
| `vscode` | `folder` | `code -r <folder>` (reuse window) |
| `app` | `name` | fallback for anything else: `open -a <name>`, no window/tab addressing - brings forward whichever window the app last had focused. Good enough for apps with no real driver yet. |

Explicitly not in v1, and why:
- **brotab** (browser tabs) - blocked on a macOS install bug (`bt install`
  writes its native-messaging manifest to the wrong path), and the
  title/URL/domain choice is per-tab, not something to pre-decide before a
  real use case exists.
- **Claude desktop chats** - addressed only by a UUID with no in-app way to
  copy it (only reachable via the *browser* version's address bar) - doesn't
  meet the "a human can maintain this by hand" bar the other drivers do.

## lanes-wm side: focused-window placement

Obsidian (and similar apps addressed by "make this the active window" rather
than title) need a placement mode that doesn't filter by title at all -
"place whichever window is currently focused for this app," via
`kAXFocusedWindowAttribute` on the app's AX element. This is the one change
needed in lanes-wm itself; everything else above is entirely lanes-cli-side.

## App-not-running behavior

Whether a target auto-launches its app is explicit per-target config, not a
default: `launch = true` (default `false`). If the app isn't running and
`launch` is false/unset, the target fails loud like any other unresolvable
target - no silent skip, no silent auto-open. If `launch = true`, lanes-cli
runs `open -a <app>` first, then proceeds to activate + place as usual.

Not designed yet, deliberately: how long to wait after launching before the
activate/place steps run (a freshly-launched app may not be ready
immediately). No retry/timing logic exists for this yet - it gets designed
against a real failure if/when one shows up, not speculatively now.

## Fail-hard behavior carries over unchanged

Ambiguous title matches still error rather than guess (existing lanes-wm
behavior). A driver command that fails (app not running, `stree`/`code` not
on PATH, etc.) is a per-target failure, doesn't abort the rest of the lane's
targets - same partial-failure semantics `lanes-wm apply` already has.

## Example configs, based on current real lanes

**`lanes-dev.toml`** - replaces the existing (never-functional) `[[windows]]`
entry:
```toml
[lane]
id = "lanes-dev"
name = "Lanes Dev"
active = false

[[scope]]
kind = "zellij_session"
session = "lanes"

[[scope]]
kind = "repo"
path = "~/src/projects/lanes"

[[targets]]
driver = "wezterm"
session = "lanes"
monitor = "lg-right"
position = "full"
```

**`lanes-wm.toml`**:
```toml
[[targets]]
driver = "wezterm"
session = "lanes-wm"
monitor = "main"
position = "full"
```

**`formation.toml`** - the vault name lines up with the lane name already:
```toml
[[targets]]
driver = "wezterm"
session = "formation"
monitor = "main"
position = "left-half"

[[targets]]
driver = "obsidian"
vault = "Formation"
monitor = "main"
position = "right-half"
```

**`sheetwork-planner.toml`** - shows `sourcetree` and the `app` fallback:
```toml
[[targets]]
driver = "wezterm"
session = "sheetwork-planner"
monitor = "main"
position = "left-half"

[[targets]]
driver = "sourcetree"
repo = "~/src/projects/sheetwork-planner"
monitor = "main"
position = "right-half"
```

## Open items before implementation

- ~~Populate `[monitors.*]`~~ Done: `~/.config/lanes.toml` already had
  `lg-left`/`lg-right` (an earlier draft of this doc wrongly assumed the
  file didn't exist at all - it does, at `~/.config/lanes.toml`, not
  `~/.config/lanes/lanes.toml`). `lg-left`'s UUID was stale (didn't match
  either currently-attached external) and has been corrected; `main` (the
  built-in display) was missing and has been added.
- Decide whether `[[windows]]`/`WindowPlacement`/`zone.rs` get deleted
  outright or kept temporarily during migration. Note: since `[monitors]`
  already existed with a valid `lg-right` entry, `activate_window_facet` may
  actually have been reachable for lanes using that handle - unconfirmed,
  don't assume it was always dead the way an earlier draft of this doc
  claimed.
- `focus_lane()` needs a loop over `lane.targets` that dispatches by
  `driver`, replacing today's separate Terminal-facet WezTerm/Zellij handling
  and the dead `[[windows]]` loop.
