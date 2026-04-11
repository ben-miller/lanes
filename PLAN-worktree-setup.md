# Plan: Worktree Setup Command & Status Visibility

## Problem

When a new worktree is created for a project like sheetwork (Phoenix/Elixir),
the worktree is a fresh checkout that isn't ready to run. It may need:

- `mix deps.get` — install/sync dependencies
- `mix ecto.migrate` — run database migrations
- `npm install` / `mix assets.setup` — frontend assets
- Any other project-specific bootstrap steps

Currently spinner just tries to start the server and it fails silently (or
noisily) with no guidance for the user.

## Design Decision: Explicit `spinner setup`

Lean toward **less magic**: don't auto-run setup behind the scenes. Instead,
give it a dedicated command and make the "not set up" state highly visible so
the user knows exactly what to do.

### `spinner.toml` addition

```toml
[server]
  command = "mix phx.server"
  setup = "mix deps.get && mix ecto.migrate"
```

The `setup` field is optional. If absent, `spinner setup` is a no-op (or
errors with "no setup command configured").

### `spinner setup [branch]`

- Runs `server.setup` in the worktree's directory
- Defaults to current branch if no arg given
- Shows output live (not swallowed)
- Marks the worktree as "initialized" in state once setup exits 0
- If setup fails, marks it as "setup failed" with the exit code

### State tracking

Add to `WorktreeState`:

```go
SetupStatus  string    // "pending" | "ok" | "failed" | ""
SetupAt      time.Time
```

- `""` — no setup command configured, not applicable
- `"pending"` — setup command exists but hasn't been run yet
- `"ok"` — setup ran and exited 0
- `"failed"` — setup ran and exited non-zero

### Visibility

**`spinner status` output:**
- Show setup status as a column or inline note
- Highlight "pending" and "failed" states prominently (red/yellow)
- Example:
  ```
  sheetwork   running
  
  BRANCH          URL                              STATUS    SETUP
  ------          ---                              ------    -----
  main            http://main.sheetwork.test:4155  running   ok
  test-spinner    http://test-spinner...    :4123  stopped   pending ← run: spinner setup test-spinner
  ```

**Dashboard (`spinner.test:7700/sheetwork`):**
- Same table, same pending/failed callout
- "pending" row links or shows the command to run

**`spinner up` behavior:**
- If a worktree has `setup = "pending"` or `setup = "failed"`, still attempt
  to start the server (user might know what they're doing)
- But print a clear warning:
  ```
  warning: test-spinner has not been set up — run: spinner setup test-spinner
  ```

**On new worktree detection (fsnotify):**
- When daemon detects a new worktree, if a setup command is configured,
  set its setup status to "pending" and log a message to stderr:
  ```
  spinner: new worktree test-spinner detected — run: spinner setup test-spinner
  ```

### Re-running setup

- `spinner setup` always re-runs, even if status is already "ok" — branches
  get new migrations, deps change, etc.
- After a successful re-run, status goes back to "ok" and `SetupAt` updates.

## What we're NOT doing

- No auto-running setup on worktree start (too magic, hides failures)
- No per-step setup (just one shell command; use `&&` for sequencing)
- No setup timeout (let it run as long as it needs)

## Execution model

The **daemon** runs setup commands, not the CLI process. The daemon knows each
worktree's path, owns the state, and has the PTY infrastructure. The CLI sends
a `SetupWorktree(branch)` RPC; the daemon executes the setup command in the
worktree's directory.

### Log file

Output is written to:

```
<state_dir>/<project>/<branch>/spinner-setup.log
```

Separate from the server log so `spinner logs <branch>` stays clean for server
output. Tail with `spinner logs <branch> --setup`.

After sending the RPC, the CLI immediately tails `spinner-setup.log`. Ctrl+C
detaches from the tail; setup continues in the daemon.

### State machine

```
""        → "pending"  (worktree detected, setup command configured)
"pending" → "running"  (spinner setup invoked)
"running" → "ok"       (exit 0)
"running" → "failed"   (exit non-zero)
"ok"/"failed" → "running"  (re-run)
```

`spinner status` shows "setting up..." when status is `"running"`.

### `--all` flag

`spinner setup --all` runs setup for all pending/failed worktrees across all
registered projects. Daemon runs them (in parallel or serial TBD). CLI reports
"setup started for: X, Y, Z" and optionally tails a multiplexed view.

## Open questions

- Parallel vs. serial execution for `--all`? Parallel is faster but interleaved
  log output is harder to read. Likely parallel with per-worktree log files and
  a summary on completion.
- Should failed setup prevent the server from starting at all, or just warn?
  **Decided: warn only, don't block.** Warning in `spinner up` output should be
  prominent but the server still attempts to start.
