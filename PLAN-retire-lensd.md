# Plan: retire lensd, its sources moved to shared code

> Written 2026-11-12 in a dev-infra session, with the user. Not started.
> Here, in lanes, because Lanes is lensd's only consumer and most of the
> work is here. Background: dev-infra's `NOTES-lensd-retirement.md` (the
> 2026-10-27 inventory: one consumer, a whole engine for about 24
> relations, 160% CPU then from a reconnect loop and polling everything).
> On 2026-11-12 it was at about 50% CPU again, steadily (1h43m up; about
> 7% a minute after a restart): the same kind of cost, and the cue to
> retire it rather than fix it again. The convention it follows:
> `facts-from-owner` (dev-infra's CONVENTIONS.md).

## The goal: one way to watch each thing, without a central daemon

lensd exists so that changes aren't watched fifty different ways. That
stays the goal; what changes is where the consolidation lives:

- **In code, not a process:** one shared module per kind of source, used
  by whichever process needs it. Not a daemon holding copies.
- **One process per piece of data:** the owner of some data runs the
  modules that watch it and **publishes the results as facts over
  appctl**; anyone else follows the owner (`appctl::follow`), never
  watches the same thing again.

| Kind | How it's watched | Shared code |
|---|---|---|
| files, git | file events | `appctl::watch` (exists) |
| an app's state | pushed over appctl | `appctl::follow` (exists) |
| Claude sessions | file events | lensd's `claude.rs`, moved |
| Zellij, WezTerm | polling (no events) | lensd's `zellij.rs`, `wezterm.rs`, moved |
| a repo's branch, dirty | file events + git | a `dp::sc`-style reader |

## Who uses lensd today

Only Lanes (checked 2026-11-12):

- **Lanes Switch** (`lanes/ui/src-tauri/src/lib.rs`, `lanes::lensd::Client`
  on `~/.local/state/lensd/lanes.sock`): its dashboard chips.
- **The `lanes` CLI** (`main.rs`, `lanes snapshot` / `lanes signals`,
  `read_snapshot`): one-off reads.
- `repos.sock` (dev-infra's `rules/repos.dl`) has no reader: drop it now.

What `lanes.dl` derives for them (about 24 relations): each lane's Zellij
session (running, missing, its panes: command, cwd, focused), its WezTerm
tab, its Claude session state, shell commands (running, ended, exit),
repos (branch, dirty, watched), app builds (changed, rebuilt), Boards'
inbox counts; plus the control relations telling sources what to watch
(`lane_watch_repo`, `zellij_watch_session`).

## Design points

1. **Where the moved modules go:** a dev-infra crate beside `appctl::watch`
   and `follow` (a new `sources` crate, or modules in `appctl`), so any app
   reads Claude sessions or Zellij the same way. Not inside Lanes, which
   would make them Lanes-only.
2. **Lanes Switch is the owner of lane data:** it runs the modules for its
   lanes and publishes the results as facts over appctl (it already
   publishes the lanes, their order and repos). Console or anything else
   follows Lanes Switch for them.
3. **Poll only what's needed:** Zellij and WezTerm have no change events,
   so they're polled, but only for enabled lanes, and ideally only while
   the dashboard is shown or someone follows the facts. Losing that
   scoping is what keeps costing lensd CPU.
4. **Read now, or watch:** each module offers both, so `lanes snapshot`
   and `lanes signals` work without a running watcher, from the same code.
5. **Datalog becomes plain Rust:** `lanes.dl`'s relations are one- and
   two-step joins: a function each, with tests.
6. **Keep the hard-won logic as it is:** the WezTerm tab to Zellij session
   mapping by tty (`wezterm.rs`), Claude session states and hooks
   (`claude.rs`), Zellij output parsing and its timeouts around a hung
   `zellij` call (`zellij.rs`), following apps' facts with build
   staleness (`appctl.rs`). Only the engine (`rules.rs`, the dataflow,
   `mod.rs`'s plumbing) and `git.rs` go.

## Steps, each ending with something working

1. **Drop `repos.dl`/`repos.sock`** (no reader), and switch Boards' inbox
   count to `appctl::follow` directly - Boards already publishes it
   correctly today (`inbox::publish`, its own `appctl::config!`'s
   `.facts(...)`), nothing to move, just change who reads it. Two free
   wins, landed together as the first commit: the easiest possible proof
   of the new pattern before the harder modules below.
2. **Create the shared crate** with the Claude session module, moved from
   `claude.rs` with its tests: read now, and watch.
3. **Claude chip, dashboard and CLI together:** Lanes Switch computes and
   publishes it from that module (the dashboard reads it instead of
   `lanes_claude` from lensd); `lanes-cli`'s `snapshot`/`signals` reads the
   same module directly for this chip too, not just the dashboard. Not a
   separate later step for the CLI - its `snapshot`/`signals` now hard-
   require a live lensd connection with no fallback (changed this
   session); leaving it on `lanes.sock` through the rest of this migration
   would mean it's fully exposed to lensd's own incremental dismantling
   for the whole middle stretch, for no benefit. lensd still running for
   every other chip.
4. **Zellij and WezTerm** the same way: the modules moved, polled for
   enabled lanes only, the session/tab/pane chips switched over - dashboard
   and CLI together, same reasoning as step 3.
5. **Repos** (branch, dirty) from file events and a git reader; **shell
   commands** from the files the shell hook writes; **builds** by
   following the apps' facts directly - dashboard and CLI together, same
   as above.
6. **Switch lensd off:** its `process-compose.yaml` (the dev daemon's own
   `cargo watch` rebuild/restart supervisor) and `lensd.toml`, `lanes.dl`,
   Lanes' `lensd.rs` client. The repo archived, its README saying where
   each part went.

## Who does it

A session started here in lanes, with this plan as its brief: the work
spans lanes, dev-infra (the shared crate) and the lensd repo (its modules
moved out, then archived). Each step above is a stopping point.

## Not decided

- The shared crate's name and whether it's part of `appctl`.
- Whether anything besides Lanes needs these facts yet (if not, step 2's
  crate could start inside lanes and move when a second reader appears).
