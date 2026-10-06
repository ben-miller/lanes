pub mod config;
pub mod drivers;
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

pub fn gather_lanes(cfg: &config::Config) -> model::LanewiseSnapshot {
    let t0 = std::time::Instant::now();
    logging::perf("gather_lanes.start", "");

    let running = drivers::zellij::running_sessions();
    let claude = claude_sessions_by_zellij();

    // Opt-in and fully isolated: not in the default `drivers` list, and a
    // panic in the driver degrades to "no command chips this refresh"
    // rather than a broken snapshot. Reading files only - no subprocesses -
    // so this adds negligible cost even when enabled. See
    // PLAN-shell-signal.md.
    let shell_records: Vec<drivers::shell::CommandRecord> = if cfg.driver_enabled("shell") {
        std::panic::catch_unwind(drivers::shell::enumerate).unwrap_or_else(|_| {
            logging::append_line("switch-ui.log", "error", "shell driver panicked; skipping this refresh");
            Vec::new()
        })
    } else {
        Vec::new()
    };

    let running_sessions: Vec<&str> = cfg.lanes.iter()
        .flat_map(|lane| lane.scope.iter())
        .filter_map(|el| el.zellij_session_name().filter(|s| running.contains(*s)))
        .collect();

    let repo_paths: Vec<String> = cfg.lanes.iter()
        .flat_map(|lane| lane.scope.iter())
        .filter_map(|el| el.repo_path().map(expand_tilde))
        .collect();

    // One list-panes call per session (giving both the pane shape for
    // display and each pane's on-screen position - see
    // shape_and_positions_for_session) plus one git-status call per repo,
    // each a separate subprocess round-trip independent of every other one
    // - fetch them all concurrently in one batch rather than once per lane
    // in sequence (this used to be the dominant cost of every UI refresh,
    // worse still when this used to be two separate per-session calls -
    // dump-layout and list-panes - see perf.log's
    // gather_lanes.subprocess_batch_done and the "Diagnostics" section of
    // the README).
    let (layouts, pane_positions, git_status): (
        HashMap<String, model::TerminalShape>,
        HashMap<String, HashMap<u32, (usize, i64, i64)>>,
        HashMap<String, RepoStatus>,
    ) = std::thread::scope(|scope| {
        let pane_handles: Vec<_> = running_sessions.into_iter()
            .map(|s| (s.to_string(), scope.spawn(move || drivers::zellij::shape_and_positions_for_session(s))))
            .collect();
        let git_handles: Vec<_> = repo_paths.into_iter()
            .map(|p| (p.clone(), scope.spawn(move || git_repo_status(&p))))
            .collect();

        let mut layouts = HashMap::new();
        let mut pane_positions = HashMap::new();
        for (s, handle) in pane_handles {
            let (shape, positions) = handle.join().unwrap_or_default();
            layouts.insert(s.clone(), shape);
            pane_positions.insert(s, positions);
        }
        let git_status = git_handles.into_iter()
            .map(|(p, handle)| (p, handle.join().unwrap_or_default()))
            .collect();
        (layouts, pane_positions, git_status)
    });
    logging::perf("gather_lanes.subprocess_batch_done", &format!("elapsed_us={}", t0.elapsed().as_micros()));

    let lanes = cfg.lanes.iter().map(|lane| {
        // This lane's scope elements + already-known observations, built
        // from the parallel-fetched layouts/git_status and claude map above
        // rather than through scope::observe() - that would re-run the same
        // subprocess calls a second time, one lane at a time, undoing the
        // whole point of prefetching them together.
        let mut resolved: Vec<(scope::ScopeElement, Vec<scope::Observation>)> = Vec::new();
        for el in &lane.scope {
            if let Some(session) = el.zellij_session_name().filter(|s| running.contains(*s)) {
                resolved.push((el.clone(), vec![]));
                let positions = pane_positions.get(session);
                let mut claude_sessions: Vec<&drivers::claude::ClaudeSession> =
                    claude.get(session).into_iter().flatten().collect();
                // Signals render in this same order in the UI - match it to
                // each session's on-screen tab/pane position, same as
                // cycling, rather than registry-file enumeration order.
                claude_sessions.sort_by_key(|c| pane_position_rank(positions, c.zellij_pane_id));
                for c in claude_sessions {
                    resolved.push((
                        scope::ScopeElement::claude_session(&c.session_id),
                        vec![scope::Observation {
                            kind: scope::KIND_CLAUDE_SESSION_STATE.to_string(),
                            data: serde_json::json!({ "state": c.state }),
                        }],
                    ));
                }
            } else if let Some(path) = el.repo_path() {
                let status = git_status.get(&expand_tilde(path));
                let mut obs = Vec::new();
                if status.is_some_and(|s| s.dirty) {
                    obs.push(scope::Observation { kind: scope::KIND_GIT_DIRTY.to_string(), data: serde_json::json!({}) });
                }
                if let Some((current, default)) = status.and_then(|s| s.non_default_branch.as_ref()) {
                    obs.push(scope::Observation {
                        kind: scope::KIND_GIT_NON_DEFAULT_BRANCH.to_string(),
                        data: serde_json::json!({ "current": current, "default": default }),
                    });
                }
                resolved.push((el.clone(), obs));
            }
        }
        let lane_signals = scope::signals_from(&resolved);

        let mut facets: Vec<model::FacetSnapshot> = lane.scope.iter().map(|el| {
            if let Some(session) = el.zellij_session_name() {
                let is_running = running.contains(session);
                let (panes, signals) = if is_running {
                    let shape = layouts.get(session).cloned();
                    let panes = build_terminal_panes(shape, &claude, session);
                    let session_ids: HashSet<&str> = claude.get(session)
                        .map(|refs| refs.iter().map(|c| c.session_id.as_str()).collect())
                        .unwrap_or_default();
                    let signals = lane_signals.iter()
                        .filter(|s| matches!(
                            &s.action,
                            Some(model::SignalAction::SwitchClaudeSession { session_id })
                                if session_ids.contains(session_id.as_str())
                        ))
                        .cloned()
                        .collect();
                    (panes, signals)
                } else {
                    (vec![], vec![])
                };
                model::FacetSnapshot::Terminal {
                    session: session.to_string(),
                    running: is_running,
                    panes,
                    signals,
                }
            } else {
                // Repo - the only other kind gather_lanes() puts in scope.
                let path = el.repo_path().unwrap_or_default();
                let signals = lane_signals.iter()
                    .filter(|s| matches!(
                        &s.action,
                        Some(model::SignalAction::FocusRepoPane { path: p, .. }) if p == path
                    ))
                    .cloned()
                    .collect();
                model::FacetSnapshot::Repo { path: path.to_string(), signals }
            }
        }).collect();

        facets.extend(lane.targets.iter().map(|t| model::FacetSnapshot::Target {
            driver: t.driver.name().to_string(),
        }));

        let reachable = lane_reachable(&facets);
        let terminal_running = lane_terminal_running(&facets);

        // Both of these used to be their own bool fields, read separately
        // from every other "this needs you" fact (session_missing on
        // LaneSnapshot; terminal_running lived only inside the Terminal
        // facet itself, surfaced nowhere). They're both just Lanes-kind
        // signals now, pushed into the Terminal facet's own signals list -
        // the one list the UI already reads for everything else.
        if let Some(model::FacetSnapshot::Terminal { session, signals, .. }) =
            facets.iter_mut().find(|f| matches!(f, model::FacetSnapshot::Terminal { .. }))
        {
            if lane_session_missing(lane.active, reachable) {
                signals.push(model::Signal::new(
                    model::SignalReason::Lanes(model::LanesReason::SessionMissing),
                    None,
                    Some(format!("no cached WezTerm tab for zellij session \"{session}\"")),
                ));
            }
            if terminal_running == Some(false) {
                signals.push(model::Signal::new(
                    model::SignalReason::Lanes(model::LanesReason::SessionNotRunning),
                    None,
                    Some(format!("expected zellij session \"{session}\" to be running")),
                ));
            }
            // Command-kind signals for long-running commands that finished /
            // failed in one of this session's panes. Reconciled against the
            // session's live pane ids (a record for a pane that's since
            // closed is dropped) and filtered by signal-dismissed. Empty
            // unless the `shell` driver is enabled.
            let live_panes = pane_positions.get(session);
            for rec in shell_records.iter().filter(|r| r.session == *session) {
                if live_panes.is_none_or(|p| !p.contains_key(&rec.pane)) {
                    continue;
                }
                if state::is_signal_dismissed(&rec.occurrence_id()) {
                    continue;
                }
                if let Some(sig) = command_signal(rec) {
                    signals.push(sig);
                }
            }
        }

        let has_claude_signal = facets.iter()
            .any(|f| f.signals().iter().any(|s| s.kind() == model::SignalKind::ClaudeSession));
        let cyclable = lane_cyclable(lane.active, reachable, has_claude_signal);

        // Every signal was constructed with cyclable=false as a placeholder
        // (signal_for() runs before a lane's own reachability is known) -
        // correct them all now that lane-level cyclable is known, so the UI
        // reads this straight off each signal instead of re-deriving "is
        // this specific chip something a cycle would land on" from
        // signal.kind() and lane.cyclable itself. Same pass also upgrades
        // Awaiting -> Ready (see upgrade_awaiting_to_ready) - that
        // reclassification depends on the exact same cyclable fact, only
        // knowable at this same point in the pipeline.
        for facet in facets.iter_mut() {
            let signals = match facet {
                model::FacetSnapshot::Terminal { signals, .. } => signals,
                model::FacetSnapshot::Repo { signals, .. } => signals,
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

        model::LaneSnapshot { id: lane.id.clone(), name: lane.name.clone(), active: lane.active, cyclable, facets }
    }).collect();

    logging::perf("gather_lanes.done", &format!("elapsed_us={}", t0.elapsed().as_micros()));

    model::LanewiseSnapshot {
        taken_at: chrono::Utc::now().to_rfc3339(),
        lanes,
        focused_lane: state::read_focused_lane(),
        focused_claude_session: state::read_claude_cursor(),
    }
}

/// A lane you're treating as active, but that isn't actually reachable
/// right now - distinct from an inactive lane (a deliberate choice, nothing
/// "wrong" about it) and from a lane with no terminal facet at all (`None`,
/// from `lane_reachable` - nothing to be missing). Only meaningful for lanes
/// that declare a terminal.
///
/// "Reachable" means state.kdl has a cached wezterm-tab-id for this
/// session - not whether the Zellij session itself is alive, and not a
/// live WezTerm query. Whichever tool actually owns the WezTerm tab's
/// lifecycle (e.g. `ztabs`) is responsible for keeping this cache
/// current: pushing the fresh id via `lanes tabs set` whenever it spawns a
/// tab, and clearing it via `lanes tabs clear` the moment it kills one.
/// `lanes` itself trusts the cache rather than re-deriving liveness by
/// polling WezTerm - the tool that owns the tab already knows the truth
/// the instant it changes, so there's nothing to rediscover.
fn lane_session_missing(active: bool, reachable: Option<bool>) -> bool {
    reachable.is_some_and(|r| lane_session_missing_decision(active, r))
}

fn lane_session_missing_decision(active: bool, reachable: bool) -> bool {
    active && !reachable
}

/// This lane's terminal reachability - state.kdl has a cached WezTerm
/// tab-id for its Zellij session - or None if it has no terminal facet at
/// all (nothing to be reachable or not). The single place that reads
/// state.kdl for this fact; both `lane_session_missing` and gather_lanes()'s
/// `cyclable` computation go through it rather than each re-deriving
/// reachability their own way.
fn lane_reachable(facets: &[model::FacetSnapshot]) -> Option<bool> {
    let session = facets.iter().find_map(|f| match f {
        model::FacetSnapshot::Terminal { session, .. } => Some(session.as_str()),
        _ => None,
    })?;
    Some(state::get_wezterm_tab_id(session).is_some())
}

/// Whether this lane's declared Zellij session is an actually-running
/// process right now - or `None` if it has no terminal facet at all
/// (nothing to be running or not). Distinct from `lane_reachable`: that's a
/// WezTerm-tab-caching fact, this is "does the Zellij session itself exist"
/// - a lane can in principle be reachable (a cached tab-id) while its
/// session has since died, or vice versa. Sourced from `FacetSnapshot::
/// Terminal.running`, already computed earlier in `gather_lanes()` from
/// `drivers::zellij::running_sessions()` - this doesn't re-derive it, just
/// reads back the one Terminal facet a lane can have.
fn lane_terminal_running(facets: &[model::FacetSnapshot]) -> Option<bool> {
    facets.iter().find_map(|f| match f {
        model::FacetSnapshot::Terminal { running, .. } => Some(*running),
        _ => None,
    })
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
    reachable.is_some_and(|r| reachable_lane_decision(active, r)) && has_claude_signal
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
/// that lane by construction (gather_lanes() only ever resolves a lane's
/// own zellij session's live sessions into it), so there's no further
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
///
/// Everything else (git chips, off-cycle Claude sessions) keeps the old
/// behaviour: visible iff cyclable.
fn signal_visible(kind: model::SignalKind, cyclable: bool) -> bool {
    matches!(kind, model::SignalKind::Command | model::SignalKind::Lanes) || cyclable
}

/// A `done`/`failed` Command signal from one shell-hook record, or `None`
/// if the record is a `running` one (Phase 2). Dismissal is checked by the
/// caller (against `dismiss_id`), kept out of here so this stays a pure
/// function. The chip is not cyclable - the correction pass leaves it that
/// way since `signal_cyclable` only ever says yes to ClaudeSession - but it
/// *is* visible via `signal_visible`.
fn command_signal(rec: &drivers::shell::CommandRecord) -> Option<model::Signal> {
    let reason = match rec.state.as_str() {
        "done" => model::CommandReason::Done,
        "failed" => model::CommandReason::Failed,
        _ => return None, // "running" is Phase 2
    };
    let id = rec.occurrence_id();

    let cmd = rec.cmd.clone().or_else(|| rec.argv0.clone()).unwrap_or_else(|| "command".to_string());
    let dur = rec.duration_secs().map(drivers::shell::fmt_duration);
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

/// drivers::claude::enumerate() grouped by zellij session, for the lanes
/// that have several claude panes under one Terminal facet. Staleness
/// correction for permission_pending lives in the driver itself now (see
/// drivers::claude::ClaudeSession) - this is just the grouping.
fn claude_sessions_by_zellij() -> HashMap<String, Vec<drivers::claude::ClaudeSession>> {
    let mut map: HashMap<String, Vec<drivers::claude::ClaudeSession>> = HashMap::new();
    for session in drivers::claude::enumerate() {
        let zs = session.zellij_session.clone().unwrap_or_default();
        map.entry(zs).or_default().push(session);
    }
    map
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

/// Signals are computed separately now (see gather_lanes(), via
/// scope::signals_from()) - this only builds the pane list, still needing
/// the claude map to know which pane (if any) is a Claude session that's
/// awaiting attention.
fn build_terminal_panes(
    shape: Option<model::TerminalShape>,
    claude: &HashMap<String, Vec<drivers::claude::ClaudeSession>>,
    session: &str,
) -> Vec<model::PaneSnapshot> {
    let Some(shape) = shape else {
        return vec![];
    };

    let needs_attention = claude.get(session).map_or(false, |refs| {
        refs.iter().any(|r| matches!(r.state.as_str(), "idle" | "permission_pending"))
    });

    shape.tabs.iter().flat_map(|tab| {
        tab.panes.iter().map(|pane| {
            let kind = match pane.command.as_deref() {
                Some("claude") => model::PaneKind::ClaudeSession { awaiting: needs_attention },
                other => model::PaneKind::from_command(other),
            };
            model::PaneSnapshot { focused: pane.focused, cwd: pane.cwd.clone(), kind }
        })
    }).collect()
}

pub(crate) fn git_has_changes(path: &str) -> bool {
    let Ok(out) = std::process::Command::new("git")
        .args(["-C", path, "status", "--porcelain"])
        .output()
    else {
        return false;
    };
    out.status.success() && !out.stdout.is_empty()
}

/// The checked-out branch, or `None` for a detached HEAD (matches git's own
/// terminology for that state - "HEAD" isn't a real branch name worth
/// surfacing as one) or a repo git itself couldn't answer for.
fn git_current_branch(path: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["-C", path, "symbolic-ref", "--short", "-q", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if branch.is_empty() { None } else { Some(branch) }
}

/// The repo's actual default branch - `origin/HEAD`, the same pointer
/// GitHub/GitLab set on clone, so this needs no per-repo config of our own
/// to know "main" isn't universal (some repos really do use "master",
/// "trunk", etc). Falls back to a local "main" or "master" branch (in that
/// order) if origin/HEAD is stale or the repo has no remote at all - `None`
/// (not a guess) if neither resolves, since a wrong guess here would create
/// false-positive "wrong branch" signals, worse than just not supporting
/// that repo yet.
fn git_default_branch(path: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["-C", path, "symbolic-ref", "--short", "-q", "refs/remotes/origin/HEAD"])
        .output()
        .ok()?;
    if out.status.success() {
        let full = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if let Some(branch) = full.strip_prefix("origin/") {
            if !branch.is_empty() {
                return Some(branch.to_string());
            }
        }
    }
    for candidate in ["main", "master"] {
        let exists = std::process::Command::new("git")
            .args(["-C", path, "show-ref", "--verify", "--quiet", &format!("refs/heads/{candidate}")])
            .status()
            .is_ok_and(|s| s.success());
        if exists {
            return Some(candidate.to_string());
        }
    }
    None
}

/// The two facts gather_lanes() needs about one repo, fetched together
/// since both are just separate `git` subprocess round-trips against the
/// same repo - one closure per repo in the concurrent batch, not two,
/// avoiding doubling the subprocess count this session already worked to
/// cut down (see the Diagnostics section of the README).
#[derive(Default)]
pub(crate) struct RepoStatus {
    pub dirty: bool,
    /// (current, default) if the checked-out branch differs from the
    /// repo's actual default - None if they match, or if the default
    /// couldn't be determined at all (see git_default_branch).
    pub non_default_branch: Option<(String, String)>,
}

pub(crate) fn git_repo_status(path: &str) -> RepoStatus {
    let dirty = git_has_changes(path);
    let non_default_branch = git_default_branch(path).and_then(|default| {
        let current = git_current_branch(path).unwrap_or_else(|| "detached HEAD".to_string());
        (current != default).then_some((current, default))
    });
    RepoStatus { dirty, non_default_branch }
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
        // focusing the terminal below - each target's own activation step
        // (open -a, an obsidian:// URI, ...) brings that app frontmost, so
        // doing this first and WezTerm/Zellij last guarantees the terminal
        // (and the Claude session in it) is what you're actually looking at
        // once the switch finishes, not whichever target happened to be
        // activated last. This is also the actual window placement step,
        // previously missing entirely from this path (it only ever
        // activated the WezTerm tab/Zellij pane, never touched [[targets]]
        // at all). Best-effort like focus_lane's own target handling: a
        // target failure (an app not running, a driver command failing)
        // doesn't roll back a terminal switch that already succeeded.
        if let Some(lane) = lane_id.as_deref().and_then(|id| cfg.lanes.iter().find(|l| l.id == id)) {
            for w in apply_targets(&lane.targets, &cfg, lane.terminal_session()) {
                eprintln!("warning: {w}");
            }
        }
        logging::perf("switch.targets_applied", &format!("session={session_id} elapsed_us={}", t0.elapsed().as_micros()));

        // Resolve the tab through the same session -> tab-id cache everything
        // else uses, rather than the wezterm_tab_id recorded in the session
        // file at hook time (which came from the same unreliable title
        // matching we removed everywhere else). Deliberately last - see above.
        activate_wezterm_tab(&zellij_session, true)?;
        logging::perf("switch.tab_activated", &format!("session={session_id} elapsed_us={}", t0.elapsed().as_micros()));

        if let Some(pane_id) = zellij_pane_id {
            focus_zellij_pane(&zellij_session, pane_id)?;
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
pub fn cycle_claude_session(direction: i32) -> Result<(), String> {
    let ct0 = std::time::Instant::now();
    let cfg = config::Config::load();
    let live_sessions = if cfg.driver_enabled("claude") {
        drivers::claude::enumerate()
    } else {
        vec![]
    };
    logging::perf("cycle.enumerated", &format!("elapsed_us={} count={}", ct0.elapsed().as_micros(), live_sessions.len()));
    // Skip sessions that live in an inactive lane, or one with no cached
    // WezTerm tab-id (see lane_session_missing) - that's exactly what
    // caused the "wezterm activate-tab failed" cycling errors, since
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
    // sessions and running what's left concurrently (same pattern as
    // gather_lanes's batch) cuts both the call count and the serialization.
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
            let has_cached_tab_id = state::get_wezterm_tab_id(zellij_session).is_some();
            reachable_lane_decision(lane.active, has_cached_tab_id)
        }
        None => true,
    }
}

fn reachable_lane_decision(active: bool, has_cached_tab_id: bool) -> bool {
    active && has_cached_tab_id
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

pub fn wezterm_socket() -> Option<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = std::path::PathBuf::from(home).join(".local/share/wezterm");
    let mut socks: Vec<_> = std::fs::read_dir(&dir).ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("gui-sock-"))
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            let modified = meta.modified().ok()?;
            Some((modified, e.path()))
        })
        .collect();
    socks.sort_by(|a, b| b.0.cmp(&a.0));
    socks.into_iter().next().map(|(_, p)| p.to_string_lossy().into_owned())
}

fn activate_wezterm_tab(session: &str, focus: bool) -> Result<(), String> {
    let cached = state::get_wezterm_tab_id(session).ok_or_else(|| {
        format!("no cached WezTerm tab for session '{}' - run `lanes tabs set {} <id>`", session, session)
    })?;

    let sock = wezterm_socket();

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

    let mut cmd = std::process::Command::new("/opt/homebrew/bin/wezterm");
    cmd.args(["cli", "activate-tab", "--tab-id", &cached.to_string()]);
    if let Some(ref s) = sock {
        cmd.env("WEZTERM_UNIX_SOCKET", s);
    }
    let output = cmd.output().map_err(|e| format!("wezterm activate-tab: {}", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "wezterm activate-tab failed for cached tab {} (session '{}'), it may no longer exist - run `lanes tabs set {} <id>`: {}",
            cached, session, session, stderr.trim()
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
        model::TargetDriver::Sourcetree { .. } => Some("com.torusknot.SourceTreeNotMAS"),
        model::TargetDriver::Vscode { .. } => Some("com.microsoft.VSCode"),
        // Only known if the config gave us one - unlike the drivers above,
        // "app" has no fixed identity, so placement only works when the
        // config author supplied a bundle_id explicitly.
        model::TargetDriver::App { bundle_id, .. } => bundle_id.as_deref(),
        // firefox-profile is never placed by bundle ID - every profile
        // shares the same one, which is exactly the ambiguity it exists to
        // resolve. It's placed by the PID activate_target resolves instead
        // (see apply_targets).
        model::TargetDriver::Zellij { .. } | model::TargetDriver::FirefoxProfile { .. } => None,
    }
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

/// Resolve a Firefox profile's display name - as set in Firefox's own
/// built-in profile switcher (the newer "Profiles" feature, distinct from
/// the legacy -P/profiles.ini system, which this doesn't touch at all) -
/// to its on-disk profile directory. Names are stored in a per-install
/// SQLite database under `Profile Groups/`, queried by shelling out to
/// `sqlite3` rather than adding a dependency for one query - same pattern
/// as everything else here (wezterm/zellij/stree are all shelled out to,
/// not linked against).
pub fn firefox_profile_path(name: &str) -> Result<String, String> {
    let groups_dir = expand_tilde("~/Library/Application Support/Firefox/Profile Groups");
    let entries = std::fs::read_dir(&groups_dir)
        .map_err(|e| format!("could not read {groups_dir}: {e}"))?;
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("sqlite") {
            continue;
        }
        let output = std::process::Command::new("sqlite3")
            .arg(&path)
            .arg(format!("SELECT path FROM Profiles WHERE name = '{}'", name.replace('\'', "''")))
            .output()
            .map_err(|e| format!("sqlite3: {e}"))?;
        let relative = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !relative.is_empty() {
            return Ok(expand_tilde(&format!("~/Library/Application Support/Firefox/{relative}")));
        }
    }
    Err(format!("no Firefox profile named '{name}' found in Firefox's profile switcher"))
}

/// Whether `profile_path` is the profile `profiles.ini` marks as the
/// default - needed because Firefox's own default profile is normally
/// launched with no `--profile` flag at all (see `firefox_profile_pid`),
/// so matching launch arguments alone can't identify it.
fn is_default_firefox_profile(profile_path: &str) -> bool {
    let ini_path = expand_tilde("~/Library/Application Support/Firefox/profiles.ini");
    let Ok(content) = std::fs::read_to_string(&ini_path) else { return false };
    let Some(default_relative) = content
        .lines()
        .find_map(|l| l.strip_prefix("Path=").map(str::trim))
    else {
        return false;
    };
    profile_path.ends_with(default_relative)
}

/// Find the PID of a currently-running Firefox process using this exact
/// profile directory, by matching `--profile <path>` in `ps`'s command
/// output. Filters to the main `firefox` binary specifically - its helper
/// processes (plugin-container, gpu-helper, crashhelper) all inherit and
/// echo the same `-profile <path>` argument in their own command lines, so
/// a plain substring match without this would find those instead. Falls
/// back to matching a bare `firefox` process with no `--profile` flag at
/// all when `profile_path` is Firefox's own default - that's how the
/// default profile normally launches.
fn firefox_profile_pid(profile_path: &str) -> Option<u32> {
    let output = std::process::Command::new("ps").args(["-eo", "pid,command"]).output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    match_firefox_pid(&text, profile_path, is_default_firefox_profile(profile_path))
}

/// The pure matching logic behind `firefox_profile_pid`, pulled out so it's
/// testable without an actual `ps` call. Filters `ps -eo pid,command`
/// output to the main `firefox` binary specifically - its helper processes
/// (plugin-container, gpu-helper, crashhelper) all inherit and echo the
/// same `-profile <path>` argument in their own command lines, so a plain
/// substring match without this would find those instead.
fn match_firefox_pid(ps_output: &str, profile_path: &str, is_default: bool) -> Option<u32> {
    let bin = "/Applications/Firefox.app/Contents/MacOS/firefox";
    for line in ps_output.lines() {
        let line = line.trim_start();
        // Each line is "PID COMMAND...", so the binary path is never a
        // prefix of the raw line itself - split off the PID first.
        let Some((pid_str, command)) = line.split_once(char::is_whitespace) else { continue };
        let Some(rest) = command.trim_start().strip_prefix(bin) else { continue };
        let matches = rest.contains(&format!("--profile {profile_path}"))
            || (is_default && !rest.contains("--profile"));
        if !matches {
            continue;
        }
        if let Ok(pid) = pid_str.parse() {
            return Some(pid);
        }
    }
    None
}

/// Resolve a Firefox profile name to a running PID. Only launches it if
/// `launch` is true - matching the opt-in, default-off launch behavior
/// every other driver has (see PLAN-window-targets.md's "App-not-running
/// behavior"); otherwise a not-running profile is a clean failure, not a
/// silent launch. A freshly-launched process's PID comes straight from
/// `Child::id()` - no need to re-scan for it.
fn resolve_or_launch_firefox_profile(name: &str, launch: bool) -> Result<u32, String> {
    let path = firefox_profile_path(name)?;
    if let Some(pid) = firefox_profile_pid(&path) {
        return Ok(pid);
    }
    if !launch {
        return Err(format!("Firefox profile '{name}' is not running (set launch = true to start it automatically)"));
    }
    // See spawn_detached: Firefox outlives `lanes` for the rest of the
    // session, so it must not inherit our stdio.
    let child = spawn_detached(
        std::process::Command::new("/Applications/Firefox.app/Contents/MacOS/firefox")
            .args(["--profile", &path]),
    )
    .map_err(|e| format!("failed to launch Firefox profile '{name}': {e}"))?;
    let pid = child.id();
    wait_for_a_window(pid, std::time::Duration::from_secs(5));
    Ok(pid)
}

/// Polls (via System Events, since lanes-cli has no direct Accessibility
/// access - that's lanes-wm's job) until a process has at least one window,
/// up to `timeout`. Best-effort: a fresh app launch's window doesn't exist
/// yet the instant the process starts, and placement immediately afterward
/// would otherwise race it and fail with "no focused window" - seen doing
/// exactly this with a freshly-launched Firefox profile. Doesn't error out
/// on timeout; the subsequent placement call just fails on its own with a
/// clear message if the window still isn't there.
fn wait_for_a_window(pid: u32, timeout: std::time::Duration) {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        let script = format!(
            "tell application \"System Events\" to count of windows of (first process whose unix id is {pid})"
        );
        let has_window = std::process::Command::new("osascript")
            .args(["-e", &script])
            .output()
            .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() != "0");
        if has_window {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// Run a target's activation step - whatever app-specific mechanism makes
/// the right window/pane/vault frontmost. Doesn't place anything itself;
/// placement is a separate, batched step in `apply_targets` (every
/// placement in a lane goes through one `lanes-wm apply` call, not one per
/// target - see PLAN-window-targets.md).
///
/// `default_session` is the owning lane's own terminal session - used when
/// a `wezterm`/`zellij` target omits `session`, which is what lets one
/// global default target (e.g. "WezTerm always goes to lg-right") apply
/// across lanes that each have a different session name.
///
/// `raise_app` controls whether the `app` driver visibly brings its app
/// forward (`open -a`) - only meaningful there. lanes-wm's `--focused`
/// placement selector resolves and repositions a window correctly whether
/// or not its app is frontmost, so a target that's only being *placed*
/// (has `monitor` set) doesn't need to steal focus to do that; raising is
/// reserved for activate-only `app` targets (e.g. a Trello peek app),
/// whose entire purpose is to be brought into view. Without this, every
/// target in a lane briefly raised its own app in sequence before the
/// final terminal-focus step landed - visibly flickering through each app
/// on every switch instead of just showing the one or two that actually
/// need to be seen.
///
/// Returns the PID a driver resolved, if any - only `firefox-profile` does
/// this, since resolving which of several same-bundle-ID processes is the
/// right one *is* its activation step. `apply_targets` uses this instead
/// of `target_bundle_id` to identify the placement target for such drivers.
fn activate_target(driver: &model::TargetDriver, raise_app: bool, default_session: Option<&str>, launch: bool) -> Result<Option<u32>, String> {
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
        model::TargetDriver::FirefoxProfile { profile } => {
            resolve_or_launch_firefox_profile(profile, launch).map(Some)
        }
    }
}

/// Activate every target in a lane, then place all of them that asked for
/// placement in a single `lanes-wm apply` call - never one `lanes-wm place`
/// per target, since a lane switch always moves several windows at once and
/// should do it in one shot (see PLAN-window-targets.md). Best-effort like
/// the scope loop above: one broken target doesn't block the rest.
///
/// `default_session` - see `activate_target`.
fn apply_targets(targets: &[model::Target], cfg: &config::Config, default_session: Option<&str>) -> Vec<String> {
    let mut warnings = Vec::new();
    let mut placements = Vec::new();

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

        // Only raise an `app` target's window when it isn't being placed,
        // unless the target explicitly asks to be raised anyway (a "peek"
        // app that also wants placement) - see activate_target's doc
        // comment and Target::raise's doc comment for why.
        let raise_app = target.monitor.is_none() || target.raise;
        let resolved_pid = match activate_target(&target.driver, raise_app, default_session, target.launch) {
            Ok(pid) => pid,
            Err(e) => {
                let e = format!("target '{}': {e}", target.driver.name());
                eprintln!("warning: {e}");
                warnings.push(e);
                continue;
            }
        };

        let Some(monitor) = &target.monitor else { continue };
        // A driver that resolved its own PID (firefox-profile) is placed by
        // PID directly, since bundle-ID matching can't tell its instances
        // apart in the first place; everything else is placed by bundle ID.
        let identity_key = if resolved_pid.is_some() { "pid" } else { "app" };
        let identity_value: serde_json::Value = match resolved_pid {
            Some(pid) => serde_json::json!(pid),
            None => match target_bundle_id(&target.driver) {
                Some(bundle_id) => serde_json::json!(bundle_id),
                None => {
                    let e = format!(
                        "target '{}' has monitor/position set but its driver can't be placed",
                        target.driver.name()
                    );
                    eprintln!("warning: {e}");
                    warnings.push(e);
                    continue;
                }
            },
        };
        let Some(uuid) = cfg.monitor_uuid(monitor) else {
            let e = format!("monitor handle '{monitor}' not found in config");
            eprintln!("warning: {e}");
            warnings.push(e);
            continue;
        };
        let position = target.position.clone().unwrap_or_else(|| toml::Value::String("full".to_string()));
        let position_json = match serde_json::to_value(&position) {
            Ok(v) => v,
            Err(e) => {
                let e = format!("target '{}': invalid position: {e}", target.driver.name());
                eprintln!("warning: {e}");
                warnings.push(e);
                continue;
            }
        };
        // `focused: true`, never `title` - the whole point of the activate
        // step above was to deterministically make the right window
        // frontmost first, so placement just acts on whatever that left
        // focused (lanes-wm's `--focused` selector).
        // PID-resolved targets (today: firefox-profile) also get raised.
        // They're exactly the ones that can end up competing for the same
        // screen slot across lanes - e.g. every profile's window gets
        // placed at the same lg-left coordinates - and placement alone
        // doesn't affect z-order, so whichever one was already on top from
        // an earlier lane would otherwise stay there, hiding the one that
        // was just correctly (but invisibly) placed underneath it.
        let mut placement = serde_json::Map::new();
        placement.insert(identity_key.to_string(), identity_value);
        placement.insert("monitor".to_string(), serde_json::json!(uuid));
        placement.insert("position".to_string(), position_json);
        placement.insert("focused".to_string(), serde_json::json!(true));
        placement.insert("raise".to_string(), serde_json::json!(resolved_pid.is_some()));
        placements.push(serde_json::Value::Object(placement));
    }

    if !placements.is_empty() {
        warnings.extend(run_lanes_wm_apply(&placements));
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
pub fn focus_lane(lane_id: &str, focus: bool) -> Result<(), String> {
    let cfg = config::Config::load();
    let lane = match cfg.lanes.iter().find(|l| l.id == lane_id) {
        Some(l) => l,
        None => return Err(format!("lane not found: {}", lane_id)),
    };

    // Targets (Firefox, peek apps, etc.) are applied *before* the
    // terminal-focus loop below, not after - each target's own activation
    // step brings that app frontmost, so doing this first and the terminal
    // last guarantees you end up looking at the terminal once focus_lane
    // finishes, not whichever target happened to be activated last.
    let mut warnings = apply_targets(&lane.targets, &cfg, lane.terminal_session());
    for el in &lane.scope {
        if let Some(session) = el.zellij_session_name() {
            if let Err(e) = activate_wezterm_tab(session, focus) {
                eprintln!("warning: {}", e);
                warnings.push(e);
                continue;
            }
            // Deterministically land on a real pane rather than whatever
            // Zellij's own internal state happens to have last focused -
            // "focus this lane" should always put you somewhere you can
            // immediately type into, not just switch WezTerm's active tab
            // and leave the actual in-session focus as a coin flip. Only
            // reached for lanes with no more specific routing already
            // handled elsewhere (a Claude session's own pane_id, via
            // switch_claude_session) - this is the general fallback.
            if let Some(pane_id) = first_pane_id(session) {
                if let Err(e) = focus_zellij_pane(session, pane_id as u64) {
                    eprintln!("warning: {}", e);
                    warnings.push(e);
                }
            }
        }
    }

    state::set_focused_lane(lane_id);

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

    /// Trimmed from real `ps -eo pid,command` output captured during
    /// development: one main firefox process for the Development profile,
    /// plus two of its helper processes that echo the same `-profile`
    /// argument in their own command lines (the exact false-positive this
    /// filtering exists to avoid).
    const PS_SAMPLE: &str = "\
28420 /Applications/Firefox.app/Contents/MacOS/firefox -foreground --profile /Users/bmiller/Library/Application Support/Firefox/Profiles/1SBXZ1GS.Profile 1 -new-tab about:newprofile
28490 /Applications/Firefox.app/Contents/MacOS/plugin-container.app/Contents/MacOS/plugin-container -profile /Users/bmiller/Library/Application Support/Firefox/Profiles/1SBXZ1GS.Profile 1 org.mozilla.machname.1 4 rdd
28454 /Applications/Firefox.app/Contents/MacOS/gpu-helper.app/Contents/MacOS/Firefox GPU Helper -profile /Users/bmiller/Library/Application Support/Firefox/Profiles/1SBXZ1GS.Profile 1
40972 /Applications/Firefox.app/Contents/MacOS/firefox";

    #[test]
    fn match_firefox_pid_finds_the_main_process_not_its_helpers() {
        let path = "/Users/bmiller/Library/Application Support/Firefox/Profiles/1SBXZ1GS.Profile 1";
        assert_eq!(match_firefox_pid(PS_SAMPLE, path, false), Some(28420));
    }

    #[test]
    fn match_firefox_pid_none_when_profile_not_running_and_not_default() {
        assert_eq!(match_firefox_pid(PS_SAMPLE, "/some/other/profile", false), None);
    }

    #[test]
    fn match_firefox_pid_falls_back_to_bare_process_for_the_default_profile() {
        // The default profile normally launches with no --profile flag at
        // all (pid 40972 above) - only accepted as a match when the caller
        // has independently confirmed (via profiles.ini) that the
        // requested profile really is the default.
        assert_eq!(match_firefox_pid(PS_SAMPLE, "/some/default/profile/path", true), Some(40972));
        assert_eq!(match_firefox_pid(PS_SAMPLE, "/some/default/profile/path", false), None);
    }

    /// A real scratch git repo (not a mock) with a single commit on
    /// `main`, no remote - init/commit are cheap and deterministic, and
    /// this exercises the actual `git` subprocess calls git_current_branch/
    /// git_default_branch/git_repo_status make, same as
    /// write_then_read_cached_panes_round_trips_within_ttl's real-I/O
    /// pattern in drivers::zellij's tests. Caller is responsible for
    /// removing the returned path when done.
    fn scratch_git_repo(initial_branch: &str) -> std::path::PathBuf {
        // cargo test runs tests in parallel on separate threads within the
        // same process - keying only on (pid, branch name) let two tests
        // that happen to use the same branch name (e.g. multiple "main"
        // repos) race on the same directory. Thread id is unique per test
        // thread, closing that gap.
        let thread_id = format!("{:?}", std::thread::current().id());
        let safe_thread_id: String = thread_id.chars().filter(|c| c.is_alphanumeric()).collect();
        let dir = std::env::temp_dir().join(format!(
            "lanes-test-repo-{}-{}-{}",
            std::process::id(),
            safe_thread_id,
            initial_branch
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(["-C", dir.to_str().unwrap()])
                .args(args)
                .status()
                .expect("git command should run");
            assert!(status.success(), "git {args:?} failed");
        };
        git(&["init", "--quiet", "--initial-branch", initial_branch]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        std::fs::write(dir.join("f.txt"), "content").unwrap();
        git(&["add", "f.txt"]);
        git(&["commit", "--quiet", "-m", "init"]);
        dir
    }

    fn shell_record(state: &str, exit: Option<i32>) -> drivers::shell::CommandRecord {
        serde_json::from_value(serde_json::json!({
            "v": 1, "session": "lanes", "pane": 3,
            "argv0": "cargo", "cmd": "cargo test --workspace",
            "started_at": "2026-09-04T10:00:00Z", "state": state,
            "ended_at": "2026-09-04T10:03:12Z", "exit_code": exit,
        })).unwrap()
    }

    #[test]
    fn command_signal_maps_state_and_exit_to_reason_and_detail() {
        let done = command_signal(&shell_record("done", Some(0))).unwrap();
        assert!(matches!(done.reason, model::SignalReason::Command(model::CommandReason::Done)));
        assert_eq!(done.detail.as_deref(), Some("cargo test --workspace · 3m12s"));
        assert_eq!(done.lifecycle, model::Lifecycle::Latched);
        assert_eq!(done.dismiss_id.as_deref(), Some("command:lanes--3--2026-09-04T10:00:00Z"));
        assert!(matches!(done.action, Some(model::SignalAction::FocusPane { pane: 3, .. })));

        let failed = command_signal(&shell_record("failed", Some(1))).unwrap();
        assert!(matches!(failed.reason, model::SignalReason::Command(model::CommandReason::Failed)));
        assert_eq!(failed.detail.as_deref(), Some("cargo test --workspace · exit 1 · 3m12s"));

        // "running" is Phase 2 - no signal yet.
        assert!(command_signal(&shell_record("running", None)).is_none());
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
        // mirrors the per-signal correction gather_lanes() runs
        let mut s = command_signal(&shell_record("failed", Some(2))).unwrap();
        s.cyclable = signal_cyclable(s.kind(), true /* lane cyclable */, false);
        s.visible = signal_visible(s.kind(), s.cyclable);
        assert!(!s.cyclable, "a finished command is never a cycle target");
        assert!(s.visible, "but it still shows in the normal view");
    }

    #[test]
    fn git_current_branch_reads_the_real_checked_out_branch() {
        let dir = scratch_git_repo("feature-xyz");
        assert_eq!(git_current_branch(dir.to_str().unwrap()), Some("feature-xyz".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn git_default_branch_falls_back_to_local_main_without_a_remote() {
        // No origin/HEAD to consult (no remote at all) - falls back to the
        // local "main" branch existing, per git_default_branch's fallback
        // chain, rather than returning None just because there's no remote.
        let dir = scratch_git_repo("main");
        assert_eq!(git_default_branch(dir.to_str().unwrap()), Some("main".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn git_default_branch_is_none_when_neither_remote_nor_local_fallback_resolves() {
        // Neither origin/HEAD nor a local main/master exists - must not
        // guess "main" anyway, since a wrong guess here creates a false
        // "wrong branch" signal, worse than not supporting this repo yet.
        let dir = scratch_git_repo("some-other-name");
        assert_eq!(git_default_branch(dir.to_str().unwrap()), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn git_repo_status_flags_non_default_branch_against_a_real_repo() {
        let dir = scratch_git_repo("main");
        std::process::Command::new("git")
            .args(["-C", dir.to_str().unwrap(), "checkout", "--quiet", "-b", "feature-xyz"])
            .status()
            .unwrap();
        let status = git_repo_status(dir.to_str().unwrap());
        assert_eq!(status.non_default_branch, Some(("feature-xyz".to_string(), "main".to_string())));
        assert!(!status.dirty);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn git_repo_status_has_no_branch_signal_when_already_on_default() {
        let dir = scratch_git_repo("main");
        let status = git_repo_status(dir.to_str().unwrap());
        assert_eq!(status.non_default_branch, None);
        let _ = std::fs::remove_dir_all(&dir);
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
            target_bundle_id(&model::TargetDriver::Vscode { folder: "x".into() }),
            Some("com.microsoft.VSCode")
        );
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

    fn test_config(lanes: Vec<model::Lane>) -> config::Config {
        config::Config { drivers: None, monitors: std::collections::HashMap::new(), order: None, lanes }
    }

    #[test]
    fn inactive_lane_is_never_reachable_regardless_of_cached_tab_id() {
        assert!(!reachable_lane_decision(false, false));
        assert!(!reachable_lane_decision(false, true));
    }

    #[test]
    fn active_lane_with_cached_tab_id_is_reachable() {
        assert!(reachable_lane_decision(true, true));
    }

    #[test]
    fn active_lane_with_no_cached_tab_id_is_not_reachable() {
        // This is the actual spinner/sheetwork-sandbox case: the lane's own
        // active flag says yes, but nothing has ever pushed (or something
        // cleared) a cached WezTerm tab-id for it - exactly what caused the
        // "wezterm activate-tab failed" cycling errors.
        assert!(!reachable_lane_decision(true, false));
    }

    fn ordered_lane(id: &str, session: &str) -> model::Lane {
        model::Lane {
            id: id.to_string(),
            name: id.to_string(),
            active: true,
            scope: vec![crate::scope::ScopeElement::zellij_session(session)],
            targets: vec![],
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
        }]);
        assert!(session_belongs_to_reachable_lane(Some("some-other-session"), &cfg));
        assert!(session_belongs_to_reachable_lane(None, &cfg));
    }

    #[test]
    fn repo_only_facets_have_no_reachability_to_speak_of() {
        // A repo-only lane has no session to be missing in the first place -
        // this goes through the real lane_reachable() (not a mock) to
        // confirm it never even reaches for state.kdl when there's no
        // terminal facet at all, just returns None.
        assert_eq!(lane_reachable(&[model::FacetSnapshot::Repo { path: "/a/b".to_string(), signals: vec![] }]), None);
    }

    #[test]
    fn lane_session_missing_is_false_when_reachability_is_unknown() {
        assert!(!lane_session_missing(true, None));
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

    // The rest go through the pure decision fn directly, not lane_session_missing()
    // itself - that one does real I/O (state::get_wezterm_tab_id reads state.kdl),
    // which would make these tests depend on whatever happens to be in that file
    // on the machine running them.

    #[test]
    fn inactive_lane_is_never_session_missing_regardless_of_reachability() {
        // Deliberately inactive isn't the same problem as "should be
        // reachable and isn't" - nothing to flag here.
        assert!(!lane_session_missing_decision(false, false));
        assert!(!lane_session_missing_decision(false, true));
    }

    #[test]
    fn active_and_reachable_is_not_session_missing() {
        assert!(!lane_session_missing_decision(true, true));
    }

    #[test]
    fn active_and_unreachable_is_session_missing() {
        assert!(lane_session_missing_decision(true, false));
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

    #[test]
    fn is_claude_command_matches_bare_and_full_path() {
        assert!(is_claude_command("claude"));
        assert!(is_claude_command("/opt/homebrew/bin/claude"));
        assert!(!is_claude_command("claude-code-helper"));
        assert!(!is_claude_command("bash"));
        assert!(!is_claude_command(""));
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
