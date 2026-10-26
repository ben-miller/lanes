# Decisions

Settled design rationale for things that are already built - *why*, not
*how* (that's the README) and not *what's left* (that's `PLAN-*.md`). One
entry per decision, written once and left alone.

## Layout templates: single inheritance, not mixins

A context may reference at most one named `layout-templates/<name>.toml`.
Multiple templates per context (mixins) are explicitly out of scope -
composing two templates reintroduces the diamond problem (if both set the
same target differently, which wins requires an invented, documented
ordering rule, same reason Python needed C3 linearization). Two concrete
use cases don't exist yet to justify that; revisit if a third cascade need
ever actually shows up (same reasoning as `cycle_policy` not being
generalized behind a trait after only two concrete policies - see that
code's own comment in `lib.rs`).

Single-template now is cheap to extend later if mixins ever are needed:
`layout_template: Option<String>` is a degenerate case of
`layout_templates: Vec<String>`, so no existing config has to change when
that day comes - only the (currently nonexistent) multi-template case needs
an ordering rule invented for it.

## Layout templates: contexts/ and layout-templates/ as separate directories

Two problems with simpler alternatives:

1. A sibling `templates/` directory next to context directories would be
   ambiguous - the directory-per-context identity convention means *any*
   directory under `~/.config/lanes/` means "this is a context," so
   `templates/` would need an explicit skip and still reads as unclear.
2. Cramming template definitions as inline-table arrays under
   `[templates.dev]` inside the global `layout.toml` avoids that collision
   but makes a template's syntax a different (uglier) shape than an
   ordinary layout file.

Resolved by giving every category its own clearly-scoped location
(`contexts/<id>/` for contexts, `layout-templates/<name>.toml` for
templates) and collapsing the "global defaults" special case into the same
mechanism as a named template: `layout-templates/default.toml` is just the
one template every context's cascade implicitly, unconditionally starts
from - no separate code path for it.

## Layout templates: no suppression primitive

The cascade only supports override-by-replacement (a context's target of
the same identity replaces the matching one from the level below) - there's
no way for a context to *suppress* an inherited default outright. Surfaced
removing Firefox/Claude from being auto-placed by default (2026-10-15):
since nothing could opt out individually, the fix was deleting the target
at its source (`default.toml`) rather than something any one context could
turn off. Not worth adding a real suppression mechanism until a concrete
case needs one default suppressed while another context still wants it -
nothing does today.

## Boards target: appctl socket first, `open boards://` as fallback only

`open "boards://open?project=..."` only reaches an **installed** Boards -
LaunchServices routes it by bundle ID, and a `just dev` build has none
(`lsappinfo` reports `CFBundleIdentifier=[ NULL ]`). That forced
`just install-dev` on every Boards iteration just to keep Lanes working.
Boards now serves an `appctl` socket per running instance
(`~/.local/state/boards/instances/<instance>.sock`), so Lanes drives that
directly and only falls back to the `open` link when no instance answers -
which still cold-launches the installed app on that project, unchanged.

Instance choice uses `appctl`'s own default policy (one live `dev-*`, else
`installed`, else error) rather than a per-lane config field: no concrete
case has ever needed Lanes and the live dev build to disagree about which
Boards window a lane should land on, so there's nothing yet to make that
field express. A `board.toml` `instance` field can be added later without
touching anything else (`appctl::client::connect`'s third argument already
exists for exactly this).

Placement switched from bundle-ID lookup to the pid `activate_target`
itself discovered mid-request (`activate_target`'s return type grew an
`Option<u32>` for this) - a bundle ID can only ever resolve the installed
copy, so placing a `just dev` window at all required addressing it by the
specific process Lanes just talked to, not by app identity.

## Display names stay denormalized

One context's human-facing name lives in up to two places lanes doesn't own
the sync between: `lane.toml`'s `name` and `board.yaml`'s `title` (Boards'
own data file, editable from inside Boards itself, in a directory lanes
only ever *points at* via `board.toml`'s `project` key - see
`board.toml`/`board.yaml` in the lanes-cli README's "Target drivers"
section for why those are two separate files at all). A CRUD layer for
contexts (rename, add, remove) could sync both on every write, but that
would mean forcing a specific relationship between them (always identical)
that isn't actually true today in every case - a "<Project> Dev" lane
legitimately shares a plain "<Project>" Boards project, for instance.

(A third copy used to live in `zellij-tabs.toml`, a different tool's
config - moot now that `infra zellij`'s `load_tabs` reads `name` straight
from `lane.toml` instead of keeping its own copy, so there's nothing left
there to drift.)

Resolved by *not* syncing: each file stays independently editable, and
`lanes doctor`'s "display name consistency" check just surfaces drift
(lane display name vs that context's board.yaml title) as a warning -
never auto-fixed, never fails the run. Fixing a real mismatch is a choice
made by hand, in whichever file is actually stale, not something the
tooling decides for you.

## Context vs lane: context is the primary type, lane is one of its facets

"Lane" and "context" used to be treated as nearly synonymous - `Config`
stored `lanes: Vec<Lane>` as if it were the primary collection, with a
context that happened to have no `lane.toml` (a board-only context like
`app-ideas`) handled as a bolted-on special case: a *second*, independent
directory scan (`context_board_targets`, since removed) re-read
`board.toml` *and* `lane.toml` again from scratch, purely to also catch
contexts the first scan (`load_lanes`, since removed, which required
`lane.toml` to exist at all) skipped entirely. Two scans of the same
directories computing overlapping data, because there was no type that
could represent "has a board, may or may not have a lane."

"Lane" is one optional facet a context can have - the same tier as having
a board or a tab/session - not a synonym for "context" itself. Resolved by
making `Context { id, lane: Option<Lane>, board: Option<BoardFacet> }` (see
`model::Context`) the primary type, built by one scan
(`config::load_contexts`) that reads each file once. `Config.lanes`/
`boards_targets`/`context_ids` still exist, now as cheap in-memory
derivations of `contexts` (`derive_lanes`, `derive_boards_targets`) rather
than separate scans - kept so the many existing callers shaped around
"just the lanes" don't all need rewriting at once, but anything new should
read `cfg.contexts` directly (see `context::list`/`context::set_display_name`,
built this way from the start).

`BoardFacet` deliberately carries only `project` (a String), not the full
placement `Target` `board.toml` also has (monitor/position/raise/...):
nothing anywhere reads those fields off a context's board facet directly -
a lane with a board already has that `Target` folded into its own
`targets` by `load_contexts`, and a board-only context is only ever named,
never activated or placed.
