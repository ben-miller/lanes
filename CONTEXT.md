# Where we left off

## What's built

- `lanes-cli/` — Rust CLI. Commands: `lanes snapshot`, `lanes doctor`, `lanes sessions list/get`.
- Drivers: zellij (sessions + KDL layout parsing), claude (active-sessions registry + aiTitle from JSONL), brotab (stubbed, disabled — broken on macOS due to `bt install` writing manifest to wrong path).
- Correlation pass links Claude sessions to Zellij sessions and lane names via registry.
- Driver whitelist in `~/.config/lanes/registry.toml` via `drivers = ["zellij", "claude"]`.
- 13 unit tests, README, rustfmt/clippy/toolchain config.

## Natural next step

Define what a lane's **desired state** looks like in the config. Right now lanes are just a name + zellij_session. Everything downstream (lanes up, lanes switch, reset) needs to know what a lane *wants* — which worktrees, which browser tabs, what terminal layout.

That design work unlocks:

1. **`lanes up` / `lanes switch`** — apply desired state to navigate to a lane. The Go `claude-session` tool does a partial version (WezTerm tab + Zellij pane focus) but isn't integrated with lane config.
2. **Replace the Go claude-session tool** — `lanes sessions switch <id>` is the natural successor.
3. **Spinner integration** — surface per-lane worktree/URL in the snapshot via `spinner.toml` + `~/.local/share/spinner/<project>/spinner-state.json`.

## Key design decisions

- **Desired-state model, not capture/restore.** Config defines what a lane should look like; reset applies that definition. `infra lanes up` is the precedent.
- **Drivers, not facets.** "Facet" terminology was dropped — driver name implies the category.
- **Worktrees are resources, not lane identifiers.** A lane can span multiple worktrees across repos. Spinner derives the URL/database/port from `spinner.toml` + branch name; the lane just needs to know which repo + worktree.
- **Durable handle vs ephemeral locator.** Session UUID and Zellij session name are durable (go in `selector`). Pane IDs and tab IDs are ephemeral (go in `locator` only). This is load-bearing throughout the model.
