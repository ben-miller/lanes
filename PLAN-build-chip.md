# Plan: a "running build is stale" chip

> **Status: built (2026-10-22).** As planned. lensd emits
> `appctl_instance_repo` (repos canonicalized when a hello arrives) and was
> restarted; the two `lanes.dl` rules loaded with no `rules_error`;
> `lanes-cli` has the `build` kind (2 new tests; 265 pass) and Lanes Switch
> renders it. Verified live: bumping the running dev Boards binary's
> mtime gave a `lanes_build_rebuilt` row for lane `boards-dev`, and
> `lanes snapshot` showed a visible Warning `build`/`rebuilt_since_launch`
> chip, detail `boards dev-main`, on that lane's repo facet. **Not yet
> checked:** the chip as drawn in Lanes Switch (by eye).

The end of a long line of work (`~/src/dev-infra/NOTES-appctl-status-and-next.md`):
show in Lanes Switch when a running app isn't the code on disk.

## What exists

- Apps serve an `appctl` socket per running instance whose hello carries a
  **build identity**: the source directories the binary was built from
  (its repo, plus Rust path dependencies in other repos; a `tauri dev`
  build counts only `src-tauri/`), each with a tree hash
  (`~/src/dev-infra/README.md`, "Build identity"). Boards stamps itself.
- lensd's `appctl` source (`~/src/projects/lensd/src/source/appctl.rs`)
  turns that into facts, live:
  `appctl_instance(app, instance, mode, pid)`,
  `appctl_instance_source(app, instance, repo, path, tree)` (built from),
  `appctl_source_tree(repo, path, tree)` (that directory now),
  `appctl_instance_rebuilt(app, instance)` (exe newer than the process).
- The comparison already works as a lensd rule (`Built != Now`), verified
  live against a running dev Boards.

## Changes

1. **lensd**: one more fact, `appctl_instance_repo(app, instance, repo)`:
   the app's *own* repo (the stamp's first source), so a chip can go on
   the right lane. Repo paths canonicalized, to join with
   `lane_repo_path`. Restart lensd before step 2 uses it.
2. **`lanes-cli/lensd/lanes.dl`**:
   ```
   lanes_build_changed(Lane, LaneRepo, App, Instance, Repo, Path) :-
       lane_repo_path(Lane, LaneRepo),
       appctl_instance_repo(App, Instance, LaneRepo),
       appctl_instance_source(App, Instance, Repo, Path, Built),
       appctl_source_tree(Repo, Path, Now),
       Built != Now.
   lanes_build_rebuilt(Lane, LaneRepo, App, Instance) :-
       lane_repo_path(Lane, LaneRepo),
       appctl_instance_repo(App, Instance, LaneRepo),
       appctl_instance_rebuilt(App, Instance).
   ```
   The chip goes on the lane whose repo is the app's own, even when the
   changed source is in another repo (Boards' lane, when
   `dev-infra/crates/appctl` changed); the chip names what changed.
3. **`lanes-cli`**: a new signal kind, `build`
   (`SignalReason::Build(BuildReason::{SourceChanged, RebuiltSinceLaunch})`),
   one chip per instance on the matching **repo facet**, detail
   `boards dev-main · src-tauri, dev-infra/crates/appctl` (the changed
   sources) or `boards dev-main` (relaunch).
   - Urgency **Warning** ("something's off, not blocking"), `Live`.
   - **Always visible** (`signal_visible`), like `command`/`lanes`/`boards`:
     repo-kind chips are hidden unless the lane is cyclable, and a stale
     build matters regardless.
   - `keys_from_mirror` gains the matching keys.
   - No action on click (nothing to focus); a "rebuild/relaunch" action is
     a later idea.
4. **`ui/switch-ui`**: `kindLabel` `build`; reasons `source_changed` ->
   "stale", `rebuilt_since_launch` -> "relaunch"; detail inline like
   command chips; not dimmed when not cyclable (the `is-command` exception).

## Tests

- lensd: the integration test asserts `appctl_instance_repo`.
- lanes-cli (`src/lensd.rs` tests, `Mirror` rows): build rows on the
  lane's repo produce a visible Warning `build` chip on that repo facet
  with the expected detail; other lanes and other repos get none;
  `keys_from_mirror` includes them.
- Live: the rule rows on `lanes.sock` for a simulated rebuild (bump the
  dev binary's mtime, then restore it) and a source change (a file in
  `dev-infra/crates/appctl`); the chip itself, by eye, in Lanes Switch.
