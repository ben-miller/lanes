# Notes: Boards disappears entirely on single-monitor lane switches

Handoff for another session - this investigation was started in the wrong
session and got interrupted partway through. Nothing was changed; this is
read-only findings so far.

## Symptom (as reported)

User is currently on a single monitor (laptop built-in display only, no
externals attached). Observed:

- Switching directly to the `boards-dev` lane: the Boards app takes up
  (per the user) "the whole screen" - clearly visible, on top.
- Switching away from `boards-dev` to another lane: Boards doesn't just
  get covered by the new lane's WezTerm window - the user reports seeing
  the **desktop background** for a moment, even though Boards "was just
  taking up the whole screen." That's stronger than "covered" - it implies
  Boards' window (or whatever was in that screen region) is briefly absent
  entirely, not just behind something else.

## Confirmed so far

- `lanes-cli`'s `focus_lane` (`src/lib.rs`) calls `apply_targets` (which
  activates + places + raises every target in the new lane, including a
  board target with `raise = true`) **before** `land_lane` (which raises
  the terminal, then the lane's primary target if it isn't the terminal).
  Comment at the call site: targets are applied first "so doing this first
  guarantees land_lane's own choice ... gets the true last word." So on
  switching to `boards-dev` specifically, Board ends up raised *last*
  within that lane's own switch (it's `boards-dev`'s primary target, not a
  terminal) - consistent with it ending up fully on top.
- `layout-templates/default.toml` has an explicit, deliberate single-monitor
  overlap: WezTerm's `position_solo = { cols = 4, col = [2, 4] }` (right
  3/4) and Boards' own `board.toml` `position_solo = { cols = 4, col = [1,
  3] }` (left 3/4). **Union of these two ranges is columns 1-4, i.e. the
  full width** - so in principle there should be no gap revealing desktop,
  just WezTerm covering Boards in the overlapping middle half. This is why
  "I can see the desktop" doesn't fully match the "it's just covered"
  theory - if both windows are placed correctly, nothing should show
  through.
- `effective_destination` (`src/lib.rs`) switches both these targets to
  `("main", position_solo)` exactly when `attached_monitor_count() ==
  Some(1)` (confirmed true right now: `lanes-wm monitors --compact` →
  one `Built-in Display`, uuid `37D8832A-2D66-02CA-B9F7-8F30A301B230`).
  `lanes.toml`'s `[monitors.main]` uuid matches this exactly - so it's not
  a stale/unattached-monitor-uuid placement bug landing the window
  off-screen.

## Not yet explained / next steps

- Why the desktop shows through at all, given the two `position_solo`
  ranges should fully tile the screen width between them. Possible
  avenues, none checked yet:
  - A transient minimize/hide step. `lanes-wm`'s `AxElement::set_minimized`
    (via `kAXMinimizedAttribute`) is only called for an explicit
    `minimized` request in a placement JSON - worth confirming neither
    Boards' nor WezTerm's target ever sets `minimized` as a side effect,
    and that `apply`'s batch processing has no step that minimizes before
    moving.
  - `AxElement`'s `AXEnhancedUserInterface` handling in `set_frame` (around
    `lanes-wm/src/main.rs:140-175`) - some apps toggle this during a frame
    change; worth checking whether it has any visible side effect
    (briefly hiding/redrawing the window) rather than just being a
    compatibility shim.
  - A real timing gap: if `lanes-wm apply`'s batch processes requests
    sequentially and Boards' window is *moved* (not just raised) as part
    of the new lane's switch (e.g. if the new lane's board target also
    repoints the same physical Boards window - `driver = "boards"` targets
    across lanes all control the one running Boards app instance), there
    could be a brief window where the move is in flight and nothing
    valid is at that screen location yet.
  - macOS Spaces: if Boards' window ended up assigned to a different Space
    than WezTerm's (e.g. via Dock icon "Options > Assign To", or some
    other mechanism), switching between them could trigger a Space
    transition animation that shows desktop briefly. Worth asking the user
    to check Mission Control / each app's Dock "Options" assignment, and
    whether "Displays have separate Spaces" is relevant here (shouldn't be,
    single display, but worth ruling out).
- **Logging gap discovered along the way**: `lanes-cli`'s
  `run_lanes_wm_apply` (`src/lib.rs`) pipes `lanes-wm apply`'s stderr but
  only surfaces it when the process exits non-zero - on a successful run
  (the normal case here), `lanes-wm`'s own `env_logger`-based debug/info
  logs (e.g. `log::info!("applying {} placement(s)", ...)` in
  `lanes-wm/src/main.rs`) are silently discarded. Running `RUST_LOG=debug
  lanes focus <id>` directly produces no visible output because of this -
  confirmed empirically. To actually see `lanes-wm`'s own log output during
  a real switch, either:
  - call `lanes-wm apply`/`place` directly (bypassing `lanes-cli`) with the
    exact JSON `apply_targets` would have sent, with `RUST_LOG=debug` set
    on that direct invocation, or
  - temporarily change `run_lanes_wm_apply` to always print `lanes-wm`'s
    stderr (not just on failure) for one investigative run.
- No screen-access tooling (computer-use) was available in the session
  that started this, so nothing was directly observed on screen - this is
  all from reading code and querying read-only state (`lanes-wm monitors`,
  `lanes current`, config files). A session with computer-use access (or
  willingness to have the user screen-record a switch) would make this
  much faster to pin down than more code reading.

## Relevant files

- `lanes-cli/src/lib.rs`: `focus_lane`, `apply_targets`, `land_lane`,
  `effective_destination`, `resolve_placement_json`, `run_lanes_wm_apply`.
- `~/.config/lanes/layout-templates/default.toml`: the single-monitor
  overlap comment and both `position_solo` values.
- `~/.config/lanes/contexts/boards-dev/board.toml` and any other context's
  `board.toml` with `raise = true` / `position_solo` set (e.g.
  `dev-infra`'s, currently pointed at project "Source Control").
- `lanes-wm/src/main.rs`: `AxElement::set_frame`, `set_minimized`,
  `enhanced_user_interface`, `place`, `apply`.

## What the user actually wants, longer-term

Originally raised as "let's fix some wm weirdness I'm seeing due to using
a single monitor" - the deliberate WezTerm-over-Boards overlap in
`default.toml` was accepted as a tradeoff when single-monitor use was
mostly theoretical; now that it's real daily use, it may be worth revisiting
regardless of what's causing the "desktop visible" symptom specifically -
e.g. not overlapping at all on a single monitor (split the screen instead
of both claiming ~75%), or not raising a non-primary board target at all
on a switch where it isn't the thing being switched to.

## Resolved (2026-10-27): stranded Spaces, not the layout

The cause was macOS Spaces. With the external monitors unplugged, the
built-in display had two Spaces: its own (3: Boards, Console, benoxer-app)
and one that had belonged to an external monitor (7: WezTerm, Lanes
Switch). Cmd-Tab to Boards jumped to Space 3 (Boards and Console appeared
together, without WezTerm); back to WezTerm jumped to Space 7 (they
vanished together, the desktop showing in the quarter WezTerm doesn't
cover). Hiding Lanes Switch let macOS activate some other app, often
Console, which jumped Spaces too (the "Ctrl+Opt+\ brings up Console" card).
Found by listing each window's Space (CGWindowListCopyWindowInfo, and
CGSCopySpacesForWindows read-only).

**Fix: on a single monitor, delete the extra Spaces** (Mission Control,
Ctrl+Up, hover a Space, close it; its windows move to the remaining one).
It recurs whenever the externals are unplugged, since each display keeps
its own Spaces; turning off "Displays have separate Spaces" would prevent
it. Lanes can't move windows between Spaces (no public API; tools that do,
like yabai, need SIP partly off), but could detect a window on another
Space and warn. Separately, Lanes Switch should hand focus back to the app
that was in front when it hides, so macOS never picks one.
