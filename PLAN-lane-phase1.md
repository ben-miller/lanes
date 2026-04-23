# Lane — Phase 1 Plan

## Naming

- **Lanes** — the overall project
- **`lane`** — the CLI command (Rust binary)
- **Lane Driver** — the daemon component (Rust, axum + tokio)
- **`lane daemon`** — CLI subcommand that starts the Lane Driver daemon

## Architecture

Two components, one binary:

**Lane Driver** — Rust axum/tokio daemon. The central brain. Runs as a local
process. Owns all lane state, handles attention events, exposes an HTTP API,
pushes updates via SSE.

**`lane`** — Rust CLI. Thin client. Calls the Lane Driver API for state/decisions,
then executes local shell operations (Zellij, WezTerm) based on the response.
Hammerspoon calls this; users call this; Claude Code hooks call this.

The CLI does the desktop manipulation. Lane Driver does the thinking.

**Start here: PLAN-poc.md** — before building phase 1, the PoC verifies the
critical feasibility assumptions (Zellij env vars in Claude Code hooks, remote
pane focusing). Phase 1 begins after the PoC succeeds.

## Repo structure

```
lanes/
  lane/             ← Rust project (cargo workspace or standalone crate)
    src/
      main.rs
      cli/
      daemon/
      registry.rs
    Cargo.toml
  spinner/          ← Go project, unchanged
  PLAN-*.md
```

Lane Driver is the daemon component inside the `lane` crate, not a separate repo.

## Lane Driver (Rust daemon)

Responsibilities:
- Lane registry (which lanes exist, their config — read from `~/.config/lanes/registry.toml`)
- Active lane state
- Attention queue (which lanes need the user, in FIFO order)
- HTTP API consumed by the `lane` CLI and Claude Code hooks
- SSE endpoint for real-time push to Hammerspoon overlay

### Lane state (per lane)

```rust
struct Lane {
    id: Uuid,
    name: String,
    zellij_session: String,
    attention: bool,
    pane_id: Option<u32>,        // set when Claude Code signals
    last_signaled_at: Option<DateTime<Utc>>,
    last_active_at: Option<DateTime<Utc>>,
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

## Rust CLI (`lane`)

Thin client — one HTTP call to daemon, then local shell operations.

### Commands

```
lane daemon                 Start the Lane Driver daemon
lane init                   Register current dir as a lane (POST /api/lanes)
lane list                   List all lanes (GET /api/lanes)
lane status                 Show attention queue and lane states
lane switch <name>          Activate a specific lane
lane next                   Activate next lane in attention queue
lane prev                   Activate previous lane
lane signal                 Mark current lane as needing attention (for hooks)
lane forget                 Unregister current lane
```

### Activating a lane

`lane next` (and `lane switch`, `lane prev`):

1. GET `/api/lanes/next` → get `{ session_name, pane_id }`
2. **WezTerm**: `wezterm cli list` → find tab by title → `wezterm cli activate-tab --tab-id <id>`
3. **Zellij**: `zellij --session <session_name> action focus-pane <pane_id>`
4. PUT `/api/lanes/:id/active` → daemon clears attention for that lane

## Lane config

Registry lives at `~/.config/lanes/registry.toml` (already used by `infra lanes up`):

```toml
[[lanes]]
name = "sheetwork"
zellij_session = "sheetwork"
position = 0

[[lanes]]
name = "lanes dev"
zellij_session = "lanes"
position = 1
```

Per-directory `.lane/lane.toml` may be added later for richer config. For now the
global registry is the source of truth.

## Integration interface (Rust CLI side)

Each integration implements a trait — defined from the start even though only
Zellij is implemented in phase 1:

```rust
trait Integration {
    fn activate(&self, lane: &Lane) -> Result<()>;
    fn deactivate(&self, lane: &Lane) -> Result<()>;
    fn status(&self, lane: &Lane) -> Result<String>;
}
```

`lane switch` iterates registered integrations and calls `activate`. Adding a new
integration = implement the trait + wire up in config. No changes to core logic.

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

- No Firefox integration (deferred post-PoC)
- No Spinner integration
- No attention-priority ordering (FIFO queue only)
- No cloud hosting, no multi-device, no identity
- No Hammerspoon window raising beyond WezTerm tab + Zellij pane switching
- No per-directory `.lane/lane.toml` (global registry only)
