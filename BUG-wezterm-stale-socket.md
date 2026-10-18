# Bug: lane switch fails (and hangs ~5 s) after WezTerm restarts

Status: fixes 1, 2, 3, and 6 applied 2026-10-18 (lanes-cli, plus a matching
fix 1 in lensd). Fix 4 (tag the tab cache with the GUI pid) is deferred -
it needs a matching change in infra/ztabs, a separate repo. Fix 5 (dedupe
the double wezterm warning) is partially addressed as a side effect of
fix 2 (both call sites now fail in microseconds with the same short
message instead of each carrying a multi-line wezterm log), but the two
call sites still aren't actually deduped.

## Summary

When the WezTerm GUI dies and a new one starts, the next lane switch fails
with a long `wezterm activate-tab failed for cached tab N` error, shown as a
Hammerspoon alert. Three separate defects stack up:

1. **Stale socket selection.** `wezterm_socket()` (`lanes-cli/src/lib.rs`)
   picks the most recently modified `gui-sock-*` file in
   `~/.local/share/wezterm/`. A GUI that dies without cleanup leaves its
   socket file behind, and until a new GUI creates its own socket, the dead
   one is still the newest. Lanes then points `WEZTERM_UNIX_SOCKET` at a
   socket nobody is listening on.
2. **Stale tab cache.** The `wezterm-tab-id` entries in `state.kdl` describe
   tabs in the *previous* GUI process. A new GUI starts with none of those
   tabs until `ztabs up` (`infra zellij up`) respawns them and re-pushes the
   IDs. Nothing in Lanes notices that the GUI changed underneath the cache.
3. **Expensive, noisy failure.** `wezterm cli` without `--no-auto-start`
   responds to a refused socket by trying to daemonize
   `wezterm-mux-server`, waits about 2.5 s, then fails. The same failure is
   then reported twice (once from the `wezterm` target driver in
   `apply_targets`, once from `land_lane`), joined with `"; "`, along with
   WezTerm's own multi-line log output. The suggested remedy in the message
   (`lanes tabs set <session> <id>`) is not the right fix here.

The switch took **5.05 s** end to end (`focus.complete elapsed_us=5053796`)
and still failed.

## Environment

- macOS, WezTerm GUI from `/Applications/WezTerm.app`, `wezterm` CLI at
  `/opt/homebrew/bin/wezterm`.
- WezTerm config (`~/src/infra/config/wezterm/.wezterm.lua`) has no
  `unix_domains`, so the GUI owns its tabs directly. Tab IDs do not survive
  a GUI restart.
- Tabs are created by `infra zellij up` / `ztabs up`
  (`~/src/infra/py/lib/infra_zellij.py`), which spawns one tab per active
  session and calls `lanes tabs set <session> <tab_id>` for each
  (`_push_lanes_tab_id`).
- Every lane inherits a `wezterm` target from
  `~/.config/lanes/layout-templates/default.toml`, so every lane switch goes
  through this path.

## The error as shown

From `~/.local/state/lanes/hammerspoon.log`, written by the cycle hotkey
handler in `~/.hammerspoon/lanesswitch.lua`:

```
2026-10-18T02:05:33Z [error] lanes cycle next failed (1): warning: target 'wezterm': wezterm activate-tab failed for cached tab 7 (session 'lensd-dev'), it may no longer exist - run `lanes tabs set lensd-dev <id>`: 11:05:30.595  WARN   wezterm_client::client > While connecting to Socket("/Users/bmiller/.local/share/wezterm/gui-sock-69736"): connecting to /Users/bmiller/.local/share/wezterm/gui-sock-69736.  Will try spawning the server.
11:05:30.598  WARN   wezterm_client::client > Running: "/opt/homebrew/bin/wezterm-mux-server" "--daemonize"
11:05:30.752  ERROR  wezterm_mux_server > unable to lock pid file /Users/bmiller/.local/share/wezterm/pid: Resource temporarily unavailable (os error 35)
11:05:33.096  ERROR  wezterm                > (after spawning server) failed to connect to Socket("/Users/bmiller/.local/share/wezterm/gui-sock-69736"): connecting to /Users/bmiller/.local/share/wezterm/gui-sock-69736: Connection refused (os error 61); terminating
warning: wezterm activate-tab failed for cached tab 7 (session 'lensd-dev'), it may no longer exist - run `lanes tabs set lensd-dev <id>`: 11:05:33.290  ERROR  wezterm > could not determine which pane should be active for tab 7; terminating
error: target 'wezterm': wezterm activate-tab failed for cached tab 7 ... (first failure repeated) ...; wezterm activate-tab failed for cached tab 7 (session 'lensd-dev') ...: 11:05:33.290  ERROR  wezterm > could not determine which pane should be active for tab 7; terminating
```

## Timeline

All times local (JST, UTC+9) on 2026-10-18 unless noted. Sources:
`perf.log`, `hammerspoon.log`, process start times, file mtimes, fish
history.

| Time | Event | Source |
|---|---|---|
| 10-17 ~18:15 | Previous GUI (pid 69736) stops logging after ~9 minutes of nothing but `writing pdu data buffer: Broken pipe` (774 lines). It is gone by the next morning; its `gui-sock-69736` file is left behind. | `wezterm-gui-log-69736.txt` |
| 11:00:58 | A `wezterm cli` caller (most likely lensd, see Related) fails to connect to `gui-sock-69736`. | `wezterm-log-13696.txt` |
| 11:05:28.181 | `keystroke key=hypo+J action=cycle_next`, `focus.trigger lane=lensd-dev` | `perf.log` |
| 11:05:28.338 | `focus.target_activated driver=boards` (fine) | `perf.log` |
| 11:05:29 | New GUI (pid 55716) starts. Nothing in Lanes launched it: no target has `launch = true`, and `land_lane`'s `open -a WezTerm` runs only at ~11:05:33. Probably started by hand. | `ps -o lstart` |
| 11:05:30.595 | `wezterm` target driver's `activate-tab` connects to `gui-sock-69736` (still the newest socket file), refused, tries to spawn a mux server | error text |
| 11:05:30.752 | Mux server spawn fails: the pid file lock is held by a stray `wezterm-mux-server` (pid 64873, running since 10-04 14:23) | error text, `ps` |
| 11:05:32 | New GUI creates `gui-sock-55716` and repoints `default-org.wezfurlong.wezterm` at it | file mtimes |
| 11:05:33.096 | First `activate-tab` gives up. `focus.target_activated driver=wezterm elapsed_us=4857094` | error text, `perf.log` |
| 11:05:33.290 | `land_lane`'s own `activate-tab` now reaches the new GUI (newest socket is 55716), which has only its initial tab: `could not determine which pane should be active for tab 7` | error text |
| 11:05:33.292 | `focus.complete lane=lensd-dev elapsed_us=5053796`; Hammerspoon shows the alert | `perf.log` |
| 11:05:45 | User runs `ztabs up`; tabs 1-11 respawned, `state.kdl` cache rewritten (`fs_watcher_emitted_sessions_changed paths=[".../state.kdl"]` at 02:05:45.795Z), initial tab 0 closed | fish history, `perf.log` |

After `ztabs up`, the cache and the live GUI agree again (tabs 1-11, tab 7 =
`lensd-dev`), so the next switch works. The IDs happen to match the old
ones only because `ztabs up` spawns tabs in the same order every time.

## Root causes in detail

### 1. `wezterm_socket()` trusts mtime, not liveness

```rust
// lanes-cli/src/lib.rs
pub fn wezterm_socket() -> Option<String> {
    // ... every gui-sock-* in ~/.local/share/wezterm, newest mtime first
}
```

A dead GUI's socket file stays on disk until a new GUI starts and cleans
up. While WezTerm is not running at all, or while a new GUI is still
starting, the dead socket wins. The socket name embeds the GUI pid
(`gui-sock-<pid>`), so liveness is cheap to check.

### 2. The tab-ID cache has no notion of which GUI it belongs to

`state::get_wezterm_tab_id` returns whatever `ztabs` last pushed.
`activate_wezterm_tab` deliberately skips a verification round-trip (see
its comment: "Trust it and let activate-tab itself fail"). That is a
reasonable hot-path choice, but a GUI restart invalidates *every* entry at
once, and the resulting error points at a per-session manual fix instead
of the real one (`ztabs up`).

By design (see `_push_lanes_tab_id`'s docstring in `infra_zellij.py`), the
tool that owns the tab lifecycle keeps the cache current and Lanes doesn't
rediscover tabs itself. So the fix here is about *detecting* a cache from a
previous GUI and saying so, not about rediscovery.

### 3. Failure mode is slow and repeated

- Lanes calls `wezterm cli activate-tab` without `--no-auto-start`, so a
  refused socket triggers WezTerm's "spawn a mux server and retry" path,
  which costs about 2.5 s (11:05:30.598 to 11:05:33.096). It also took
  ~2.2 s from the start of the wezterm target to the first connection
  warning; I didn't determine why.
- That spawn path is very likely how the stray mux server (pid 64873,
  10-04 14:23, matching the `pid` file's mtime) got started in the first
  place: on 10-04 a spawn *succeeded*, and that daemon has held the pid
  lock ever since. Nothing connects to it (no `unix_domains`), so it only
  makes later spawn attempts fail, after the full wait.
- `apply_targets` (wezterm target driver, `focus=false`) and `land_lane`
  (`focus=true`) both call `activate_wezterm_tab` for the same session, so
  one root problem produces two errors, both carrying WezTerm's stderr.

## Impact

- Any lane switch right after WezTerm restarts (or while it is not running)
  fails after ~5 s instead of failing fast or succeeding.
- The alert is long and the advice (`lanes tabs set`) is misleading.
- Side effects outside Lanes: see Related.

## Reproduction

1. Have WezTerm running with tabs from `ztabs up`, and a cached tab ID for
   some lane (`lanes tabs list`).
2. Kill the GUI hard so its socket file is left behind:
   `kill -9 $(pgrep -f wezterm-gui)`. Check `ls ~/.local/share/wezterm/gui-sock-*`
   still lists the dead pid.
3. Run `lanes focus <lane>` (or cycle with the hotkey). Expect the
   "Connection refused ... Will try spawning the server" failure and a
   ~2.5 s+ hang.
4. Start WezTerm (`open -a WezTerm`) and *without* running `ztabs up`,
   switch again. Expect `could not determine which pane should be active
   for tab N`.
5. Run `ztabs up`; switching works again.

## Suggested fixes

In rough priority order:

1. **Only return live sockets from `wezterm_socket()`.** Parse the pid out
   of `gui-sock-<pid>` and skip it unless the process exists (`kill(pid, 0)`
   or `libc::kill`), or try `UnixStream::connect` and skip on
   `ECONNREFUSED`/`ENOENT`. Both take microseconds. If nothing is live,
   return `None`.
2. **Treat "no live GUI" as its own case.** If `wezterm_socket()` returns
   `None`, skip `activate-tab` entirely and return one clear error such as
   `WezTerm isn't running`. If `focus` is true and `open -a WezTerm` was
   just issued, optionally wait (bounded, e.g. up to ~2 s) for a live
   socket to appear before giving up.
3. **Pass `--no-auto-start` on every Lanes `wezterm cli` call**
   (`activate_wezterm_tab` and the `cli list` in `cmd/doctor.rs`), as lensd
   already does. That removes the ~2.5 s mux-server detour and stops Lanes
   from ever creating stray mux servers.
4. **Tag the tab cache with the GUI it came from.** Have `lanes tabs set`
   also record the live GUI's pid (from the socket name). On
   `activate_wezterm_tab`, if the live GUI pid differs from the recorded
   one, fail immediately with `WezTerm restarted since tabs were cached -
   run \`ztabs up\``. No extra round-trip on the hot path, since the socket
   lookup already happens.
5. **Report one wezterm failure once.** If the `wezterm` target driver
   already failed for the session, `land_lane` shouldn't retry
   `activate_wezterm_tab` for the same session in the same switch (or the
   warnings should be de-duplicated before `warnings.join("; ")`). Also
   consider trimming WezTerm's stderr to its last `ERROR` line in the
   message.
6. **`lanes doctor` check:** flag dead `gui-sock-*` files and a
   `wezterm-mux-server` running while no `unix_domains` are configured.

## Related, outside this repo

- **lensd has the same socket bug.** `lensd/src/source/wezterm.rs`
  `gui_socket()` uses the same newest-mtime rule ("the same rule Lanes
  uses") and polls `wezterm cli --no-auto-start list` every 2 s
  (`zellij_poll_secs`, default 2). Its doc comment says `None` means
  WezTerm isn't running, which isn't true while a dead socket file exists.
  Each failed poll leaves a `wezterm-log-<pid>.txt` in
  `~/.local/share/wezterm/`: there are **54,051** of them, 15,817 written
  since 2026-10-17 00:00. Fix 1 should be shared with (or copied to) lensd.
- **infra already works around a related staleness.**
  `infra_zellij._wezterm_env()` strips an inherited `WEZTERM_UNIX_SOCKET`
  so `wezterm cli` does its own discovery. That handles stale env in
  long-lived panes, not the stale-file case above, and WezTerm's own
  default symlink was also still pointing at the dead socket until
  11:05:32, so plain discovery wouldn't have helped during startup either.

## Cleanup (manual, not done)

- Stop the stray mux server: `kill 64873` (verify first with
  `ps -o pid,lstart,command -p 64873`).
- Delete the accumulated `~/.local/share/wezterm/wezterm-log-*.txt` files
  (`find ~/.local/share/wezterm -name 'wezterm-log-*.txt' -delete`).

## Open questions

- What killed GUI 69736? Its log shows only `Broken pipe` writes from
  18:06 to 18:15 on 10-17, with no exit or panic logged.
- What started GUI 55716 at 11:05:29, one second after the keystroke? The
  Lanes code paths that ran don't launch WezTerm before 11:05:33.
