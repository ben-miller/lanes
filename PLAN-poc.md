# Lanes — Proof of Concept Plan

## Goal

Verify the two critical technical assumptions and, if confirmed, build the thinnest possible working slice: Claude Code signals → attention queue → Hammerspoon shortcut → Zellij navigation.

---

## Step 0: Verify the assumptions (before writing any Rust)

### Assumption 1: Claude Code Stop hook has Zellij env vars

The Stop hook fires in the process context of Claude Code. If Claude Code is running inside a Zellij pane, `$ZELLIJ_SESSION_NAME` and `$ZELLIJ_PANE_ID` should be set (Zellij injects these into every process in the session).

**Test:** Add to `.claude/settings.json` in any repo running inside Zellij:

```json
{
  "hooks": {
    "Stop": [{ "command": "printenv | grep ZELLIJ >> /tmp/lane-probe.txt" }]
  }
}
```

Let Claude Code finish a task and return the prompt. Check `/tmp/lane-probe.txt` — both `ZELLIJ_SESSION_NAME` and `ZELLIJ_PANE_ID` must appear. If they're missing, the whole approach needs rethinking.

### Assumption 2: Can focus a specific Zellij pane from outside the session

`zellij --session <name> action focus-pane <pane-id>` — needs to work from a process that is not inside that Zellij session (e.g., a Hammerspoon-invoked shell command).

**Test:**
1. Inside Zellij, note your pane ID: `echo $ZELLIJ_PANE_ID`
2. Open a different Zellij session, or a plain terminal outside Zellij entirely
3. Run: `zellij --session <session-name> action focus-pane <pane-id>`
4. Verify focus switches to the target pane

If `focus-pane` doesn't accept a pane ID directly, check `zellij action --help` for alternatives. May need to use `go-to-tab` or another mechanism. This needs to actually be tested, not assumed.

### Assumption 3: WezTerm tab activation works from Hammerspoon context

`wezterm cli activate-tab --tab-id <id>` needs to work when invoked from a Hammerspoon `hs.task`. WezTerm's CLI communicates over a Unix domain socket, so it should work from any process — but worth confirming.

**Test:** From Hammerspoon console: `hs.task.new("/opt/homebrew/bin/wezterm", nil, {"cli", "list"}):start()` — verify it returns tab data.

---

## PoC scope

**In:**
- Read existing `~/.config/lanes/registry.toml` (already used by `infra lanes up`)
- `lane daemon` — minimal in-memory HTTP server holding the attention queue
- `lane signal` — reads `$ZELLIJ_SESSION_NAME` + `$ZELLIJ_PANE_ID`, posts to daemon
- `lane next` — pops next item from attention queue, runs WezTerm + Zellij navigation
- Hammerspoon Hyper+J → `lane next`
- Hammerspoon status overlay — shows current attention queue on demand

**Out (save for later):**
- Firefox, Obsidian, or any integration besides Zellij
- SQLite persistence (in-memory is fine for PoC — restarting the daemon clears the queue)
- `lane init` / `lane forget` — registry is hand-edited for now
- `lane prev` (add after `next` works)
- Any web dashboard or browser UI
- Plugin abstraction (define the seam in code, but only one implementor)

---

## Tech stack

- **Language:** Rust
- **HTTP server:** axum + tokio
- **CLI:** clap
- **Config parsing:** toml crate (reads existing `registry.toml`)
- **Persistence:** in-memory only for PoC

---

## Navigation flow (`lane next`)

1. HTTP GET to daemon → returns `{ session_name, pane_id }` for next lane in queue
2. `wezterm cli list` → find tab with title matching `session_name` → extract tab ID
3. `wezterm cli activate-tab --tab-id <id>` → bring that WezTerm tab into focus
4. `zellij --session <session_name> action focus-pane <pane_id>` → focus the Claude pane

---

## Attention queue behavior

- Lanes enter the queue when `lane signal` fires (Claude Code returned the prompt)
- Lanes leave the queue when visited via `lane next`
- If the same session signals again while already queued: update `pane_id`, keep position
- Queue is ordered FIFO by signal time
- Sessions not in `registry.toml` are silently ignored — no rogue entries

---

## Rust project structure

```
lanes/
  lane/                  ← new Rust project
    src/
      main.rs
      cli/
        signal.rs        ← reads env vars, POSTs to daemon
        next.rs          ← asks daemon, runs wezterm + zellij commands
        daemon.rs        ← starts the axum server
      daemon/
        mod.rs
        routes.rs
        queue.rs         ← attention queue data structure
      registry.rs        ← parses ~/.config/lanes/registry.toml
    Cargo.toml
  spinner/               ← unchanged Go project
  PLAN-*.md
```

---

## Hammerspoon integration

```lua
-- Hyper+J: jump to next lane needing attention
hs.hotkey.bind({"cmd","alt","ctrl","shift"}, "j", function()
  hs.task.new("/usr/local/bin/lane", nil, {"next"}):start()
end)

-- Hyper+L: show attention queue as overlay
hs.hotkey.bind({"cmd","alt","ctrl","shift"}, "l", function()
  hs.http.asyncGet("http://localhost:7701/api/status", nil, function(status, body)
    if status == 200 then
      hs.alert.show(body, 3)  -- placeholder; refine with hs.canvas for real UI
    end
  end)
end)
```

---

## Daemon API (PoC only)

```
POST /api/signal          { session_name, pane_id } → add/update queue entry
GET  /api/next            → pop and return next entry, or 404 if queue empty
GET  /api/status          → return queue as plain text (for Hammerspoon overlay)
```

---

## Success criteria

You are in the `infra` Zellij session. Claude Code finishes in the `sheetwork` session and returns the prompt. You press Hyper+J. The system:

1. Activates the WezTerm tab titled "sheetwork"
2. Focuses the Zellij pane where Claude Code is waiting

That's the PoC. If those two things happen reliably, the architecture is sound and phase 1 begins.

---

## What's not settled yet

- **`lane next` behavior when queue is empty:** cycle through all registered lanes round-robin, or do nothing? Decide when implementing.
- **Hammerspoon overlay format:** `hs.alert` is a quick start; `hs.canvas` allows a real styled panel. Decide after seeing the data.
- **`lane signal` daemon discovery:** hardcode port 7701 for now; make configurable later.
