# PLAN (stub): incremental signal updates

**Status: not designed. Parked.** This is a placeholder to resume from, not
a design. Blocks `PLAN-shell-signal.md` Phase 2 (the live `running` chip).

## The problem

`gather_lanes()` is monolithic: on *any* trigger it re-fetches everything
(one `zellij list-panes` per running session, one `git status` per repo,
the whole `~/.claude/active-sessions/` dir, the wezterm-tab cache) and
rebuilds every lane's signals. Cost: ~500-600ms of subprocess I/O (see the
README's Diagnostics section and the perf.log work in git history).

It runs on: the Switch UI's 10s timer, every `sessions-changed` fs event,
and `switch-shown`. `sessions-changed` fires every 1-3s under normal
background activity (hook writes touching `active-sessions/*.json`, git
index changes, growing transcripts). So the app is doing a full 500ms
recompute every couple of seconds, most of it recomputing signals nothing
changed for.

The shell driver's `running` records make this acute - an `ls` would write
a transient file and, under the current model, trigger a full
`gather_lanes`. That's the forcing function for fixing this.

## The direction (rough, unverified)

Move from "any event → recompute everything" to "event on source X →
recompute only the signals derived from X → patch them into the UI's held
snapshot."

### 1. Source → signal dependency map

| signal kind | source(s) |
|---|---|
| `claude_session` | `~/.claude/active-sessions/<uuid>.json` (per file) + zellij running-session list |
| `repo` (pending commit, non-default branch) | `<repo>/.git/index`, `<repo>/.git/refs/**` |
| `lanes` (session missing / not running) | zellij running-session list + `state.kdl` wezterm-tab cache |
| `command` (done / failed / running) | `shell/<session>--<pane>.json` (per file) |

An fs event carries a path → resolves to one source → resolves to the
lane(s) that depend on it. Everything else in the snapshot is untouched.

### 2. Split `gather_lanes` into per-source recompute functions

`command_signals_for(session, pane)`, `repo_signals_for(path)`,
`claude_signals_for(session)`, `lanes_signals_for(lane)` - each does only
the I/O for that one source. `gather_lanes` becomes "call them all",
retained for the initial load and a slow (10s?) full-refresh correctness
backstop. The fs-event path calls just the one it needs.

Note the correction pass (`cyclable` / `visible` / `lifecycle` / the
Awaiting→Ready upgrade) currently runs *after* all signals for a lane
exist and depends on lane-level facts (`lane_cyclable`, which needs the
claude-signal presence). A per-source recompute has to re-run that pass for
the affected lane, or the lane-level facts have to be cached and only
recomputed when their own inputs change.

### 3. Patch channel to the UI

Replace `emit("sessions-changed")` → frontend refetches all, with
`emit("signals-changed", { lane_id, source_key, signals })` → frontend
splices those signals into its held `snapshot` in place. The frontend
already does this splice for optimistic updates (`dismissSignal`,
`toggleLaneActive`) - the pattern exists.

## Pilot: the shell driver

Best first slice - it's the cleanest case:

- Source is one tiny JSON file per pane.
- `command_signal(rec)` is **pure** - read file, build 0-1 signals, no
  subprocess. (claude/repo/lanes all involve subprocess I/O.)
- The fs event path is unambiguous: `shell/lanes--3.json` → session
  `lanes`, pane 3 → one lane.

Build Phase 2's `running` chip *as* the incremental path: fs event or
3s-timer wake → read that one file → `command_signal` → emit
`{lane, "shell:3", signals}` → frontend splices. `ls` → sub-threshold
record → recompute yields nothing → splice removes any stale chip. No
`gather_lanes`.

Then generalise the pattern to the other three kinds.

## Open questions

- **Held snapshot: backend or frontend?** Backend `Mutex<LanewiseSnapshot>`
  mutated in place (`get_snapshot` returns it instantly, more robust for
  future consumers) vs. stateless backend that only emits patches the
  frontend applies to its own copy (lighter, fits what's there). Leaning
  stateless to start.
- **Lane-level fact caching** (see 2 above) - how to recompute `cyclable` /
  reachability only when *its* inputs change, not on every signal patch.
- **The `cycle_claude_session` path** (hypo+J/K) also enumerates claude
  sessions independently - does it read the held snapshot, or keep its own
  fast path?
- **Watching `<repo>/.git`** already happens (`watch_paths` in
  `switch-ui/src-tauri`) but currently just to trigger the monolithic
  refresh. The per-repo → per-signal resolution needs the repo-path→lane
  map, which `config` has.
- **Ordering / coalescing** - a burst of events on one source shouldn't
  fire N recomputes; the existing 100ms debounce logic needs a per-source
  equivalent.
- Does this obsolete the 10s full-refresh timer, or stays as a backstop?

## Not now

Parked until there's appetite to design it properly. `PLAN-shell-signal.md`
Phase 1 (done/failed) shipped without needing any of this.
