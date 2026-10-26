# Plan: one `enabled` flag on the lane; fully divorce Boards' ordering from lanes

## Context

This session built up a context model where a context's facets (`lane.toml`,
`tab.toml`, `layout.toml`, `board.toml`) were each independently
enable/disable-able, and where Boards read its project order straight from
`contexts.toml` (reasoning: "no second source of truth to drift"). Working
through the actual use cases (disabling `admin`'s lane+tab but keeping its
board; wanting a lane's dev apps raised without repositioning; wanting fewer
visible WezTerm tabs) surfaced that the facet model was finer-grained than
anything actually needs:

- `tab.toml`'s `scope` is already functionally dependent on `lane.toml`
  existing - `load_contexts` only ever reads it inside the branch where
  `lane.toml` is present. A tab has never really been independent of a lane.
- `layout.toml`'s targets are the same - no meaning without a lane.
- Board, by contrast, genuinely is independent (several board-only contexts
  exist: `app-ideas`, `platform-console`).
- Tying Boards' *order* to lanes' `contexts.toml` coupled two concepts the
  user wants to keep separable ("I can see that getting weird if we don't
  fundamentally have the concepts tied together").

Initial direction was a full file merge (`tab.toml` + `layout.toml` folded
into `lane.toml`, one file per context). On reflection that was judged too
big a change for what's needed right now. **Revised, lighter plan**: keep
the three files as they are, physically - just make `lane.toml`'s `enabled`
the single flag that governs all three as one bundle, instead of each file
carrying its own independent `enabled`. Board stays fully separate either
way, and goes back to fully owning its own order.

## Decisions

1. **Single enabled flag, living on the lane**: `lane.toml`'s `enabled`
   becomes the *only* enable/disable flag for the lane+tab+layout bundle.
   `tab.toml` loses its own `enabled` key entirely (it becomes just
   `[[scope]]` entries, nothing else). `layout.toml` never had its own
   `enabled` key, so it's unaffected. `load_contexts` gates reading
   `tab.toml`/`layout.toml` on `lane.toml`'s `enabled` - when the lane is
   disabled, both are treated as absent regardless of their own content,
   same "disabled means absent" rule as before, just keyed off one flag
   instead of three. **No file merge, no data migration of file contents,
   no file deletion.** `lane.toml`'s `enabled` is taken as the source of
   truth for migration: whatever a context's lane is currently enabled/
   disabled as, that's what it stays - not reconciled against `tab.toml`'s
   current flag.

2. **Board stays independent, and its *order* is fully divorced from
   lanes**: revert `boards`' `board.rs::list_projects()` to read its own
   local `<data_dir>/.order` file again (the pre-"lanes_order" mechanism),
   not `~/.config/lanes/contexts.toml`. Remove `lanes_order`/`lanes_order_at`
   and their tests from `boards/src-tauri/src/board.rs`. Nothing in
   `lanes-cli` writes `.order` either (that push mechanism was already fully
   removed earlier this session, per DECISIONS.md/doctor cleanup - no
   reversal needed there). Whether/how `.order` gets written going forward
   is Boards' own concern, out of scope here - it's fine for it to just not
   exist (falls back to alphabetical, `apply_order`'s existing behavior).
   A lane still *points at* a board via an ordinary target in its own
   `targets` (`driver = "boards"`, same as today) - that linkage is
   unaffected, only the *order* coupling goes away.

3. **Disable lanes to cut WezTerm tab clutter**: use existing Boards project
   data (inbox counts / activity) as a first-pass signal for which lanes are
   stale enough to disable via `lanes ctx <id> lane disable` (now the sole
   disable flag per decision 1). Propose a list, user corrects by hand
   afterward. Not a code change - a one-time cleanup pass using tools that
   already exist (once decision 1 lands).

4. **Layout naming: moot under this plan.** The original question ("should
   `layout_template`/`layout-templates/` be renamed now that `layout.toml`
   itself no longer exists as a standalone file?") only made sense under
   the full-merge version. Since `layout.toml` isn't going away under this
   lighter plan, there's no naming mismatch to resolve - nothing to do here.

## Concrete migration steps

### `lanes-cli`

- `config.rs`: remove `TabToml`'s `enabled` field. `load_contexts` reads
  `lane.toml`'s `enabled` as the sole gate for the whole bundle - when
  false, skip reading `tab.toml`/`layout.toml` entirely (treat as absent),
  same as today's per-file gating but keyed off one field.
- Remove `set_tab_enabled` and its tests.
- Remove `ContextCommand::Tab`/`TabFacetCommand`'s disable/enable path from
  `main.rs` - folded into `lane disable`/`enable`, which already exists and
  already governs the bundle.
- `cmd/add.rs`: no change needed to file-writing (files stay separate), but
  `check_against_existing_facets` should stop checking `tab.toml`'s own
  `enabled` - presence/absence of the file plus `lane.toml`'s flag is now
  the whole story.
- `doctor.rs`: update any check that reads `tab.toml`'s `enabled` directly.
- No data migration of `tab.toml`/`layout.toml` content needed - only
  removing the now-unused `enabled` key from each context's `tab.toml` on
  disk (cosmetic; can be left stale and just ignored, or cleaned up in
  passing).
- Update tests across `config.rs`/`main.rs`/`cmd/add.rs`/`lib.rs` that
  construct `TabToml` with an `enabled` field or reference the now-removed
  tab-facet disable/enable command.

### `infra` (`~/src/infra/py/lib/infra_zellij.py`)

- `load_tabs()` currently reads `tab.toml` directly for `scope` and checks
  its own `enabled` key. Update it to read `scope` from `tab.toml` as
  before, but check `enabled` from `lane.toml` instead.
- Update `py/tests/test_infra_zellij.py` fixtures accordingly.

### `boards` (`~/src/projects/boards/src-tauri/src/board.rs`)

- Revert `list_projects()` to read `<data_dir>/.order` directly again.
- Remove `lanes_order()`/`lanes_order_at()` and their tests
  (`lanes_order_at_resolves_context_ids_to_their_board_toml_project`,
  `lanes_order_at_skips_a_context_with_no_board_toml`,
  `lanes_order_at_is_empty_when_contexts_toml_is_absent`).
- Remove the `toml` dependency from `src-tauri/Cargo.toml` if nothing else
  in that crate needs it (check first - it was added solely for this).
- Doc comments referencing "reads lanes' contexts.toml directly" (in
  `board.rs`, and `lanes-cli`'s `DECISIONS.md`/doc comments that reference
  this) need updating back to describe the divorced state.

### Tab-disabling pass (decision 3)

- Once decision 1's change lands: look at current Boards project data
  (inbox counts, recency) for each lane with a board target, propose which
  ones look stale, disable via `lanes ctx <id> lane disable`, then let the
  user correct by hand.

## Order of operations

1. `lanes-cli`: single-flag change (decision 1) - the foundational change
   everything else reads from.
2. `infra_zellij.py`: follow the schema change (decision 1 completion).
3. `boards`: revert to local `.order` (decision 2) - independent of 1/2,
   can happen in parallel or after.
4. Tab-disabling pass (decision 3) - last, since it depends on (1) being
   live (lane disable as the sole flag) and benefits from Boards already
   being stable.

## Explicitly not doing yet

- Not merging `tab.toml`/`layout.toml` into `lane.toml` - decision 1 is a
  flag change, not a file-format change.
- Not building any push-mechanism for Boards' `.order` file from
  `lanes-cli` - decision 2 is a full divorce, not a different coupling.
- Not touching `layout_template`/`layout-templates/` naming - decision 4
  is moot under this plan (see above).
