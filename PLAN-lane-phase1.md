# Lane — Phase 1 Plan

## Naming

- **Lanes** — the overall project
- **`lane`** — the CLI command (Go binary)
- **Lane Driver** — the Phoenix/LiveView application (separate repo)
- **`lane drive`** — CLI subcommand that starts/manages the Lane Driver server

## Architecture

Two components:

**Lane Driver** — Phoenix/LiveView application. The central brain. Runs as a
local daemon (LaunchAgent). Owns all lane state, handles attention events,
serves a LiveView dashboard, exposes an HTTP API.

**`lane`** — Go CLI. Thin client. Calls the Lane Driver API for state/decisions,
then executes local shell operations (Zellij, Firefox) based on the response.
Hammerspoon calls this; users call this; Claude Code hooks call this.

The CLI does the desktop manipulation. Lane Driver does the thinking.

## Repo structure

The `lane` CLI binary lives alongside `spinner` in this repo:

```
cmd/
  spinner/
    main.go         ← current main.go moved here
  lane/
    main.go         ← new
Makefile            ← builds both binaries
```

Lane Driver is a separate repo (not yet created). For now, planning only.

## Lane Driver (Phoenix app)

Responsibilities:
- Lane registry (which lanes exist, their config)
- Active lane state
- Attention state per lane (which lanes need the user)
- HTTP API consumed by the `lane` CLI and Claude Code hooks
- LiveView dashboard
- PubSub — pushes state changes to connected clients

### Lane state (per lane)

```elixir
%Lane{
  id: uuid,
  name: "sheetwork-feature",
  path: "/Users/bmiller/src/...",
  zellij_session: "sheetwork-feature",
  firefox_container: "sheetwork-feature",
  urls: ["http://localhost:4000"],
  attention: false,
  last_active: ~U[...]
}
```

### HTTP API (phase 1)

```
GET  /api/lanes              list all lanes
POST /api/lanes              register a new lane
GET  /api/lanes/next         get next lane to switch to (attention-aware)
GET  /api/lanes/prev         get previous lane
POST /api/lanes/:id/signal   mark lane as needing attention (Claude Code hook)
PUT  /api/lanes/:id/active   set as active lane
```

### GenServer per lane

Each lane is its own GenServer for isolated lifecycle and state. Supervised
under a DynamicSupervisor. Clean OTP model, extensible to async events per lane.

## Go CLI (`lane`)

Thin client — one HTTP call to Phoenix, then local shell operations.

### Commands

```
lane init                   Register current dir as a lane (POST /api/lanes)
lane list                   List all lanes (GET /api/lanes)
lane status                 Show all lanes with state
lane switch <name>          Activate a specific lane
lane next                   Activate next lane (GET /api/lanes/next → switch)
lane prev                   Activate previous lane (GET /api/lanes/prev → switch)
lane signal                 Mark current lane as needing attention (for hooks)
lane forget                 Unregister current lane
lane drive                  Start the Lane Driver server
```

### Activating a lane

`lane switch` (and `lane next`/`lane prev`, which resolve via Phoenix then switch):

1. Ask Phoenix: `GET /api/lanes/next` → get lane config (session name, URLs, etc.)
2. **Zellij**: `zellij attach --create <session>` — attaches if exists, creates if not
3. **Firefox**: for each URL: `open 'ext+container:name=<container>&url=<url>'`
4. Tell Phoenix: `PUT /api/lanes/:id/active`

## Lane config

`lane init` drops a `.lane/lane.toml` in the current directory:

```toml
[lane]
name = "sheetwork-feature"

[zellij]
session = "sheetwork-feature"   # defaults to lane name

[firefox]
container = "sheetwork-feature" # defaults to lane name
urls = ["http://localhost:4000"]
```

Presence of a section = that integration is active. No explicit integration list.

## Integration interface (Go CLI side)

Each integration implements:

```go
type Integration interface {
    Activate(cfg LaneConfig) error
    Deactivate(cfg LaneConfig) error
    Status(cfg LaneConfig) (string, error)
}
```

`lane switch` iterates registered integrations and calls `Activate`. Adding a
new integration (WezTerm, Obsidian, etc.) = implement the interface + add a
config section. No changes to core logic.

## Claude Code hook (phase 1)

In any repo using `lane`, add to `.claude/settings.json`:

```json
{
  "hooks": {
    "Stop": [{"command": "lane signal"}]
  }
}
```

`lane signal` finds the lane for the current directory and POSTs to
`/api/lanes/:id/signal`. Lane Driver marks it as needing attention, pushes to
LiveView and any connected clients (Hammerspoon).

## Hammerspoon

Hyper+J → `hs.task.new("/usr/local/bin/lane", nil, {"next"}):start()`
Hyper+K → same with `"prev"`

No Hammerspoon-side state. All intelligence is in Lane Driver.

## WezTerm + Zellij setup

- Install Zellij: `brew install zellij`
- WezTerm launches a shell (not Zellij directly) — Zellij sessions are attached
  explicitly per lane via `lane switch`
- Named sessions persist across WezTerm restarts — the key feature

WezTerm workspaces are not used. Zellij sessions provide the equivalent with
better persistence.

## Firefox setup

Install: [open-url-in-container](https://addons.mozilla.org/en-US/firefox/addon/open-url-in-container/)

Then from the CLI:
```bash
open 'ext+container:name=sheetwork-feature&url=http://localhost:4000'
```

Container is created on first use. No pre-registration needed.

## What this is NOT (phase 1)

- No multi-server support (Spinner integration deferred)
- No WezTerm workspace switching (Zellij handles it)
- No attention-priority carousel logic (just round-robin next/prev for now)
- No cloud hosting, no multi-device, no identity
- No Hammerspoon window raising beyond Zellij session switching
