# Lane Driver — Design Document

> **Note:** This document predates the language decision. Lane Driver is being
> built in Rust (axum + tokio + SQLite), not Phoenix/Elixir. The data model,
> HTTP API, and attention queue behavior below remain accurate. The supervision
> tree and LiveView sections are superseded. See **PLAN-poc.md** for the current
> implementation plan.

## What it is

Lane Driver is a Rust daemon (axum + tokio) that runs locally as the central brain of the Lanes system. It owns all lane state, handles attention events, serves an SSE endpoint for real-time push, and exposes an HTTP API consumed by the `lane` CLI and Claude Code hooks.

## Project setup

```bash
cd ~/src/projects/lanes
cargo new lane --bin
# dependencies: axum, tokio, sqlx, clap, toml, uuid, serde
```

Directory: `lanes/lane/`  
Port: `7701`  
Runtime: `lane daemon`

## Plugin architecture

A lane is a named context. Things associated with a lane are called **frames** — mirroring Hammerspoon's vocabulary (windows, screens, applications, spaces, frames). A frame is a narrow, lane-specific view into a singleton application. You don't own the whole app; you claim a specific view of it for this lane.

The exact frame model — what types exist, what fields they carry, how they map to Hammerspoon's concepts — is deferred until we're actually configuring real lanes. The right abstractions will emerge then.

What's known now:
- Phase 1 frames: Zellij session, Firefox container, Claude conversation (URL), Spinner project
- A lane can have multiple frames, one per integration
- Each frame has a `type` and a `config` map (plugin-specific JSON)
- Some frames can emit signals (e.g. Claude Code stop hook signals Lane Driver)
- The `lane` CLI knows how to activate each frame type; Lane Driver just stores config

## Runtime structure

```
lane daemon (tokio runtime)
├── axum HTTP server (routes, SSE)
├── Arc<RwLock<AppState>>  (shared lane state + attention queue)
└── sqlx connection pool   (SQLite persistence)
```

On startup, the daemon loads all persisted lanes from SQLite into `AppState`.
State changes are written back to SQLite and broadcast via SSE to connected clients.

## Data model

### `lanes` table

```sql
CREATE TABLE lanes (
  id              TEXT PRIMARY KEY,   -- UUID
  name            TEXT NOT NULL,
  path            TEXT,
  attention       BOOLEAN DEFAULT 0,
  active          BOOLEAN DEFAULT 0,
  position        INTEGER,
  attention_source TEXT,              -- which frame type last signaled
  last_signaled_at TEXT,             -- UTC ISO 8601
  last_active_at  TEXT,
  created_at      TEXT,
  updated_at      TEXT
);
```

### `lane_frames` table

```sql
CREATE TABLE lane_frames (
  id       TEXT PRIMARY KEY,   -- UUID
  lane_id  TEXT NOT NULL REFERENCES lanes(id),
  type     TEXT NOT NULL,      -- "zellij", "firefox", "claude", "spinner"
  config   TEXT NOT NULL       -- JSON blob, plugin-specific
);
```

`position` on lanes is assigned at insert (`COALESCE(MAX(position), 0) + 1`), used for stable next/prev ordering.

## State management

All lane state lives in `Arc<RwLock<AppState>>` shared across axum handlers.
On any state change: write to SQLite, broadcast updated lane via SSE to all
connected clients.

```rust
struct AppState {
    lanes: HashMap<Uuid, Lane>,
    attention_queue: VecDeque<Uuid>,  // FIFO, only lanes needing attention
}
```

**SSE topic:** single `/api/events` endpoint streams `lane_updated` events as JSON.

## HTTP API

```
GET    /api/lanes              List all lanes with frames (ordered by position)
POST   /api/lanes              Register a new lane
DELETE /api/lanes/:id          Unregister a lane
GET    /api/lanes/next         Next lane in carousel (round-robin from active)
GET    /api/lanes/prev         Previous lane
POST   /api/lanes/:id/signal   Mark lane as needing attention
PUT    /api/lanes/:id/active   Set lane as active (clears attention)
```

### POST /api/lanes body

```json
{
  "name": "sheetwork-feature",
  "path": "/Users/bmiller/src/projects/sheetwork",
  "frames": [
    {"type": "zellij", "config": {"session": "sheetwork-feature"}},
    {"type": "firefox", "config": {"container": "sheetwork-feature", "urls": ["http://localhost:4000"]}},
    {"type": "claude", "config": {"url": "https://claude.ai/..."}},
    {"type": "spinner", "config": {"project": "sheetwork"}}
  ]
}
```

### Carousel logic (phase 1: round-robin)

`GET /api/lanes/next`: find active lane by position, return next wrapping around. No attention filtering yet — attention is tracked and shown in the dashboard but does not affect routing in phase 1.

## Dashboard

**V1:** Hammerspoon overlay — `lane status` HTTP call returns plain text, rendered via `hs.alert`.

**Later:** A minimal HTML page served at `http://localhost:7701/` that subscribes to `/api/events` (SSE) and re-renders lane state on each event. No framework required — small JS fetch loop or HTMX. No auth.

## Claude Code hook integration

In any repo using `lane`, add to `.claude/settings.json`:

```json
{
  "hooks": {
    "Stop": [{"command": "lane signal"}]
  }
}
```

`lane signal` finds the lane for the current directory and POSTs to `/api/lanes/:id/signal`. Lane Driver marks it as needing attention, pushes to LiveView and any connected clients (Hammerspoon).

The lane ID for a given directory is resolved by the `lane` CLI by calling `GET /api/lanes` and matching on `path`.

### Response shape (lane)

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "name": "sheetwork-feature",
  "path": "/Users/bmiller/src/projects/sheetwork",
  "attention": false,
  "active": true,
  "position": 1,
  "attention_source": "claude_code",
  "last_signaled_at": "2026-04-12T10:00:00Z",
  "last_active_at": "2026-04-12T10:00:00Z",
  "frames": [
    {"type": "zellij", "config": {"session": "sheetwork-feature"}},
    {"type": "firefox", "config": {"container": "sheetwork-feature", "urls": ["http://localhost:4000"]}}
  ]
}
```

## Config

Hardcoded for now, configurable later:
- Port: `7701`
- DB path: `~/.local/share/lane-driver/lane_driver.db`
- Registry: `~/.config/lanes/registry.toml`

## What this is NOT (phase 1)

- No authentication
- No multi-device or cloud deployment
- No attention-priority ordering (FIFO queue only)
- No Spinner plugin yet — deferred
- Frame model details (exact Hammerspoon alignment) — deferred until real configuration
