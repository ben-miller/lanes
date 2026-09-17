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
| `app` | `name`, `bundle_id` (optional) | fallback for anything else: `open -a <name>`, no window/tab addressing - brings forward whichever window the app last had focused. `bundle_id` unlocks placement (`monitor`/`position`); without it, activate-only. |
| `firefox-profile` | `profile` | switches which Firefox profile occupies a target, added after v1 - see its own section below |

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

Note: the global default above was later replaced by `firefox-profile` (see
below) - Firefox itself is now switched per profile, not just placed once.

## Firefox profiles

Firefox's newer built-in "Profiles" feature (distinct from the legacy
`-P`/`profiles.ini` system, which it doesn't touch at all) lets multiple
profiles run simultaneously, each as a genuinely separate process - but
every one of them shares the same bundle ID (`org.mozilla.firefox`), which
none of the existing drivers could disambiguate.

```toml
[[targets]]
driver = "firefox-profile"
profile = "Development"
monitor = "lg-left"
position = "full"
```

`profile` is the name you gave it in Firefox's own profile switcher.
Resolution (`lanes-cli/src/lib.rs`):
1. Look up the profile's on-disk path by name, from the per-install SQLite
   database under `~/Library/Application Support/Firefox/Profile Groups/`
   (queried via the `sqlite3` CLI - not `profiles.ini`, which the new
   Profiles feature doesn't write to at all).
2. Match a running process's `--profile <path>` launch argument via `ps`,
   filtered to the main `firefox` binary specifically - its helper
   processes (plugin-container, gpu-helper, crashhelper) all inherit and
   echo the same `-profile <path>` argument, so a plain substring match
   without that filter finds those instead (caught by a test, not by
   inspection - worth remembering for the next process-scanning driver).
   Falls back to matching a bare `firefox` process with no `--profile` flag
   for the *default* profile specifically, since that's how it normally
   launches.
3. Launch it (`firefox --profile <path>`) only if `launch = true` - same
   opt-in default-off behavior every other driver has.

This is what motivated **lanes-wm's `--pid`**: an alternative to `--app`
that targets one specific process directly, since bundle-ID resolution
can't tell multiple Firefox profiles apart. `apply_targets` uses whichever
identity a driver actually resolved (a bundle ID for everything else, the
PID `firefox-profile` resolved as a byproduct of its own activation step).

It also motivated **lanes-wm's `--raise`**: placement (`setFrame`) doesn't
touch z-order, so an app placed at the same coordinates on every call - the
whole point of "whichever profile is active occupies this exact screen
region" - leaves whichever instance was already on top still on top,
hiding the newly-placed one underneath. `--raise` activates the exact PID a
window was resolved from (`NSRunningApplication`, not `open`, which can't
target one instance among several sharing a bundle ID either). Every
PID-resolved target gets `raise: true` automatically in `apply_targets` -
that's currently synonymous with "needs raising," since PID resolution
only exists for the same-screen-slot-competition problem in the first
place. Note: `NSApplicationActivationOptions::ActivateIgnoringOtherApps` is
deprecated on macOS 14+ and documented as having no effect - empty options
is what actually works now.

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
