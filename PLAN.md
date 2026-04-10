# Spinner — Project Kickoff

## What it is

A CLI tool that manages per-worktree dev server instances across git repos. Each git worktree gets its own server process, a deterministic port, and a stable URL. You can start and stop watching explicitly, view logs in real time, and switch between branches via a local dashboard or CLI command.

## Background — what this replaces

Previously handled by a manual stack: portree (port assignment + proxy), worktrunk hooks, dnsmasq, nginx, and a hand-rolled fish function. All of that is being replaced by spinner + dnsmasq only.

The reference implementation lives in `~/src/projects/sheetwork` — specifically `docs/local-worktree-setup.md`, `.portree.toml`, `.config/wt.toml`, and `docs/nginx/sheetwork.conf`. Read those for concrete examples of what spinner supersedes.

---

## Decisions made

**Language:** Go. Single static binary, excellent stdlib for process management and HTTP, battle-tested PTY library (`github.com/creack/pty`), fast build times.

**URL pattern:** `http://branch.project.test:PORT`
- Example: `http://feature-foo.sheetwork.test:4137`
- `.test` is IANA-reserved for local use
- Project name is part of the domain — critical for multi-repo clarity
- Port is included in the URL — no proxy layer, no port 80 binding, no nginx, no root required

**DNS:** dnsmasq wildcard per project (`address=/.sheetwork.test/127.0.0.1`). `spinner init` automates this setup with a single sudo prompt. One-time per project, permanent after that.

**No proxy layer.** dnsmasq resolves the hostname to 127.0.0.1. The port in the URL routes directly to the dev server. Nothing in between.

**Process model:** docker-compose convention.
- `spinner up` — foreground, streams labeled output from all worktree servers, Ctrl+C tears everything down
- `spinner up --detach` / `-d` — backgrounds, logs go to files
- `spinner logs [branch]` — tails a specific worktree's log file with color intact
- `spinner down` — stops everything

**PTY:** Each server subprocess runs in a pseudo-TTY so it believes it's connected to a terminal. ANSI color codes are preserved in log files. `spinner logs` output is colored.

**Port assignment:** Deterministic hash of branch name into a configured port range. Same branch always gets the same port. Spinner owns this — no portree.

**Framework agnostic:** The dev server command is just an arbitrary shell command in `spinner.toml`. Spinner doesn't know or care what it runs.

**Explicit over magical:** No auto-discovery. Repos must run `spinner init` to opt in.

**Lifecycle detection:** fswatch/kqueue on `.git/worktrees/`. New subdirectory → assign port, start server. Removed → reverse.

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
env = { MIX_ENV = "dev" }
```

---

## Commands

| Command | Description |
|---|---|
| `spinner init` | Create `spinner.toml`, register repo in global registry, configure dnsmasq (sudo) |
| `spinner up` | Start watching current repo's worktrees, foreground |
| `spinner up -d` | Same but detached |
| `spinner up --all` | All registered repos |
| `spinner down` | Stop current repo's servers and watcher |
| `spinner down --all` | All registered repos |
| `spinner status` | Global view: all repos, worktrees, ports, URLs, server status |
| `spinner logs [branch]` | Tail log for a specific worktree (default: current branch) |
| `spinner open [branch]` | Open worktree URL in default browser (default: current branch) |

---

## Local dashboard

Served by spinner itself at `http://spinner.test` (or similar). Shows all registered repos, active worktrees, ports, URLs as clickable links. Acts as a browser-based switcher — open it once, pin the tab.

---

## What's deferred (v2)

- **Docker support** — `command` abstraction is already compatible, but container port mapping needs specific handling
- **Log rotation** — max size, TTL on log files
- **`spinner attach [branch]`** — if tmux support is added, attach directly to a server's PTY session

---

## Testing approach (to be designed in next session)

Open questions:

- **Minimal test app:** A trivially small web server (probably a Go `net/http` handler that returns the branch name and port) to validate spinner's lifecycle management without depending on Phoenix. Lives either as a subdirectory of the spinner repo or a separate repo — TBD.
- **Unit tests:** Port assignment hashing, config parsing, registry read/write.
- **Integration tests:** Spin up the test app in a temp git repo with synthetic worktrees, assert URLs respond correctly, assert logs contain expected output.
- **The sheetwork repo** (`~/src/projects/sheetwork`) as the real-world integration test once the tool is working end-to-end.

---

## One-time machine setup (what `spinner init` handles)

1. Add `address=/.PROJECT.test/127.0.0.1` to `/opt/homebrew/etc/dnsmasq.conf`
2. Create `/etc/resolver/PROJECT.test` with `nameserver 127.0.0.1`
3. Reload dnsmasq: `sudo brew services restart dnsmasq`
