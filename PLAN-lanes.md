# Lanes — Design Document

## What it is

Lanes is a context switcher for developers working across multiple simultaneous
workstreams. A "lane" is a unit of focused work that spans multiple applications
— terminal sessions, IDEs, browsers, AI chats, git GUIs, note vaults. Lanes
formalizes the act of switching between these workstreams and makes it instant
and intelligent.

## Project / CLI naming

- **Lanes** — the project name
- **`lane`** — the CLI command
- New repository, separate from Spinner

## The core problem

Switching between projects today means manually rearranging windows, finding
the right tabs, remembering which AI sessions belong where. This is friction
that compounds when you're running multiple AI coding sessions simultaneously.
The goal is to eliminate that friction entirely — switching a lane rearranges
the whole desktop automatically.

## Primary workflow

You are working with multiple Claude Code sessions simultaneously. You give one
a task, then jump to the next lane that needs your attention, do work there,
jump again. The system knows which lanes need you and which are still waiting on
the AI.

The primary interaction is a carousel:
- **Hyper+J** — next lane (skipping lanes where AI is still working)
- **Hyper+K** — previous lane
- On switch: the whole desktop rearranges — right terminal workspace, right IDE
  window, right browser profile, right SourceTree repos, etc.

## What a lane is

A lane is not the same as a repository. A lane can span multiple repos, or no
repo at all. A lane is initialized anywhere in the filesystem:

```bash
lane init          # drops a .lane/ folder with lane.toml, registers with Lanes
lane switch main   # switch to the "main" lane
lane next          # go to next lane needing attention
lane prev          # go to previous lane
lane status        # overview of all lanes and attention state
lane list          # list all registered lanes
```

### `.lane/lane.toml`

Declares which resources the lane needs, organized by application plugin:

```toml
[lane]
name = "sheetwork-feature"
description = "Working on PDF export feature"

[wezterm]
workspace = "sheetwork-feature"

[vscode]
window = "sheetwork"

[sourcetree]
repos = [
  "/Users/bmiller/src/projects/sheetwork",
  "/Users/bmiller/src/projects/sheetwork-lib",
]

[firefox]
profile = "sheetwork"

[obsidian]
workspace = "sheetwork"
```

Resources can be shared across lanes (e.g. two lanes both reference the same
shared library repo in SourceTree).

## Architecture

### Spinner stays a pure Go CLI

Spinner remains what it is — a Go process manager for git worktrees and dev
servers. No Rust ambitions there (for now). Lanes is entirely separate.
Spinner becomes one of the plugins Lanes integrates with.

### Rust/axum as the central brain

The core logic lives in a Rust daemon (`lane daemon`). Tech stack:
- **axum + tokio** for the HTTP API and async event handling
- **SQLite via sqlx** for persistence
- Per-lane state held in a shared in-memory structure (Arc<RwLock<...>>)
- Server-Sent Events (SSE) for real-time push to connected clients (dashboard, Hammerspoon)
- Single binary — `lane` serves as both CLI and daemon

Rust was chosen over Elixir/Phoenix for this use case because:
- Single-user local daemon — BEAM's distribution/hot-reload/clustering benefits don't apply
- Strong static typing catches lane state transition errors the compiler doesn't allow
- No runtime dependency; single binary deploys trivially

### Hammerspoon as a plugin (not baked-in)

"Desktop manipulation on macOS via Hammerspoon" is just one plugin, same as any
other. The server doesn't know or care how clients act on its instructions.

- On Hyper+J: asks Phoenix "what's next?" → gets back a response → raises
  windows, switches workspaces, hides others
- No business logic in Hammerspoon — Lua stays minimal
- Other clients (Windows, Linux tiling WMs, other macOS tools) are just
  different plugins that implement the same activate/deactivate interface

### Status UI

A Hammerspoon overlay (v1) and/or a terminal `lane status` command showing:
- All lanes
- Attention state per lane
- Which Claude sessions are idle vs running
- Which lanes are ready for your attention

A web dashboard (served via SSE from the daemon) is planned but deferred.

## Plugin architecture

Each application integration is a plugin. Each app has completely different
mechanics for what "switch lane" means, so plugins encapsulate that complexity.

Each plugin implements:
- **activate** — bring this app into focus for the given lane
- **deactivate** — move this app out of focus
- **status** (optional) — report whether this lane needs attention

### Application integrations and strategies

| App | Strategy |
|-----|----------|
| **WezTerm** | `wezterm cli` for workspace switching; `$WEZTERM_PANE` for pane tracking |
| **VS Code** | Separate windows per project; match by window title; raise/minimize |
| **IntelliJ** | Separate windows per project; match by window title; raise/minimize |
| **SourceTree** | Multiple windows naturally; find by repo path in title |
| **Firefox** | Separate profiles via `firefox -P <name> --no-remote` |
| **Obsidian** | Advanced URI plugin to switch workspaces within a single vault |
| **Emacs** | `emacsclient --eval` to switch projects, or multiple daemon instances |
| **Claude.ai** | Conversation URLs stored per lane; opened via nativefied instances or browser |

### The singleton problem

Many macOS apps want to be one instance. This is the primary challenge for the
plugin architecture:
- Firefox profiles solve it for the browser
- Nativefier-style wrappers can break web apps into independent windows
- Obsidian and Emacs have internal multiplexing that can be driven
  programmatically
- Each plugin has to deal with this on a per-app basis

## Data model: local-first, cloud-ready

The Phoenix app runs locally to start. But the data model should not assume
locality — that's what makes it cloud-ready without a rewrite.

Concretely:
- **IDs are UUIDs**, not local paths or integers
- **Timestamps are UTC** with timezone awareness
- **No hardcoded locality assumptions** — don't assume one machine, one user

When ready to go hosted: deploy the Phoenix app, point clients at it instead of
localhost. You're adding a transport layer, not rearchitecting the domain.

### Identity is deferred

Don't design identity upfront. Building identity too early creates infrastructure
for use cases that might not matter. The value of Lanes needs to be proven
locally first — you need to actually use it and understand the attention model
before thinking about syncing across devices or users.

Build with clean boundaries so identity can be added later. Premature identity is
worse than bolt-on identity.

## Attention system

The carousel is smart — it skips lanes where nothing needs your attention yet.
Plugins can report whether their lane "needs attention".

### Initial attention source: Claude Code

The only attention source being built initially is Claude Code's prompt-return
hook. Claude Code already has a bell that fires when it returns the prompt. That
same hook calls a Lanes endpoint to signal "this lane is ready".

Lanes endpoint receives the signal, marks the lane as needing attention, pushes
update to all clients (Hammerspoon, LiveView dashboard).

### Future attention sources (not building yet)

- CI completion
- PR reviews ready
- Build results
- Slack/messages
- Any async event that means "this lane needs you"

## Commands

```
lane init                  Initialize a lane in the current directory
lane list                  List all registered lanes
lane status                Show all lanes, attention state, plugin status
lane switch <name>         Switch to a named lane
lane next                  Switch to next lane needing attention (carousel)
lane prev                  Switch to previous lane
lane add <plugin> <args>   Add a resource to the current lane
```

## Relationship to Spinner

Spinner is one of the plugins/integrations. A lane can declare which Spinner
project and branch it uses. Lanes asks Spinner for status (server running, setup
state) rather than reimplementing any of that. Spinner CLI remains completely
independent.

## Lanes as an attention operating system

The desktop switching is the most immediate and visible payoff, but it's almost
the least interesting part. Once attention is a first-class concept with a real
server behind it, the system generalizes far beyond window management.

The mental model: **Lanes is an attention operating system**. It manages where
your focus goes and why. Desktop manipulation is one output. Other outputs:

- Phone notifications when a lane needs you
- Daily summary of where attention actually went
- Intelligent suggestions about what to work on next
- Any async event source that produces a "ready for human input" signal

The Claude Code integration is a perfect first beachhead — it's the most
concrete and immediately useful, and you're already working this way manually.
But the architecture naturally generalizes.

### Future extrapolations (not building yet)

- **Multi-client** — phone, tablet, any device getting attention pushes
- **Passive context recording** — building a history of where attention went
- **Team lanes** — shared attention state across collaborators
- **AI-aware scheduling** — smarter suggestions based on workload and state

## What we're NOT doing yet

- Building any application plugins beyond the architecture
- Mobile or cloud clients
- Any attention source beyond Claude Code
- Auto-creating browser profiles or app instances
- Any CI/PR integrations
- Multi-user or multi-device support (deferred with identity)
