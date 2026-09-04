# PLAN: long-running shell command signal

Surface long-running foreground shell commands in Lanes Switch: while a
command runs (past a threshold), and after it finishes or fails.

## Status

**Phase 1 implemented** (2026-09-04): `done`/`failed` signals via the fish
hook, the `shell` driver, `Lifecycle` (`Live`/`Latched`), the
`visible`/`cyclable` split, `signal-dismissed` + dismiss controls, the
`FocusPane` action, `driver_enabled` + `catch_unwind` isolation, `lanes
shell-init fish`, and a doctor check. Enable with `drivers = [..., "shell"]`
and `lanes shell-init fish | source` in config.fish.

**Phase 2 not started, and now blocked on `PLAN-incremental-signals.md`.**
The live `running` chip, `lane-last-focused` + "focused at completion"
courtesy, the `claude-session-disabled` → `signal-muted` rename,
`bash`/`zsh` hooks. Design settled (2026-09-05): the fish hook stays
fork-free - `fish_preexec` writes a `running` record with a plain `printf`
redirect; the "3s elapsed, still running" moment is handled Lanes-side by a
`tokio` task spawned when the fs-watcher first sees the record. See the
Phasing section and `PLAN-incremental-signals.md`.

## Principles (settled in design discussion)

- **Detection is shell hooks only.** No process polling, no dependency on
  atuin or any external history tool. fish first; bash/zsh are later
  adapters onto the same record format.
- **Two thresholds, both configurable.** `LANES_SHELL_NOTICE_SECS`
  (default 3s) is the floor below which nothing is ever recorded - so `ls`,
  `cd`, `git status` are structurally incapable of showing up.
  `LANES_SHELL_LATCH_SECS` (default 30s) is the floor for a `done`/`failed`
  chip - routine 3-30s commands (`cargo check`, `git push`, a small test
  run) finish without leaving a chip. In Phase 1, only the latch threshold
  matters. Both are read hook-side, so a sub-threshold command never writes
  a file (no churn, no refresh).
- **One way of naming a process.** No server-vs-command classification, no
  argv0 heuristics. A running `vite` and a running `cargo build` are the
  same kind of thing: "a command that has been running a while."
- **Three states:** `running`, `done`, `failed`.
- **`visible` and `cyclable` are separate concepts.** A running command is
  visible but not cyclable (hypo+J/K should never land on a dev server).
- **Signal lifecycle is a reusable axis, not shell-specific.** See below.

## The `lifecycle` abstraction

New property on every `Signal`: `lifecycle: Live | Latched`.

| | **Live** | **Latched** |
|---|---|---|
| exists while | the condition is true *now* | until explicitly cleared |
| auto-clears | yes, when the condition ends | never |
| user can mute from cycling | yes | yes (if cyclable) |
| user can dismiss | no (would misreport reality) | yes, in edit mode |
| identity key | per **source** | per **occurrence** |

Every existing signal is **Live** (Claude active/awaiting/ready/permission,
repo pending-commit/non-default-branch, lanes session-missing/not-running).
`shell/done` and `shell/failed` are the first **Latched** signals - and not
the last: a failed CI check, a failed deploy, "review requested" all want
the same "X happened, dismiss when dealt with" behaviour.

State markers in `state.kdl`, both keyed by a stable signal id:

- **`signal-muted id=<x>`** - exclude from cycling, signal still shows.
  Generalises today's `claude-session-disabled` (rename; the existing code
  already treats stale entries as harmless no-ops, so no migration needed).
- **`signal-dismissed id=<x>`** - hide a Latched signal entirely, one-way
  (no un-dismiss - a fresh run in that pane recreates the chip anyway).
  Only applies to Latched signals; a Live signal can't be dismissed.

Identity:
- Live shell signal (`running`): id = `shell:<session>--<pane>`.
- Latched shell signal (`done`/`failed`): id = `shell:<session>--<pane>--<started_at>`,
  so dismissing one run never suppresses a future run. Stale markers never
  match a live signal and are dropped, same as `claude-session-disabled`.

## Data flow

```
fish hook  ──writes──▶  ~/.local/state/lanes/shell/<session>--<pane>.json
                              │
              fs-watcher sees the write ──▶ sessions-changed ──▶ refresh
                              │
                     drivers::shell::enumerate()  (lenient read + pane reconcile)
                              │
                     gather_lanes(): record ──▶ Signal (kind=Shell)
                              │
                     lib.rs correction pass: visible / cyclable / lifecycle
                              │
                     Lanes Switch renders the chip
```

## Record format

`~/.local/state/lanes/shell/<session>--<pane>.json`, one per pane (a pane
runs one foreground command at a time):

```json
{
  "v": 1,
  "shell": "fish",
  "session": "lanes",
  "pane": "3",
  "argv0": "cargo",
  "cmd": "cargo test --workspace",
  "started_at": "2026-09-03T21:00:00Z",
  "state": "running",
  "ended_at": null,
  "exit_code": null
}
```

- `v` is the hook↔driver contract version. Old hook + new binary must
  degrade (skip the record), not mis-parse.
- `shell` is diagnostic only - nothing branches on it.

## Component 1 — `lanes shell-init` + the fish hook

Install is one line in `config.fish`, standard `starship`/`zoxide`/`direnv`
style:

```fish
lanes shell-init fish | source
```

The hook code lives in the Lanes repo (emitted by the subcommand), not
copied into the user's dotfiles - upgrades ride with the binary. Same
subcommand later emits `bash` / `zsh`. Uninstall = delete the line.

**`done` / `failed` need no background process.** fish's `fish_postexec`
receives `$CMD_DURATION` (ms) for free:

```fish
function __lanes_postexec --on-event fish_postexec
    set -l code $status
    test "$CMD_DURATION" -lt $__lanes_threshold_ms; and return   # hot path: no-op
    # signal-killed (Ctrl-C a server, SIGTERM): clear, don't latch
    if test $code -gt 128
        rm -f $__lanes_record; return
    end
    # write {state: done|failed, ended_at, exit_code}
end
function __lanes_preexec --on-event fish_preexec
    rm -f $__lanes_record          # next command in this pane clears the old chip
    set -g __lanes_cmd $argv[1]
end
```

**`running` is Phase 2 and does NOT use a hook-side timer** (rejected -
see Phasing). `fish_preexec` writes a `{"state":"running"}` record with a
plain `printf >` redirect (no fork); the "3s elapsed" moment is a
`tokio::time::sleep` task on the Lanes Switch backend, armed when the
fs-watcher first sees the record. `LANES_SHELL_NOTICE_SECS` (default 3s)
is checked Lanes-side, not in the hook.

## Component 2 — `drivers/shell.rs`

Mirrors `drivers/claude.rs`:

- `enumerate() -> Vec<ShellRecord>` - reads every `*.json` in the shell
  state dir. Malformed / wrong-`v` file → skip + `logging::warn`, never
  `?`-propagate.
- Reconcile against `drivers::zellij` pane data already fetched in
  `gather_lanes` - drop records whose `<session>--<pane>` is gone.
- No subprocesses. (Tier-3 inline pane output via `zellij action
  dump-screen` is deferred and, when added, stays lazy / bounded / out of
  the `gather_lanes` hot path.)

## Component 3 — signal model (`model.rs`)

- `SignalKind::Command` (driver is `shell`; the kind is domain-named, like `Repo`).
- `SignalReason::Command(CommandReason)` with `CommandReason { Done, Failed }` (Running is Phase 2)
  (adjacently-tagged, same as the other three domains).
- `SignalAction::FocusPane { session: String, pane: String }` - new variant
  (existing `FocusRepoPane` takes a path, not a pane id).
- `Signal` gains:
  - `lifecycle: Lifecycle` (`Live` | `Latched`) - set by the correction
    pass, like `cyclable`.
  - `visible: bool` - set by the correction pass.
- Urgency: `Running → Info`, `Done → Attention` (green, "ready"),
  `Failed → Warning` (orange).

## Component 4 — `gather_lanes` wiring & isolation (`lib.rs`)

```rust
let shell_records = if cfg.driver_enabled("shell") {
    std::panic::catch_unwind(drivers::shell::enumerate)
        .unwrap_or_else(|_| { logging::warn("shell driver panicked"); Vec::new() })
} else {
    Vec::new()
};
```

- `driver_enabled` is **already** in `config.rs`; today `gather_lanes`
  ignores it - this wires it in for the shell driver (and could be extended
  to the others later).
- Opt-in: `shell` is **not** in the default `drivers` list. README documents
  `drivers = ["zellij", "claude", "shell"]` to enable it (experimental).
- If enabled but the hook was never installed → empty state dir → no
  records → no signals → no error. Every failure mode degrades to "no
  shell chips," never to a broken refresh.

Correction pass (extends `signal_cyclable` / adds `signal_visible`):

| CommandReason | lifecycle | visible | cyclable |
|---|---|---|---|
| Running | Live | true | **false** |
| Done | Latched | true unless `signal-dismissed` | true |
| Failed | Latched | true unless `signal-dismissed` | true |

Latched signal is suppressed entirely (not just made non-cyclable) when its
id is in `signal-dismissed`. (Phase 2: also suppress a `done`/`failed`
whose lane was focused at completion - needs `lane-last-focused`.)

## Component 5 — visible / cyclable split

- `Signal.visible` added (above).
- Frontend `App.svelte`: `visibleSignals` filters on `signal.visible`
  instead of `isCyclable` in non-edit mode. Edit mode still shows all,
  including dismissed Latched signals (with their toggle off).
- `isCyclable` stays as-is for the hypo+J/K rotation and the `.is-cyclable`
  styling.

## Component 6 — UI (`App.svelte`)

- Render `SHELL` chips: `[SHELL] cargo test · done 3m12s` /
  `· failed (exit 1) 3m12s` (Phase 2 adds `· running 0:52`). Duration +
  exit from the record, shown without a click.
- Click → `FocusPane` action: jump to that pane (same machinery as
  `focus_lane` / `switch_claude_session` - WezTerm tab activate + zellij
  `focus-pane`; the unreachable-pane case already has handling). The click
  overlay also gets a **dismiss this signal** button.
- Edit mode: a one-way **×** dismiss control on Latched shell chips. Writes
  `signal-dismissed <id>` via a new `set_signal_dismissed` command. No
  un-dismiss.
- `execute_action` / backend: handle `FocusPane`.

## Component 7 — doctor check (`cmd/doctor.rs`)

`check_shell()` (only when `shell` driver enabled):
- hook installed? (`lanes shell-init fish` output present in a sourced file,
  or a sentinel the hook writes on load)
- state dir exists and is writable
- any records present, and are their timestamps sane (not far-future,
  not implausibly old)

Gives a place to look when the feature silently does nothing.

## Phasing

**Phase 1 — `done` / `failed` only.** No background process anywhere
(`$CMD_DURATION` in `postexec`). `shell` driver, `SignalKind::Command` +
`CommandReason` + `lifecycle` + `visible`, visible/cyclable split,
`signal-dismissed` + dismiss controls, `FocusPane` action, `driver_enabled`
wiring + `catch_unwind`, `lanes shell-init fish`, doctor check,
`LANES_SHELL_LATCH_SECS`. Lowest-risk slice that delivers "your long
command finished / failed, go look."

**Phase 2 — `running` state.** *Blocked on the incremental-signals refactor
— see `PLAN-incremental-signals.md`.* Design settled in discussion
(2026-09-05):

- **The fish hook stays fork-free.** No disowned-sleep timer (rejected as
  absurd - nobody spawns a process per command; every mainstream tool
  notifies on completion only). `fish_preexec` writes a
  `{"state":"running", "started_at":…}` record with a plain `printf >`
  redirect, every command. `fish_postexec` rewrites it to `done`/`failed`,
  or `rm`s it (signal-killed, or under `LANES_SHELL_LATCH_SECS`).
- **The "3s elapsed, still running" moment is handled Lanes-side.** No fish
  event fires then, and no fs event either (the file hasn't changed). When
  the fs-watcher sees a new `running` record, the Lanes Switch backend
  `tauri::async_runtime::spawn`s a task (a tokio coroutine, not a thread or
  process) that captures the record's `started_at`, `tokio::time::sleep`s
  `LANES_SHELL_NOTICE_SECS`, then re-reads the file. If it's still the same
  `running` command → emit the chip. If it's gone / `done` / superseded →
  the task just returns. No cancellation bookkeeping; `ls` arms a task that
  wakes to find nothing and no-ops.
- **`done`/`failed` need no timer** - the file rewrite fires its own fs
  event.
- The live chip shows an elapsed counter (`vite · running 1m40s`), updated
  on whatever refresh traffic already exists.

Also in Phase 2: rename `claude-session-disabled` → `signal-muted` and
unify the mute path; add `lane-last-focused id=<x> at=<rfc3339>` (updated
in `set_focused_lane`) and the "focused at completion → no latch"
courtesy.

**Phase 3 — deferred, not committed to.** bash/zsh `shell-init`;
server/command classification (cosmetic only if ever); Tier-3 inline pane
output via `zellij action dump-screen`; per-command learned thresholds.

## Testing

`cargo test` green before and after every change; every change carries
tests (project rule).

- `drivers/shell.rs`: parse valid record; skip malformed; skip wrong `v`;
  reconcile drops records for absent panes.
- `model.rs`: Shell signal serialization shape (`kind`/`reason`/`lifecycle`/
  `visible` on the wire); `FocusPane` action round-trip.
- `lib.rs`: record → signal mapping per `CommandReason`; visible/cyclable
  table above; `signal-dismissed` suppresses a Latched signal; a
  deliberately-panicking stub driver leaves `gather_lanes` returning a
  normal (shell-less) snapshot.
- `config.rs`: `driver_enabled("shell")` false when absent from the list.
- Hook: shell-script test (like the `g upd` test) - source the hook against
  a stub, run a fast command (asserts no record) and a slow one (asserts
  `done` record contents), and a nonzero exit (asserts `failed`), and a
  `kill -TERM` (asserts record cleared).
- Frontend: no test infra - manual check of chip rendering + dismiss.

## Open questions

- Exact "focused at completion" mechanism (Phase 2): compare `ended_at` to
  `lane-last-focused`, or check `focused-lane == this lane` with a recency
  window. Decide when building Phase 2.
- Whether `running` also latches briefly on process exit vs. hard-swapping
  to `done`/`failed` (currently: hard swap; Latched signal appears in the
  same refresh the Live one disappears).
- (resolved) Split thresholds, both configurable: `LANES_SHELL_NOTICE_SECS`
  3s / `LANES_SHELL_LATCH_SECS` 30s.
- (resolved) Phase 1 chips get the `FocusPane` click action, not
  detail-only.
- (resolved) Dismiss is one-way, Latched-only.
