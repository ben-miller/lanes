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
config. At the time this was written that lived at `~/.config/lanes.toml`,
a flat file sibling to the `lanes/` directory; **as of 2026-10-14 it's moved
to `~/.config/lanes/lanes.toml`**, matching the broader `~/.config`
convention of one app directory holding both its main config file and any
per-item subdirectory (e.g. Zellij's own `~/.config/zellij/config.kdl`
beside `layouts/`) - see `lanes-cli/src/config.rs`'s `load_global_config`
for the current, authoritative path. It already exists with
`lg-left`/`lg-right` entries; `main` (the built-in display) has been added
alongside them. `position` is one of lanes-wm's five presets
(`full`/`left-half`/`right-half`/`top-half`/`bottom-half`).

**Also as of 2026-10-14:** the per-lane `[[targets]]` facet file (called
`targets.toml` throughout the rest of this doc) was renamed to
`layout.toml` - the `Target`/`TargetDriver` Rust vocabulary is unchanged,
only the TOML filename. The top-level global defaults described above also
moved out of `lanes.toml` into their own sibling `~/.config/lanes/layout.toml`,
same file name as the per-context override - see the lanes-cli README for
the current shape.

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
| `boards` | `project` | `open 'boards://open?project=<project>'` (Boards, `com.bmiller.boards` - a standalone kanban app, own repo at `~/src/projects/boards`), same focused-window placement as `obsidian`. `project` matches a folder name under Boards' own data root exactly as its `list_projects` returns it - case-sensitive, no fuzzy matching. Added post-v1, 2026-10-07. |
| `sourcetree` | `repo` | `cd <repo> && stree` |
| `vscode` | `folder` | `code -r <folder>` (reuse window) |
| `app` | `name`, `bundle_id` (optional) | fallback for anything else: `open -a <name>`, no window/tab addressing - brings forward whichever window the app last had focused. `bundle_id` unlocks placement (`monitor`/`position`); without it, activate-only. |

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

## Raising a placed target

`raise_app = target.monitor.is_none()` was the original rule: raise an
`app` target only when it has no placement, since a placed target's window
gets correctly repositioned via `--focused` whether or not it's frontmost -
true for WezTerm/Firefox, which don't need to be seen immediately after a
switch. It's false for a "peek" app (a Trello board glance-view): its whole
purpose is to be seen, so a placed-but-never-raised one just sits hidden
behind whatever else occupies that screen. `Target.raise` (default false)
overrides the inferred default - `raise_app = target.monitor.is_none() ||
target.raise`. Found by testing live: the Japanese peek app, given a
`monitor` for the first time, stopped showing at all until this was added.

## Minimizing instead of placing (added 2026-10-14)

`Target.minimized: Option<bool>`, mutually exclusive with `monitor`/
`position` - for a window an app insists on keeping open but that's never
actually useful (Wispr Flow, the motivating case: it has no window at rest,
only while actively in use, and that window has no reason to ever be seen).
`apply_targets` short-circuits on it before the activate/raise step - no
raising a window right before hiding it again - and sends lanes-wm a
minimize-only request (`{"app": "...", "minimized": true}`), which sets
`kAXMinimizedAttribute` directly rather than moving/resizing. See the
lanes-cli and lanes-wm READMEs for the current field-level detail.

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

## Cascading defaults

`[[targets]]` in the global `~/.config/lanes.toml` apply to every lane,
merged with (and overridable by) that lane's own `[[targets]]` - same idea
as CSS: a default applies everywhere until something more specific
overrides it. A lane's own target overrides a default of the same identity
(driver kind, plus app name for the `app` driver) rather than duplicating
it; anything else from both lists is kept (`merge_targets` in `config.rs`).

```toml
# ~/.config/lanes.toml
[[targets]]
driver = "wezterm"
monitor = "lg-right"
position = "full"

[[targets]]
driver = "app"
name = "Firefox"
bundle_id = "org.mozilla.firefox"
monitor = "lg-left"
position = "full"
```

For this to mean the same thing across lanes with different Zellij session
names, `wezterm`/`zellij`'s `session` field is optional - omitted, it
defaults to the owning lane's own terminal session at apply time.

`app` targets also gained an optional `bundle_id`, needed for placement:
`target_bundle_id` only knows a bundle id for an `app` target that supplies
one (Firefox does; a Trello peek app with no placement doesn't need to).

### Firefox profiles (tried, reverted)

A `firefox-profile` driver briefly existed here, switching which Firefox
profile occupied a target by resolving a specific PID (profiles.ini's
`Default=` doesn't reliably say which profile a bare `firefox` launch uses
when `StartWithLastProfile=1` is set - "last used," not "default," decides
it - so profile detection kept misfiring and spawning stray windows). Removed
2026-10-15: not worth the ongoing fragility: the config now just targets the
single "Original profile" `app`-driver Firefox everywhere, same as any other
`app` target.

## Two bugs found by actually using this on real lane switches

Both only showed up once tested through the real hotkey-driven switch path
(`sessions switch`/`next`/`prev`), not through `lanes focus` alone - worth
remembering for the next feature like this.

1. **Every target's activation step raised its app unconditionally**, even
   one that was only being placed. `lanes-wm`'s `--focused` placement
   resolves and repositions a window correctly whether or not its app is
   frontmost, so a placement-only target never needed to steal focus in the
   first place. Fixed: `activate_target` only raises an `app` target when
   it has *no* placement at all (a Trello peek app, whose whole purpose is
   to be seen) - this is what was causing a visible flicker through every
   app in a lane on every switch.
2. **Targets were applied before the terminal/Claude session's own focus
   step**, so whichever target happened to run last stole final focus - the
   terminal never reliably ended up frontmost. Fixed by reordering: targets
   first, terminal focus (WezTerm tab + Zellij pane) always last, in both
   `focus_lane` and `switch_claude_session`.

## Open items before implementation

- ~~Populate `[monitors.*]`~~ Done: `~/.config/lanes.toml` already had
  `lg-left`/`lg-right` (an earlier draft of this doc wrongly assumed the
  file didn't exist at all - it does, at `~/.config/lanes.toml`, not
  `~/.config/lanes/lanes.toml`). `lg-left`'s UUID was stale (didn't match
  either currently-attached external) and has been corrected; `main` (the
  built-in display) was missing and has been added.
- ~~Decide whether `[[windows]]`/`WindowPlacement`/`zone.rs` get deleted~~
  Done: deleted outright, confirmed fully dead once `activate_window_facet`
  was removed.
- ~~`focus_lane()` needs a loop over `lane.targets`~~ Done, and extended to
  `switch_claude_session` too - see above, that was the actual real-switch
  entry point and initially didn't call `apply_targets` at all.
