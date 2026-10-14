# PLAN: directory-per-context model

Design only - not yet built. Captures a design conversation (2026-10-13)
about cleaning up the three-way drift between lane config, Zellij tabs
(`ztabs`), and Boards projects. See `PLAN-window-targets.md` for the
`[[targets]]` system this builds on, and the lanes-cli README's "lensd
spike" section for the signal-computation daemon this is independent of
(lensd computes *signals*; this plan is about *identity and placement*
config - different subsystems, both affected by the file-layout change).

## Settled, in order of how we got there

**1. No renaming of the product.** "Lanes" stays the tool/app name,
`lanes` stays the binary name. The vocabulary work below introduces a new,
narrower concept (`context`) rather than replacing `lane` everywhere - see
point 3.

**2. Blast radius of touching the *wire* layer (file formats, not just
Rust-internal names) spans four repos.** Internal renaming (struct/function
names within one repo) is cheap and was explicitly declined - not needed,
since we're not renaming the concept. What this plan *does* touch is the
on-disk shape, which is read by:
- `lanes-cli` (this repo) - obviously, it owns the format today.
- `ui/switch-ui` (this repo) - reads the Rust backend's JSON.
- `lensd` (`~/src/projects/lensd`) - has its own dedicated
  `src/source/lanes.rs` module that reads `~/.config/lanes/` directly to
  feed its `lanes_*` datalog relations. **This will need updating** when
  the on-disk shape changes, or it silently stops seeing contexts
  correctly. Flagged, not designed here - out of scope for phase 1 (see
  Phasing).
- `infra` (`~/src/infra`) - `ztabs` (`py/lib/infra_zellij.py`) currently
  hand-maintains `~/.config/infra/zellij-tabs.toml` independently.
- `boards` and `lanes-wm` are **not** affected - confirmed zero "lane"
  coupling in either; Boards' data model (`~/boards/<project>/board.yaml`,
  identity = folder name) is deliberately generic and this plan keeps it
  that way (see point 5).

**3. Vocabulary: `context` is the directory; `lane` is one optional facet
of it.** Not a rename - a new, narrower concept introduced underneath the
existing one. A context is "a project/area you switch into" - the thing a
directory under `~/.config/lanes/` represents. A *lane* is specifically the
switcher-UI-facing view of a context (does it show up in Lanes Switch, what
name, is it active/cyclable). Today "lane" conflates three things: identity,
the switcher view, and the Zellij/repo scope. This plan splits those apart.

**4. Directory-per-context, with optional facet files inside.** Replaces
one flat `~/.config/lanes/<id>.toml` per context with a directory:

```
~/.config/lanes/
  job-hunting/
    lane.toml      # optional: display name, active, cyclability
    tab.toml       # optional: Zellij session, repo path/cwd for the tab
    board.toml     # optional: Boards project name + placement
  infra/
    lane.toml
    tab.toml
    board.toml
  lanes-wm/
    lane.toml
    tab.toml
    # no board.toml - this context has no Boards project
```

- **Identity is the directory name, full stop.** No `id` field anywhere -
  it was always redundant with the directory name, and removing it means
  there's no way for the two to disagree.
- **Every facet is optional**, including `lane.toml`. A context with only
  `tab.toml` (+ maybe `board.toml`) gets a real Zellij session (and maybe a
  Boards project) but never renders a row in the Lanes Switch dashboard.
  Genuinely useful for a low-priority/background context you don't want
  cluttering the switcher.
- **Open item, not resolved here:** `tab.toml` and `lane.toml` both have a
  legitimate claim on "the repo path" - `tab.toml` wants it as the pane's
  cwd, `lane.toml`'s dashboard signals (pending-commit, non-default-branch)
  want it as what to watch. One facet should own it and the other
  reference it, not each duplicate their own copy that can drift. Decide
  when designing the actual TOML schema, not before.

**5. `ztabs` and Boards projects become generated, not hand-maintained -
by different mechanisms, because they have different coupling to begin
with:**

- **ztabs**: already lane-specific (it shells out to `lanes tabs set/clear`
  today), so the natural fix is to make `ztabs` itself read context
  directories directly - no intermediate generated `zellij-tabs.toml` at
  all. This is the `infra` repo, so it's a cross-repo change, not a
  lanes-cli-only one.
- **Boards**: deliberately generic (per its own README, "a project is any
  subfolder of the data root containing `board.yaml`" - it has zero concept
  of lanes/contexts and should stay that way). So lanes-cli doesn't teach
  Boards anything new; instead `lanes doctor` (or a `--fix` mode) *ensures*
  `~/boards/<project>/board.yaml` exists for any context whose `board.toml`
  names a project, materializing into Boards' existing unmodified format
  rather than coupling Boards to this project.

**6. Ordering lives in exactly one place: the existing top-level `order`**
(in `~/.config/lanes.toml`, sibling to the context directories - moved to
`~/.config/lanes/lanes.toml` on 2026-10-14, see PLAN-window-targets.md's own
note on this) - not a new mechanism, just enforcement of the one that's
already there. `ztabs` and
the Boards-materialization step both read *this* list rather than ever
inventing their own sequence (today's `zellij-tabs.toml` tab order is
exactly that already-happening problem, just never named as one).
`sort_by_order`'s existing fallback (unlisted → alphabetical-by-id, after
any listed ones) stays as the *runtime* behavior - see point 7 for why it's
not removed outright.

**7. Registration gap: tolerate at runtime, fail hard in `doctor` - modeled
on an existing precedent in `lensd`, not a new philosophy:**

`lensd/src/main.rs` already draws exactly this line: `Config::load(path)?`,
`source::rules::check(&rules_dirs)?`, and `Server::new(&dir)?` all run
*synchronously before* any background thread spawns, each with a bare `?` -
a bad config or rules dir kills the whole daemon before it starts. Once
past that point, each source (git, zellij, claude, shell, lanes, ...) runs
on its own thread with its own `Result`, isolated from the others - a
runtime hiccup in one source never takes the daemon down. The criterion:
**checks in the synchronous pre-flight sequence are fail-fast; checks that
only ever run inside an already-started background loop are isolated.**

Applied here:
- A context directory missing from `order` does **not** break anything at
  runtime - it still works, falls back to alphabetical placement. Matches
  every other graceful-degradation choice already in this codebase (a
  malformed per-lane TOML today just drops that one lane with a warning,
  never the whole config).
- `lanes doctor`'s existing `check_lane_order`/`check_lane_order_vs_ztabs`
  checks currently only ever warn (`⚠`, never `✗`). This plan promotes "a
  context directory isn't in `order`" specifically to `✗ Fail` severity
  (nonzero exit) - registration becomes a real, enforced constraint via
  `doctor`, without making ordinary `lanes` operations fragile.
- `lanes` the CLI is one-shot - it has no meaningful "startup vs running"
  distinction of its own. **`ui/switch-ui` does** (it's the long-running
  process). Its `setup()` currently has zero fail-fast checks at all (state
  reads degrade to defaults, same as everywhere else). This plan adds the
  registration check to `setup()`, synchronously, before the window shows -
  mirroring lensd's pattern: refuse to present a working dashboard over an
  unregistered context, the same way lensd refuses to boot over a bad rule
  file. Once already running, the fs-watcher noticing a *newly*-created
  unregistered directory must **not** crash the running app - just flag it
  (matches "fail at startup, tolerate at runtime" exactly).

## Phasing (not yet sequenced/estimated - for discussion)

1. Lanes-cli: directory-per-context loading (`load_lanes` reads
   `~/.config/lanes/<ctx>/{lane,tab,board}.toml`, each optional), identity
   = directory name, migration of the 10 existing flat files into
   directories (mechanical, one-time).
2. `doctor`: promote the registration check to `✗ Fail`.
3. `switch-ui`: synchronous registration check in `setup()`.
4. `infra`/`ztabs`: read context directories directly; retire hand-written
   `zellij-tabs.toml`.
5. Boards materialization: `doctor --fix` (or similar) ensures referenced
   project folders exist.
6. `lensd`: update `source/lanes.rs` for the new on-disk shape. Separate
   repo, separate PR, not blocking 1-5.

## Explicitly out of scope / unresolved

- Exact TOML schema for `lane.toml`/`tab.toml`/`board.toml` (field names,
  which facet owns the repo path).
- Whether `zellij-tabs.toml` is deleted outright or kept briefly as a
  transitional generated cache during migration.
- Any UI for creating/editing contexts (still TOML-by-hand for now, per the
  earlier "edit the source, don't build a two-way sync" decision).
