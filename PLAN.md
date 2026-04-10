# Spinner — Project Kickoff

## What it is

A CLI tool that manages per-worktree dev server instances across git repos. Each git worktree gets its own server process, a deterministic port, and a stable URL. You can start and stop watching explicitly, view logs in real time, and switch between branches via a local dashboard or CLI command.

## Background — what this replaces

Previously handled by a manual stack: portree (port assignment + proxy), worktrunk hooks, dnsmasq, and a hand-rolled fish function. All of that is being replaced by spinner + dnsmasq only.

The reference implementation lives in `~/src/projects/sheetwork` — specifically `docs/local-worktree-setup.md`, `.portree.toml`, `.config/wt.toml`, and `docs/nginx/sheetwork.conf`. Read those for concrete examples of what spinner supersedes.

---

## Decisions made

**Language:** Go. Single static binary, excellent stdlib for process management and HTTP, battle-tested PTY library (`github.com/creack/pty`), fast build times.

**Go module path:** `github.com/bmiller/spinner`

**URL pattern:** `http://branch.project.test:PORT`
- Example: `http://feature-foo.sheetwork.test:4137`
- `.test` is IANA-reserved for local use
- Project name is part of the domain — critical for multi-repo clarity
- Port is included in the URL — no proxy layer, no port 80 binding, no nginx, no root required

**DNS:** dnsmasq wildcard per project (`address=/.sheetwork.test/127.0.0.1`). `spinner init` automates this setup with a single sudo prompt. One-time per project, permanent after that.

**No proxy layer.** dnsmasq resolves the hostname to 127.0.0.1. The port in the URL routes directly to the dev server. Nothing in between.

**Process model:** docker-compose convention.
- `spinner up` — foreground, streams labeled output from current repo's worktree servers, Ctrl+C tears everything down
- `spinner up --detach` / `-d` — backgrounds, logs go to files
- `spinner up --all` — **requires `-d`**; errors if run without `--detach`. Starts all registered repos detached.
- `spinner logs [branch]` — tails a specific worktree's log file with color intact
- `spinner down` — stops current repo's servers and watcher
- `spinner down --all` — stops all registered repos

**PTY:** Each server subprocess runs in a pseudo-TTY so it believes it's connected to a terminal. ANSI color codes are preserved in log files. `spinner logs` output is colored.

**Port assignment:** Deterministic hash of branch name into a configured port range. Same branch always gets the same port. Spinner owns this — no portree.

**Framework agnostic:** The dev server command is just an arbitrary shell command in `spinner.toml`. Spinner doesn't know or care what it runs.

**Explicit over magical:** No auto-discovery. Repos must run `spinner init` to opt in.

**Lifecycle detection:** fswatch/kqueue on `.git/worktrees/`. New subdirectory → assign port, start server. Removed → reverse.

**Status output:** Styled terminal output using [Lipgloss](https://github.com/charmbracelet/lipgloss) (Charm). One-shot render, no interactive TUI needed for `spinner status`.

---

## Config

**Global** (`~/.config/spinner/registry.toml`):
```toml
[[repos]]
path = "/Users/bmiller/src/projects/sheetwork"
name = "sheetwork"
```

**Per-project** (`spinner.toml` in main worktree root):
```toml
[project]
name = "sheetwork"
domain_suffix = "sheetwork.test"
port_range = { min = 4100, max = 4199 }

[server]
command = "mix phx.server"

[server.env]
MIX_ENV = "dev"
DATABASE_URL = "postgres://localhost/sheetwork_{branch}_dev"
```

`{branch}` is a template variable spinner substitutes at runtime with the worktree's branch name. This is how spinner handles infrastructure conflicts across worktrees — not by running separate database instances, but by injecting branch-derived values for things like database names, so each worktree points at its own slice of a shared Postgres instance.

---

## Commands

| Command | Description |
|---|---|
| `spinner init` | Create `spinner.toml`, register repo in global registry, configure dnsmasq (sudo) |
| `spinner up` | Start watching current repo's worktrees, foreground |
| `spinner up -d` | Same but detached |
| `spinner up --all` | All registered repos — **detached only**, errors without `-d` |
| `spinner down` | Stop current repo's servers and watcher |
| `spinner down --all` | All registered repos |
| `spinner status` | Global view: all repos, worktrees, ports, URLs, server status (Lipgloss-styled) |
| `spinner logs [branch]` | Tail log for a specific worktree (default: current branch) |
| `spinner open [branch]` | Open worktree URL in default browser (default: current branch) |

---

## Local dashboard

Served by spinner itself. Uses path-based routing so there's no collision with branch-named subdomains:

- `http://spinner.test:7700` — global view: all registered repos and their worktrees
- `http://spinner.test:7700/sheetwork` — project-specific view

`spinner init` adds `address=/spinner.test/127.0.0.1` to dnsmasq on first run (globally, once). The dashboard port is fixed at `7700`.

The dashboard is served by the detached spinner daemon and acts as a browser-based switcher — open it once, pin the tab.

---

## Infrastructure isolation

The typical dev setup — app running locally, backing services (Postgres, Redis, etc.) in Docker — works naturally with spinner. The app is what spinner manages; the Docker services are started once with `docker-compose up -d` and forgotten. Spinner just injects the right env vars so each worktree connects to its own database within the shared instance.

Per-worktree Docker Compose projects (true infrastructure isolation per branch) are possible via `docker-compose --project-name {branch}` but add significant complexity. Not a v1 concern — the shared-instance-with-per-branch-database approach covers realistic development scenarios without it.

---

## Testing strategy

**Test app:** `testapp/` subdirectory contains source for a minimal Go HTTP server (returns branch name + port). Integration tests build it into a temp binary, then create a fresh `git init` repo in a temp directory, copy it in, and create synthetic worktrees from there. This mirrors real usage exactly — no sub-repo quirks.

**Unit tests:** Port assignment hashing, config parsing, registry read/write, template variable substitution.

**Integration tests:** Spin up the test app in a temp git repo with synthetic worktrees, assert URLs respond correctly, assert logs contain expected output.

**Real-world test:** The sheetwork repo (`~/src/projects/sheetwork`) once the tool is working end-to-end.

---

## What's deferred (v2)

- **Per-worktree Docker Compose projects** — true infrastructure isolation; spinner would manage `docker-compose --project-name {branch} up/down` alongside the app server
- **Log rotation** — max size, TTL on log files
- **`spinner attach [branch]`** — if tmux support is added, attach directly to a server's PTY session
- **Multiplexed foreground output** — `spinner up --all` in foreground with color-coded per-worktree output streams, likely using a BubbleTea-based TUI

---

## One-time machine setup (what `spinner init` handles)

1. Add `address=/.PROJECT.test/127.0.0.1` to `/opt/homebrew/etc/dnsmasq.conf`
2. Create `/etc/resolver/PROJECT.test` with `nameserver 127.0.0.1`
3. Reload dnsmasq: `sudo brew services restart dnsmasq`
4. On first-ever `spinner init` across all projects: also add `address=/spinner.test/127.0.0.1` and create `/etc/resolver/spinner.test`
