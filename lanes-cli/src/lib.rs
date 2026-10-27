pub mod config;

/// Tests that read or fake HOME take this first: HOME is the whole
/// process's, so a test faking it (config.rs's `with_fake_home`) and one
/// using the real one (drivers::zellij's cache test) can't overlap.
#[cfg(test)]
pub(crate) static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub mod context;
pub mod drivers;
pub mod facts;
pub mod lensd;
pub mod logging;
pub mod model;
pub mod scope;
pub mod state;

pub use drivers::claude::RenamedCandidate;

/// Spawn `cmd` as a fire-and-forget child that may outlive this process for
/// the rest of the session (a launched app, a raised window) - every caller
/// that doesn't plan to wait on or communicate with the child should go
/// through this rather than a bare `.spawn()`.
///
/// `Command`'s default stdio is `inherit()`: an unconfigured spawn hands the
/// child our own stdin/stdout/stderr. That's invisible and harmless when
/// `lanes` is run interactively, but when Hammerspoon invokes `lanes` via
/// `hs.task.new`, those fds are a pipe hs.task owns to capture output. If
/// the child we spawn here is long-lived (Firefox, WezTerm, any GUI app),
/// it holds that pipe's write end open indefinitely - `lanes` and bash both
/// exit, but the pipe never sees EOF. hs.task's internals eventually do a
/// synchronous drain-to-EOF read on Hammerspoon's *main thread*, which then
/// blocks forever: every hotkey stops responding, with no crash and no log
/// line to explain it. `Stdio::null()` on all three streams closes that gap
/// - the child gets its own /dev/null, never shares our fds.
fn spawn_detached(cmd: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
}

pub fn possibly_renamed_claude_sessions() -> Vec<RenamedCandidate> {
    drivers::claude::possibly_renamed_sessions()
}

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Every signal is constructed with cyclable=false as a placeholder (a
/// lane's own reachability isn't known until all its facets are built).
/// Corrects them all once lane-level `cyclable` is known, so the UI reads it
/// straight off each signal instead of re-deriving "is this chip something a
/// cycle would land on" from signal.kind() and lane.cyclable itself. The same
/// pass upgrades Awaiting -> Ready (see `upgrade_awaiting_to_ready`), which
/// depends on that same fact. Called from `lensd::snapshot_from_mirror`,
/// the one place lane snapshots get built.
pub(crate) fn correct_signals(facets: &mut [model::FacetSnapshot], cyclable: bool) {
    for facet in facets.iter_mut() {
        let signals = match facet {
            model::FacetSnapshot::Terminal { signals, .. } => signals,
            model::FacetSnapshot::Repo { signals, .. } => signals,
            model::FacetSnapshot::Board { signals, .. } => signals,
            model::FacetSnapshot::Target { .. } => continue,
        };
        for s in signals.iter_mut() {
            let session_disabled = match &s.action {
                Some(model::SignalAction::SwitchClaudeSession { session_id }) => {
                    state::is_claude_session_disabled(session_id)
                }
                _ => false,
            };
            s.cyclable = signal_cyclable(s.kind(), cyclable, session_disabled);
            s.reason = upgrade_awaiting_to_ready(s.reason.clone(), s.cyclable);
            s.urgency = s.reason.urgency();
            s.lifecycle = s.reason.lifecycle();
            s.visible = signal_visible(s.kind(), s.cyclable);
        }
    }
}

/// Whether this lane would actually be visited by `sessions next`/`prev`
/// right now. Reuses `reachable_lane_decision()` - the exact same pure
/// function `session_belongs_to_reachable_lane()` filters
/// `cycle_claude_session`'s live-session list through - rather than the UI
/// re-deriving an equivalent-looking rule of its own that could silently
/// drift from the real cycling behavior over time. Having a live Claude
/// session is the other half: a reachable lane with only a pending-commit
/// signal still has nothing for a cycle to land on.
fn lane_cyclable(active: bool, reachable: Option<bool>, has_claude_signal: bool) -> bool {
    // reachable is Some only when the lane has a Terminal facet at all, so
    // has_terminal_session is unconditionally true here too.
    reachable.is_some_and(|r| reachable_lane_decision(active, true, r)) && has_claude_signal
}

/// Whether one specific signal is something `sessions next`/`prev` would
/// actually land on. `cycle_claude_session` only ever collects live Claude
/// sessions (`drivers::claude::enumerate()`) - it never looks at repo or
/// lanes-kind facts at all, regardless of a lane's own reachability - so a
/// signal can only be cyclable if it's a ClaudeSession-kind one *and* its
/// lane is cyclable *and* that specific session hasn't been individually
/// excluded (see state::is_claude_session_disabled - edit mode's per-session
/// toggle, keyed by session_id so it resets whenever the session itself
/// does). Every ClaudeSession signal shown under a given lane belongs to
/// that lane by construction (a lane's facets only ever resolve its own
/// zellij session's live sessions into it), so there's no further
/// per-session check beyond these two facts.
fn signal_cyclable(kind: model::SignalKind, lane_cyclable: bool, session_disabled: bool) -> bool {
    kind == model::SignalKind::ClaudeSession && lane_cyclable && !session_disabled
}

/// Whether a signal renders as a chip in the dashboard's normal (non-edit)
/// view. Distinct from `cyclable`: the non-edit filter used to key on
/// `cyclable` directly, which conflated "worth showing" with "would a cycle
/// land here". Two kinds break that conflation and must stay visible even
/// when a cycle wouldn't land on them:
///
///   - `Command` - a finished/failed build is a come-look, and Phase 2's
///     running chip is visible-but-not-a-cycle-target by design.
///   - `Lanes` - these are Lanes reporting its *own* tracking is broken
///     (SessionMissing / SessionNotRunning). Hiding them turns a real
///     problem ("your zellij session died", e.g. after the laptop slept
///     and sessions went EXITED) into a silent "no signals" - the exact
///     opposite of what a status chip is for. They're never noise you
///     manage away in edit mode.
///   - `Boards` - a non-empty Inbox is a come-look, same category as a
///     finished build; whether this lane happens to be cyclable right now
///     has nothing to do with whether there's something to triage.
///
///   - `Build` - a running app that isn't the code on disk is worth seeing
///     whether or not a cycle would land on this lane (see
///     PLAN-build-chip.md).
///
/// Everything else (git chips, off-cycle Claude sessions) keeps the old
/// behaviour: visible iff cyclable.
fn signal_visible(kind: model::SignalKind, cyclable: bool) -> bool {
    matches!(
        kind,
        model::SignalKind::Command | model::SignalKind::Lanes | model::SignalKind::Boards | model::SignalKind::Build
    ) || cyclable
}

/// Seconds a command must have been running before it gets a chip at all -
/// below this, a fast command (`ls`, `cd`) never shows one, same floor
/// `LANES_SHELL_LATCH_SECS` is for `done`/`failed` but checked here instead
/// of hook-side, since the hook now writes a `running` record for every
/// command unconditionally (it can't know in advance how long one will
/// take). Read once per call rather than cached - this is only evaluated
/// on an already-infrequent path (a fs-watch event or the backend's own
/// debounce timer), not a hot loop.
fn shell_notice_secs() -> i64 {
    std::env::var("LANES_SHELL_NOTICE_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(3)
}

/// A Command signal from one shell-hook record, or `None` if there's
/// nothing to show yet - either the record's `state` isn't one of the three
/// known values, or it's `running` but hasn't crossed `shell_notice_secs()`
/// (`now` injected rather than read internally, so this stays a pure,
/// deterministically-testable function). Dismissal is checked by the caller
/// (against `dismiss_id`), kept out of here. The chip is not cyclable - the
/// correction pass leaves it that way since `signal_cyclable` only ever
/// says yes to ClaudeSession - but it *is* visible via `signal_visible`.
fn command_signal(rec: &drivers::shell::CommandRecord, now: chrono::DateTime<chrono::FixedOffset>) -> Option<model::Signal> {
    let reason = match rec.state.as_str() {
        "running" => {
            let started = chrono::DateTime::parse_from_rfc3339(rec.started_at.as_deref()?).ok()?;
            if (now - started).num_seconds() < shell_notice_secs() {
                return None;
            }
            model::CommandReason::Running
        }
        "done" => model::CommandReason::Done,
        "failed" => model::CommandReason::Failed,
        _ => return None,
    };
    let id = rec.occurrence_id();

    let cmd = rec.cmd.clone().or_else(|| rec.argv0.clone()).unwrap_or_else(|| "command".to_string());
    let dur = match reason {
        model::CommandReason::Running => rec
            .started_at
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|started| drivers::shell::fmt_duration((now - started).num_seconds().max(0))),
        _ => rec.duration_secs().map(drivers::shell::fmt_duration),
    };
    let detail = match (reason, rec.exit_code, dur) {
        (model::CommandReason::Failed, Some(code), Some(d)) => format!("{cmd} · exit {code} · {d}"),
        (model::CommandReason::Failed, Some(code), None) => format!("{cmd} · exit {code}"),
        (_, _, Some(d)) => format!("{cmd} · {d}"),
        (_, _, None) => cmd,
    };

    let mut sig = model::Signal::new(
        model::SignalReason::Command(reason),
        Some(model::SignalAction::FocusPane { session: rec.session.clone(), pane: rec.pane }),
        Some(detail),
    );
    sig.dismiss_id = Some(id);
    Some(sig)
}

/// Upgrades an idle-Claude signal to Ready once its lane turns out
/// cyclable - the distinction the user actually cares about isn't Claude's
/// own busy/idle state (that's Active vs Awaiting, untouched here), it's
/// whether this particular idle session is one `sessions next`/`prev` would
/// actually land on. Every other reason (Active, Permission, and anything
/// non-ClaudeSession) passes through unchanged regardless of cyclable -
/// this only ever narrows Awaiting specifically.
fn upgrade_awaiting_to_ready(reason: model::SignalReason, cyclable: bool) -> model::SignalReason {
    use model::{ClaudeSessionReason, SignalReason};
    match reason {
        SignalReason::ClaudeSession(ClaudeSessionReason::Awaiting) if cyclable => {
            SignalReason::ClaudeSession(ClaudeSessionReason::Ready)
        }
        other => other,
    }
}

/// The recorded pid can't be the process that wrote a file older than it: a
/// live Claude session rewrites its registry file on start and on every
/// state change, so the file's last-modified time is always at or after the
/// writing process's start. If `ps` says the process is meaningfully
/// *younger* than the file, the pid was recycled to some unrelated `claude`
/// after the real session died - common after a laptop sleep churns pids.
/// Slack covers the gap between a process starting and its first file write
/// plus `etimes`' whole-second truncation.
const PID_REUSE_SLACK_SECS: u64 = 15;

/// What `ps` tells us about a pid: its `comm` (verified to be `claude`) and
/// its age in whole seconds (`etimes`), used for the reuse check above.
#[derive(Debug, Clone)]
pub(crate) struct ProcInfo {
    pub comm: String,
    pub age_secs: u64,
}

/// Whether a registry entry for a Claude session still refers to something actually
/// running, rather than a file orphaned by a session that ended without firing
/// `SessionEnd` (crash, force-quit, killed pane).
///
/// Sessions living in a Zellij pane are first checked against currently running
/// Zellij sessions - reliable, no guessing. When a PID is also recorded we
/// additionally verify it's (a) still alive and a `claude` process and (b) not
/// older-file-than-process (see `PID_REUSE_SLACK_SECS`) - `kill -0` alone isn't
/// enough since PIDs get reused. Sessions started outside Zellij have no
/// session-name anchor at all, so they rely on the PID check alone.
///
/// `file_age_secs` is how long ago the registry file was last written (`None`
/// if that couldn't be read - then the reuse check is skipped, not failed).
pub(crate) fn session_is_live(
    zellij_session: &str,
    live_zellij_sessions: &HashSet<String>,
    pid: Option<u32>,
    file_age_secs: Option<u64>,
) -> bool {
    session_is_live_with(zellij_session, live_zellij_sessions, pid, file_age_secs, process_info)
}

fn session_is_live_with(
    zellij_session: &str,
    live_zellij_sessions: &HashSet<String>,
    pid: Option<u32>,
    file_age_secs: Option<u64>,
    lookup: impl Fn(u32) -> Option<ProcInfo>,
) -> bool {
    let pid_ok = |p: u32| pid_is_this_session_with(p, file_age_secs, &lookup);
    if !zellij_session.is_empty() {
        if !live_zellij_sessions.contains(zellij_session) {
            return false;
        }
        return match pid {
            Some(p) => pid_ok(p),
            None => true,
        };
    }
    match pid {
        Some(p) => pid_ok(p),
        None => false,
    }
}

/// The pid is a live `claude` process AND isn't a recycled one pointing at a
/// different session (older file than process - see `PID_REUSE_SLACK_SECS`).
pub(crate) fn pid_is_this_session(pid: u32, file_age_secs: Option<u64>) -> bool {
    pid_is_this_session_with(pid, file_age_secs, &process_info)
}

fn pid_is_this_session_with(
    pid: u32,
    file_age_secs: Option<u64>,
    lookup: &impl Fn(u32) -> Option<ProcInfo>,
) -> bool {
    let Some(info) = lookup(pid) else { return false };
    if !is_claude_command(&info.comm) {
        return false;
    }
    match file_age_secs {
        // Process is meaningfully younger than the file it supposedly wrote.
        Some(file_age) => info.age_secs + PID_REUSE_SLACK_SECS >= file_age,
        None => true,
    }
}

/// Seconds since `path` was last modified, or `None` if that can't be read
/// (or the clock is behind the file's mtime).
pub(crate) fn file_age_secs(path: &std::path::Path) -> Option<u64> {
    let mtime = std::fs::metadata(path).ok()?.modified().ok()?;
    std::time::SystemTime::now().duration_since(mtime).ok().map(|d| d.as_secs())
}

fn is_claude_command(cmd: &str) -> bool {
    cmd.trim().rsplit('/').next().unwrap_or("") == "claude"
}

/// A `HISTIGNORE`-style glob: `*` matches any run of characters (including
/// none), `?` matches exactly one, everything else matches literally.
/// Deliberately this small a subset (no `[...]` classes, no `&` previous-
/// command reference) - lanes.toml's `ignored_shell_commands` only needs
/// "starts with"/"contains" style patterns like `ssh *` or `claude*`, not
/// full shell glob semantics. Matched against the *whole* command line
/// (see `is_shell_signal_ignored`), the same thing `HISTIGNORE` matches
/// against - not just argv0 - so `ssh *` can exclude only some hosts
/// while `tail -f *` only excludes a `tail` that's actually following
/// something.
fn glob_match(pattern: &str, text: &str) -> bool {
    fn inner(p: &[u8], t: &[u8]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some(b'*') => inner(&p[1..], t) || (!t.is_empty() && inner(p, &t[1..])),
            Some(b'?') => !t.is_empty() && inner(&p[1..], &t[1..]),
            Some(&c) => t.first() == Some(&c) && inner(&p[1..], &t[1..]),
        }
    }
    inner(pattern.as_bytes(), text.as_bytes())
}

/// Whether a shell-hook record's command should never produce a Command
/// signal at all, regardless of how long it's run - checked before
/// `command_signal` even looks at state/timing, so an ignored command never
/// flashes a chip even briefly. Entirely config-driven (lanes.toml's
/// `ignored_shell_commands`, HISTIGNORE-style globs) - nothing is excluded
/// by name in code, Claude included. Claude's generic "running" chip is
/// always redundant with its own dedicated ClaudeSession signal, but that's
/// a reason to put a `claude*` entry in the config, not to hardcode it here
/// invisibly - see the real lanes.toml for the actual default list.
fn is_shell_signal_ignored(cmd: Option<&str>, argv0: Option<&str>, ignored: &[String]) -> bool {
    let Some(cmd) = cmd.or(argv0) else { return false };
    ignored.iter().any(|pattern| glob_match(pattern, cmd))
}

fn process_info(pid: u32) -> Option<ProcInfo> {
    // `etime` not `etimes`: the latter is a GNU/Linux keyword, absent from
    // BSD/macOS `ps`. `etime` is the same value, just formatted.
    let out = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "etime=,comm="])
        .output()
        .ok()?;
    if !out.status.success() { return None; }
    parse_ps_line(&String::from_utf8_lossy(&out.stdout))
}

/// One `ps -o etime=,comm=` line: elapsed time `[[DD-]HH:]MM:SS`, then the
/// command (which may itself contain spaces, e.g. "claude bg-spare").
fn parse_ps_line(line: &str) -> Option<ProcInfo> {
    let (etime, comm) = line.trim().split_once(char::is_whitespace)?;
    Some(ProcInfo {
        age_secs: parse_etime(etime)?,
        comm: comm.trim().to_string(),
    })
}

/// `ps` elapsed-time format: `MM:SS`, `HH:MM:SS`, or `DD-HH:MM:SS`.
fn parse_etime(s: &str) -> Option<u64> {
    let (days, hms) = match s.split_once('-') {
        Some((d, rest)) => (d.parse::<u64>().ok()?, rest),
        None => (0, s),
    };
    let mut it = hms.split(':').rev();
    let secs: u64 = it.next()?.parse().ok()?;
    let mins: u64 = it.next()?.parse().ok()?;
    let hours: u64 = it.next().map_or(Some(0), |h| h.parse().ok())?;
    Some(days * 86_400 + hours * 3_600 + mins * 60 + secs)
}

pub fn switch_claude_session(session_id: &str) -> Result<(), String> {
    let t0 = std::time::Instant::now();
    logging::perf("switch.trigger", &format!("session={session_id}"));

    let home = std::env::var("HOME").unwrap_or_default();
    let path = std::path::PathBuf::from(&home)
        .join(".claude")
        .join("active-sessions")
        .join(format!("{}.json", session_id));

    let data = std::fs::read_to_string(&path)
        .map_err(|_| format!("session not found: {}", session_id))?;
    let val: serde_json::Value = serde_json::from_str(&data)
        .map_err(|e| format!("bad session file: {}", e))?;

    let zellij_session = val["zellij_session"].as_str().unwrap_or("").to_string();
    let zellij_pane_id = val["zellij_pane_id"].as_u64();

    // Switching to a session is a deliberate lane change, same as clicking a
    // lane in the UI or `lanes focus` - update focused-lane too if this
    // session lives in a configured lane, in the same write as the cursor
    // (see write_claude_cursor_and_lane) so this is one fs-change event, not two.
    // Loaded once and kept around (rather than only inside the block below)
    // so the switch closure further down can also apply the lane's targets -
    // this used to only activate WezTerm/Zellij and never touch
    // [[targets]] at all, so a real lane switch never actually moved any
    // windows, only the terminal.
    let cfg = config::Config::load();
    let lane_id = if !zellij_session.is_empty() {
        cfg.lane_for_session(&zellij_session).map(|lane| lane.id.clone())
    } else {
        None
    };

    // Write the new cursor/lane immediately, before the actual WezTerm/Zellij
    // switch even runs, so the UI updates as close to the keystroke as
    // possible rather than waiting on IPC round-trips it doesn't need to
    // wait on. If the switch below turns out to fail partway through, this
    // optimism is undone in the Err branch so state.kdl (and the UI) never
    // claims we're somewhere we didn't actually reach.
    let old_cursor = state::read_claude_cursor();
    let old_lane = state::read_focused_lane();
    state::write_claude_cursor_and_lane(Some(session_id), lane_id.as_deref());
    // state.kdl is the persisted record (so a not-yet-running or restarting
    // UI still picks up the right lane), but if the UI is already running,
    // notify it directly over a socket instead of waiting on it to notice
    // the file changed - a filesystem watcher has an inherent floor latency
    // no amount of reordering removes, since the UI is a different process.
    // Carries the session id alongside the lane so the UI's per-session
    // highlight can update from this same message instead of waiting on the
    // next full snapshot refresh.
    notify_switch_socket(&format!("switch:{}|{}\n", lane_id.as_deref().unwrap_or(""), session_id));
    logging::perf(
        "switch.optimistic_notify",
        &format!("session={session_id} lane={} elapsed_us={}", lane_id.as_deref().unwrap_or(""), t0.elapsed().as_micros()),
    );

    let switch_result: Result<(), String> = (|| {
        if zellij_session.is_empty() {
            return Ok(());
        }

        // Apply the lane's own targets (Firefox, peek apps, etc.) *before*
        // land_lane below - each target's own activation step (open -a, an
        // obsidian:// URI, ...) brings that app frontmost, so doing this
        // first guarantees land_lane's own choice (the terminal, or the
        // lane's primary target) gets the true last word, not whichever
        // target happened to be activated last. This is also the actual
        // window placement step, previously missing entirely from this
        // path (it only ever activated the WezTerm tab/Zellij pane, never
        // touched [[targets]] at all). Best-effort like focus_lane's own
        // target handling: a target failure (an app not running, a driver
        // command failing) doesn't roll back a terminal switch that
        // already succeeded.
        let lane = lane_id.as_deref().and_then(|id| cfg.lanes.iter().find(|l| l.id == id));
        if let Some(lane) = lane {
            for w in apply_targets(&lane.targets, &cfg, lane.terminal_session()) {
                eprintln!("warning: {w}");
            }
        }
        logging::perf("switch.targets_applied", &format!("session={session_id} elapsed_us={}", t0.elapsed().as_micros()));

        // land_lane resolves the tab through the same session -> tab-id
        // cache everything else uses, rather than the wezterm_tab_id
        // recorded in the session file at hook time (which came from the
        // same unreliable title matching we removed everywhere else).
        // Deliberately last - see above. The exact pane this Claude
        // session was registered in is a more specific answer than
        // anything land_lane could infer on its own, so it's passed
        // through as the explicit override. A session whose zellij_session
        // maps to no configured lane at all (lane is None) has no targets
        // or primary to consider - fall back to the bare tab/pane switch
        // this path always did before land_lane existed.
        match lane {
            Some(lane) => {
                for w in land_lane(lane, zellij_pane_id, t0) {
                    eprintln!("warning: {w}");
                }
            }
            None => {
                activate_wezterm_tab(&zellij_session, true)?;
                if let Some(pane_id) = zellij_pane_id {
                    focus_zellij_pane(&zellij_session, pane_id)?;
                }
            }
        }
        logging::perf("switch.pane_focused", &format!("session={session_id} elapsed_us={}", t0.elapsed().as_micros()));

        Ok(())
    })();

    if switch_result.is_err() {
        // The optimistic write above assumed this switch would succeed - it
        // didn't, so put both state.kdl and the already-notified UI back the
        // way they were, not just state.kdl. Without the socket ping here,
        // an already-running UI would keep showing the lane we failed to
        // reach until its next unrelated refresh (up to 10s later).
        state::write_claude_cursor_and_lane(old_cursor.as_deref(), old_lane.as_deref());
        notify_switch_socket(&format!("switch:{}|{}\n", old_lane.as_deref().unwrap_or(""), old_cursor.as_deref().unwrap_or("")));
    }

    logging::perf(
        "switch.complete",
        &format!(
            "session={session_id} lane={} status={} elapsed_us={}",
            lane_id.as_deref().unwrap_or(""),
            if switch_result.is_ok() { "ok" } else { "err" },
            t0.elapsed().as_micros(),
        ),
    );

    switch_result
}

/// zellij's own focus-pane-id treats "the target pane is already focused"
/// as an error (exit 2), even though that's the desired end state, not a
/// failure - confirmed directly against zellij 0.44.3. Don't roll back a
/// switch that actually landed correctly just because of this quirk.
fn is_benign_zellij_focus_error(stderr: &str) -> bool {
    stderr.contains("already focused")
}

/// Focuses one pane within an already-active Zellij session - pulled out of
/// switch_claude_session so focus_lane's deterministic first-pane fallback
/// (see its own doc comment) can reuse the exact same call and the same
/// tolerance for zellij's "already focused" quirk, rather than
/// reimplementing it.
fn focus_zellij_pane(zellij_session: &str, pane_id: u64) -> Result<(), String> {
    let output = std::process::Command::new("/opt/homebrew/bin/zellij")
        .args(["--session", zellij_session, "action", "focus-pane-id", &pane_id.to_string()])
        .output()
        .map_err(|e| format!("zellij focus-pane-id: {}", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !is_benign_zellij_focus_error(&stderr) {
            return Err(format!("zellij focus-pane-id failed: {}", stderr.trim()));
        }
    }
    Ok(())
}

/// The pane_id of the leftmost/topmost pane in a Zellij session's current
/// layout - the same on-screen reading-order tie-break
/// pane_position_rank/cycling already use, reused here as focus_lane's
/// deterministic "which pane, absent anything more specific" answer.
/// `None` if the session has no pane data available (list-panes failed,
/// or genuinely no terminal panes) - callers treat that as "nothing to
/// focus more specifically," not an error.
fn first_pane_id(session: &str) -> Option<u32> {
    drivers::zellij::pane_positions(session)
        .into_iter()
        .min_by_key(|(_, position)| *position)
        .map(|(pane_id, _)| pane_id)
}

fn switch_socket_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local/state/lanes/switch.sock")
}

/// Best-effort direct notification to an already-running Lanes Switch UI
/// process over a local socket - a filesystem watcher has an inherent floor
/// latency (write -> OS notices -> watcher wakes up -> app reacts) that no
/// amount of reordering removes, since the UI is a different process.
/// Silently does nothing if the UI isn't running or isn't listening yet;
/// show/hide have no meaning for an app that isn't running to have a window
/// in the first place, so there's nothing to fall back to for those. Lane
/// changes are still separately persisted to state.kdl (see
/// switch_claude_session), which the UI reads fresh on its own next startup.
fn notify_switch_socket(message: &str) {
    use std::io::Write;
    if let Ok(mut stream) = std::os::unix::net::UnixStream::connect(switch_socket_path()) {
        let _ = stream.write_all(message.as_bytes());
    }
}

pub fn notify_switch_show() {
    notify_switch_socket("show\n");
}

pub fn notify_switch_hide() {
    notify_switch_socket("hide\n");
}

/// Same direct-socket bypass as show/hide, extended to edit-mode - the CLI
/// already wrote the new value to state.kdl (see `ToggleEditMode`), this
/// just tells an already-running app about it immediately instead of
/// leaving it to notice via the fs watcher's inherent floor latency.
pub fn notify_switch_edit_mode(enabled: bool) {
    notify_switch_socket(&format!("edit-mode:{enabled}\n"));
}

/// Tells an already-running app to briefly pulse acknowledgment of a
/// show-inactive keypress that had no effect (edit mode was on - see
/// `ToggleShowInactive`, which never writes the value in that case). No
/// value rides along; this is purely "you pressed something, here's why
/// nothing changed."
pub fn notify_switch_show_inactive_noop() {
    notify_switch_socket("show-inactive-noop\n");
}

/// Cycle to the next (direction=1) or previous (direction=-1) live Claude
/// session, ordered to match lanes.toml's `order` (same ordering the display
/// uses), and within a shared Zellij session by each session's on-screen
/// reading-order position - Zellij tab left-to-right, then top-to-bottom/
/// left-to-right within that tab (see `pane_position_rank`) - falling back
/// to Zellij session name then session ID for any session whose lane isn't
/// listed in `order`, or whose pane position couldn't be resolved.
/// Thin lock-holding wrapper - see `state::with_cycle_lock`'s own doc
/// comment for why this needs to exist at all (concurrent/rapid cycle
/// invocations otherwise race, each reading the same stale "current"
/// before any of them has written). `cycle_claude_session_impl` is the
/// actual logic, unchanged.
pub fn cycle_claude_session(direction: i32) -> Result<(), String> {
    state::with_cycle_lock(|| cycle_claude_session_impl(direction))
}

fn cycle_claude_session_impl(direction: i32) -> Result<(), String> {
    let ct0 = std::time::Instant::now();
    let cfg = config::Config::load();
    let live_sessions = if cfg.driver_enabled("claude") {
        drivers::claude::enumerate()
    } else {
        vec![]
    };
    logging::perf("cycle.enumerated", &format!("elapsed_us={} count={}", ct0.elapsed().as_micros(), live_sessions.len()));
    // Skip sessions that live in an inactive lane, or one with no cached
    // WezTerm tab-id - that's exactly what caused the "wezterm
    // activate-tab failed" cycling errors, since
    // there's nothing for a switch to actually land on. Also skip anything
    // individually excluded via edit mode's per-session toggle (see
    // state::is_claude_session_disabled) - same rule signal_cyclable()
    // applies for the dashboard's own cyclable flag, kept in sync here
    // rather than re-derived.
    let live_sessions: Vec<_> = live_sessions.into_iter()
        .filter(|s| session_belongs_to_reachable_lane(s.zellij_session.as_deref(), &cfg))
        .filter(|s| !state::is_claude_session_disabled(&s.session_id))
        .collect();
    logging::perf("cycle.filtered", &format!("elapsed_us={} count={}", ct0.elapsed().as_micros(), live_sessions.len()));

    // pane_position_rank is only ever consulted as a tiebreaker between two
    // live Claude sessions in the *same* Zellij session (see its own doc
    // comment) - every session with just one live Claude session sorts
    // entirely on lane_order_rank, so querying list-panes for it resolves
    // nothing. This used to run unconditionally and sequentially, one
    // ~120-170ms `list-panes` call per distinct live Zellij session, every
    // single cycle keypress, before switch_claude_session (and thus
    // switch.trigger) even ran - perf.log's keystroke -> switch.trigger gap
    // was almost entirely this loop. Restricting to the actually-ambiguous
    // sessions and running what's left concurrently cuts both the call
    // count and the serialization.
    let ambiguous_sessions = sessions_needing_pane_positions(live_sessions.iter().map(|s| s.zellij_session.as_deref()));
    logging::perf("cycle.ambiguous_sessions", &format!("elapsed_us={} sessions={:?}", ct0.elapsed().as_micros(), ambiguous_sessions));
    let positions: HashMap<String, HashMap<u32, (usize, i64, i64)>> = std::thread::scope(|scope| {
        let handles: Vec<_> = ambiguous_sessions.into_iter()
            .map(|s| (s.clone(), scope.spawn(move || drivers::zellij::pane_positions(&s))))
            .collect();
        handles.into_iter().map(|(s, h)| (s, h.join().unwrap_or_default())).collect()
    });
    logging::perf("cycle.positions_resolved", &format!("elapsed_us={}", ct0.elapsed().as_micros()));

    let mut sessions: Vec<(String, String, (usize, i64, i64))> = live_sessions.into_iter()
        .map(|s| {
            let session = s.zellij_session.clone().unwrap_or_default();
            let pane_rank = pane_position_rank(positions.get(&session), s.zellij_pane_id);
            (session, s.session_id, pane_rank)
        })
        .collect();
    sessions.sort_by_key(|(session, id, pane_rank)| {
        (lane_order_rank(session, &cfg), *pane_rank, session.clone(), id.clone())
    });

    if sessions.is_empty() {
        return Ok(());
    }

    let ids: Vec<String> = sessions.into_iter().map(|(_, id, _)| id).collect();
    let cursor = state::read_claude_cursor();
    let current_index = cursor.as_deref().and_then(|c| ids.iter().position(|id| id == c));
    let idx = cycle_index(ids.len(), current_index, direction);
    logging::perf("cycle.target_resolved", &format!("elapsed_us={} target={}", ct0.elapsed().as_micros(), ids[idx]));

    switch_claude_session(&ids[idx])
}

/// Cycle to the next (direction=1) or previous (direction=-1) active,
/// reachable lane - a second, deliberately independent cycling policy
/// alongside `cycle_claude_session`, not a generalization of it. Doesn't
/// care whether anything is running inside a lane at all, Claude or
/// otherwise - this is "which lane," not "which session." Kept as its own
/// small function rather than unified with `cycle_claude_session` behind a
/// shared abstraction for now: a real third policy (the lensd-backed one
/// that's coming) will show a lot more about what that abstraction should
/// actually look like than guessing from two policies would.
/// Same lock-holding wrapper as `cycle_claude_session` - see
/// `state::with_cycle_lock`'s doc comment.
pub fn cycle_lane(direction: i32) -> Result<(), String> {
    state::with_cycle_lock(|| cycle_lane_impl(direction))
}

fn cycle_lane_impl(direction: i32) -> Result<(), String> {
    let cfg = config::Config::load();
    // cfg.lanes is already sorted by lanes.toml's `order` (Config::load) -
    // no separate ranking step needed, unlike cycle_claude_session's own
    // sessions (which aren't lanes themselves, just grouped by one).
    let ids: Vec<&str> = cfg.lanes.iter()
        .filter(|l| {
            let has_cached_tab_id = l.terminal_session()
                .is_some_and(|s| state::get_wezterm_tab_id(s).is_some());
            reachable_lane_decision(l.active, l.terminal_session().is_some(), has_cached_tab_id)
        })
        .map(|l| l.id.as_str())
        .collect();

    if ids.is_empty() {
        return Ok(());
    }

    let cursor = state::read_focused_lane();
    let current_index = cursor.as_deref().and_then(|c| ids.iter().position(|id| *id == c));
    let idx = cycle_index(ids.len(), current_index, direction);
    focus_lane(ids[idx])
}

/// The one entry point a cycling hotkey should actually call - dispatches
/// to whichever policy `cycle_policy` names in the global `lanes.toml`
/// (default: `ClaudeSessions`, today's existing behavior). One policy or
/// the other, never both at once - see `CyclePolicy`'s own doc comment.
/// `cycle_claude_session`/`cycle_lane` stay directly callable too (as
/// `sessions next`/`prev` and `lane-cycle next`/`prev`) for testing a
/// specific policy regardless of what's currently configured.
pub fn cycle(direction: i32) -> Result<(), String> {
    match config::Config::load().cycle_policy {
        model::CyclePolicy::ClaudeSessions => cycle_claude_session(direction),
        model::CyclePolicy::Lanes => cycle_lane(direction),
    }
}

/// A Zellij session's position in `cfg.lanes` (already sorted by lanes.toml's
/// `order` in `Config::load`) - `cfg.lanes.len()` for anything not found, so
/// unmatched sessions sort after every real lane rather than interleaving
/// with them.
fn lane_order_rank(zellij_session: &str, cfg: &config::Config) -> usize {
    cfg.lanes
        .iter()
        .position(|l| l.terminal_session() == Some(zellij_session))
        .unwrap_or(cfg.lanes.len())
}

/// A live Claude session's on-screen reading-order position within its
/// Zellij session, as (tab_position, pane_y, pane_x) - looked up by its
/// `zellij_pane_id` in the map `drivers::zellij::pane_positions` returns.
/// Matching by pane id rather than cwd is deliberate: two Claude sessions in
/// the same repo but different tabs share a cwd and would otherwise be
/// indistinguishable. Sessions with no positions available (list-panes
/// failed) or no recorded pane id sort after every resolved session, in the
/// same relative order cycling used before this ranking existed.
fn pane_position_rank(
    positions: Option<&HashMap<u32, (usize, i64, i64)>>,
    zellij_pane_id: Option<u32>,
) -> (usize, i64, i64) {
    let (Some(positions), Some(pane_id)) = (positions, zellij_pane_id) else {
        return (usize::MAX, i64::MAX, i64::MAX);
    };
    positions.get(&pane_id).copied().unwrap_or((usize::MAX, i64::MAX, i64::MAX))
}

/// Which Zellij sessions among the given live Claude sessions actually need
/// a `pane_positions` (list-panes) lookup - only ones hosting 2+ live Claude
/// sessions, since `pane_position_rank` is never consulted otherwise (a
/// session with a single live Claude session already sorts entirely on
/// `lane_order_rank`). Pulled out of `cycle_claude_session` so the "which
/// sessions are ambiguous" decision is testable without any subprocess I/O.
fn sessions_needing_pane_positions<'a>(zellij_sessions: impl Iterator<Item = Option<&'a str>>) -> Vec<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for zs in zellij_sessions.flatten() {
        *counts.entry(zs).or_insert(0) += 1;
    }
    counts.into_iter().filter(|(_, count)| *count > 1).map(|(zs, _)| zs.to_string()).collect()
}

/// Whether a live Claude session should be reachable by cycling, based on
/// the activity of whatever lane (if any) its Zellij session belongs to. A
/// session with no matching lane at all (not part of any configured lane)
/// isn't subject to this - it was never something you could mark inactive
/// in the first place, so it stays reachable.
fn session_belongs_to_reachable_lane(zellij_session: Option<&str>, cfg: &config::Config) -> bool {
    let zellij_session = zellij_session.unwrap_or("");
    match cfg.lane_for_session(zellij_session) {
        Some(lane) => {
            // Matched by terminal_session() == Some(zellij_session), so this
            // lane has a terminal by construction - true unconditionally.
            let has_cached_tab_id = state::get_wezterm_tab_id(zellij_session).is_some();
            reachable_lane_decision(lane.active, true, has_cached_tab_id)
        }
        None => true,
    }
}

/// `has_cached_tab_id` only matters when the lane actually has a terminal to
/// land on - a lane with no Terminal facet at all (no `tab.toml`, or one
/// with no `zellij_session` scope element) has nothing for that cache to be
/// missing, so it's reachable on `active` alone. Without this, a lane never
/// shows up in cycling at all until it's given a terminal - the terminal
/// isn't what cycling fundamentally depends on, landing on *something* is.
fn reachable_lane_decision(active: bool, has_terminal_session: bool, has_cached_tab_id: bool) -> bool {
    active && (!has_terminal_session || has_cached_tab_id)
}

fn cycle_index(len: usize, current_index: Option<usize>, direction: i32) -> usize {
    let n = len as i32;
    let current = current_index.map(|i| i as i32).unwrap_or(-1);
    (((current + direction) % n + n) % n) as usize
}

/// The existing tab, if any, with a pane already at `path` - so navigating
/// to a repo reuses that pane instead of always spawning a new tab. `path`
/// must already be in the same (absolute) form as pane cwds.
fn find_tab_at_path<'a>(shape: &'a model::TerminalShape, path: &str) -> Option<&'a model::TabInfo> {
    shape.tabs.iter().find(|tab| tab.panes.iter().any(|p| p.cwd.as_deref() == Some(path)))
}

/// Jump to a specific Zellij pane by its numeric id: raise the session's
/// WezTerm tab, then focus the pane. Used by a Command-kind signal's
/// FocusPane action (the pane a long-running command finished in). Same two
/// steps as switch_claude_session's own switch, minus the Claude cursor
/// bookkeeping - a shell command isn't a session to make "current".
pub fn focus_pane(session: &str, pane: u32) -> Result<(), String> {
    activate_wezterm_tab(session, true)?;
    focus_zellij_pane(session, pane as u64)?;
    Ok(())
}

pub fn navigate_to_repo_pane(session: &str, path: &str) -> Result<(), String> {
    // Activate the WezTerm tab for this session
    activate_wezterm_tab(session, true)?;

    // Navigate within Zellij to the right tab. Pane cwds observed from Zellij
    // are always absolute, but a lane's configured repo path is often written
    // with a `~/` shorthand - compare expanded forms so an existing pane at
    // the same directory is actually found instead of always falling through
    // to spawning a new tab.
    let path = expand_tilde(path);
    let path = path.as_str();

    let Some((shape, _)) = drivers::zellij::layout_for_session(session) else {
        return Ok(());
    };

    let target_tab = find_tab_at_path(&shape, path);

    if let Some(tab) = target_tab {
        std::process::Command::new("/opt/homebrew/bin/zellij")
            .args(["--session", session, "action", "go-to-tab-name", &tab.name])
            .output()
            .map_err(|e| e.to_string())?;

        // Focus the shell pane at the target path (prefer shell over claude/editor)
        let panes = &tab.panes;
        let target_idx = panes.iter().position(|p| {
            p.cwd.as_deref() == Some(path) && p.command.is_none()
        }).or_else(|| {
            panes.iter().position(|p| p.cwd.as_deref() == Some(path))
        });

        if let Some(target) = target_idx {
            let focused = panes.iter().position(|p| p.focused).unwrap_or(0);
            let n = panes.len();
            if target != focused && n > 1 {
                let steps = (target + n - focused) % n;
                for _ in 0..steps {
                    std::process::Command::new("/opt/homebrew/bin/zellij")
                        .args(["--session", session, "action", "focus-next-pane"])
                        .output()
                        .map_err(|e| e.to_string())?;
                }
            }
        }
    } else {
        std::process::Command::new("/opt/homebrew/bin/zellij")
            .args(["--session", session, "action", "new-tab", "--cwd", path])
            .output()
            .map_err(|e| e.to_string())?;
    }

    Ok(())
}

/// The newest `gui-sock-*` in WezTerm's data dir that's actually accepting
/// connections right now - not just the newest by mtime. A GUI that dies
/// without cleanup leaves its socket file behind; until a new GUI starts
/// and replaces it, the dead one has the newest mtime and would otherwise
/// win, pointing every caller at a socket nobody is listening on (see
/// BUG-wezterm-stale-socket.md). Each candidate costs microseconds to
/// probe (connect, then immediately drop - no data sent), so trying every
/// candidate newest-first is cheap even though in practice only the
/// newest ever matters.
pub fn wezterm_socket() -> Option<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = std::path::PathBuf::from(home).join(".local/share/wezterm");
    wezterm_socket_in(&dir)
}

/// [`wezterm_socket`]'s logic against an arbitrary directory, so a test can
/// point it at a throwaway one instead of the real `~/.local/share/wezterm`.
fn wezterm_socket_in(dir: &std::path::Path) -> Option<String> {
    let mut socks: Vec<_> = std::fs::read_dir(dir).ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("gui-sock-"))
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            let modified = meta.modified().ok()?;
            Some((modified, e.path()))
        })
        .collect();
    socks.sort_by(|a, b| b.0.cmp(&a.0));
    socks.into_iter()
        .map(|(_, p)| p)
        .find(|p| std::os::unix::net::UnixStream::connect(p).is_ok())
        .map(|p| p.to_string_lossy().into_owned())
}

fn activate_wezterm_tab(session: &str, focus: bool) -> Result<(), String> {
    let cached = state::get_wezterm_tab_id(session).ok_or_else(|| {
        format!("no cached WezTerm tab for session '{}' - run `lanes tabs set {} <id>`", session, session)
    })?;

    // No live GUI at all is its own case, not a wezterm-cli failure to
    // surface - distinguishing it here means failing in microseconds
    // instead of the ~2.5s "spawn a mux server and retry" detour
    // `wezterm cli` takes on a refused socket (see
    // BUG-wezterm-stale-socket.md), and skips a confusing error that
    // blames the tab cache for something that isn't the tab cache's fault.
    let Some(sock) = wezterm_socket() else {
        return Err("WezTerm isn't running".to_string());
    };

    // Deliberately not verifying the cached tab still exists via a `wezterm
    // cli list` round-trip first - that's a full extra subprocess+socket
    // connect (~60-100ms) just to sanity-check a cache that's almost always
    // correct. Trust it and let activate-tab itself fail (with wezterm's own
    // error surfaced below) on the rare occasion it's stale.
    if focus {
        // Fire-and-forget: raising the WezTerm window doesn't need to block
        // this call, and waiting on `open`'s own ~90ms launchservices
        // round-trip was pure latency on the hot path. See spawn_detached.
        spawn_detached(std::process::Command::new("open").args(["-a", "WezTerm"])).ok();
    }

    // --no-auto-start: same reasoning as wezterm_socket() above - a refused
    // socket must never trigger wezterm's own "spawn a mux server" detour.
    // With a live socket already confirmed, this only matters if the GUI
    // dies in the gap between the two calls, but there's no reason to
    // leave the slow path reachable at all. lensd already does this.
    let mut cmd = std::process::Command::new("/opt/homebrew/bin/wezterm");
    cmd.args(["cli", "--no-auto-start", "activate-tab", "--tab-id", &cached.to_string()]);
    cmd.env("WEZTERM_UNIX_SOCKET", &sock);
    let output = cmd.output().map_err(|e| format!("wezterm activate-tab: {}", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // The last ERROR line, not the whole multi-line wezterm log - the
        // rest (connection attempts, mux-server spawn chatter) is noise a
        // Hammerspoon alert shouldn't have to show.
        let last_error = stderr.lines().rev().find(|l| l.contains("ERROR")).unwrap_or_else(|| stderr.trim());
        return Err(format!(
            "wezterm activate-tab failed for cached tab {} (session '{}') - WezTerm may have restarted since tabs were cached; run `ztabs up`: {}",
            cached, session, last_error.trim()
        ));
    }

    Ok(())
}

/// Bundle ID a driver's app is addressed by, when placement (`monitor` +
/// `position`) is requested for a target using it. WezTerm has a real OS
/// window of its own and is placeable like any other app - only `zellij`
/// (a multiplexer running inside whatever terminal hosts it, no window of
/// its own) and `app` (no fixed identity beyond its display name) can't be.
/// A target using either of those with `monitor`/`position` set is rejected
/// in `apply_targets`.
fn target_bundle_id(driver: &model::TargetDriver) -> Option<&str> {
    match driver {
        model::TargetDriver::Wezterm { .. } => Some("com.github.wez.wezterm"),
        model::TargetDriver::Obsidian { .. } => Some("md.obsidian"),
        model::TargetDriver::Boards { .. } => Some("com.dev-platform.boards"),
        model::TargetDriver::Sourcetree { .. } => Some("com.torusknot.SourceTreeNotMAS"),
        model::TargetDriver::Vscode { .. } => Some("com.microsoft.VSCode"),
        // Only known if the config gave us one - unlike the drivers above,
        // "app" has no fixed identity, so placement only works when the
        // config author supplied a bundle_id explicitly.
        model::TargetDriver::App { bundle_id, .. } => bundle_id.as_deref(),
        // Same as Boards: its own activation already returns the exact
        // pid to place by, so placement never needs a bundle id for this
        // driver at all.
        model::TargetDriver::Appctl { .. } => None,
        model::TargetDriver::Zellij { .. } => None,
    }
}

/// A regex matching an Obsidian window's title for this specific vault -
/// "<note> - <vault> - Obsidian <app version>", confirmed live, e.g.
/// "Inbox - Formation - Obsidian 1.14.5". No trailing `$` anchor right
/// after "Obsidian" itself, or every title would silently fail to match
/// and the window would just stay wherever it already was, since an
/// upgrade changes that version suffix. Requires the vault name to be
/// preceded by either start-of-string or the literal "- " separator, not
/// just a word boundary: a word boundary alone would still let "Formation"
/// match inside "My Formation", since both are preceded by one; requiring
/// the separator itself doesn't.
fn obsidian_title_pattern(vault: &str) -> String {
    format!(r"(^|- ){} - Obsidian\b", regex::escape(vault))
}

/// Percent-encode everything but RFC 3986's unreserved characters. Minimal,
/// dependency-free - the only user right now is the Obsidian vault URI,
/// where vault names are typically just letters/digits/spaces.
fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{:02X}", b)
            }
        })
        .collect()
}

fn run_command_checked(cmd: &mut std::process::Command, label: &str) -> Result<(), String> {
    let output = cmd.output().map_err(|e| format!("{label}: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("{label} failed: {}", stderr.trim()));
    }
    Ok(())
}

/// Run a target's activation step - whatever app-specific mechanism makes
/// the right window/pane/vault frontmost. Doesn't place anything itself;
/// placement is a separate, batched step in `apply_targets` (every
/// placement in a lane goes through one `lanes-wm apply` call, not one per
/// target).
///
/// `default_session` is the owning lane's own terminal session - used when
/// a `wezterm`/`zellij` target omits `session`, which is what lets one
/// global default target (e.g. "WezTerm always goes to lg-right") apply
/// across lanes that each have a different session name.
///
/// `raise_app` controls whether the `app` driver visibly brings its app
/// forward (`open -a`) - only meaningful there, and just `target.raise`
/// passed straight through (see its doc comment). lanes-wm's `--focused`
/// placement selector resolves and repositions a window correctly whether
/// or not its app is frontmost, so a target that's only being *placed*
/// doesn't need this to do that - raising is for a target that actually
/// wants to be seen, placed or not. Without gating this at all, every
/// target in a lane would briefly raise its own app in sequence before
/// the final terminal-focus step landed - visibly flickering through each
/// app on every switch instead of just showing the ones that asked for it.
fn activate_target(
    driver: &model::TargetDriver,
    raise_app: bool,
    default_session: Option<&str>,
    pre_place: Option<(String, serde_json::Value)>,
) -> Result<Option<u32>, String> {
    let resolve_session = |session: &Option<String>| -> Result<String, String> {
        session.clone().or_else(|| default_session.map(String::from))
            .ok_or_else(|| "no session given and this lane has no terminal session to default to".to_string())
    };
    match driver {
        // Never raises WezTerm itself - that's always the caller's own
        // explicit, final step (see focus_lane/switch_claude_session),
        // deliberately run after every target here so the terminal ends up
        // frontmost regardless of target order. Raising it here too would
        // just be a redundant, visible extra flip before that final step.
        model::TargetDriver::Wezterm { session } => {
            activate_wezterm_tab(&resolve_session(session)?, false)?;
            Ok(None)
        }
        model::TargetDriver::Zellij { session, pane } => {
            let session = resolve_session(session)?;
            let pane_id = match pane {
                Some(p) => *p as u64,
                None => first_pane_id(&session)
                    .ok_or_else(|| format!("no panes found in zellij session '{session}'"))?
                    as u64,
            };
            focus_zellij_pane(&session, pane_id)?;
            Ok(None)
        }
        model::TargetDriver::Obsidian { vault } => {
            let uri = format!("obsidian://open?vault={}", percent_encode(vault));
            run_command_checked(std::process::Command::new("open").arg(uri), "open obsidian:// URI")?;
            Ok(None)
        }
        model::TargetDriver::Boards { project } => activate_boards_target(project, pre_place),
        model::TargetDriver::Sourcetree { repo } => {
            let path = expand_tilde(repo);
            run_command_checked(
                std::process::Command::new("/opt/homebrew/bin/stree").current_dir(&path).arg("."),
                "stree",
            )?;
            Ok(None)
        }
        model::TargetDriver::Vscode { folder } => {
            let path = expand_tilde(folder);
            run_command_checked(
                std::process::Command::new("code").args(["-r", &path]),
                "code -r (is the 'code' shell command installed? VS Code > Cmd+Shift+P > \"Shell Command: Install 'code' command in PATH\")",
            )?;
            Ok(None)
        }
        model::TargetDriver::App { name, .. } => {
            if raise_app {
                run_command_checked(std::process::Command::new("open").args(["-a", name]), "open -a")?;
            }
            Ok(None)
        }
        // The command (`navigate view=... lane=...`) is what the app should
        // do on this switch, and is sent either way; raising is a separate
        // decision (`raise`), carried out with the app's `show` - never
        // folded into the command.
        model::TargetDriver::Appctl { app, cmd, args } => activate_appctl_target(app, cmd, args.clone(), raise_app, pre_place),
    }
}

/// Switches Boards to `project` over its `appctl` socket (reaches a
/// `just dev` instance, not just the installed app - see
/// `PLAN-boards-via-appctl.md`), falling back to `open 'boards://...'` when
/// no instance is reachable. Returns the instance's pid when the socket
/// path was used, so `apply_targets` can place that exact process instead
/// of guessing by bundle id (which only resolves the installed copy). Just
/// Boards' own argument-shape wrapper around `activate_via_appctl` - see
/// its doc comment for the actual mechanism, shared with `TargetDriver::Appctl`.
fn activate_boards_target(project: &str, pre_place: Option<(String, serde_json::Value)>) -> Result<Option<u32>, String> {
    let t0 = std::time::Instant::now();
    let mut args = serde_json::Map::new();
    args.insert("project".to_string(), serde_json::json!(project));
    let outcome = match appctl::default_state_root() {
        Some(root) => activate_via_appctl(&root, "boards", vec![("open_project", args)], pre_place),
        None => AppctlOutcome::NoInstance,
    };
    let result = match outcome {
        AppctlOutcome::Activated(pid) => Ok(Some(pid)),
        AppctlOutcome::AppError(msg) => Err(msg),
        // The normal "Boards isn't running" case - `open boards://` cold-launches
        // the installed app on that project, same as always, so this is quiet.
        AppctlOutcome::NoInstance => {
            let uri = format!("boards://open?project={}", percent_encode(project));
            run_command_checked(std::process::Command::new("open").arg(uri), "open boards:// URI")
                .map(|()| None)
                .map_err(rewrite_boards_open_error)
        }
    };
    logging::perf(
        "focus.boards_appctl",
        &format!("outcome={} elapsed_us={}", if matches!(result, Ok(Some(_))) { "appctl" } else { "fallback" }, t0.elapsed().as_micros()),
    );
    result
}

/// Generic `appctl`-based activation for the `appctl` driver (any app that
/// registers its own command the way Boards registers `open_project` -
/// see DECISIONS.md, "Boards target: appctl socket first," the pattern
/// this generalizes). No URL-scheme fallback of its own, unlike Boards'
/// `boards://` - most apps don't have one, so "not running" is just a
/// plain failure here.
/// An `appctl` target: sends `cmd` (unless it's `show` itself, which is
/// the raise), and with `raise` also `show`s the app and returns its pid for
/// placement. An app that isn't running is an error only when it was to be
/// raised; a target that only sends a command (`navigate` to follow the
/// lane) skips it quietly, so a closed app doesn't fail every switch.
fn activate_appctl_target(
    app: &str,
    cmd: &str,
    args: serde_json::Map<String, serde_json::Value>,
    raise: bool,
    pre_place: Option<(String, serde_json::Value)>,
) -> Result<Option<u32>, String> {
    let requests = appctl_requests(cmd, args, raise);
    let pre_place = pre_place.filter(|_| raise);
    let outcome = match appctl::default_state_root() {
        Some(root) => activate_via_appctl(&root, app, requests, pre_place),
        None => AppctlOutcome::NoInstance,
    };
    match outcome {
        AppctlOutcome::Activated(pid) => Ok(raise.then_some(pid)),
        AppctlOutcome::AppError(msg) => Err(msg),
        AppctlOutcome::NoInstance if raise => Err(format!("{app} isn't running")),
        AppctlOutcome::NoInstance => Ok(None),
    }
}

/// `open boards://...` failing with the raw macOS LaunchServices error
/// ("No application knows how to open URL boards://...",
/// kLSApplicationNotFoundErr) means one specific, common thing on this
/// setup: no dev Boards instance is running (so there was nothing for
/// `activate_via_appctl` to reach), and nothing is installed to register
/// the scheme as a fallback either (deliberately - see DECISIONS.md,
/// "Boards target: appctl socket first"). That's an actionable, ordinary
/// state, not a mysterious OS failure - so surface it as one instead of
/// the cryptic `NSOSStatusErrorDomain Code=-10814` text. Any other `open`
/// failure (a different error, or a future installed build hitting
/// something else) passes through unchanged.
fn rewrite_boards_open_error(e: String) -> String {
    if e.contains("No application knows how to open URL") {
        "Boards isn't running".to_string()
    } else {
        e
    }
}

/// Prepends "target '{driver}': " to `e`, unless `e` already names the
/// driver itself (e.g. "Boards isn't running") - that's already
/// self-identifying, so the prefix is only noise on top of it.
fn prefix_target_error(driver: &str, e: String) -> String {
    if e.to_lowercase().contains(driver) {
        e
    } else {
        format!("target '{driver}': {e}")
    }
}

#[derive(Debug)]
enum AppctlOutcome {
    Activated(u32),
    AppError(String),
    NoInstance,
}

/// Connects to `app`'s `appctl` instance and sends it `cmd`/`args` -
/// the one mechanism behind both `TargetDriver::Boards` (`cmd =
/// "open_project"`, generalized out of what used to be Boards-specific
/// code here) and `TargetDriver::Appctl` (any `cmd` the target app
/// registered for itself). Getting `pid` needs nothing app-specific -
/// every `appctl` app has it for free, from the connection handshake
/// (`appctl`'s own built-in `info`), before `cmd` is ever sent.
///
/// `pre_place`, when given (a resolved monitor uuid + position, only ever
/// supplied by `apply_targets` for a target that's actually getting
/// placed), moves the window to its destination *before* `cmd` runs -
/// most such commands raise the app's window as a side effect (same as
/// `open 'boards://...'` always did for Boards), so without this the
/// window becomes visible wherever it last was for the ~200-600ms gap
/// until the lane switch's own (separate, batched) lanes-wm apply call
/// catches up. Moving it first means the raise reveals a window that's
/// already in the right place - no visible flash on the wrong screen.
/// The commands to send an app, in order.
type Requests<'a> = Vec<(&'a str, serde_json::Map<String, serde_json::Value>)>;

/// What an `appctl` target sends: its command first (what to show), then,
/// with `raise`, `show`, so the window comes forward already showing the
/// right thing. `show` as the command is the raise itself: sent only with
/// `raise`, and once.
fn appctl_requests(cmd: &str, args: serde_json::Map<String, serde_json::Value>, raise: bool) -> Requests<'_> {
    let mut requests: Requests = Vec::new();
    if cmd != "show" {
        requests.push((cmd, args));
    }
    if raise {
        requests.push(("show", serde_json::Map::new()));
    }
    requests
}

fn activate_via_appctl(
    root: &std::path::Path,
    app: &str,
    requests: Requests<'_>,
    pre_place: Option<(String, serde_json::Value)>,
) -> AppctlOutcome {
    let mut conn = match appctl::client::connect(root, app, None) {
        Ok(conn) => conn,
        Err(appctl::client::ClientError::NoRunningInstance { .. }) => return AppctlOutcome::NoInstance,
        Err(e) => {
            eprintln!("warning: {app} appctl: {e}");
            return AppctlOutcome::NoInstance;
        }
    };
    let pid = conn.hello().pid;

    // `Selector::Any` (no "focused"/"title" in this request) is only valid
    // when the app has exactly one window - true for every app this has
    // been used with so far (Boards' single board view). Best-effort: a
    // failure here (e.g. two windows) just means falling back to the old
    // raise-then-place timing for this one switch, not a hard error.
    if let Some((monitor, position)) = pre_place {
        let mut placement = serde_json::Map::new();
        placement.insert("pid".to_string(), serde_json::json!(pid));
        placement.insert("monitor".to_string(), serde_json::json!(monitor));
        placement.insert("position".to_string(), position);
        for warning in run_lanes_wm_apply(&[serde_json::Value::Object(placement)]) {
            eprintln!("warning: {app} pre-placement: {warning}");
        }
    }

    for (name, args) in requests {
        match conn.request(name, args) {
            Ok(_) => {}
            Err(appctl::client::RequestError::App(msg)) => return AppctlOutcome::AppError(msg),
            Err(appctl::client::RequestError::Io(msg)) => {
                eprintln!("warning: {app} appctl: {msg}");
                return AppctlOutcome::NoInstance;
            }
        }
    }
    AppctlOutcome::Activated(pid)
}

/// Whether a wezterm/zellij target has no session to act on at all - no
/// `session` field of its own, and no lane-level `default_session` to fall
/// back to either. True for a target a lane only has because it inherited
/// it from a global default (e.g. `default.toml`'s wezterm target) while
/// the lane itself has no terminal facet yet - there's nothing for it to
/// address, so it's nothing-to-do, not a failure. Any other driver always
/// returns false here - they don't have a session concept at all.
fn terminal_target_has_no_resolvable_session(driver: &model::TargetDriver, default_session: Option<&str>) -> bool {
    let own_session = match driver {
        model::TargetDriver::Wezterm { session } => session,
        model::TargetDriver::Zellij { session, .. } => session,
        _ => return false,
    };
    own_session.is_none() && default_session.is_none()
}

/// Activate every target in a lane, then place all of them that asked for
/// placement in a single `lanes-wm apply` call - never one `lanes-wm place`
/// per target, since a lane switch always moves several windows at once and
/// should do it in one shot. Best-effort like the scope loop above: one
/// broken target doesn't block the rest.
///
/// `default_session` - see `activate_target`.
/// A target's monitor handle + position, resolved to what `lanes-wm` wants:
/// the handle's configured uuid, and the position (or `"full"`, its
/// default) as JSON. Shared by `apply_targets`' own placement step and
/// Boards' pre-placement (`activate_boards_target`'s `pre_place`), so the
/// two don't drift on what "this target's destination" means.
fn resolve_placement_json(cfg: &config::Config, monitor: &str, position: &Option<toml::Value>) -> Result<(String, serde_json::Value), String> {
    let uuid = cfg.monitor_uuid(monitor).ok_or_else(|| format!("monitor handle '{monitor}' not found in config"))?;
    let position = position.clone().unwrap_or_else(|| toml::Value::String("full".to_string()));
    let position_json = serde_json::to_value(&position).map_err(|e| format!("invalid position: {e}"))?;
    Ok((uuid.to_string(), position_json))
}

/// Live count of attached monitors, via `lanes-wm monitors --compact` -
/// `None` if the call fails for any reason (lanes-wm not on PATH, bad
/// JSON, ...), in which case callers treat it as "not the single-monitor
/// case" rather than guessing from a failure.
fn attached_monitor_count() -> Option<usize> {
    let output = std::process::Command::new("lanes-wm").args(["monitors", "--compact"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let monitors: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).ok()?;
    Some(monitors.len())
}

/// A target's effective (monitor handle, position) for this switch - its
/// own `monitor`/`position` normally, or `("main", position_solo)` when
/// `solo` (exactly one monitor attached) and the target declares one. See
/// `Target::position_solo`'s doc comment for why "does my own monitor
/// still resolve" isn't the right trigger - `main` always resolves, so a
/// target that's normally full-screen there (Boards) still needs this to
/// override its usual position, not just unresolvable ones (WezTerm).
/// `None` means this target isn't placed at all this switch, same as a
/// bare `target.monitor` check before this field existed.
fn effective_destination(target: &model::Target, solo: bool) -> Option<(String, Option<toml::Value>)> {
    if solo {
        if let Some(position) = &target.position_solo {
            return Some(("main".to_string(), Some(position.clone())));
        }
    }
    target.monitor.clone().map(|monitor| (monitor, target.position.clone()))
}

fn apply_targets(targets: &[model::Target], cfg: &config::Config, default_session: Option<&str>) -> Vec<String> {
    let t0 = std::time::Instant::now();
    let solo = attached_monitor_count() == Some(1);
    let mut warnings = Vec::new();
    let mut placements = Vec::new();
    let mut pending = Vec::new();

    for target in targets {
        if target.launch {
            // See spawn_detached: a launched app can outlive `lanes` for
            // the rest of the session.
            if let model::TargetDriver::App { name, .. } = &target.driver {
                spawn_detached(std::process::Command::new("open").args(["-a", name])).ok();
            } else if let Some(bundle_id) = target_bundle_id(&target.driver) {
                spawn_detached(std::process::Command::new("open").args(["-b", bundle_id])).ok();
            }
        }

        // A minimize request never activates/raises first - doing so would
        // just bring the window forward right before hiding it again - and
        // never places, so it short-circuits before all of that.
        if let Some(minimized) = target.minimized {
            let Some(bundle_id) = target_bundle_id(&target.driver) else {
                let e = format!(
                    "target '{}' has `minimized` set but its driver can't be addressed by bundle id",
                    target.driver.name()
                );
                warnings.push(e);
                continue;
            };
            let mut placement = serde_json::Map::new();
            placement.insert("app".to_string(), serde_json::json!(bundle_id));
            placement.insert("minimized".to_string(), serde_json::json!(minimized));
            placements.push(serde_json::Value::Object(placement));
            continue;
        }

        // A wezterm/zellij target with no session of its own, in a lane
        // with no terminal session to default to (e.g. inherited from the
        // global default.toml by a lane that doesn't have one of its own
        // yet) - nothing to do, not a failure. Without this, such a lane
        // couldn't be landed on at all: every switch would hard-error on a
        // target that was never meant to apply to it in the first place.
        if terminal_target_has_no_resolvable_session(&target.driver, default_session) {
            continue;
        }

        pending.push((target, effective_destination(target, solo)));
    }

    // Each pending target's activation is an independent blocking
    // subprocess call (an `open` URI, a wezterm/zellij CLI round trip) that
    // doesn't depend on any other target's - land_lane always runs after
    // this and has the true final say on what ends up frontmost (see its
    // own doc comment), so activation order here was never load-bearing for
    // the end state. Running them concurrently rather than summing their
    // durations one by one measurably shortens every lane switch -
    // confirmed via perf.log (~420ms -> dominated by the slower of the two
    // calls instead of their sum).
    let results: Vec<Result<Option<u32>, String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = pending
            .iter()
            .map(|(target, effective)| {
                scope.spawn(move || {
                    let target_t0 = std::time::Instant::now();
                    // `raise` is the sole gate, regardless of whether this
                    // target has a position - a target with neither is a
                    // deliberate no-op (left exactly as it is, never
                    // touched), not an implicit "no position means raise
                    // it" rule. See Target::raise's doc comment.
                    let raise_app = target.raise;
                    // Only an appctl-activated driver (Boards, or the
                    // generic Appctl one) can use this - see
                    // activate_via_appctl's doc comment for why it needs
                    // its destination before raising, unlike every other
                    // driver here. `.ok()` deliberately throws the error
                    // away: an unresolvable monitor handle is reported
                    // properly below, in the placement loop that runs for
                    // every target uniformly - this is only a best-effort
                    // head start for these two drivers specifically.
                    let pre_place = match (&target.driver, effective) {
                        (model::TargetDriver::Boards { .. } | model::TargetDriver::Appctl { .. }, Some((monitor, position))) => {
                            resolve_placement_json(cfg, monitor, position).ok()
                        }
                        _ => None,
                    };
                    let result = activate_target(&target.driver, raise_app, default_session, pre_place);
                    logging::perf(
                        "focus.target_activated",
                        &format!("driver={} elapsed_us={}", target.driver.name(), target_t0.elapsed().as_micros()),
                    );
                    result
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| Err("activation thread panicked".to_string())))
            .collect()
    });

    for ((target, effective), result) in pending.into_iter().zip(results) {
        let activated_pid = match result {
            Ok(pid) => pid,
            Err(e) => {
                warnings.push(prefix_target_error(target.driver.name(), e));
                continue;
            }
        };

        let Some((monitor, position)) = &effective else { continue };
        let (identity_key, identity_value): (&str, serde_json::Value) = match activated_pid {
            Some(pid) => ("pid", serde_json::json!(pid)),
            None => match target_bundle_id(&target.driver) {
                Some(bundle_id) => ("app", serde_json::json!(bundle_id)),
                None => {
                    warnings.push(format!(
                        "target '{}' has monitor/position set but its driver can't be placed",
                        target.driver.name()
                    ));
                    continue;
                }
            },
        };
        let (uuid, position_json) = match resolve_placement_json(cfg, monitor, position) {
            Ok(v) => v,
            Err(e) => {
                warnings.push(prefix_target_error(target.driver.name(), e));
                continue;
            }
        };
        // `focused: true` is right for a single-window app: the activate
        // step above deterministically made the right window frontmost
        // first, so placement just acts on whatever that left focused
        // (lanes-wm's `--focused` selector). But Obsidian can have several
        // windows at once (one vault per window, "Open in new window"), and
        // `open obsidian://...` returns as soon as it hands off to Launch
        // Services - it doesn't wait for that vault's window to actually
        // become focused, so `--focused` placement right after is racy and
        // can grab a different vault's window instead. Obsidian's title is
        // always "<note> - <vault> - Obsidian" (or just "<vault> -
        // Obsidian" with no note open), so a title regex keyed on the vault
        // name sidesteps the race entirely instead of needing one.
        let mut placement = serde_json::Map::new();
        placement.insert(identity_key.to_string(), identity_value);
        placement.insert("monitor".to_string(), serde_json::json!(uuid));
        placement.insert("position".to_string(), position_json);
        match &target.driver {
            model::TargetDriver::Obsidian { vault } => {
                placement.insert(
                    "title".to_string(),
                    serde_json::json!(obsidian_title_pattern(vault)),
                );
            }
            _ => {
                placement.insert("focused".to_string(), serde_json::json!(true));
            }
        }
        placements.push(serde_json::Value::Object(placement));
    }

    if !placements.is_empty() {
        logging::perf("focus.wm_apply_start", &format!("count={} elapsed_us={}", placements.len(), t0.elapsed().as_micros()));
        warnings.extend(run_lanes_wm_apply(&placements));
        logging::perf("focus.wm_apply_done", &format!("elapsed_us={}", t0.elapsed().as_micros()));
    }

    warnings
}

/// One `lanes-wm apply` call for a whole batch of placements. Returns a
/// warning string per placement lanes-wm reported as failed - never panics
/// or aborts on a single bad placement, same partial-failure contract
/// `lanes-wm apply` itself has.
fn run_lanes_wm_apply(placements: &[serde_json::Value]) -> Vec<String> {
    use std::io::Write;

    let payload = serde_json::to_string(placements).unwrap_or_default();
    let mut child = match std::process::Command::new("lanes-wm")
        .arg("apply")
        .arg("--compact")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return vec![format!("lanes-wm apply: could not start lanes-wm: {e}")],
    };
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = stdin.write_all(payload.as_bytes());
    }
    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => return vec![format!("lanes-wm apply: {e}")],
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return vec![format!("lanes-wm apply exited with an error: {}", stderr.trim())];
    }

    #[derive(serde::Deserialize)]
    struct PlacementResult {
        app: String,
        ok: bool,
        #[serde(default)]
        error: Option<String>,
    }
    let results: Vec<PlacementResult> = match serde_json::from_slice(&output.stdout) {
        Ok(r) => r,
        Err(e) => return vec![format!("lanes-wm apply: could not parse its output: {e}")],
    };
    results
        .into_iter()
        .filter(|r| !r.ok)
        .map(|r| format!("placement failed for {}: {}", r.app, r.error.unwrap_or_default()))
        .collect()
}

fn focused_lane<'a>(cfg: &'a config::Config, focused_id: Option<&str>) -> Option<&'a model::Lane> {
    let id = focused_id?;
    cfg.lanes.iter().find(|l| l.id == id)
}

/// The focused lane's own target for a given driver, if either the lane or
/// the target exists - pulled out of `activate_context` as a pure function
/// (no process/state I/O) so the actual lookup logic is testable without
/// a live focused-lane file or real config directory.
fn context_target<'a>(cfg: &'a config::Config, focused_id: Option<&str>, driver_name: &str) -> Option<&'a model::Target> {
    focused_lane(cfg, focused_id)?.targets.iter().find(|t| t.driver.name() == driver_name)
}

/// Activate whichever target the *currently focused* lane has for the
/// given driver (e.g. "obsidian"), independent of lane
/// switching - this is what a context-aware hotkey (Hammerspoon's K/O)
/// calls on every press, not just on `lanes focus`. Deliberately
/// activation-only: never applies the target's placement, since re-moving
/// a window on every keypress would be a jarring side effect the hotkey
/// never asked for - raising is as far as this goes.
///
/// Falls back to a plain `open -a` of a sensible default app when there's
/// no focused lane, or the focused lane has no target for this driver -
/// the same bare behavior the hotkey had before it became context-aware,
/// so "no better answer available" is never worse than today.
pub fn activate_context(driver_name: &str) -> Result<(), String> {
    let cfg = config::Config::load();
    let focused = state::read_focused_lane();
    let target = context_target(&cfg, focused.as_deref(), driver_name);

    let Some(target) = target else {
        return fallback_activate(driver_name);
    };

    let default_session = focused_lane(&cfg, focused.as_deref()).and_then(|l| l.terminal_session());
    activate_and_raise(target, default_session)
}

/// Activate a target and guarantee it's actually raised, regardless of
/// driver. Shared by `activate_context` (a context-aware hotkey) and
/// `land_lane` (a lane switch's final "make the primary target win" step) -
/// both want exactly "bring this one thing forward, right now," never
/// placement.
fn activate_and_raise(target: &model::Target, default_session: Option<&str>) -> Result<(), String> {
    activate_target(&target.driver, true, default_session, None).map(|_pid| ())
}

fn fallback_activate(driver_name: &str) -> Result<(), String> {
    let app_name = match driver_name {
        "obsidian" => "Obsidian",
        other => return Err(format!("no fallback activation for driver '{other}'")),
    };
    run_command_checked(std::process::Command::new("open").args(["-a", app_name]), "open -a")
}

/// Resolve a lane id: use `explicit` if given, otherwise fall back to
/// `$ZELLIJ_SESSION_NAME` and find the lane whose Terminal facet matches it.
/// Used by `focus`, `activate`, and `deactivate` so all three can be run
/// with no argument from inside the lane's own zellij session.
pub fn resolve_lane_id(explicit: Option<String>, cfg: &config::Config) -> Result<String, String> {
    if let Some(id) = explicit {
        return Ok(id);
    }
    let session = std::env::var("ZELLIJ_SESSION_NAME").map_err(|_| {
        "No lane id given and $ZELLIJ_SESSION_NAME is unset - pass an id explicitly.".to_string()
    })?;
    cfg.lane_for_session(&session)
        .map(|l| l.id.clone())
        .ok_or_else(|| format!("No lane found with zellij session {session:?}"))
}

/// Best-effort: attempts every scope element and window placement even if
/// an earlier one failed (one broken WezTerm tab shouldn't block the rest of
/// the lane from focusing), collecting anything that went wrong along the
/// way. Each warning is still eprintln!'d immediately as before - so running
/// this from a terminal sees them right away - and also returned so a
/// caller with no attached terminal (Lanes Switch, calling this in-process)
/// has something to actually log instead of the warnings vanishing into a
/// GUI process's unreachable stderr.
/// The lane's own designated primary target, if any - assumes `doctor`
/// already rejected a lane with more than one, so the first match is the
/// only one there should ever be.
fn primary_target(targets: &[model::Target]) -> Option<&model::Target> {
    targets.iter().find(|t| t.primary)
}

/// Land on whichever target a lane switch should end up visually looking
/// at - the terminal by default, or the lane's own designated `primary`
/// target when it has one. Two things that used to be the same decision
/// now have to split apart: switching the Zellij/WezTerm *session state*
/// (other things depend on "which session is current" regardless of what
/// ends up visually frontmost) always happens if the lane has one, but it
/// only gets to keep the frontmost result if nothing else is primary. When
/// something else is primary, the terminal switch still happens (for the
/// state), WezTerm itself just doesn't get raised doing it, and the
/// primary target gets raised again immediately after - so it's the true
/// last word on what's actually in front.
///
/// `explicit_pane_id` is a more specific answer than anything primary (or
/// the plain first-pane fallback) can give - today, the exact pane a
/// Claude session was registered in (see `switch_claude_session`). Same
/// "more specific signal wins" hierarchy as everywhere else in this file:
/// explicit override > primary's own pane (if it's a Zellij target) >
/// first pane in the session.
/// Whether a primary target is itself the terminal (Zellij/WezTerm) - if
/// so, the terminal-focus step below already lands on it, there's nothing
/// further to raise afterward.
fn primary_is_terminal(primary: Option<&model::Target>) -> bool {
    matches!(
        primary.map(|t| &t.driver),
        Some(model::TargetDriver::Zellij { .. }) | Some(model::TargetDriver::Wezterm { .. })
    )
}

/// Only actually raise WezTerm if nothing else is going to win the
/// frontmost question anyway - otherwise this would visibly flash to
/// WezTerm right before the primary target gets raised over it.
fn should_raise_wezterm(primary: Option<&model::Target>) -> bool {
    primary.is_none() || primary_is_terminal(primary)
}

/// Which pane to land on, given everything known *before* falling back to
/// the session's first pane (that fallback needs a live `pane_positions`
/// query, so it's not part of this pure function) - explicit override >
/// primary's own pane (if it's a Zellij target) > nothing, defer to the
/// caller's own fallback.
fn preferred_pane(primary: Option<&model::Target>, explicit_pane_id: Option<u64>) -> Option<u64> {
    explicit_pane_id.or_else(|| {
        primary.and_then(|t| match &t.driver {
            model::TargetDriver::Zellij { pane, .. } => pane.map(|p| p as u64),
            _ => None,
        })
    })
}

fn land_lane(lane: &model::Lane, explicit_pane_id: Option<u64>, t0: std::time::Instant) -> Vec<String> {
    let mut warnings = Vec::new();
    let primary = primary_target(&lane.targets);

    if let Some(session) = lane.terminal_session() {
        if let Err(e) = activate_wezterm_tab(session, should_raise_wezterm(primary)) {
            warnings.push(e);
        } else {
            logging::perf("focus.wezterm_activated", &format!("elapsed_us={}", t0.elapsed().as_micros()));
            let pane_id = preferred_pane(primary, explicit_pane_id)
                .or_else(|| first_pane_id(session).map(|p| p as u64));
            if let Some(pane_id) = pane_id {
                if let Err(e) = focus_zellij_pane(session, pane_id) {
                    warnings.push(e);
                }
            }
            logging::perf("focus.pane_focused", &format!("elapsed_us={}", t0.elapsed().as_micros()));
        }
    }

    if let Some(target) = primary {
        if !primary_is_terminal(primary) {
            if let Err(e) = activate_and_raise(target, lane.terminal_session()) {
                warnings.push(format!("primary target '{}': {e}", target.driver.name()));
            }
            logging::perf("focus.primary_target_activated", &format!("elapsed_us={}", t0.elapsed().as_micros()));
        }
    }

    warnings
}

pub fn focus_lane(lane_id: &str) -> Result<(), String> {
    let t0 = std::time::Instant::now();
    logging::perf("focus.trigger", &format!("lane={lane_id}"));

    let cfg = config::Config::load();
    let lane = match cfg.lanes.iter().find(|l| l.id == lane_id) {
        Some(l) => l,
        None => return Err(format!("lane not found: {}", lane_id)),
    };
    logging::perf("focus.config_loaded", &format!("lane={lane_id} elapsed_us={}", t0.elapsed().as_micros()));

    // Written and broadcast immediately, before the actual window-moving
    // work below even starts - same reasoning as switch_claude_session's
    // own optimistic write (see its doc comment): the UI's highlight should
    // update as close to the keystroke as possible, not after waiting on
    // apply_targets/land_lane's subprocess calls it doesn't actually depend
    // on. Without the direct socket notify here, an already-running UI only
    // learned the new focused lane via the state.kdl filesystem watcher's
    // own floor latency - this is the ClaudeSessions cycle policy's fast
    // path, previously missing entirely from the Lanes cycle policy.
    state::set_focused_lane(lane_id);
    notify_switch_socket(&format!("switch:{lane_id}|\n"));

    // Targets (Firefox, peek apps, etc.) are applied *before* land_lane -
    // each target's own activation step brings that app frontmost, so
    // doing this first guarantees land_lane's own choice (terminal, or the
    // primary target) gets the true last word, not whichever target
    // happened to be activated last.
    let mut warnings = apply_targets(&lane.targets, &cfg, lane.terminal_session());
    logging::perf("focus.targets_applied", &format!("lane={lane_id} elapsed_us={}", t0.elapsed().as_micros()));
    warnings.extend(land_lane(lane, None, t0));

    logging::perf("focus.complete", &format!("lane={lane_id} elapsed_us={}", t0.elapsed().as_micros()));

    if warnings.is_empty() {
        Ok(())
    } else {
        Err(warnings.join("; "))
    }
}

pub fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{}/{}", home, rest)
    } else {
        path.to_string()
    }
}

/// Boards' own data-root resolution order (`BOARDS_DATA_DIR` env var, then
/// `data_dir` in `~/.config/boards/config.yaml`, then
/// `~/.local/share/bmiller/boards`) - duplicated here deliberately rather
/// than linking against Boards, which is a separate Tauri app with no
/// library to depend on. Boards' own `src-tauri/src/data_dir.rs` is the
/// source of truth for this order; keep it in sync if that ever changes
/// (as it did when the default moved off `~/boards`).
pub fn boards_data_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("BOARDS_DATA_DIR") {
        return std::path::PathBuf::from(expand_tilde(&dir));
    }
    let config_path = std::path::PathBuf::from(expand_tilde("~/.config/boards/config.yaml"));
    if let Ok(content) = std::fs::read_to_string(&config_path) {
        for line in content.lines() {
            if let Some(val) = line.trim().strip_prefix("data_dir:") {
                let val = val.trim().trim_matches('"').trim_matches('\'');
                if !val.is_empty() {
                    return std::path::PathBuf::from(expand_tilde(val));
                }
            }
        }
    }
    std::path::PathBuf::from(expand_tilde("~/.local/share/bmiller/boards"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for the Hammerspoon-hang bug: a child spawned via
    /// `spawn_detached` must not share our stdio, since a long-lived child
    /// holding one of our fds open (e.g. a pipe `hs.task` is waiting on to
    /// see EOF) is exactly what froze Hammerspoon's main thread. Can't
    /// observe this through the child's own stdout (it's deliberately
    /// null), so the child reports what its fd 1 resolves to via a file
    /// instead.
    #[test]
    fn spawn_detached_points_child_stdio_at_dev_null() {
        let out_path = std::env::temp_dir().join(format!(
            "lanes-spawn-detached-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id(),
        ));
        // The result can't go through fd 1 via a plain `>` - that redirect
        // would replace fd 1 with the output file *before* the check runs,
        // so it'd always report "same" against whatever it just became.
        // Open the output file on fd 3 instead, leaving fd 1 (what's
        // actually being inspected) untouched. `-ef` (same device+inode)
        // is the portable way to compare a special file like /dev/null -
        // macOS doesn't expose /dev/fd/N as a plain `readlink`-able symlink.
        let mut child = spawn_detached(std::process::Command::new("/bin/sh").arg("-c").arg(format!(
            "exec 3>{out_path:?}; if [ /dev/fd/1 -ef /dev/null ]; then echo same 1>&3; else echo different 1>&3; fi"
        )))
        .expect("spawn_detached should start /bin/sh");
        child.wait().expect("child should exit");

        let result = std::fs::read_to_string(&out_path).unwrap_or_default();
        let _ = std::fs::remove_file(&out_path);
        assert_eq!(
            result.trim(),
            "same",
            "spawn_detached's child should have fd 1 pointed at /dev/null"
        );
    }

    fn shell_record(state: &str, exit: Option<i32>) -> drivers::shell::CommandRecord {
        serde_json::from_value(serde_json::json!({
            "v": 1, "session": "lanes", "pane": 3,
            "argv0": "cargo", "cmd": "cargo test --workspace",
            "started_at": "2026-09-04T10:00:00Z", "state": state,
            "ended_at": "2026-09-04T10:03:12Z", "exit_code": exit,
        })).unwrap()
    }

    fn rfc3339(s: &str) -> chrono::DateTime<chrono::FixedOffset> {
        chrono::DateTime::parse_from_rfc3339(s).unwrap()
    }

    #[test]
    fn command_signal_maps_state_and_exit_to_reason_and_detail() {
        let now = rfc3339("2026-09-04T10:03:12Z");
        let done = command_signal(&shell_record("done", Some(0)), now).unwrap();
        assert!(matches!(done.reason, model::SignalReason::Command(model::CommandReason::Done)));
        assert_eq!(done.detail.as_deref(), Some("cargo test --workspace · 3m12s"));
        assert_eq!(done.lifecycle, model::Lifecycle::Latched);
        assert_eq!(done.dismiss_id.as_deref(), Some("command:lanes--3--2026-09-04T10:00:00Z"));
        assert!(matches!(done.action, Some(model::SignalAction::FocusPane { pane: 3, .. })));

        let failed = command_signal(&shell_record("failed", Some(1)), now).unwrap();
        assert!(matches!(failed.reason, model::SignalReason::Command(model::CommandReason::Failed)));
        assert_eq!(failed.detail.as_deref(), Some("cargo test --workspace · exit 1 · 3m12s"));
    }

    #[test]
    fn command_signal_running_is_gated_on_shell_notice_secs() {
        let started = rfc3339("2026-09-04T10:00:00Z");
        // 2s in - under the 3s default floor, no chip yet.
        assert!(command_signal(&shell_record("running", None), started + chrono::Duration::seconds(2)).is_none());

        // 3s in - crosses the floor.
        let sig = command_signal(&shell_record("running", None), started + chrono::Duration::seconds(3)).unwrap();
        assert!(matches!(sig.reason, model::SignalReason::Command(model::CommandReason::Running)));
        assert_eq!(sig.detail.as_deref(), Some("cargo test --workspace · 3s"));
        assert_eq!(sig.lifecycle, model::Lifecycle::Live);
    }

    #[test]
    fn signal_visible_keeps_command_and_lanes_chips_but_gates_the_rest_on_cyclable() {
        // Command + Lanes stay visible even off-cycle - a finished build,
        // or "your zellij session died", must not vanish into "no signals".
        assert!(signal_visible(model::SignalKind::Command, false));
        assert!(signal_visible(model::SignalKind::Lanes, false));
        // Everything else: visible iff a cycle would land on it.
        assert!(signal_visible(model::SignalKind::ClaudeSession, true));
        assert!(!signal_visible(model::SignalKind::ClaudeSession, false));
        assert!(!signal_visible(model::SignalKind::Repo, false));
    }

    #[test]
    fn command_signals_are_visible_but_not_cyclable_after_the_correction_pass() {
        // mirrors the per-signal correction correct_signals() runs
        let mut s = command_signal(&shell_record("failed", Some(2)), rfc3339("2026-09-04T10:03:12Z")).unwrap();
        s.cyclable = signal_cyclable(s.kind(), true /* lane cyclable */, false);
        s.visible = signal_visible(s.kind(), s.cyclable);
        assert!(!s.cyclable, "a finished command is never a cycle target");
        assert!(s.visible, "but it still shows in the normal view");
    }

    fn pane(cwd: Option<&str>) -> model::PaneInfo {
        model::PaneInfo { command: None, focused: false, cwd: cwd.map(String::from) }
    }

    fn tab(name: &str, panes: Vec<model::PaneInfo>) -> model::TabInfo {
        model::TabInfo { name: name.to_string(), focused: false, panes }
    }

    #[test]
    fn find_tab_at_path_matches_a_pane_with_that_exact_cwd() {
        let shape = model::TerminalShape {
            cwd: None,
            tabs: vec![
                tab("shell", vec![pane(Some("/Users/bmiller/src/other"))]),
                tab("infra", vec![pane(Some("/Users/bmiller/src/infra")), pane(Some("/Users/bmiller/src/infra"))]),
            ],
        };
        let found = find_tab_at_path(&shape, "/Users/bmiller/src/infra");
        assert_eq!(found.map(|t| t.name.as_str()), Some("infra"));
    }

    #[test]
    fn find_tab_at_path_finds_nothing_when_no_pane_matches() {
        let shape = model::TerminalShape {
            cwd: None,
            tabs: vec![tab("shell", vec![pane(Some("/Users/bmiller/src/other"))])],
        };
        assert!(find_tab_at_path(&shape, "/Users/bmiller/src/infra").is_none());
    }

    #[test]
    fn find_tab_at_path_requires_already_expanded_paths() {
        // Regression: a lane's configured repo path is often "~/src/infra",
        // but pane cwds observed from Zellij are always absolute. Comparing
        // the raw unexpanded form against a real pane cwd must not match -
        // callers are responsible for expand_tilde()-ing first.
        let shape = model::TerminalShape {
            cwd: None,
            tabs: vec![tab("infra", vec![pane(Some("/Users/bmiller/src/infra"))])],
        };
        assert!(find_tab_at_path(&shape, "~/src/infra").is_none());
        assert!(find_tab_at_path(&shape, &expand_tilde("~/src/infra")).is_some());
    }

    #[test]
    fn already_focused_zellij_error_is_benign() {
        assert!(is_benign_zellij_focus_error("Pane Terminal(0) is already focused\n"));
    }

    #[test]
    fn other_zellij_focus_errors_are_not_benign() {
        assert!(!is_benign_zellij_focus_error("No pane with id Terminal(7) found\n"));
        assert!(!is_benign_zellij_focus_error(""));
    }

    #[test]
    fn target_bundle_id_known_for_placeable_drivers() {
        assert_eq!(
            target_bundle_id(&model::TargetDriver::Wezterm { session: Some("x".into()) }),
            Some("com.github.wez.wezterm")
        );
        assert_eq!(
            target_bundle_id(&model::TargetDriver::Obsidian { vault: "x".into() }),
            Some("md.obsidian")
        );
        assert_eq!(
            target_bundle_id(&model::TargetDriver::Boards { project: "x".into() }),
            Some("com.dev-platform.boards")
        );
        assert_eq!(
            target_bundle_id(&model::TargetDriver::Vscode { folder: "x".into() }),
            Some("com.microsoft.VSCode")
        );
    }

    #[test]
    fn obsidian_title_pattern_matches_real_titles_version_suffix_and_all() {
        let re = regex::Regex::new(&obsidian_title_pattern("Formation")).unwrap();
        // Confirmed live via `osascript ... get name of every window`:
        // Obsidian's title always carries a trailing app version.
        assert!(re.is_match("Inbox - Formation - Obsidian 1.14.5"));
        assert!(re.is_match("Formation - Obsidian 1.14.5"));
        // A different vault whose name happens to contain this one as a
        // substring must not match - the whole reason for the `\b` before
        // the vault name (the one after "Obsidian" alone isn't enough to
        // stop this, since "Obsidian" is always a whole word either way).
        assert!(!re.is_match("Daily Note - My Formation - Obsidian 1.14.5"));
        assert!(!re.is_match("Daily Note - Planning - Obsidian 1.14.5"));
    }

    #[test]
    fn obsidian_title_pattern_escapes_regex_metacharacters_in_the_vault_name() {
        let re = regex::Regex::new(&obsidian_title_pattern("Self (Formation)")).unwrap();
        assert!(re.is_match("Note - Self (Formation) - Obsidian 1.14.5"));
    }

    #[test]
    fn target_bundle_id_none_for_activate_only_drivers() {
        // zellij is a multiplexer running inside whatever terminal hosts
        // it, no window of its own - and `app` with no bundle_id given has
        // no fixed identity to place by. Either used with monitor/position
        // set should be rejected upstream (apply_targets), not silently
        // resolved to some bundle id.
        assert_eq!(
            target_bundle_id(&model::TargetDriver::Zellij { session: Some("x".into()), pane: None }),
            None
        );
        assert_eq!(
            target_bundle_id(&model::TargetDriver::App { name: "x".into(), bundle_id: None }),
            None
        );
    }

    #[test]
    fn target_bundle_id_uses_app_bundle_id_when_given() {
        assert_eq!(
            target_bundle_id(&model::TargetDriver::App {
                name: "Firefox".into(),
                bundle_id: Some("org.mozilla.firefox".into()),
            }),
            Some("org.mozilla.firefox")
        );
    }

    /// A throwaway `/tmp` dir for an `appctl` state root in these tests,
    /// cleaned up on drop. Short path, same reasoning as `appctl`'s own
    /// tests: macOS caps a Unix socket path at ~104 bytes.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // `/tmp`, not `std::env::temp_dir()` - on macOS that resolves to a
            // long per-user path under /var/folders/..., which blows the ~104
            // byte cap on a Unix socket path once joined with app/instance.
            let dir = std::path::PathBuf::from(format!("/tmp/lanes-actl-{tag}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A real git repo, since `appctl::start` resolves a dev instance's
    /// name from the source dir's branch (`git_branch`).
    fn appctl_test_repo(branch: &str) -> TempDir {
        let dir = TempDir::new("repo");
        for args in [
            &["init", "-q", "-b", branch][..],
            &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "init"][..],
        ] {
            assert!(std::process::Command::new("git").arg("-C").arg(&dir.0).args(args).status().unwrap().success());
        }
        dir
    }

    /// A live `boards` appctl instance, answering `open_project` like the
    /// real app (`boards/src-tauri/src/open_project.rs`): any project
    /// except "missing" succeeds, "missing" reports `no such project:
    /// missing` the way the real app's lookup failure would.
    fn serve_boards(branch: &str, repo: &TempDir, state: &TempDir) -> appctl::Handle {
        let config = appctl::Config::new("boards", &repo.0, "1.0.0").state_dir(&state.0).command("open_project", |req| {
            let project = req["project"].as_str().unwrap_or_default();
            if project == "missing" {
                Err(format!("no such project: {project}"))
            } else {
                Ok(serde_json::json!(null))
            }
        });
        let _ = branch; // branch comes from the repo fixture itself, kept as a param for test-site clarity
        appctl::start(config).unwrap()
    }

    fn boards_args(project: &str) -> serde_json::Map<String, serde_json::Value> {
        let mut args = serde_json::Map::new();
        args.insert("project".to_string(), serde_json::json!(project));
        args
    }

    #[test]
    fn appctl_switches_a_live_instance_and_returns_its_pid() {
        let (repo, state) = (appctl_test_repo("main"), TempDir::new("st"));
        let handle = serve_boards("main", &repo, &state);
        match activate_via_appctl(&state.0, "boards", vec![("open_project", boards_args("myproj"))], None) {
            AppctlOutcome::Activated(pid) => assert_eq!(pid, handle.info().pid),
            other => panic!("expected Activated, got {other:?}"),
        }
    }

    #[test]
    fn appctl_app_error_is_not_no_instance() {
        let (repo, state) = (appctl_test_repo("main"), TempDir::new("st"));
        let _handle = serve_boards("main", &repo, &state);
        match activate_via_appctl(&state.0, "boards", vec![("open_project", boards_args("missing"))], None) {
            AppctlOutcome::AppError(msg) => assert_eq!(msg, "no such project: missing"),
            other => panic!("expected AppError, got {other:?}"),
        }
    }

    #[test]
    fn appctl_reports_no_instance_when_nothing_is_running() {
        let state = TempDir::new("st");
        assert!(matches!(
            activate_via_appctl(&state.0, "boards", vec![("open_project", boards_args("myproj"))], None),
            AppctlOutcome::NoInstance
        ));
    }

    #[test]
    fn appctl_is_generic_over_the_app_name_and_command() {
        // Same mechanism, a different app/command entirely - not Boards-
        // specific at all, which is the whole point of factoring this out.
        let (repo, state) = (appctl_test_repo("main"), TempDir::new("st"));
        let config = appctl::Config::new("console", &repo.0, "1.0.0")
            .state_dir(&state.0)
            .command("show", |_| Ok(serde_json::json!(null)));
        let handle = appctl::start(config).unwrap();
        match activate_via_appctl(&state.0, "console", vec![("show", serde_json::Map::new())], None) {
            AppctlOutcome::Activated(pid) => assert_eq!(pid, handle.info().pid),
            other => panic!("expected Activated, got {other:?}"),
        }
    }

    #[test]
    fn an_appctl_target_sends_its_command_and_raises_separately() {
        let names = |cmd: &str, raise: bool| appctl_requests(cmd, serde_json::Map::new(), raise).into_iter().map(|(n, _)| n.to_string()).collect::<Vec<_>>();
        assert_eq!(names("navigate", false), ["navigate"], "following the lane, not raising");
        assert_eq!(names("navigate", true), ["navigate", "show"], "the view first, then the raise");
        assert_eq!(names("show", true), ["show"], "show as the command is the raise, once");
        assert!(names("show", false).is_empty(), "and never without raise");

        // Both reach the app, in that order.
        let (repo, state) = (appctl_test_repo("main"), TempDir::new("st"));
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let (a, b) = (seen.clone(), seen.clone());
        let config = appctl::Config::new("console", &repo.0, "1.0.0")
            .state_dir(&state.0)
            .command("navigate", move |req| {
                a.lock().unwrap().push(format!("navigate {}", req["lane"].as_str().unwrap_or_default()));
                Ok(serde_json::json!(null))
            })
            .command("show", move |_| {
                b.lock().unwrap().push("show".into());
                Ok(serde_json::json!(null))
            });
        let _handle = appctl::start(config).unwrap();
        let mut args = serde_json::Map::new();
        args.insert("lane".into(), serde_json::json!("dev-infra"));
        assert!(matches!(activate_via_appctl(&state.0, "console", appctl_requests("navigate", args, true), None), AppctlOutcome::Activated(_)));
        assert_eq!(*seen.lock().unwrap(), ["navigate dev-infra", "show"]);
    }

    #[test]
    fn activate_appctl_target_turns_no_instance_into_a_plain_error() {
        let state = TempDir::new("st");
        // No appctl::default_state_root() override in this test process,
        // so there's nothing running under whatever root it resolves to
        // either - either way, no instance means a plain, named failure,
        // never a silent success.
        let _ = &state;
        let err = activate_appctl_target("definitely-not-running", "show", serde_json::Map::new(), true, None).unwrap_err();
        assert_eq!(err, "definitely-not-running isn't running");
        // Only sending a command (no raise): a closed app is skipped, not a failure.
        let quiet = activate_appctl_target("definitely-not-running", "navigate", serde_json::Map::new(), false, None);
        assert_eq!(quiet, Ok(None));
    }

    #[test]
    fn rewrite_boards_open_error_replaces_the_cryptic_launch_services_message() {
        let raw = "open boards:// URI failed: No application knows how to open URL boards://open?project=Pikabu \
                    (Error Domain=NSOSStatusErrorDomain Code=-10814 \"kLSApplicationNotFoundErr\")"
            .to_string();
        let rewritten = rewrite_boards_open_error(raw);
        assert_eq!(rewritten, "Boards isn't running");
    }

    #[test]
    fn rewrite_boards_open_error_passes_through_an_unrelated_failure() {
        let raw = "open boards:// URI failed: something else entirely".to_string();
        assert_eq!(rewrite_boards_open_error(raw.clone()), raw);
    }

    #[test]
    fn prefix_target_error_skips_a_message_that_already_names_the_driver() {
        assert_eq!(prefix_target_error("boards", "Boards isn't running".to_string()), "Boards isn't running");
    }

    #[test]
    fn prefix_target_error_adds_the_prefix_otherwise() {
        assert_eq!(prefix_target_error("wezterm", "no socket".to_string()), "target 'wezterm': no socket");
    }

    #[test]
    fn appctl_pre_placement_failure_does_not_block_the_activation() {
        // No real Boards window exists in a test process, so the pre-place
        // call to lanes-wm is guaranteed to fail (or lanes-wm may not even
        // be on PATH) - exactly the "best-effort" case activate_via_appctl's
        // doc comment describes: a failed pre-placement must never stop the
        // actual command from running.
        let (repo, state) = (appctl_test_repo("main"), TempDir::new("st"));
        let handle = serve_boards("main", &repo, &state);
        let pre_place = Some(("not-a-real-monitor-uuid".to_string(), serde_json::json!("full")));
        match activate_via_appctl(&state.0, "boards", vec![("open_project", boards_args("myproj"))], pre_place) {
            AppctlOutcome::Activated(pid) => assert_eq!(pid, handle.info().pid),
            other => panic!("expected Activated despite a failed pre-placement, got {other:?}"),
        }
    }

    #[test]
    fn appctl_reports_no_instance_when_ambiguous() {
        let (repo_a, repo_b, state) = (appctl_test_repo("dev-a"), appctl_test_repo("dev-b"), TempDir::new("st"));
        let _a = serve_boards("dev-a", &repo_a, &state);
        let _b = serve_boards("dev-b", &repo_b, &state);
        assert!(matches!(
            activate_via_appctl(&state.0, "boards", vec![("open_project", boards_args("myproj"))], None),
            AppctlOutcome::NoInstance
        ));
    }

    #[test]
    fn cycle_index_advances_and_wraps_forward() {
        assert_eq!(cycle_index(4, Some(0), 1), 1);
        assert_eq!(cycle_index(4, Some(3), 1), 0);
    }

    #[test]
    fn cycle_index_retreats_and_wraps_backward() {
        assert_eq!(cycle_index(4, Some(1), -1), 0);
        assert_eq!(cycle_index(4, Some(0), -1), 3);
    }

    #[test]
    fn cycle_index_starts_at_first_when_no_cursor_and_moving_forward() {
        assert_eq!(cycle_index(4, None, 1), 0);
    }

    #[test]
    fn cycle_index_no_cursor_moving_backward_matches_prior_tool_behavior() {
        // Not index n-1 ("last") - the no-cursor sentinel (-1) combined with
        // direction -1 lands on n-2 under this modular arithmetic. Pinned
        // here to match the original Go tool's exact behavior rather than
        // any "more correct" semantics, so muscle memory carries over.
        assert_eq!(cycle_index(4, None, -1), 2);
    }

    #[test]
    fn cycle_index_single_session_always_stays_put() {
        assert_eq!(cycle_index(1, Some(0), 1), 0);
        assert_eq!(cycle_index(1, Some(0), -1), 0);
    }

    fn solo_target(monitor: &str, position_solo: Option<&str>) -> model::Target {
        model::Target {
            driver: model::TargetDriver::Obsidian { vault: "x".into() },
            monitor: Some(monitor.to_string()),
            position: Some(toml::Value::String("full".to_string())),
            position_solo: position_solo.map(|p| toml::Value::String(p.to_string())),
            launch: false,
            raise: false,
            minimized: None,
            primary: false,
        }
    }

    #[test]
    fn effective_destination_uses_normal_monitor_and_position_when_not_solo() {
        let target = solo_target("lg-right", Some("left-half"));
        assert_eq!(
            effective_destination(&target, false),
            Some(("lg-right".to_string(), Some(toml::Value::String("full".to_string()))))
        );
    }

    #[test]
    fn effective_destination_switches_to_main_and_position_solo_when_solo() {
        let target = solo_target("lg-right", Some("left-half"));
        assert_eq!(
            effective_destination(&target, true),
            Some(("main".to_string(), Some(toml::Value::String("left-half".to_string()))))
        );
    }

    #[test]
    fn effective_destination_ignores_solo_when_target_has_no_position_solo() {
        // main always resolves, so a target with no position_solo (e.g.
        // one that's never adapted for this) keeps its normal placement
        // even when solo - this is the "no change from before this field
        // existed" case Target::position_solo's doc comment describes.
        let target = solo_target("main", None);
        assert_eq!(
            effective_destination(&target, true),
            Some(("main".to_string(), Some(toml::Value::String("full".to_string()))))
        );
    }

    #[test]
    fn effective_destination_none_for_an_activate_only_target_regardless_of_solo() {
        let mut target = solo_target("lg-right", Some("left-half"));
        target.monitor = None;
        target.position_solo = None;
        assert_eq!(effective_destination(&target, false), None);
        assert_eq!(effective_destination(&target, true), None);
    }

    fn test_config(lanes: Vec<model::Lane>) -> config::Config {
        config::Config {
            drivers: None,
            monitors: std::collections::HashMap::new(),
            order: None,
            indented: Vec::new(),
            cycle_policy: model::CyclePolicy::default(),
            contexts: Vec::new(),
            lanes,
            boards_targets: Vec::new(),
            context_ids: Vec::new(),
            shell_ignored_commands: Vec::new(),
        }
    }

    fn lane_with_target(id: &str, target: model::Target) -> model::Lane {
        model::Lane {
            id: id.to_string(),
            name: id.to_string(),
            active: true,
            scope: vec![],
            targets: vec![target],
            layout_template: None,
        }
    }

    fn obsidian_target(vault: &str) -> model::Target {
        model::Target {
            driver: model::TargetDriver::Obsidian { vault: vault.to_string() },
            monitor: None,
            position: None,
            position_solo: None,
            launch: false,
            raise: false,
            minimized: None,
            primary: false,
        }
    }

    #[test]
    fn context_target_none_when_no_lane_is_focused() {
        let cfg = test_config(vec![lane_with_target("infra", obsidian_target("Dev"))]);
        assert!(context_target(&cfg, None, "obsidian").is_none());
    }

    #[test]
    fn context_target_none_when_focused_lane_has_no_matching_driver() {
        let cfg = test_config(vec![lane_with_target("infra", obsidian_target("Dev"))]);
        assert!(context_target(&cfg, Some("infra"), "boards").is_none());
    }

    #[test]
    fn context_target_finds_the_focused_lanes_target() {
        let cfg = test_config(vec![
            lane_with_target("infra", obsidian_target("Dev")),
            lane_with_target("job-hunting", obsidian_target("Planning")),
        ]);
        let target = context_target(&cfg, Some("job-hunting"), "obsidian").unwrap();
        assert!(matches!(&target.driver, model::TargetDriver::Obsidian { vault } if vault == "Planning"));
    }

    #[test]
    fn context_target_none_when_focused_lane_id_is_not_in_config() {
        let cfg = test_config(vec![lane_with_target("infra", obsidian_target("Dev"))]);
        assert!(context_target(&cfg, Some("nonexistent"), "obsidian").is_none());
    }

    fn zellij_target(pane: Option<u32>, primary: bool) -> model::Target {
        model::Target {
            driver: model::TargetDriver::Zellij { session: None, pane },
            monitor: None,
            position: None,
            position_solo: None,
            launch: false,
            raise: false,
            minimized: None,
            primary,
        }
    }

    fn primary_obsidian_target(vault: &str) -> model::Target {
        let mut t = obsidian_target(vault);
        t.primary = true;
        t
    }

    #[test]
    fn primary_target_none_when_no_target_is_marked_primary() {
        assert!(primary_target(&[obsidian_target("Dev")]).is_none());
    }

    #[test]
    fn primary_target_finds_the_marked_one() {
        let targets = vec![obsidian_target("Dev"), primary_obsidian_target("Planning")];
        let found = primary_target(&targets).unwrap();
        assert!(matches!(&found.driver, model::TargetDriver::Obsidian { vault } if vault == "Planning"));
    }

    #[test]
    fn primary_is_terminal_true_for_zellij_and_wezterm() {
        assert!(primary_is_terminal(Some(&zellij_target(None, true))));
        let wezterm = model::Target {
            driver: model::TargetDriver::Wezterm { session: None },
            monitor: None,
            position: None,
            position_solo: None,
            launch: false,
            raise: false,
            minimized: None,
            primary: true,
        };
        assert!(primary_is_terminal(Some(&wezterm)));
    }

    #[test]
    fn primary_is_terminal_false_for_non_terminal_driver_or_no_primary() {
        assert!(!primary_is_terminal(Some(&primary_obsidian_target("Dev"))));
        assert!(!primary_is_terminal(None));
    }

    #[test]
    fn should_raise_wezterm_when_nothing_is_primary() {
        assert!(should_raise_wezterm(None));
    }

    #[test]
    fn should_raise_wezterm_when_the_terminal_itself_is_primary() {
        assert!(should_raise_wezterm(Some(&zellij_target(None, true))));
    }

    #[test]
    fn should_not_raise_wezterm_when_something_else_is_primary() {
        // The whole point: don't flash WezTerm forward right before the
        // primary target gets raised over it.
        assert!(!should_raise_wezterm(Some(&primary_obsidian_target("Dev"))));
    }

    #[test]
    fn preferred_pane_explicit_override_wins_over_primary() {
        let primary = zellij_target(Some(7), true);
        assert_eq!(preferred_pane(Some(&primary), Some(3)), Some(3));
    }

    #[test]
    fn preferred_pane_falls_back_to_primarys_own_pane() {
        let primary = zellij_target(Some(7), true);
        assert_eq!(preferred_pane(Some(&primary), None), Some(7));
    }

    #[test]
    fn preferred_pane_none_when_primary_is_not_zellij_or_unset() {
        assert_eq!(preferred_pane(Some(&primary_obsidian_target("Dev")), None), None);
        assert_eq!(preferred_pane(None, None), None);
    }

    #[test]
    fn inactive_lane_is_never_reachable_regardless_of_cached_tab_id() {
        assert!(!reachable_lane_decision(false, true, false));
        assert!(!reachable_lane_decision(false, true, true));
    }

    #[test]
    fn active_lane_with_cached_tab_id_is_reachable() {
        assert!(reachable_lane_decision(true, true, true));
    }

    #[test]
    fn active_lane_with_no_cached_tab_id_is_not_reachable() {
        // This is the actual spinner/sheetwork-sandbox case: the lane's own
        // active flag says yes, but nothing has ever pushed (or something
        // cleared) a cached WezTerm tab-id for it - exactly what caused the
        // "wezterm activate-tab failed" cycling errors.
        assert!(!reachable_lane_decision(true, true, false));
    }

    #[test]
    fn active_lane_with_no_terminal_at_all_is_reachable_regardless_of_tab_id() {
        // job-hunting's actual case: no tab.toml yet at all, so there's no
        // terminal to have a missing/stale tab-id in the first place -
        // cycling shouldn't skip it just because it hasn't been given a
        // terminal yet.
        assert!(reachable_lane_decision(true, false, false));
    }

    #[test]
    fn inherited_wezterm_target_with_no_default_session_has_no_resolvable_session() {
        // job-hunting's actual case: it inherits default.toml's wezterm
        // target (session omitted, meant to default to whichever lane owns
        // it) but has no terminal session of its own to default to.
        let driver = model::TargetDriver::Wezterm { session: None };
        assert!(terminal_target_has_no_resolvable_session(&driver, None));
    }

    #[test]
    fn wezterm_target_resolves_via_its_own_session_even_with_no_default() {
        let driver = model::TargetDriver::Wezterm { session: Some("explicit".to_string()) };
        assert!(!terminal_target_has_no_resolvable_session(&driver, None));
    }

    #[test]
    fn wezterm_target_resolves_via_the_lanes_default_session() {
        let driver = model::TargetDriver::Wezterm { session: None };
        assert!(!terminal_target_has_no_resolvable_session(&driver, Some("lane-session")));
    }

    #[test]
    fn zellij_target_with_no_session_and_no_default_has_no_resolvable_session() {
        let driver = model::TargetDriver::Zellij { session: None, pane: None };
        assert!(terminal_target_has_no_resolvable_session(&driver, None));
    }

    #[test]
    fn non_terminal_driver_never_has_an_unresolvable_session() {
        // Drivers without a session concept at all (obsidian, app, ...)
        // always return false here - this check only ever applies to
        // wezterm/zellij.
        let driver = model::TargetDriver::App { name: "Firefox".to_string(), bundle_id: None };
        assert!(!terminal_target_has_no_resolvable_session(&driver, None));
    }

    fn ordered_lane(id: &str, session: &str) -> model::Lane {
        model::Lane {
            id: id.to_string(),
            name: id.to_string(),
            active: true,
            scope: vec![crate::scope::ScopeElement::zellij_session(session)],
            targets: vec![],
            layout_template: None,
        }
    }

    #[test]
    fn lane_order_rank_follows_configured_lane_order() {
        // cfg.lanes is already sorted by lanes.toml's `order` by the time
        // Config::load hands it out, so rank is just position in that list.
        let cfg = test_config(vec![
            ordered_lane("sheetwork-planner", "sheetwork-planner"),
            ordered_lane("infra", "infra"),
            ordered_lane("lanes-dev", "lanes"),
        ]);
        assert_eq!(lane_order_rank("sheetwork-planner", &cfg), 0);
        assert_eq!(lane_order_rank("infra", &cfg), 1);
        assert_eq!(lane_order_rank("lanes", &cfg), 2);
    }

    #[test]
    fn lane_order_rank_sorts_unmatched_sessions_after_every_real_lane() {
        let cfg = test_config(vec![ordered_lane("infra", "infra")]);
        assert_eq!(lane_order_rank("some-unrelated-session", &cfg), 1);
    }

    #[test]
    fn pane_position_rank_looks_up_by_pane_id() {
        // Mirrors the real formation case: two Claude panes share a cwd, in
        // tab 0 (pane id 0) and tab 1 (pane id 3) respectively.
        let positions: HashMap<u32, (usize, i64, i64)> =
            [(0u32, (0usize, 1i64, 0i64)), (3u32, (1usize, 1i64, 0i64))].into_iter().collect();
        assert_eq!(pane_position_rank(Some(&positions), Some(0)), (0, 1, 0));
        assert_eq!(pane_position_rank(Some(&positions), Some(3)), (1, 1, 0));
    }

    #[test]
    fn pane_position_rank_falls_back_to_max_when_unresolved() {
        let positions: HashMap<u32, (usize, i64, i64)> = [(0u32, (0usize, 1i64, 0i64))].into_iter().collect();
        assert_eq!(pane_position_rank(Some(&positions), Some(99)), (usize::MAX, i64::MAX, i64::MAX));
        assert_eq!(pane_position_rank(Some(&positions), None), (usize::MAX, i64::MAX, i64::MAX));
        assert_eq!(pane_position_rank(None, Some(0)), (usize::MAX, i64::MAX, i64::MAX));
    }

    #[test]
    fn sessions_needing_pane_positions_skips_sessions_with_only_one_live_session() {
        // The regression this guards: querying list-panes for a session that
        // hosts only one live Claude session resolves nothing (there's
        // nothing to disambiguate), but used to run unconditionally -
        // sequentially, once per distinct live Zellij session - adding a
        // whole extra ~150ms `list-panes` call to every cycle keypress for
        // every ordinary single-session lane.
        let sessions = vec![Some("infra"), Some("lanes-dev"), Some("sheetwork")];
        assert!(sessions_needing_pane_positions(sessions.into_iter()).is_empty());
    }

    #[test]
    fn sessions_needing_pane_positions_includes_only_sessions_with_two_or_more() {
        let sessions = vec![Some("formation"), Some("formation"), Some("infra"), None];
        assert_eq!(sessions_needing_pane_positions(sessions.into_iter()), vec!["formation".to_string()]);
    }

    #[test]
    fn session_with_no_matching_lane_is_always_included_in_cycling() {
        // No I/O involved here - cfg.lane_for_session returns None before
        // session_belongs_to_reachable_lane ever reaches for state.kdl.
        let cfg = test_config(vec![model::Lane {
            id: "infra".to_string(),
            name: "Infra".to_string(),
            active: false,
            scope: vec![scope::ScopeElement::zellij_session("infra")],
            targets: vec![],
            layout_template: None,
        }]);
        assert!(session_belongs_to_reachable_lane(Some("some-other-session"), &cfg));
        assert!(session_belongs_to_reachable_lane(None, &cfg));
    }

    #[test]
    fn active_reachable_lane_with_a_claude_session_is_cyclable() {
        assert!(lane_cyclable(true, Some(true), true));
    }

    #[test]
    fn reachable_lane_with_only_a_repo_signal_is_not_cyclable() {
        // Nothing for a cycle to land on - matches
        // session_belongs_to_reachable_lane() only ever filtering live
        // Claude sessions in the first place.
        assert!(!lane_cyclable(true, Some(true), false));
    }

    #[test]
    fn inactive_lane_is_never_cyclable_even_with_a_claude_session() {
        assert!(!lane_cyclable(false, Some(true), true));
    }

    #[test]
    fn unreachable_lane_is_never_cyclable_even_with_a_claude_session() {
        assert!(!lane_cyclable(true, Some(false), true));
    }

    #[test]
    fn lane_with_unknown_reachability_is_not_cyclable() {
        assert!(!lane_cyclable(true, None, true));
    }

    #[test]
    fn claude_session_signal_in_a_cyclable_lane_is_cyclable() {
        assert!(signal_cyclable(model::SignalKind::ClaudeSession, true, false));
    }

    #[test]
    fn claude_session_signal_in_a_non_cyclable_lane_is_not_cyclable() {
        assert!(!signal_cyclable(model::SignalKind::ClaudeSession, false, false));
    }

    #[test]
    fn individually_disabled_claude_session_is_not_cyclable_even_in_a_cyclable_lane() {
        assert!(!signal_cyclable(model::SignalKind::ClaudeSession, true, true));
    }

    #[test]
    fn repo_and_lanes_signals_are_never_cyclable_even_in_a_cyclable_lane() {
        // cycle_claude_session only ever collects live Claude sessions -
        // a pending-commit or session-missing signal is never something a
        // cycle would land on, regardless of the lane's own cyclable fact.
        assert!(!signal_cyclable(model::SignalKind::Repo, true, false));
        assert!(!signal_cyclable(model::SignalKind::Lanes, true, false));
    }

    #[test]
    fn awaiting_upgrades_to_ready_when_cyclable() {
        use model::{ClaudeSessionReason, SignalReason};
        assert!(matches!(
            upgrade_awaiting_to_ready(SignalReason::ClaudeSession(ClaudeSessionReason::Awaiting), true),
            SignalReason::ClaudeSession(ClaudeSessionReason::Ready)
        ));
    }

    #[test]
    fn awaiting_stays_awaiting_when_not_cyclable() {
        use model::{ClaudeSessionReason, SignalReason};
        assert!(matches!(
            upgrade_awaiting_to_ready(SignalReason::ClaudeSession(ClaudeSessionReason::Awaiting), false),
            SignalReason::ClaudeSession(ClaudeSessionReason::Awaiting)
        ));
    }

    #[test]
    fn non_awaiting_reasons_are_never_upgraded_regardless_of_cyclable() {
        use model::{ClaudeSessionReason, LanesReason, RepoReason, SignalReason};
        let untouched = [
            SignalReason::ClaudeSession(ClaudeSessionReason::Active),
            SignalReason::ClaudeSession(ClaudeSessionReason::Permission),
            SignalReason::Repo(RepoReason::PendingCommit),
            SignalReason::Lanes(LanesReason::SessionMissing),
            SignalReason::Lanes(LanesReason::SessionNotRunning),
        ];
        for reason in untouched {
            let upgraded = upgrade_awaiting_to_ready(reason.clone(), true);
            assert_eq!(format!("{upgraded:?}"), format!("{reason:?}"));
        }
    }

    fn proc(comm: &str, age_secs: u64) -> Option<ProcInfo> {
        Some(ProcInfo { comm: comm.to_string(), age_secs })
    }

    #[test]
    fn zellij_backed_session_live_iff_session_running() {
        let live: HashSet<String> = ["lanes".to_string()].into_iter().collect();
        assert!(session_is_live_with("lanes", &live, None, None, |_| unreachable!("should not need pid lookup")));
        assert!(!session_is_live_with("job-hunting", &live, None, None, |_| unreachable!("should not need pid lookup")));
    }

    #[test]
    fn zellij_backed_session_dead_if_session_itself_is_gone_regardless_of_pid() {
        let live: HashSet<String> = HashSet::new();
        assert!(!session_is_live_with("lanes", &live, Some(123), None, |_| proc("claude", 10)));
    }

    #[test]
    fn zellij_backed_session_dead_if_pid_no_longer_a_claude_process() {
        let live: HashSet<String> = ["infra".to_string()].into_iter().collect();
        assert!(!session_is_live_with("infra", &live, Some(8547), None, |_| proc("fish", 10)));
        assert!(!session_is_live_with("infra", &live, Some(4393), None, |_| None));
    }

    #[test]
    fn zellij_backed_session_live_if_pid_still_a_claude_process() {
        let live: HashSet<String> = ["infra".to_string()].into_iter().collect();
        assert!(session_is_live_with("infra", &live, Some(89568), None, |_| proc("claude", 10)));
    }

    #[test]
    fn zellij_backed_session_dead_if_pid_recycled_to_a_newer_claude() {
        // The real post-sleep bug: the registry file was last written 2h ago
        // (7200s), but the claude at the recorded pid only started 90s ago -
        // it's a different session that happened to get this pid.
        let live: HashSet<String> = ["japanese".to_string()].into_iter().collect();
        assert!(!session_is_live_with("japanese", &live, Some(33544), Some(7200), |_| proc("claude", 90)));
        // Real live (idle) session: claude wrote the file at startup and
        // hasn't since, so the process is at least as old as the file.
        assert!(session_is_live_with("japanese", &live, Some(33544), Some(7200), |_| proc("claude", 7210)));
        // A resume: file rewritten seconds ago, process seconds old, within
        // slack - still live.
        assert!(session_is_live_with("japanese", &live, Some(33544), Some(3), |_| proc("claude", 1)));
    }

    #[test]
    fn pid_reuse_check_is_skipped_when_file_age_is_unknown() {
        let live: HashSet<String> = ["japanese".to_string()].into_iter().collect();
        assert!(session_is_live_with("japanese", &live, Some(1), None, |_| proc("claude", 5)));
    }

    #[test]
    fn paneless_session_live_only_if_pid_is_a_claude_process() {
        let live: HashSet<String> = HashSet::new();
        assert!(session_is_live_with("", &live, Some(123), None, |_| proc("claude", 10)));
        assert!(session_is_live_with("", &live, Some(123), None, |_| proc("/opt/homebrew/bin/claude", 10)));
    }

    #[test]
    fn paneless_session_dead_if_pid_reused_by_other_process() {
        let live: HashSet<String> = HashSet::new();
        assert!(!session_is_live_with("", &live, Some(123), None, |_| proc("Slack", 10)));
    }

    #[test]
    fn paneless_session_dead_if_pid_no_longer_exists() {
        let live: HashSet<String> = HashSet::new();
        assert!(!session_is_live_with("", &live, Some(123), None, |_| None));
    }

    #[test]
    fn paneless_session_dead_if_no_pid_recorded() {
        let live: HashSet<String> = HashSet::new();
        assert!(!session_is_live_with("", &live, None, None, |_| unreachable!("no pid to look up")));
    }

    #[test]
    fn parse_etime_handles_every_ps_shape() {
        assert_eq!(parse_etime("05:32"), Some(332));
        assert_eq!(parse_etime("01:05:32"), Some(3932));
        assert_eq!(parse_etime("3-01:05:32"), Some(263_132));
        assert_eq!(parse_etime("00:00"), Some(0));
        assert_eq!(parse_etime("garbage"), None);
    }

    #[test]
    fn parse_ps_line_splits_etime_from_command() {
        let c = parse_ps_line("      15:57:57 claude").unwrap();
        assert_eq!(c.age_secs, 57477);
        assert_eq!(c.comm, "claude");
        // command can contain spaces
        let bg = parse_ps_line("02:00 claude bg-spare").unwrap();
        assert_eq!(bg.age_secs, 120);
        assert_eq!(bg.comm, "claude bg-spare");
        assert!(parse_ps_line("").is_none());
    }

    fn wezterm_test_dir(label: &str) -> std::path::PathBuf {
        // Unix socket paths are capped at ~104 bytes (SUN_LEN) - keep this
        // short rather than the usual long descriptive temp-dir name.
        let dir = std::path::PathBuf::from("/tmp").join(format!("lwtsock-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn wezterm_socket_skips_a_dead_socket_file_even_with_the_newest_mtime() {
        let dir = wezterm_test_dir("dead-newest");

        // The live one, created first (older mtime).
        let live_path = dir.join("gui-sock-111");
        let listener = std::os::unix::net::UnixListener::bind(&live_path).unwrap();

        // A dead one: a plain file at the right name, nothing listening -
        // created after, so it has the newer mtime and would win under the
        // old mtime-only rule.
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(dir.join("gui-sock-222"), b"").unwrap();

        let found = wezterm_socket_in(&dir);
        assert_eq!(found, Some(live_path.to_string_lossy().into_owned()));

        drop(listener);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn wezterm_socket_is_none_when_every_candidate_is_dead() {
        let dir = wezterm_test_dir("all-dead");
        std::fs::write(dir.join("gui-sock-333"), b"").unwrap();
        std::fs::write(dir.join("gui-sock-444"), b"").unwrap();

        assert_eq!(wezterm_socket_in(&dir), None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn wezterm_socket_is_none_when_the_directory_does_not_exist() {
        let dir = std::env::temp_dir().join("lanes-wezterm-sock-test-does-not-exist");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(wezterm_socket_in(&dir), None);
    }

    #[test]
    fn is_claude_command_matches_bare_and_full_path() {
        assert!(is_claude_command("claude"));
        assert!(is_claude_command("/opt/homebrew/bin/claude"));
        assert!(!is_claude_command("claude-code-helper"));
        assert!(!is_claude_command("bash"));
        assert!(!is_claude_command(""));
    }

    #[test]
    fn glob_match_literal_with_no_wildcards_requires_exact_equality() {
        assert!(glob_match("ssh", "ssh"));
        assert!(!glob_match("ssh", "ssh prod-box"));
        assert!(!glob_match("ssh", "sshd"));
    }

    #[test]
    fn glob_match_star_matches_any_run_including_empty() {
        assert!(glob_match("ssh *", "ssh prod-box"));
        assert!(glob_match("ssh*", "ssh"));
        assert!(glob_match("claude*", "claude --resume boards"));
        assert!(!glob_match("ssh *", "ssh"));
    }

    #[test]
    fn glob_match_star_in_the_middle_matches_across_the_gap() {
        assert!(glob_match("tail -f *", "tail -f /var/log/app.log"));
        assert!(!glob_match("tail -f *", "tail /var/log/app.log"));
    }

    #[test]
    fn glob_match_question_mark_matches_exactly_one_char() {
        assert!(glob_match("l?", "ls"));
        assert!(!glob_match("l?", "ls -la"));
        assert!(!glob_match("l?", "l"));
    }

    #[test]
    fn is_shell_signal_ignored_is_entirely_config_driven_claude_included() {
        // Nothing is excluded by name in code - with an empty list, even
        // claude itself shows a chip. Excluding it is lanes.toml's own
        // `ignored_shell_commands` entry (see config.rs), not a hardcoded
        // special case here.
        assert!(!is_shell_signal_ignored(Some("claude --resume boards"), Some("claude"), &[]));
        let ignored = vec!["claude*".to_string()];
        assert!(is_shell_signal_ignored(Some("claude --resume boards"), Some("claude"), &ignored));
    }

    #[test]
    fn is_shell_signal_ignored_matches_a_configured_glob_against_the_full_command() {
        let ignored = vec!["ssh *".to_string()];
        assert!(is_shell_signal_ignored(Some("ssh prod-box"), Some("ssh"), &ignored));
        assert!(!is_shell_signal_ignored(Some("cargo build"), Some("cargo"), &ignored));
    }

    #[test]
    fn is_shell_signal_ignored_falls_back_to_argv0_when_cmd_is_absent() {
        let ignored = vec!["ssh".to_string()];
        assert!(is_shell_signal_ignored(None, Some("ssh"), &ignored));
    }

    #[test]
    fn is_shell_signal_ignored_false_when_nothing_matches_and_no_command_name_at_all() {
        assert!(!is_shell_signal_ignored(None, None, &["ssh".to_string()]));
    }

    #[test]
    fn percent_encode_leaves_unreserved_chars_alone() {
        assert_eq!(percent_encode("Formation-2026_v1.0~x"), "Formation-2026_v1.0~x");
    }

    #[test]
    fn percent_encode_escapes_spaces_and_other_bytes() {
        assert_eq!(percent_encode("My Vault"), "My%20Vault");
        assert_eq!(percent_encode("a&b"), "a%26b");
    }

}
