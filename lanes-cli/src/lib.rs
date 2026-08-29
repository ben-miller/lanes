pub mod config;
mod drivers;
pub mod logging;
pub mod model;
pub mod scope;
pub mod state;
pub mod zone;

pub use drivers::claude::RenamedCandidate;

pub fn possibly_renamed_claude_sessions() -> Vec<RenamedCandidate> {
    drivers::claude::possibly_renamed_sessions()
}

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

pub fn gather_lanes(cfg: &config::Config) -> model::LanewiseSnapshot {
    let running = drivers::zellij::running_sessions();
    let claude = claude_sessions_by_zellij();

    let running_sessions: Vec<&str> = cfg.lanes.iter()
        .flat_map(|lane| lane.scope.iter())
        .filter_map(|el| el.zellij_session_name().filter(|s| running.contains(*s)))
        .collect();

    let repo_paths: Vec<String> = cfg.lanes.iter()
        .flat_map(|lane| lane.scope.iter())
        .filter_map(|el| el.repo_path().map(expand_tilde))
        .collect();

    // dump-layout and list-panes (per session) and git status (per repo) are
    // each a separate subprocess round-trip, independent of every other one
    // - fetch them all concurrently in one batch rather than once per lane
    // in sequence (this used to be the dominant cost of every UI refresh).
    let (layouts, pane_positions, git_status): (
        HashMap<String, Option<(model::TerminalShape, Option<String>)>>,
        HashMap<String, HashMap<u32, (usize, i64, i64)>>,
        HashMap<String, bool>,
    ) = std::thread::scope(|scope| {
        let layout_handles: Vec<_> = running_sessions.iter()
            .map(|&s| (s.to_string(), scope.spawn(move || drivers::zellij::layout_for_session(s))))
            .collect();
        let pane_position_handles: Vec<_> = running_sessions.into_iter()
            .map(|s| (s.to_string(), scope.spawn(move || drivers::zellij::pane_positions(s))))
            .collect();
        let git_handles: Vec<_> = repo_paths.into_iter()
            .map(|p| (p.clone(), scope.spawn(move || git_has_changes(&p))))
            .collect();

        let layouts = layout_handles.into_iter()
            .map(|(s, handle)| (s, handle.join().ok().flatten()))
            .collect();
        let pane_positions = pane_position_handles.into_iter()
            .map(|(s, handle)| (s, handle.join().unwrap_or_default()))
            .collect();
        let git_status = git_handles.into_iter()
            .map(|(p, handle)| (p, handle.join().unwrap_or(false)))
            .collect();
        (layouts, pane_positions, git_status)
    });

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
                let dirty = git_status.get(&expand_tilde(path)).copied().unwrap_or(false);
                let obs = if dirty {
                    vec![scope::Observation { kind: scope::KIND_GIT_DIRTY.to_string(), data: serde_json::json!({}) }]
                } else {
                    vec![]
                };
                resolved.push((el.clone(), obs));
            }
        }
        let lane_signals = scope::signals_from(&resolved);

        let mut facets: Vec<model::FacetSnapshot> = lane.scope.iter().map(|el| {
            if let Some(session) = el.zellij_session_name() {
                let is_running = running.contains(session);
                let (panes, signals) = if is_running {
                    let shape = layouts.get(session).cloned().flatten();
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

        facets.extend(lane.windows.iter().map(|w| model::FacetSnapshot::Window {
            path: w.path.clone(),
            zone: w.zone.clone(),
        }));

        let session_missing = lane_session_missing(lane.active, &facets);

        model::LaneSnapshot { id: lane.id.clone(), name: lane.name.clone(), active: lane.active, session_missing, facets }
    }).collect();

    model::LanewiseSnapshot {
        taken_at: chrono::Utc::now().to_rfc3339(),
        lanes,
        focused_lane: state::read_focused_lane(),
        focused_claude_session: state::read_claude_cursor(),
    }
}

/// A lane you're treating as active, but that isn't actually reachable
/// right now - distinct from an inactive lane (a deliberate choice, nothing
/// "wrong" about it) and from a lane with no terminal facet at all (nothing
/// to be missing). Only meaningful for lanes that declare a terminal.
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
fn lane_session_missing(active: bool, facets: &[model::FacetSnapshot]) -> bool {
    let Some(session) = facets.iter().find_map(|f| match f {
        model::FacetSnapshot::Terminal { session, .. } => Some(session.as_str()),
        _ => None,
    }) else {
        return false;
    };
    let reachable = state::get_wezterm_tab_id(session).is_some();
    lane_session_missing_decision(active, reachable)
}

fn lane_session_missing_decision(active: bool, reachable: bool) -> bool {
    active && !reachable
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

/// Whether a registry entry for a Claude session still refers to something actually
/// running, rather than a file orphaned by a session that ended without firing
/// `SessionEnd` (crash, force-quit, killed pane).
///
/// Sessions living in a Zellij pane are first checked against currently running
/// Zellij sessions - reliable, no guessing. But the Zellij session outliving the
/// pane's original occupant is exactly how stale entries accumulate (a resumed or
/// restarted `claude` process leaves the old registry file behind), so when a PID
/// is also recorded we additionally verify it's still alive and is actually a
/// `claude` process - a bare `kill -0` isn't enough since PIDs get reused, so a
/// dead session's orphaned PID could later collide with an unrelated process.
/// Sessions started outside Zellij have no session-name anchor at all, so they
/// rely on the PID check alone.
pub(crate) fn session_is_live(zellij_session: &str, live_zellij_sessions: &HashSet<String>, pid: Option<u32>) -> bool {
    session_is_live_with(zellij_session, live_zellij_sessions, pid, process_command)
}

fn session_is_live_with(
    zellij_session: &str,
    live_zellij_sessions: &HashSet<String>,
    pid: Option<u32>,
    lookup: impl Fn(u32) -> Option<String>,
) -> bool {
    if !zellij_session.is_empty() {
        if !live_zellij_sessions.contains(zellij_session) {
            return false;
        }
        return match pid {
            Some(p) => lookup(p).map_or(false, |cmd| is_claude_command(&cmd)),
            None => true,
        };
    }
    match pid {
        Some(p) => lookup(p).map_or(false, |cmd| is_claude_command(&cmd)),
        None => false,
    }
}

fn is_claude_command(cmd: &str) -> bool {
    cmd.trim().rsplit('/').next().unwrap_or("") == "claude"
}

fn process_command(pid: u32) -> Option<String> {
    let out = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .ok()?;
    if !out.status.success() { return None; }
    let cmd = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if cmd.is_empty() { None } else { Some(cmd) }
}

/// Signals are computed separately now (see gather_lanes(), via
/// scope::signals_from()) - this only builds the pane list, still needing
/// the claude map to know which pane (if any) is a Claude session that's
/// awaiting attention.
fn build_terminal_panes(
    shape: Option<(model::TerminalShape, Option<String>)>,
    claude: &HashMap<String, Vec<drivers::claude::ClaudeSession>>,
    session: &str,
) -> Vec<model::PaneSnapshot> {
    let Some((shape, _)) = shape else {
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

pub fn switch_claude_session(session_id: &str) -> Result<(), String> {
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
    let lane_id = if !zellij_session.is_empty() {
        let cfg = config::Config::load();
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

    let switch_result: Result<(), String> = (|| {
        if zellij_session.is_empty() {
            return Ok(());
        }

        // Resolve the tab through the same session -> tab-id cache everything
        // else uses, rather than the wezterm_tab_id recorded in the session
        // file at hook time (which came from the same unreliable title
        // matching we removed everywhere else).
        activate_wezterm_tab(&zellij_session, true)?;

        if let Some(pane_id) = zellij_pane_id {
            let output = std::process::Command::new("/opt/homebrew/bin/zellij")
                .args(["--session", &zellij_session, "action", "focus-pane-id", &pane_id.to_string()])
                .output()
                .map_err(|e| format!("zellij focus-pane-id: {}", e))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if !is_benign_zellij_focus_error(&stderr) {
                    return Err(format!("zellij focus-pane-id failed: {}", stderr.trim()));
                }
            }
        }

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

    switch_result
}

/// zellij's own focus-pane-id treats "the target pane is already focused"
/// as an error (exit 2), even though that's the desired end state, not a
/// failure - confirmed directly against zellij 0.44.3. Don't roll back a
/// switch that actually landed correctly just because of this quirk.
fn is_benign_zellij_focus_error(stderr: &str) -> bool {
    stderr.contains("already focused")
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

/// Cycle to the next (direction=1) or previous (direction=-1) live Claude
/// session, ordered to match lanes.toml's `order` (same ordering the display
/// uses), and within a shared Zellij session by each session's on-screen
/// reading-order position - Zellij tab left-to-right, then top-to-bottom/
/// left-to-right within that tab (see `pane_position_rank`) - falling back
/// to Zellij session name then session ID for any session whose lane isn't
/// listed in `order`, or whose pane position couldn't be resolved.
pub fn cycle_claude_session(direction: i32) -> Result<(), String> {
    let cfg = config::Config::load();
    let live_sessions = if cfg.driver_enabled("claude") {
        drivers::claude::enumerate()
    } else {
        vec![]
    };
    // Skip sessions that live in an inactive lane, or one with no cached
    // WezTerm tab-id (see lane_session_missing) - that's exactly what
    // caused the "wezterm activate-tab failed" cycling errors, since
    // there's nothing for a switch to actually land on.
    let live_sessions: Vec<_> = live_sessions.into_iter()
        .filter(|s| session_belongs_to_reachable_lane(s.zellij_session.as_deref(), &cfg))
        .collect();

    // Pane positions are only worth querying for sessions that actually
    // have a live Claude session in them - one list-panes call per distinct
    // Zellij session, not per Claude session.
    let mut positions: HashMap<String, HashMap<u32, (usize, i64, i64)>> = HashMap::new();
    for zellij_session in live_sessions.iter().filter_map(|s| s.zellij_session.as_deref()) {
        if positions.contains_key(zellij_session) {
            continue;
        }
        positions.insert(zellij_session.to_string(), drivers::zellij::pane_positions(zellij_session));
    }

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
        // round-trip was pure latency on the hot path.
        std::process::Command::new("open").args(["-a", "WezTerm"]).spawn().ok();
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

fn activate_window_facet(path: &str, zone: &str, cfg: &config::Config) -> Result<(), String> {
    let bundle_id = parse_bundle_id(path)
        .ok_or_else(|| format!("could not parse bundle id from path '{}'", path))?;

    let rect = zone::parse(zone)?;

    let uuid = cfg.monitor_uuid(&rect.monitor_handle)
        .ok_or_else(|| format!("monitor handle '{}' not found in config", rect.monitor_handle))?
        .to_string();

    let lua = format!(
        "local s=nil; \
         for _,sc in ipairs(hs.screen.allScreens()) do \
           if sc:getUUID()=='{uuid}' then s=sc; break end \
         end; \
         if s then \
           local apps=hs.application.applicationsForBundleID('{bundle}'); \
           local a=apps and apps[1]; \
           if a then \
             local w=a:mainWindow(); \
             if w then \
               local f=s:frame(); \
               w:setFrame({{x=f.x+{x}*f.w, y=f.y+{y}*f.h, w={ww}*f.w, h={h}*f.h}}) \
             end \
           end \
         end",
        uuid = uuid,
        bundle = bundle_id,
        x = rect.x,
        y = rect.y,
        ww = rect.w,
        h = rect.h,
    );

    match std::process::Command::new("/opt/homebrew/bin/hs").args(["-c", &lua]).output() {
        Err(e) => Err(format!("hs call failed for '{}': {}", bundle_id, e)),
        Ok(o) if !o.status.success() => {
            Err(format!("hs returned error for '{}':\n{}", bundle_id, String::from_utf8_lossy(&o.stderr)))
        }
        _ => Ok(())
    }
}

fn parse_bundle_id(path: &str) -> Option<String> {
    let first = path.split(" / ").next()?;
    let bundle = first.strip_prefix("app:")?;
    Some(bundle.trim().to_string())
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

    let mut warnings = Vec::new();
    for el in &lane.scope {
        if let Some(session) = el.zellij_session_name() {
            if let Err(e) = activate_wezterm_tab(session, focus) {
                eprintln!("warning: {}", e);
                warnings.push(e);
            }
        }
    }
    for w in &lane.windows {
        if let Err(e) = activate_window_facet(&w.path, &w.zone, &cfg) {
            eprintln!("warning: {}", e);
            warnings.push(e);
        }
    }

    state::set_focused_lane(lane_id);

    if warnings.is_empty() {
        Ok(())
    } else {
        Err(warnings.join("; "))
    }
}

fn expand_tilde(path: &str) -> String {
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
    fn parses_bundle_id() {
        assert_eq!(
            parse_bundle_id("app:com.github.wez.wezterm / window"),
            Some("com.github.wez.wezterm".to_string())
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
            windows: vec![],
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
    fn session_with_no_matching_lane_is_always_included_in_cycling() {
        // No I/O involved here - cfg.lane_for_session returns None before
        // session_belongs_to_reachable_lane ever reaches for state.kdl.
        let cfg = test_config(vec![model::Lane {
            id: "infra".to_string(),
            name: "Infra".to_string(),
            active: false,
            scope: vec![scope::ScopeElement::zellij_session("infra")],
            windows: vec![],
        }]);
        assert!(session_belongs_to_reachable_lane(Some("some-other-session"), &cfg));
        assert!(session_belongs_to_reachable_lane(None, &cfg));
    }

    #[test]
    fn active_lane_with_no_terminal_facet_is_not_session_missing() {
        // A repo-only lane has no session to be missing in the first place -
        // this goes through the real lane_session_missing() (not the pure
        // decision fn below) specifically to confirm it never even reaches
        // for state.kdl when there's no terminal facet at all.
        assert!(!lane_session_missing(true, &[model::FacetSnapshot::Repo { path: "/a/b".to_string(), signals: vec![] }]));
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

    #[test]
    fn zellij_backed_session_live_iff_session_running() {
        let live: HashSet<String> = ["lanes".to_string()].into_iter().collect();
        assert!(session_is_live_with("lanes", &live, None, |_| unreachable!("should not need pid lookup")));
        assert!(!session_is_live_with("job-hunting", &live, None, |_| unreachable!("should not need pid lookup")));
    }

    #[test]
    fn zellij_backed_session_dead_if_session_itself_is_gone_regardless_of_pid() {
        // Even a "live" pid shouldn't matter once the Zellij session itself is gone -
        // the session is checked first and short-circuits to dead.
        let live: HashSet<String> = HashSet::new();
        assert!(!session_is_live_with("lanes", &live, Some(123), |_| Some("claude".to_string())));
    }

    #[test]
    fn zellij_backed_session_dead_if_pid_no_longer_a_claude_process() {
        // Regression test: a Zellij session can outlive the Claude process that
        // originally occupied its pane (resume, restart, pid reused by the shell).
        // The session name alone isn't enough - the recorded pid must still resolve
        // to `claude`, or the registry entry is a stale leftover.
        let live: HashSet<String> = ["infra".to_string()].into_iter().collect();
        assert!(!session_is_live_with("infra", &live, Some(8547), |_| Some("fish".to_string())));
        assert!(!session_is_live_with("infra", &live, Some(4393), |_| None));
    }

    #[test]
    fn zellij_backed_session_live_if_pid_still_a_claude_process() {
        let live: HashSet<String> = ["infra".to_string()].into_iter().collect();
        assert!(session_is_live_with("infra", &live, Some(89568), |_| Some("claude".to_string())));
    }

    #[test]
    fn paneless_session_live_only_if_pid_is_a_claude_process() {
        let live: HashSet<String> = HashSet::new();
        assert!(session_is_live_with("", &live, Some(123), |_| Some("claude".to_string())));
        assert!(session_is_live_with("", &live, Some(123), |_| Some("/opt/homebrew/bin/claude".to_string())));
    }

    #[test]
    fn paneless_session_dead_if_pid_reused_by_other_process() {
        let live: HashSet<String> = HashSet::new();
        assert!(!session_is_live_with("", &live, Some(123), |_| Some("Slack".to_string())));
    }

    #[test]
    fn paneless_session_dead_if_pid_no_longer_exists() {
        let live: HashSet<String> = HashSet::new();
        assert!(!session_is_live_with("", &live, Some(123), |_| None));
    }

    #[test]
    fn paneless_session_dead_if_no_pid_recorded() {
        let live: HashSet<String> = HashSet::new();
        assert!(!session_is_live_with("", &live, None, |_| unreachable!("no pid to look up")));
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
    fn parses_bundle_id_bare() {
        assert_eq!(
            parse_bundle_id("app:org.mozilla.firefox / window"),
            Some("org.mozilla.firefox".to_string())
        );
    }

}
