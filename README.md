# spinner

A CLI tool that manages per-worktree dev server instances across git repositories. Each git worktree gets its own server process, a deterministic port, and a stable local URL. Switch between branches by switching URLs — no manual server restarts, no port juggling.

```
http://feature-foo.sheetwork.test:4137   ← feature-foo worktree
http://main.sheetwork.test:4142          ← main worktree
http://fix-bar.sheetwork.test:4159       ← fix-bar worktree
```

---

## How it works

- **One server per worktree.** `spinner up` watches `.git/worktrees/` and starts a dev server for each worktree. Add a worktree → server starts. Remove it → server stops.
- **Deterministic ports.** Each branch name hashes to a stable port within your configured range. Same branch, same port, every time.
- **PTY-wrapped processes.** Each server runs under a pseudo-TTY so it thinks it's connected to a terminal. ANSI colors are preserved in log files.
- **Framework agnostic.** The dev server command is just a shell command in `spinner.toml`. Works with Phoenix, Rails, Next.js, or anything else.
- **Template variables.** `{branch}` in env values is substituted at runtime, so each worktree can point at its own database, bucket, etc.

---

## Prerequisites

- Go 1.21+
- [dnsmasq](https://formulae.brew.sh/formula/dnsmasq) (`brew install dnsmasq`) — for `.test` domain resolution
- macOS (Linux support is possible but untested; the dnsmasq setup differs)

---

## Installation

```bash
git clone https://github.com/bmiller/spinner
cd spinner
go build -o ~/go/bin/spinner .
```

---

## Quick start

### 1. Initialize a project

Run this once from the root of a git repo:

```bash
cd ~/src/projects/myapp
spinner init
```

This will prompt you for:
- **Project name** (default: directory name)
- **Domain suffix** (default: `projectname.test`)
- **Port range** (default: 4100–4199)
- **Dev server command** (e.g. `mix phx.server`, `npm run dev`, `rails server`)

It creates `spinner.toml`, registers the project globally, and configures dnsmasq (requires one `sudo` prompt). After the first project, `spinner.test` is also set up for the dashboard.

### 2. Start spinner

```bash
spinner up          # foreground — Ctrl+C to stop
spinner up -d       # detached (background daemon)
```

### 3. Check status

```bash
spinner status
```

```
  myapp  running
  /Users/bmiller/src/projects/myapp

  branch       url                              port   status
  ──────────────────────────────────────────────────────────
  main         http://main.myapp.test:4142      4142   running
  feature-foo  http://feature-foo.myapp.test:4137  4137  running
```

### 4. Stop spinner

```bash
spinner down        # current project
spinner down --all  # all registered projects
```

---

## Commands

| Command | Description |
|---|---|
| `spinner init` | Initialize project, create `spinner.toml`, configure dnsmasq |
| `spinner up` | Start servers for all worktrees (foreground) |
| `spinner up -d` | Start servers (detached/background) |
| `spinner up --all -d` | Start all registered projects (detached only) |
| `spinner down` | Stop current project |
| `spinner down --all` | Stop all registered projects |
| `spinner status` | Show all projects, worktrees, ports, server status, and setup status |
| `spinner logs [branch]` | Tail server logs for a worktree (default: current branch) |
| `spinner setup [branch]` | Run the setup command for a worktree (default: current branch) |
| `spinner setup --all` | Run setup for all worktrees with pending or failed status |
| `spinner clean [branch]` | Remove spinner log artifacts for a worktree (default: current branch) |
| `spinner clean --all` | Remove spinner log artifacts for all branches |
| `spinner open [branch]` | Open worktree URL in default browser (default: current branch) |

---

## Configuration

### Per-project: `spinner.toml`

Placed in the main worktree root, committed to the repo.

```toml
[project]
name = "sheetwork"
domain_suffix = "sheetwork.test"
port_range = { min = 4100, max = 4199 }

[server]
command = "mix phx.server"
setup = "mix deps.get && mix ecto.migrate"   # optional

[server.env]
MIX_ENV = "dev"
DATABASE_URL = "postgres://localhost/sheetwork_{branch}_dev"
```

**`setup`** — optional shell command run by `spinner setup` before a worktree's server is first started. Use `&&` to sequence multiple steps. When configured:

- New worktrees are marked `pending` automatically when detected by the watcher.
- `spinner up` warns if a worktree hasn't been set up yet but still starts the server.
- `spinner status` shows a SETUP column with status (`pending`, `ok`, `failed`, `setting up...`) and a hint when action is needed.

**Template variables** — `{branch}` in any env value is replaced with the worktree's branch name at runtime. Use this to point each worktree at its own database, S3 bucket prefix, etc.

The `PORT` environment variable is always injected automatically — your server should bind to it.

### Global registry: `~/.config/spinner/registry.toml`

Managed by `spinner init`. Lists all registered projects:

```toml
[[repos]]
path = "/Users/bmiller/src/projects/sheetwork"
name = "sheetwork"

[[repos]]
path = "/Users/bmiller/src/projects/otherapp"
name = "otherapp"
```

---

## Local dashboard

When spinner is running, a dashboard is available at:

```
http://spinner.test:7700          — all projects
http://spinner.test:7700/myapp    — single project view
```

Shows all registered projects, active worktrees, ports, and clickable URLs. Pin this tab in your browser as a switcher.

---

## Infrastructure isolation

Spinner manages your app server. Your backing services (Postgres, Redis, etc.) run separately — typically via `docker-compose up -d` once, then forgotten.

Per-worktree isolation is achieved through env vars, not separate service instances. For example:

```toml
[server.env]
DATABASE_URL = "postgres://localhost/myapp_{branch}_dev"
```

Each worktree connects to its own database (`myapp_main_dev`, `myapp_feature-foo_dev`, etc.) within the same Postgres instance. This requires creating the databases manually (or via a `mix ecto.setup` equivalent), but avoids the overhead of running a full Docker Compose stack per branch.

---

## Port assignment

Ports are assigned by hashing the branch name into the configured port range using FNV-32a. The hash is stable — the same branch name always gets the same port regardless of what other worktrees exist. There are no port conflict checks; if two branches hash to the same port, the second server will fail to start and log an error.

To see what port a branch will get without starting anything:

```bash
# The formula: fnv32a(branch) % (max - min + 1) + min
# spinner status shows it once running
spinner status
```

---

## Runtime files

Spinner stores runtime state under `~/.local/share/spinner/`:

```
~/.local/share/spinner/
└── myapp/
    ├── daemon.pid              — PID of the running daemon
    ├── spinner-state.json      — all worktree state: server status + setup status
    └── logs/
        ├── main.log
        ├── feature-foo.log
        ├── spinner-setup-main.log
        └── spinner-setup-feature-foo.log
```

`spinner-state.json` is written by both the daemon (server status, on a 5s tick) and the CLI (`spinner setup` updates setup status). Setup status persists across daemon restarts — the daemon merges it in on each save rather than overwriting it.

Log files persist across restarts and accumulate until deleted manually. Use `spinner clean [branch]` to remove them.

---

## DNS setup (what `spinner init` does)

For each project, `spinner init` appends a wildcard entry to `/opt/homebrew/etc/dnsmasq.conf`:

```
address=/.sheetwork.test/127.0.0.1
```

And creates a resolver file at `/etc/resolver/sheetwork.test`:

```
nameserver 127.0.0.1
```

Then restarts dnsmasq. This is a one-time, permanent setup per project.

On first-ever `spinner init` across all projects, the dashboard domain is also configured:
```
address=/.spinner.test/127.0.0.1
```

---

## Manual testing with testapp

`testapp/` contains a minimal Go HTTP server for use in integration tests and manual experimentation. It reads `PORT` and `BRANCH` from env and returns them as JSON.

```bash
# Build both binaries
go build -o /tmp/spinner .
go build -o /tmp/testapp ./testapp

# Create a scratch git repo
mkdir /tmp/scratch && cd /tmp/scratch
git init && git commit --allow-empty -m "init"

# Write spinner.toml
cat > spinner.toml << 'EOF'
[project]
name = "scratch"
domain_suffix = "scratch.test"

[project.port_range]
min = 5000
max = 5099

[server]
command = "/tmp/testapp"

[server.env]
BRANCH = "{branch}"
EOF

# Add worktrees
git worktree add /tmp/scratch-feat-a feat-a
git worktree add /tmp/scratch-feat-b feat-b

# Start spinner (foreground)
/tmp/spinner up
```

Spinner will log the assigned port for each branch. Since dnsmasq isn't configured for `scratch.test`, connect directly via `localhost:PORT`:

```bash
curl localhost:PORT
# {"branch":"feat-a","port":"5042"}
```

---

## Development

```bash
# Run all tests
go test ./...

# Run only unit tests (fast)
go test ./internal/...

# Run integration tests (starts real server processes)
go test -timeout 60s .

# Build
go build -o spinner .
```

### Project structure

```
spinner/
├── main.go                      # entry point
├── spinner.toml                 # (not present; created by spinner init in a real project)
├── cmd/                         # CLI commands (cobra)
│   ├── root.go
│   ├── init.go                  # spinner init
│   ├── up.go                    # spinner up [--detach] [--all]
│   ├── down.go                  # spinner down [--all]
│   ├── status.go                # spinner status (lipgloss-styled output)
│   ├── logs.go                  # spinner logs [branch]
│   ├── open.go                  # spinner open [branch]
│   └── daemon.go                # spinner _daemon (internal, hidden)
├── internal/
│   ├── config/                  # spinner.toml and registry.toml parsing
│   ├── port/                    # deterministic port assignment (FNV hash)
│   ├── git/                     # worktree discovery, branch detection
│   ├── state/                   # runtime state (JSON), PID files, log paths
│   ├── process/                 # PTY-wrapped server process lifecycle
│   ├── watcher/                 # fsnotify watcher for .git/worktrees/
│   ├── daemon/                  # main daemon loop (manager)
│   ├── dashboard/               # HTTP dashboard server
│   └── dnsmasq/                 # dnsmasq.conf editing, resolver setup
├── testapp/                     # minimal HTTP server for integration tests
└── integration_test.go          # integration tests (real git repos, real processes)
```

---

## What's not implemented yet (v2)

- **Log rotation** — log files grow indefinitely
- **Per-worktree Docker Compose** — `docker-compose --project-name {branch}` for true infrastructure isolation per branch
- **`spinner attach [branch]`** — attach to a server's PTY session directly (requires tmux)
- **Multiplexed foreground output** — `spinner up --all` in foreground with color-coded per-project output (likely via a Bubbletea TUI)
- **`spinner up --all` foreground** — currently requires `--detach`; foreground multiplexing is deferred to v2
