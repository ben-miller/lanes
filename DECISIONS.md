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
