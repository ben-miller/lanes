# Layout templates: a third cascade level between defaults and a context

## Problem

Several contexts share a recognizable "flavor" (e.g. a software-development
lane wants WezTerm + Boards placed the same way every time) but there's
currently only one cascade level: the global `layout.toml` defaults, and
each context's own `layout.toml` overriding/extending them
(`merge_targets`, `config.rs`). There's no way to share a pattern across
*some* (not all) contexts without copy-pasting it into each one's
`layout.toml`.

## Decision: single inheritance, not mixins

A context may reference **at most one** named template. Multiple templates
per context (mixins) are explicitly out of scope - composing two templates
reintroduces the diamond problem (if both set the same target differently,
which wins requires an invented, documented ordering rule, same reason
Python needed C3 linearization). Two concrete use cases don't exist yet to
justify that; revisit if a third cascade need ever actually shows up (same
reasoning as `cycle_policy` not being generalized behind a trait after only
two concrete policies - see that code's own comment in `lib.rs`).

Single-template now is cheap to extend later if mixins ever are needed:
`layout_template: Option<String>` is a degenerate case of
`layout_templates: Vec<String>`, so no existing config has to change when
that day comes - only the (currently nonexistent) multi-template case needs
an ordering rule invented for it.

## Directory restructuring

Two problems with just adding a `templates/` directory alongside existing
context directories:

1. The directory-per-context identity convention
   (`load_lanes`/`config.rs`) means *any* directory under
   `~/.config/lanes/` currently means "this is a context" - a sibling
   `templates/` directory would need an explicit skip, and still reads as
   ambiguous.
2. Cramming template definitions as inline-table arrays under
   `[templates.dev]` inside the existing global `layout.toml` avoids the
   directory collision but makes a template's syntax a different (uglier)
   shape than an ordinary layout file.

Resolution - restructure so every category has its own clearly-scoped
location, and collapse the "global defaults" special case into the same
mechanism as a named template:

```
~/.config/lanes/
  lanes.toml
  layout-templates/
    default.toml      # always applied, first in the cascade (was the
                       # top-level layout.toml)
    dev.toml
    writing.toml
  contexts/
    job-hunting/
      lane.toml
      tab.toml
      board.toml
      layout.toml
    lanes-dev/
      ...
```

- `contexts/<id>/` replaces today's `~/.config/lanes/<id>/` - unambiguously
  "every directory here is a context," full stop.
- `layout-templates/<name>.toml` replaces today's top-level `layout.toml`.
  `default.toml` is the implicit, unconditional base every context's
  cascade starts from (same as today's global `layout.toml` - no opt-out,
  consistent with current behavior). Every other file in this directory is
  an explicitly-opt-in template, referenced by filename stem.
- Both directories are plain files/dirs with normal `[[targets]]` syntax -
  a template file is indistinguishable in shape from a context's own
  `layout.toml`.

## Reference syntax

One new optional key in a context's own `layout.toml`, read before its
`[[targets]]`:

```toml
# ~/.config/lanes/contexts/lanes-dev/layout.toml
layout_template = "dev"

[[targets]]
driver = "app"
name = "Claude"
bundle_id = "com.anthropic.claudefordesktop"
monitor = "lg-left"
position = { cols = 3, col = [2, 3] }
```

Naming matches existing snake_case config keys (`bundle_id`,
`cycle_policy`). Omitting the key means "no template" - identical to every
context's behavior today.

## Cascade order

Exactly three fixed steps, each the existing `merge_targets` identity-override
call (a lane's own target of the same driver identity replaces the matching
one from the previous step rather than duplicating it):

1. `layout-templates/default.toml`'s targets
2. `layout-templates/<layout_template>.toml`'s targets, if set - overrides (1)
   on identity match, adds anything new
3. the context's own `layout.toml` targets - overrides (1)+(2) on identity
   match, adds anything new

## Implementation (`lanes-cli/src/config.rs`)

- `LayoutToml` gains `layout_template: Option<String>` (`#[serde(default)]`).
- New `fn load_layout_template(name: &str) -> Vec<Target>` - reads
  `layout-templates/<name>.toml` via the existing `read_facet::<LayoutToml>`,
  returns its `.targets`. `load_global_config`'s current
  `config_dir().join("layout.toml")` read is replaced by this same function
  called with `"default"` - no more separately-special global-defaults
  path.
- `Config::load()`: load `default_targets` via
  `load_layout_template("default")`, then `load_lanes` changes its per-context
  merge from one `merge_targets(default_targets, own_targets)` call to two:
  `merge_targets(&merge_targets(default_targets, template_targets),
  own_targets)` where `template_targets` is `load_layout_template(name)` if
  `layout_toml.layout_template` is `Some(name)`, else `&[]`.
- `load_lanes`'s directory scan changes from `config_dir()` to
  `config_dir().join("contexts")`.
- `doctor.rs`: one new check - every context's `layout_template` (if set)
  must name a file that exists under `layout-templates/`, same style as the
  existing stale-monitor-handle check. Unknown template name is a `Fail`,
  not a `Warn` - a context silently missing its whole template layer is a
  real, easy-to-miss bug, not a soft problem.

## Migration of the real config (`~/.config/lanes` via `infra`'s stow package)

This is restructuring *live*, actively hotkey-switched config, not just
adding new code - done carefully, verified at each step with `lanes doctor`
and a real `lanes focus` switch before considering it finished (same
discipline as the firefox-profile removal).

1. Move every existing `infra/config/lanes/.config/lanes/<id>/` directory to
   `infra/config/lanes/.config/lanes/contexts/<id>/`.
2. Move `infra/config/lanes/.config/lanes/.config/lanes/layout.toml` to
   `infra/config/lanes/.config/lanes/layout-templates/default.toml`.
3. Re-stow (`stow --target ~ lanes` from `infra`, or just verify the symlink
   tree already reflects the move - stow mirrors the package directory
   structure, no explicit re-run needed unless a new top-level path was
   added).
4. `lanes doctor` clean, then a real `lanes focus <id>` switch confirmed
   working, before calling the migration done.

No templates need to actually exist yet for this migration to land cleanly
- `layout-templates/` can start with only `default.toml` and grow templates
as real shared patterns emerge (e.g. a `dev.toml` once there's an actual
second or third dev-flavored context to justify it).
