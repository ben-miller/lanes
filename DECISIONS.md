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
