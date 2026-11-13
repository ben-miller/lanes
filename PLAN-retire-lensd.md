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
4. **Zellij and WezTerm - bigger than one step, split in two.** Scoped out
   2026-11-12 after checking what's actually there (same "check for an
   existing duplicate first" diligence as step 2):

   **4a. Zellij session/pane chip - done.** Added
   `drivers::zellij::panes()` (full pane info - id, command, cwd, focused
   - pre-sorted in reading order; `pane_positions()` only kept the
   position tuple, discarding command/cwd). `running` now comes from
   `running_sessions()` instead of `lanes_session_running`; the
   dashboard's pane list (`panes_from_zellij`, replacing
   `panes_from_mirror`) comes from it instead of `lanes_pane`/
   `lanes_pane_cmd`/`lanes_pane_cwd`/`lanes_pane_focused`; `claude_signals`'
   reading order comes from the same already-fetched pane list (a pane's
   index in it, since it's pre-sorted) instead of the Mirror's `lanes_pane`
   positions - finishing what step 3 deliberately left on the Mirror.
   Both `running_sessions` and the per-session pane lists
   (`lensd::panes_by_session`) are fetched once per snapshot by the
   caller and threaded through as parameters, same "fetch once up top,
   pass pure data down" shape as `inbox_counts`/`claude_sessions` - kept
   `snapshot_from_mirror`/`lane_from_mirror`/`claude_signals` unit-
   testable without shelling out to the real `zellij` binary. Dashboard
   and CLI together, verified live against real panes (exact match with
   the Mirror-based output it replaced).

   **4b. WezTerm tab presence - done.** Added `drivers::wezterm`
   (new module - no existing equivalent to reconcile, unlike 4a/steps
   2-3; `lanes-cli` only had cached-tab-id *activation*, never a live
   query). `sessions_with_tabs()` ports lensd's tty-based matching (not
   `infra_zellij.py`'s fragile tab-title heuristic): find the live
   WezTerm GUI socket (reused `crate::wezterm_socket` - `lanes-cli`
   already independently fixed the same stale-socket bug lensd's version
   also had to fix, per `BUG-wezterm-stale-socket.md` - nothing new to
   port there), list panes, link each one's tty to whichever `zellij
   attach <session>` process is running on it. One `wezterm cli list` +
   one `ps` call covers every lane at once. `has_tab`
   (reachability/cyclability) and the `SessionMissing` signal now read
   this directly instead of lensd's `lanes_tab`; `SessionNotRunning`
   (left over from step 4a) now reads `running_sessions` directly too
   instead of lensd's `lanes_session_not_running` - same data, finishing
   what 4a started. `lane_from_mirror`'s now-8 live-data parameters
   (one added per step 1-4b) were collapsed into one `LiveData` struct
   (clippy flagged 8 positional args; the struct is also just clearer).
   Verified live: correct cyclable/signal output across every current
   lane.
5. **Done, all three.**
   - **Shell commands: done.** Another case like steps 2/4a, not a new
     port: `lanes-cli` already had `drivers::shell::enumerate()` (reading
     the same files the shell hook writes, `CommandRecord` and all - its
     own doc comment already said the live dashboard read these via
     lensd instead, which is exactly the swap made here). `command_records`
     now filters an already-fetched `Vec<CommandRecord>` by session
     instead of querying `lanes_command`/`lanes_command_cmd`/
     `lanes_command_ended`/`lanes_command_exit`. `int`/`rows_for`/`by_pane`
     (the Mirror-row helpers only these and the pane-list code used)
     were fully dead after this and step 4a, so removed.
   - **Repos (branch, dirty): done, as a reader in `lanes-cli` itself, not
     a new crate.** No existing duplicate (unlike shell/claude/zellij) -
     the closest thing was `dev-infra`'s `dp::sc` (used by Console),
     which does cover this and more, but pulling in the whole `dp` crate
     for one slice would mean pulling in its unrelated dependencies too
     (`rusqlite`, `xtask`, ...) just to parse `git status --porcelain=v2`
     - a packaging mismatch, not a reason to accept the weight or to
     duplicate the logic wholesale into a new crate either. Landed as:
     new `drivers::git::status(path)` (dirty + current branch, via one
     `git status --porcelain=v2 --branch` call) plus reuse, not
     duplication, of `cmd::doctor`'s existing `cached_origin_head` for
     the default branch (moved into `drivers::git` so both the dashboard
     and doctor's own diagnostic share the one reader - doctor's
     `remote_origin_head`/`parse_ls_remote_symref` stayed in `doctor.rs`,
     since that slower, live network check is doctor-specific).
     `repo_signals` reads an already-fetched `HashMap<path, RepoStatus>`
     (`repo_status_by_path`) instead of `lanes_repo_dirty`/
     `lanes_repo_branch`.
   - **Builds: done.** `lensd::build_checks_by_repo` calls
     `appctl::build::check()` directly per running instance (already
     available over its existing `hello`, via `appctl::client::list` - no
     extra round trip), grouped by owning repo into
     `LiveData::build_checks`. `build_signals` reads that instead of
     `lanes_build_changed`/`lanes_build_rebuilt`; one chip per instance
     and reason, same as before (`appctl::build::Check`'s own
     `Source::Changed` already groups every changed source per instance,
     so no separate grouping step was needed).
6. **Done.** `lensd.rs`'s `Mirror`/`Client`/`Shared`/`stream_into`/
   `read_snapshot`/`default_socket`/`keys_from_mirror`/`watched_repos`/
   `claude_reason` - all fully dead (the last reader, `lane_from_mirror`'s
   `m: &Mirror` parameter, had already stopped being read partway through
   step 5) - deleted; `snapshot_from_mirror`/`lane_from_mirror` renamed to
   `live_snapshot`/`lane_snapshot`, no daemon connection left anywhere.
   `main.rs`'s `lanes_snapshot_from_lensd` (renamed `lanes_live_snapshot`)
   and `ui/src-tauri`'s `get_snapshot` both dropped their socket/`Client`
   state entirely - the latter's "lensd not connected" branch and the
   frontend's matching `lensdDown` UI state went with it, since there is
   no longer a daemon that could be disconnected. The dashboard's 5s
   WezTerm-tab/Zellij-session poll (previously lensd's own
   `zellij_poll_secs`-driven push into the UI's `Client`) is now an
   in-process polling thread in `ui/src-tauri` emitting the same
   `sessions-changed` event on the same cadence - same "supported as it
   already was" behavior, no daemon needed to get it. `dev-infra/rules/
   lanes.dl` deleted (confirmed dead - nothing subscribes to `lanes.sock`
   anymore) and `rules/README.md` rewritten to say where each relation
   went; the lensd process stopped (`dp ps down lensd`) and its own
   README given an "archived" banner at the top pointing to where every
   module's logic now lives, with the rest kept as a design record rather
   than deleted. `process-compose.yaml`/`lensd.toml` left in place in the
   lensd repo (nothing left to invoke them, so nothing to delete) rather
   than removed - archiving meant stopping and redirecting, not erasing
   the repo's history.

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
