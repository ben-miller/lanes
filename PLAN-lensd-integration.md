# PLAN: Lanes consumes lensd

## Goal

The Switch UI gets its signals from lensd's `lanes` stream instead of calling
`gather_lanes()` on a 10s timer and on every `sessions-changed` event, and
`gather_lanes()` is then deleted. That removes the ~500ms of subprocess work per refresh and delivers the incremental
updates `PLAN-incremental-signals.md` parked.

## Ownership split

- **lensd (via `lensd/lanes.dl`)** answers what is true: which signals exist,
  with identity, kind and reason (a Claude session is idle, a command failed,
  a repo is dirty or on a non-default branch).
- **Lanes** decides how to treat it: the correction pass (`cyclable`,
  `visible`, `lifecycle`, the Awaiting-to-Ready upgrade), dismissals, latched
  state, muting, focus, urgency ordering, and every actuator. lensd never
  produces these fields and must not learn about them.

## Correctness stance

`gather_lanes()` is a prototype, so it is not the answer key. Parity with it is
not an exit criterion.

- lensd sources are correct when they match the underlying truth (Zellij's
  session list, `git status`, the Claude registry). That is established by the
  per-source tests in the lensd repo.
- `lensd-compare` stays as a diagnostic. Each difference is triaged: lensd is
  wrong (fix it), Lanes is wrong (note it), or the behaviour is a deliberate
  choice. It compares signal identity, kind and reason only.
- Cutover happens when a UI built from lensd looks right in daily use.

## Phases

1. **Settle the semantics.** Decided:
   - Detached HEAD: do what Lanes does today, a "non-default branch" signal.
     lensd's git source emits a placeholder `branch` for it so the existing
     `Current != Default` rule fires.
   - `SessionMissing`: lensd gets a WezTerm source (`wezterm_tab_session`,
     done), and `lanes.dl` exposes `lanes_tab(Lane, Tab)`. Lanes computes
     "missing" as `lanes_session` minus `lanes_tab` client-side. No dependence
     on the `state.kdl` tab cache.
   - Claude pid-reuse slack: port into lensd's `claude` source (process age
     versus registry file age, 15s slack, command name), plus a slow periodic
     rescan so a silently dead process is noticed.
   - Lane repos: have `lanes.dl` derive
     `watch_repo(Path, Lane) :- lane_repo_path(Lane, Path).` so every lane's
     repo is watched. `rules.d/repos.dl` stays for now (project grouping).
2. **Live client** (`lanes-cli/src/lensd.rs`). Replace the one-shot
   `read_snapshot` with a background thread that keeps the `Mirror` current
   from `snapshot` and `diff` messages, reconnects with backoff, and bumps a
   version counter on every diff. Expose `current()` and a change callback.
3. **Snapshot from the mirror.** Add `snapshot_from_mirror(cfg, mirror)`. Lane
   definitions and layout still come from Lanes' config; signals come from
   lensd. Extract the correction pass out of `gather_lanes()` so both paths
   share it. Apply dismissals and latched state client-side.
4. **Wire into Tauri** (`ui/switch-ui/src-tauri/src/lib.rs`, `App.svelte`).
   lensd diffs trigger the refresh; the 10s timer goes away. lensd is a system
   utility: there is no fallback. When the socket is down the UI shows a
   "lensd not connected" state (like a missing network connection) and
   recovers on its own when lensd returns. Keep emitting `sessions-changed`
   first so the frontend changes little. Later move to per-lane
   `signals-changed` patches, piloted by the shell `running` chip.
5. **Shadow, then cut over, then delete.** Run both paths briefly, log
   divergences to `perf.log` for triage (not pass/fail), switch to lensd, then
   remove `gather_lanes()`, the drivers it uses, `lensd-compare`, and the
   perf logging that only served it. `doctor` is reviewed at that point.

## Decided

- lensd is assumed running, like a system utility. Lanes does not start it.
  Without it, Lanes shows a not-connected error state.
- `gather_lanes()` is throwaway and is removed once lensd works. No fallback.

## Decided (rules delivery)

- lensd takes a list of rules directories (`[rules] dirs` in `lensd.toml`) and
  watches each in place. Lanes keeps `lanes.dl` in its own directory
  (`lanes-cli/lensd/`) and that directory is listed in lensd's config. No
  copying or symlinking. Lanes' rules are expected to change often and may
  later be edited from within Lanes; edits flow live because lensd watches
  the directory.
- The socket name follows the file stem (`lanes.dl` gives `lanes.sock`).
  Adding a new directory to the list needs a lensd restart; edits to files
  in listed directories do not.

## Status

Spike done: `lensd/lanes.dl`, the socket client and `lensd-compare` (see the
"lensd spike" section of `lanes-cli/README.md`). Nothing in the UI uses lensd
yet. Phase 1 done: WezTerm source (`lanes_tab`), detached HEAD as a
`detached HEAD` branch, pid-reuse check plus 5s rescan in the `claude` source,
and `watch_repo` derived from `lane_repo_path` in `lanes.dl`. Phase 2 done: `lensd::Client` in
`lanes-cli/src/lensd.rs` (live mirror, reconnect, version, change callback,
tested against a fake socket). Phase 3 (`snapshot_from_mirror`) is next.
