# Plan: Explicit Worktree Management

## Problem

The current auto-detection approach (fsnotify watcher auto-adopts any new
`.git/worktrees/` entry and immediately starts a server) is too magical:

- Creates servers for worktrees the user didn't intend spinner to manage
- Can't enforce naming conventions
- No clear lifecycle ownership

## Design

Spinner maintains a **managed worktree list** — a persistent file tracking
which branches it created or explicitly adopted. Only managed worktrees get
servers.

### Naming convention

Worktrees created by spinner are placed at `../<project>.<branch>`:

```
/Users/bmiller/src/projects/sheetwork           ← main worktree
/Users/bmiller/src/projects/sheetwork.feature-foo  ← spinner-managed worktree
/Users/bmiller/src/projects/sheetwork.fix-bar      ← spinner-managed worktree
```

Convention is only enforced for `spinner worktree add`. Adopted worktrees keep
whatever directory name they have.

### Managed worktree list

Stored at `~/.local/share/spinner/<project>/worktrees.json`:

```json
{
  "worktrees": [
    { "branch": "main", "path": "/Users/bmiller/src/projects/sheetwork", "managed_by": "git" },
    { "branch": "feature-foo", "path": "/Users/bmiller/src/projects/sheetwork.feature-foo", "managed_by": "spinner" }
  ]
}
```

`managed_by`:
- `"spinner"` — created or adopted by spinner; spinner owns the full lifecycle
- `"git"` — main worktree, always present, always managed

### New commands: `spinner worktree`

```
spinner worktree add <branch>     Create worktree at ../<project>.<branch>, mark setup pending
spinner worktree remove <branch>  Stop server, remove worktree from disk, clean artifacts
spinner worktree adopt <branch>   Add existing worktree to managed list, mark setup pending
spinner worktree list             List managed worktrees (also visible in spinner status)
```

### Watcher behavior change

The fsnotify watcher continues to watch `.git/worktrees/` but no longer
auto-adopts. Instead:

- New entry detected → check if it's in managed list → if not, log:
  `spinner: unmanaged worktree <branch> detected — run: spinner worktree adopt <branch>`
- No server is started for unmanaged worktrees

### `spinner status` changes

Two sections per project:

```
sheetwork  running

BRANCH          URL                              PORT   STATUS   SETUP
──────────────────────────────────────────────────────────────────────
main            http://main.sheetwork.test:4142  4142   running  ok
feature-foo     http://feature-foo...    :4137   stopped  pending  → spinner setup feature-foo

Unmanaged worktrees (not tracked by spinner):
  fix-bar  /Users/bmiller/src/projects/sheetwork-fix-bar  → spinner worktree adopt fix-bar
```

Unmanaged worktrees are shown dimmed with no port/URL/status — just path and
adopt hint. Branch name is always readable from git metadata regardless of
directory naming.

### `spinner up` behavior

On startup, only managed worktrees get servers. Unmanaged worktrees are
logged as a warning but otherwise ignored.

## Migration

Existing spinner users with auto-detected worktrees: on first run after this
change, unmanaged worktrees appear in the "Unmanaged" section with adopt hints.
No servers start for them until adopted.

## What we're NOT doing

- Renaming existing worktrees to match convention (adopt them as-is)
- Blocking `git worktree add` directly (spinner just won't manage those unless adopted)
- Auto-running setup on `worktree add` (separate explicit step, setup status shown in `spinner status`)
