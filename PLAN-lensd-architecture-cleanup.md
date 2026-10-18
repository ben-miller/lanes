# Plan: lensd/lanes-cli architecture cleanup

## Context

A long design discussion converged on what `lensd`'s job actually is: maintain
and push one live, current set of Signals (bounded fact + pointer to go
address it), to whoever's listening. The existing `.dl` file + socket
mechanism already does this correctly for the one real consumer (`lanes-cli`/
`switch-ui`) via the `lanes` rule set/socket. Along the way, inspecting the
*actual* running system (not just the design) turned up concrete, real
problems: dead/duplicate code, a hand-maintained global list standing in for
per-context ownership, missing query-engine operators forcing hand-rolled
Rust workarounds, a crash-on-typo failure mode, and a second, independently-
drifting implementation of the same signal logic. This plan fixes exactly
those - no new engine, no FUSE/9P, no new protocol. Two repos:
`~/src/projects/lensd` and `~/src/projects/lanes/lanes-cli`.

## 1. `lensd`: remove the `default` rule set's dead/duplicate output

`rules.dl` (built-in, compiled via `include_str!`) derives `lane_claude_attention`
and `lane_repo_attention`. Verified via direct socket connection this session:
nobody ever reads `default.sock` - `lanes-cli`'s client only ever connects to
`"lanes"`. These two relations also duplicate what `lanes.dl`'s `lanes_claude`/
`lanes_repo_dirty` already compute, apparently superseded and never removed.

- Remove `lane_claude_attention`/`lane_repo_attention` from `rules.dl`.
- Keep `attention`/`claude_attention`/`dirty`/`unpushed`/`has_change` - these
  are project-level (not lane-level) and may still be used by something
  project-scoped outside `lanes-cli`; confirm via grep for `default.sock`
  usage anywhere in `~/src/projects` before deleting more than the two
  lane-rollup rules.

## 2. `lensd`: re-derive lane-tied `watch_repo` entries instead of hand-listing them

`rules.d/repos.dl` currently hand-lists every repo, including lane-tied ones
(`boards`, `infra`, `lanes`, etc.) that duplicate what each lane's own
`tab.toml` already declares via its `repo` scope element. The old fix
(`lanes.dl`'s `watch_repo(Path, Lane) :- lane_repo_path(Lane, Path).`) raced
`repos.dl`'s own definition of the same relation name and got removed this
session rather than fixed, because the engine drops the *entire losing rule
set* on a name collision (confirmed, hit in production this session).

- Add a *new*, non-colliding control relation in `lanes.dl`, e.g.
  `lane_watch_repo(Path, Lane) :- lane_repo_path(Lane, Path).`
- Wire `main.rs`'s existing `watch_tx`/`watch_rx` control-relation feedback
  (currently only watching `watch_repo`'s output) to *also* watch
  `lane_watch_repo`'s output and feed the git source the same way.
- Remove the lane-tied entries from `repos.dl` (`boards`, `infra`,
  `/Users/bmiller/life/formation`, and any of `lanes`/`lanes-wm`/`spinner`/
  `sheetwork-*`/`formation`/`formation-fastapi`/`japanese` that correspond to
  a real `tab.toml` repo scope element - check each against current
  `~/.config/lanes/contexts/*/tab.toml` before removing). Leave genuinely
  context-less entries (`vitae`, `life-api-graphql`, `life-etl`,
  `presentation`) as the file's sole remaining content.
- Update `repos.dl`'s header comment to reflect it's now only for
  context-less repos.

## 3. `lensd`: add aggregation + negation to the Datalog compiler

`differential-dataflow` already provides `.count()`/`.reduce()` and antijoin
as real operators - the gap is purely that `datalog/compile.rs`'s rule
parser/lowering doesn't expose them in rule syntax yet. This is the cheapest
of the options compared this session (vs. a SQL engine swap), since the hard
incremental-computation primitives already exist in the dependency.

- Extend the rule grammar (wherever `Atom`/`Literal`/`Term` are parsed - in
  `datalog/` module, exact files TBD during implementation) to support at
  least: a count/aggregate literal in a rule body, and a negated atom
  (`!atom(...)` or similar, stratified - i.e. a negated relation's own
  definition can't depend on the rule using the negation, same restriction
  already enforced for `watch_repo`-style control relations).
- Lower aggregate literals to `.reduce()`/`.count()` and negated atoms to an
  antijoin against the positive relation, in `build_dataflow`.
- This is real new work, not a quick fix - scope it as its own follow-up
  task once the rest of this plan lands, not bundled into the same PR/commit
  batch as items 1/2/4/5 below. Flagging it in this plan for completeness and
  prioritization, but the concrete grammar/lowering design should get its own
  focused pass (likely its own `EnterPlanMode` round) rather than being
  speced blind here.

## 4. `lensd`: degrade instead of panic on an unregistered relation

`engine.rs`'s `advance_and_apply` currently panics the entire daemon the
moment a fact arrives for a relation absent from `BASE_RELATION_ARITIES` or
any loaded rule head - confirmed, hit twice this session (once for
`lane_boards_project`, once for `board_inbox_count`, each requiring a
rebuild to fix). Every other failure mode in this codebase degrades
(`source_degraded` fact, last-known-good retained) rather than crashing.

- In `advance_and_apply` (`engine.rs:~259`), replace the `panic!` on a
  missing `self.sessions` entry with: log via `eprintln!` (matching the
  existing warning style elsewhere in this file) and skip that one delta,
  rather than unwrapping.
- Add a regression test: feed a delta for a relation no loaded program
  references, assert the engine keeps running and other deltas in the same
  batch still apply.

## 5. `lanes-cli`: delete `gather_lanes()` and collapse to one implementation

Two independent implementations of "turn facts into lane signals" exist:
`gather_lanes()` (`lib.rs`, no `lensd` involvement, used by `lanes snapshot`/
`lanes signals`/`lensd-compare`) and `lane_from_mirror()`/
`snapshot_from_mirror()` (`lensd.rs`, used exclusively by `switch-ui`). This
is the confirmed root cause of at least two bugs this session needing
duplicate fixes (`shell_ignored_commands`, the `lane.toml`-presence rule).

- Delete `gather_lanes()` and every private helper that exists *only* to
  support it (confirmed exclusively-used-by-it helpers: check each of
  `signal_cyclable`/`signal_visible`/`correct_signals`/`upgrade_awaiting_to_ready`
  are actually shared with `lensd.rs` before deleting - per earlier session
  notes they are shared, so keep those; delete only the `gather_lanes`-only
  subprocess-batching/driver-polling code path itself).
- `Command::Snapshot` (`lanes snapshot`) and `Command::Signals`
  (`lanes signals`) in `main.rs`: reimplement on top of `lensd::read_snapshot`
  + `lensd::snapshot_from_mirror(&cfg, &mirror)` instead of `gather_lanes(&cfg)`.
  This changes their behavior slightly (requires a live `lensd` connection;
  previously worked standalone) - call this out explicitly when done, it's a
  real behavior change the user should be aware of, not just a refactor.
- `Command::LensdCompare` (`lensd-compare`): delete entirely. Its only purpose
  was comparing the two implementations; once there's only one, there's
  nothing to compare.
- Run the full `lanes-cli` test suite after; expect to delete a meaningful
  number of `gather_lanes`-specific tests alongside it (check `cfg(test)`
  blocks for tests that only exercise deleted code paths).

## Verification

- `cargo build --release` and `cargo test` clean in both `lensd` and
  `lanes-cli` after each numbered item (not just at the end) - this project's
  own convention from this session: build/test before moving to the next
  item, don't batch.
- After item 1/2: restart the `lensd` daemon fresh (kill + relaunch, not
  reuse a running process - this session's own hard-learned lesson), run
  `lanes doctor`, confirm `context order`/`boards order` checks still pass,
  and connect directly to `lanes.sock` (same raw-socket Python snippet used
  throughout this session) to confirm `lanes_inbox_count` and the git-derived
  relations still populate correctly for a sample of lanes.
- After item 4: the regression test is the primary verification; also
  manually trigger the old crash scenario (a fact for a relation with no
  rule reference) against a running daemon and confirm it logs and continues
  rather than exiting.
- After item 5: restart `switch-ui` fresh, confirm the dashboard still shows
  correct signals for a few real lanes, and run `lanes snapshot`/
  `lanes signals` by hand to confirm they still produce sensible output
  against the live `lensd` connection.
- Stage every change immediately via `git pc` per the user's standing
  instruction, one logical change at a time, not batched at the end.
