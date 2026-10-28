# Lanes

Lanes gathers a working environment into *contexts* (lanes): the Zellij
session, the repos, the Claude sessions, the Boards project and the windows
that belong to one piece of work, and switches between them. Part of the
Dev Platform (catalog: `catalog-info.yaml`; conventions:
`~/src/projects/CONVENTIONS.md`).

## What's here

| Directory | What | Docs |
|---|---|---|
| `lanes-cli/` | `lanes`, the Rust CLI and library: the contexts (`~/.config/lanes`), the snapshot, focus and cycle, the drivers (Zellij, Claude, shell, Boards), Lanes' published facts. | `lanes-cli/README.md` |
| `ui/` | Lanes Switch (appctl name `lanes`), the Tauri switcher dashboard (Svelte frontend, `src-tauri/` Rust side). It depends on lanes-cli by path. | this file; `lanes-cli/README.md` ("Facts Lanes Switch publishes", "Lanes Switch window state") |
| `claude-session/` | `claude-session`, a Go CLI and the Claude Code hooks that keep the active-sessions registry (`~/.claude/active-sessions/`). | `claude-session/README.md` |

Design notes at the root: `DECISIONS.md` (settled rationale), `CONTEXT.md`
(where work left off), `PLAN-*.md` (plans), `NOTES-*.md` and `BUG-*.md`.

## Where things live

- **Config:** `~/.config/lanes/` (TOML; tracked in `~/src/infra`), read by
  both lanes-cli and Lanes Switch. Layout: `lanes-cli/README.md`,
  "Configuration".
- **State:** `~/.local/state/lanes/state.kdl` (the focused lane, toggles),
  plus runtime files beside it (`switch.sock`, `cache/`, `shell/`,
  `perf.log`).
- **Logs:** Lanes Switch's in `~/.local/state/bmiller/lanes/logs/`
  (`appctl logs lanes`); what process-compose captures in each app's
  `logs/process-compose.log` there (`dp ps logs`).

## Development

Tasks are recipes in the `justfile` (`just --list`):

- `just dev`: Lanes Switch (`npm run tauri dev`) and a lanes-cli rebuild on
  every change, under process-compose (`process-compose.yaml`). In the
  background: `dp ps up -d`, `dp ps logs lanes -f`, `dp ps down -p lanes`.
  `tauri dev` also rebuilds on lanes-cli edits (`build.additionalWatchFolders`).
- `just test`, `just clippy`: both Rust crates.
- `just build`: the `lanes` binary; `~/.local/bin/lanes` links to
  `lanes-cli/target/debug/lanes`, so the next `lanes` call runs it.
- `just build-claude-session`: `claude-session/bin/claude-session`.
- `just install`: Lanes Switch's release bundle into `/Applications`.
