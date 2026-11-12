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
| Claude sessions | file events | `lanes-cli`'s `drivers::claude` (done - reconciled with lensd's own `claude.rs`, see step 2) |
| Zellij, WezTerm | polling (no events) | lensd's `zellij.rs`, `wezterm.rs`, moved |
| a repo's branch, dirty | file events + git | a `dp::sc`-style reader |

## Who uses lensd today

Only Lanes (checked 2026-11-12):

- **Lanes Switch** (`lanes/ui/src-tauri/src/lib.rs`, `lanes::lensd::Client`
  on `~/.local/state/lensd/lanes.sock`): its dashboard chips.
- **The `lanes` CLI** (`main.rs`, `lanes snapshot` / `lanes signals`,
  `read_snapshot`): one-off reads.
- `repos.sock` (dev-infra's `rules/repos.dl`) has no reader: drop it now.
  **Done** (step 1).

What `lanes.dl` derives for them (about 24 relations): each lane's Zellij
session (running, missing, its panes: command, cwd, focused), its WezTerm
tab, its Claude session state, shell commands (running, ended, exit),
repos (branch, dirty, watched), app builds (changed, rebuilt), Boards'
inbox counts; plus the control relations telling sources what to watch
(`lane_watch_repo`, `zellij_watch_session`).

## Design points

1. **Where the moved modules go:** decided, for Claude sessions at least
   (step 2) - inside `lanes-cli` itself (`src/drivers/claude.rs`), not a
   new dev-infra crate, since Lanes is still the only consumer (per "Not
   decided" below, this was always the sanctioned fallback when nothing
   else needs it yet). Move to a shared crate beside `appctl::watch`/
   `follow` only if and when a second reader actually appears - not
   speculatively.
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

1. **Done.** Dropped `repos.dl`/`repos.sock` (no reader), and switched
   Boards' inbox count to a direct, one-shot appctl read
   (`lensd::boards_inbox_counts`) instead of lensd's derived
   `lanes_inbox_count` - Boards already published it correctly
   (`inbox::publish`), nothing to move, just who reads it. Landed as one
   commit across `lanes`, `dev-infra` (`rules/README.md`) and `lensd` (a
   stale doc-comment pointer).
2. **Done, but not as written.** The plan assumed one clean module to
   move; there turned out to be two independently-evolved Claude-session
   scanners already: `lensd`'s `source::claude` (fed the dashboard chip,
   via lensd) and `lanes-cli`'s own pre-existing
   `drivers::claude::enumerate()` (fed `lanes cycle`'s session-cycling,
   nothing to do with lensd at all). Each had something the other didn't:
   lensd's had the TUI-interrupt check (`dump-screen`-based, catching a
   tool call Escape killed with no hook to report it); `enumerate()`'s had
   an extra correctness check lensd's lacked (cross-referencing against
   live Zellij sessions, not just the pid). Kept `enumerate()` as the
   survivor (already in `lanes-cli`, already had a caller) and ported the
   TUI-interrupt check into it, plus a new `watch()` (file events via
   `appctl::watch`, with the same 5s liveness-rescan lensd's version had,
   for a dead process whose file a hook never rewrites). No new crate -
   see design point 1.
3. **Done, mostly as written - except "publishes."** Dashboard and CLI
   both switched together: `snapshot_from_mirror`/`lane_from_mirror`/
   `claude_signals` now take a `&[drivers::claude::ClaudeSession]`
   (from `enumerate()`) instead of reading `lanes_claude` from the
   Mirror, and both `ui/src-tauri`'s `get_snapshot` and `lanes-cli`'s
   `lanes_snapshot_from_lensd` supply it - `lanes cycle` already did
   (that's what `enumerate()` was for before this plan existed). Reading
   order within a terminal still comes from lensd's `lanes_pane`
   positions - that's the Zellij/WezTerm chip's own data (step 4), not
   Claude session state, so it stayed on the Mirror for now, deliberately.
   Did *not* add a Lanes-Switch-publishes-this-as-a-fact step: nothing
   reads Claude session state from Lanes today (unlike the inbox count in
   step 1, which Console or others could plausibly want), so publishing
   it now would be speculative infra with zero readers - skip it until
   something actually needs it, same call as step 2's "no new crate yet."
   `lensd`'s own `lanes_claude` relation and `keys_from_mirror` (a
   `pub fn` that turned out to have no caller outside its own tests -
   found along the way, not touched, out of scope here) are now dead
   weight on the Mirror side; cleaning those up is step 6's job
   (`lanes.dl` itself goes then), not this one's.
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
spans lanes, maybe dev-infra (a shared crate, only if a module actually
ends up needing one - see "Not decided") and the lensd repo (its modules
moved out, then archived). Each step above is a stopping point.

## Not decided

- Whether anything besides Lanes needs these facts yet. Resolved for
  Claude sessions (step 2): nothing else does yet, so it stayed inside
  `lanes-cli` rather than becoming a new dev-infra crate. Still open for
  Zellij/WezTerm and the repo reader (steps 4-5) - decide per module, not
  all at once, the same way.
