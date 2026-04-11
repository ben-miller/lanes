# Future Work: Failed State Visibility

## Idea

Surface failed server and setup states in `spinner status` so users can tell at a glance when something went wrong, not just when something is stopped.

## Server failed state

Currently, if a server fails to start (port conflict, bad command, crash on launch), the daemon logs an error and moves on — the worktree simply doesn't appear as running. No persistent record of the failure.

To implement:
- Track failed starts in daemon state (new `StatusFailed` alongside `StatusRunning`/`StatusStopped`)
- Daemon catches `startWorktree` errors and records them in state with an error message/exit code
- `spinner status` renders failed state in red with a hint to check logs

## Setup failed state

Already planned as part of the setup feature (`SetupStatus = "failed"`). This item is specifically about making sure `spinner status` renders it prominently — red, with the command to re-run setup — not just a dim label.

## Open questions

- Should a failed server auto-retry? (Probably not — too magic. Log it and let the user decide.)
- Should `spinner up` exit non-zero if any server fails to start, or just warn?
