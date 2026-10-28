# Claude Directives (lanes)

Shared directives are in `../CLAUDE.md`.

See `README.md` for what Lanes is, its parts and how it's built.

## Working in this repo

- Lanes is in daily use, and `just dev` is usually running: an edit under
  `lanes-cli/` rebuilds the `lanes` binary every hotkey and Hammerspoon call
  runs, and restarts Lanes Switch. Keep each save building.
- lanes-cli is the library behind Lanes Switch too (`ui/src-tauri` depends
  on it by path): config and state go through `lanes::config` and
  `lanes::state`, never read or written by the UI on its own.
- Run tasks through the `justfile` (`just test`, `just clippy`).
