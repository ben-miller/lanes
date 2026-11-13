//! Builds Lanes' live snapshot (`live_snapshot`) - the one place lane
//! snapshots get built, used by `switch-ui` and by `lanes snapshot`/
//! `lanes signals`. Used to be built from a `Mirror` of lensd's `lanes`
//! socket; as of PLAN-retire-lensd.md step 6, lensd is retired entirely -
//! everything here reads its own live sources directly (see [`LiveData`]),
//! not a daemon's derived copy of them.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::config::Config;
use crate::drivers::shell::CommandRecord;
use crate::model::{
    ClaudeSessionReason, FacetSnapshot, LaneSnapshot, LanesReason, LanewiseSnapshot, PaneKind, PaneSnapshot, RepoReason,
    Signal, SignalAction, SignalReason,
};

/// Boards' own `inbox_count(project, count)` facts, read directly over its
/// appctl socket - `project -> count`, straight from the owner, bypassing
/// lensd's derived `lanes_inbox_count` entirely (PLAN-retire-lensd.md step
/// 1: the first chip moved off the daemon). One-shot, not a live follow:
/// every caller already re-reads this on each snapshot (`lanes snapshot`/
/// `signals`, and switch-ui's polled `get_snapshot`), the same way they
/// already re-read `Config::load()` fresh each time - no separate watcher
/// needed. Boards not running, or genuinely having no projects, both just
/// mean an empty map - `board_signals` already treats "absent" as "no
/// signal", same as it did reading lensd's mirror.
pub fn boards_inbox_counts() -> HashMap<String, i64> {
    let Some(root) = appctl::default_state_root() else {
        return HashMap::new();
    };
    boards_inbox_counts_at(&root)
}

fn boards_inbox_counts_at(root: &std::path::Path) -> HashMap<String, i64> {
    let Ok(mut conn) = appctl::client::connect(root, "boards", None) else {
        return HashMap::new();
    };
    let Ok(rows) = conn.subscribe_facts() else {
        return HashMap::new();
    };
    rows.into_iter()
        .filter(|r| r.relation == "inbox_count")
        .filter_map(|r| {
            let mut values = r.values.into_iter();
            let project = match values.next()? {
                appctl::facts::FactValue::Str(s) => s,
                _ => return None,
            };
            let count = match values.next()? {
                appctl::facts::FactValue::Int(n) => n,
                _ => return None,
            };
            Some((project, count))
        })
        .collect()
}

/// Every running lane's zellij session, fetched once per snapshot (not
/// once per lane) via `drivers::zellij::panes` - the "fetch once up top"
/// half of `live_snapshot`'s `panes_by_session` parameter
/// (PLAN-retire-lensd.md step 4a). A session not in `running_sessions`
/// (`drivers::zellij::running_sessions`) is skipped - no point asking
/// Zellij for panes of a session that isn't running.
pub fn panes_by_session(cfg: &Config, running_sessions: &HashSet<String>) -> HashMap<String, Vec<crate::drivers::zellij::LivePane>> {
    cfg.lanes
        .iter()
        .filter_map(|lane| lane.terminal_session())
        .filter(|session| running_sessions.contains(*session))
        .map(|session| (session.to_string(), crate::drivers::zellij::panes(session)))
        .collect()
}

/// The dashboard's pane list for one zellij session - read live from
/// `drivers::zellij::panes` (PLAN-retire-lensd.md step 4a), not lensd's
/// derived `lanes_pane`/`lanes_pane_cmd`/`lanes_pane_cwd`/
/// `lanes_pane_focused` relations.
/// The dashboard's pane list for one zellij session, from an already-
/// fetched, pre-sorted `panes` list (`drivers::zellij::panes`, fetched
/// once per snapshot by the caller - same "fetch once up top, pass the
/// data down" shape as `inbox_counts`/`claude_sessions`, so this stays
/// pure and testable without shelling out). PLAN-retire-lensd.md step 4a.
fn panes_from_zellij(panes: &[crate::drivers::zellij::LivePane], any_claude_needs_you: bool) -> Vec<PaneSnapshot> {
    panes
        .iter()
        .map(|p| {
            let command = p.command.as_deref();
            let kind = match command {
                Some("claude") => PaneKind::ClaudeSession { awaiting: any_claude_needs_you },
                other => PaneKind::from_command(other),
            };
            PaneSnapshot { focused: p.focused, cwd: p.cwd.clone(), kind }
        })
        .collect()
}

/// This session's command records, from an already-fetched
/// `drivers::shell::enumerate()` list rather than lensd's derived
/// `lanes_command`/`lanes_command_cmd`/`lanes_command_ended`/
/// `lanes_command_exit` relations (PLAN-retire-lensd.md step 5). The shell
/// hook writes these files directly - lensd was always just relaying
/// Lanes' own data back to it.
fn command_records(records: &[CommandRecord], session: &str) -> Vec<CommandRecord> {
    records.iter().filter(|r| r.session == session).cloned().collect()
}

/// Claude-session signals for one zellij session - read from
/// `drivers::claude` (a live `enumerate()`/`watch()` scan) rather than
/// lensd's derived `lanes_claude` relation (PLAN-retire-lensd.md step 3).
/// Reading order ("same as the on-screen panes") now comes from the same
/// already-fetched `panes` list `panes_from_zellij` uses (step 4a) - the
/// last thing step 3 had deliberately left on the Mirror. `panes` is
/// already sorted in reading order, so a pane's position is just its
/// index in it.
fn claude_signals(sessions: &[crate::drivers::claude::ClaudeSession], panes: &[crate::drivers::zellij::LivePane], session: &str) -> Vec<Signal> {
    let position_of: HashMap<u32, usize> = panes.iter().enumerate().map(|(i, p)| (p.id, i)).collect();
    let mut matching: Vec<&crate::drivers::claude::ClaudeSession> =
        sessions.iter().filter(|cs| cs.zellij_session.as_deref() == Some(session)).collect();
    // Same reading order as the on-screen panes.
    matching.sort_by_key(|cs| {
        let idx = cs.zellij_pane_id.and_then(|p| position_of.get(&p).copied()).unwrap_or(usize::MAX);
        (idx, cs.session_id.clone())
    });
    matching
        .into_iter()
        .map(|cs| {
            let reason = match cs.state.as_str() {
                "idle" => ClaudeSessionReason::Awaiting,
                "permission_pending" => ClaudeSessionReason::Permission,
                _ => ClaudeSessionReason::Active,
            };
            Signal::new(
                SignalReason::ClaudeSession(reason),
                Some(SignalAction::SwitchClaudeSession { session_id: cs.session_id.clone() }),
                None,
            )
        })
        .collect()
}

/// This project's inbox count, if any - a signal only when it's nonzero
/// (an empty Inbox is a clean state, not a problem, same as a clean repo
/// never producing `PendingCommit`). Read straight from Boards' own
/// `inbox_count` facts (`boards_inbox_counts`), not lensd's derived
/// `lanes_inbox_count` - see PLAN-retire-lensd.md. A project absent from
/// the map is indistinguishable from a genuine zero here, same as before.
fn board_signals(inbox_counts: &HashMap<String, i64>, project: &str) -> Vec<Signal> {
    let count = inbox_counts.get(project).copied();
    match count {
        Some(n) if n > 0 => vec![Signal::new(
            SignalReason::Boards(crate::model::BoardsReason::InboxNonEmpty),
            None,
            Some(n.to_string()),
        )],
        _ => vec![],
    }
}

/// Dirty/non-default-branch signals for one repo - read from an already-
/// fetched `drivers::git::status` (via `repo_status`, keyed by resolved
/// path) rather than lensd's derived `lanes_repo_dirty`/`lanes_repo_branch`
/// relations (PLAN-retire-lensd.md step 5).
fn repo_signals(
    repo_status: &HashMap<String, crate::drivers::git::RepoStatus>,
    resolved_path: &str,
    raw_path: &str,
    session: Option<&str>,
) -> Vec<Signal> {
    let action = || session.map(|session| SignalAction::FocusRepoPane { session: session.to_string(), path: raw_path.to_string() });
    let mut out = Vec::new();
    let Some(status) = repo_status.get(resolved_path) else { return out };
    if status.dirty {
        out.push(Signal::new(SignalReason::Repo(RepoReason::PendingCommit), action(), None));
    }
    if let (Some(current), Some(default)) = (&status.branch, &status.default_branch) {
        if current != default {
            out.push(Signal::new(
                SignalReason::Repo(RepoReason::NonDefaultBranch),
                action(),
                Some(format!("on \"{current}\", default is \"{default}\"")),
            ));
        }
    }
    out
}

/// "Running build is stale" chips for the app instances whose own repo is
/// this lane repo (`lanes_build_changed`/`lanes_build_rebuilt`, lanes.dl).
/// One chip per instance and reason; a source-changed chip names the
/// changed sources (`src-tauri`, or `dev-infra/crates/appctl` for a path
/// dependency in another repo).
/// "Running build is stale" chips for one lane repo, from an already-
/// fetched `build_checks_by_repo` (one `appctl::build::check` per running
/// instance whose own repo is this one), rather than lensd's derived
/// `lanes_build_changed`/`lanes_build_rebuilt` relations
/// (PLAN-retire-lensd.md step 5). One chip per instance and reason, same
/// as before - a source-changed chip names every changed source in one
/// go (`appctl::build::Check`'s own `Source::Changed` already groups
/// them per instance, so no separate grouping step is needed here).
fn build_signals(checks: &[(String, String, appctl::build::Check)], resolved_path: &str) -> Vec<Signal> {
    use crate::model::BuildReason;
    let mut out = Vec::new();
    for (app, instance, check) in checks {
        if let appctl::build::Source::Changed { changed } = &check.source {
            let labels: BTreeSet<String> = changed
                .iter()
                .map(|c| {
                    let repo = resolve_path(&c.now.repo);
                    if repo == resolved_path {
                        if c.now.path.is_empty() { "source".to_string() } else { c.now.path.clone() }
                    } else {
                        let name = std::path::Path::new(&c.now.repo).file_name().and_then(|n| n.to_str()).unwrap_or(&c.now.repo);
                        if c.now.path.is_empty() { name.to_string() } else { format!("{name}/{}", c.now.path) }
                    }
                })
                .collect();
            let detail = format!("{app} {instance} · {}", labels.into_iter().collect::<Vec<_>>().join(", "));
            out.push(Signal::new(SignalReason::Build(BuildReason::SourceChanged), None, Some(detail)));
        }
        if check.rebuilt_since_launch == Some(true) {
            out.push(Signal::new(SignalReason::Build(BuildReason::RebuiltSinceLaunch), None, Some(format!("{app} {instance}"))));
        }
    }
    out
}

/// Everything `live_snapshot`/`lane_snapshot` read directly
/// instead of through lensd's derived relations, bundled into one value
/// instead of a growing positional-argument list (PLAN-retire-lensd.md
/// steps 1-4b, one field added per step): Boards' inbox counts
/// (`boards_inbox_counts`), Claude session state
/// (`drivers::claude::enumerate`), which zellij sessions are actually
/// running (`drivers::zellij::running_sessions`), their panes
/// (`drivers::zellij::panes`, via `panes_by_session`), and which zellij
/// sessions have an open WezTerm tab
/// (`drivers::wezterm::sessions_with_tabs`). Each caller fetches these
/// once per snapshot, not once per lane - the "fetch once up top, pass
/// pure data down" shape that keeps the Mirror-consuming functions
/// unit-testable without shelling out.
pub struct LiveData<'a> {
    pub inbox_counts: &'a HashMap<String, i64>,
    pub claude_sessions: &'a [crate::drivers::claude::ClaudeSession],
    pub running_sessions: &'a HashSet<String>,
    pub panes_by_session: &'a HashMap<String, Vec<crate::drivers::zellij::LivePane>>,
    pub tabbed_sessions: &'a HashSet<String>,
    pub shell_records: &'a [CommandRecord],
    pub repo_status: &'a HashMap<String, crate::drivers::git::RepoStatus>,
    pub build_checks: &'a HashMap<String, Vec<(String, String, appctl::build::Check)>>,
}

/// For each lane repo, every running `appctl` instance whose own repo is
/// that one, with its build staleness (`appctl::build::check`) - read
/// directly over each instance's existing `hello` (already in
/// `appctl::client::list`'s result, no extra round trip per instance)
/// rather than lensd's derived `lanes_build_changed`/`lanes_build_rebuilt`
/// (PLAN-retire-lensd.md step 5). Fetched once per snapshot, the "fetch
/// once up top" half of `LiveData::build_checks`.
pub fn build_checks_by_repo(cfg: &Config) -> HashMap<String, Vec<(String, String, appctl::build::Check)>> {
    let Some(root) = appctl::default_state_root() else {
        return HashMap::new();
    };
    let repo_paths: HashSet<String> = cfg.lanes.iter().flat_map(|lane| lane.scope.iter()).filter_map(|el| el.repo_path()).map(resolve_path).collect();
    let mut out: HashMap<String, Vec<(String, String, appctl::build::Check)>> = HashMap::new();
    for entry in appctl::client::list(&root) {
        let appctl::client::Status::Live(info) = entry.status else { continue };
        let Some(primary) = info.build.as_ref().and_then(|b| b.sources.first()) else { continue };
        let repo = resolve_path(&primary.repo);
        if !repo_paths.contains(&repo) {
            continue;
        }
        let check = appctl::build::check(&info);
        out.entry(repo).or_default().push((info.app.clone(), info.instance.clone(), check));
    }
    out
}

/// Every lane repo's git status, fetched once per snapshot (not once per
/// lane, and not once per *target* of a lane that lists the same repo
/// twice) via `drivers::git::status` - the "fetch once up top" half of
/// `LiveData::repo_status` (PLAN-retire-lensd.md step 5).
pub fn repo_status_by_path(cfg: &Config) -> HashMap<String, crate::drivers::git::RepoStatus> {
    cfg.lanes
        .iter()
        .flat_map(|lane| lane.scope.iter())
        .filter_map(|el| el.repo_path())
        .map(resolve_path)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|path| {
            let status = crate::drivers::git::status(&path);
            (path, status)
        })
        .collect()
}

/// The UI snapshot built from lensd's rows. Lane definitions come from
/// Lanes' config; signals and live state come from the mirror, except
/// what `live` carries - see [`LiveData`]. Lanes' own policy (dismissals,
/// disabled sessions, `cyclable`, `visible`, lifecycle) is applied here,
/// never by lensd.
pub fn live_snapshot(cfg: &Config, live: &LiveData) -> LanewiseSnapshot {
    let lanes = cfg.lanes.iter().map(|lane| lane_snapshot(lane, &cfg.shell_ignored_commands, live)).collect();
    LanewiseSnapshot {
        taken_at: chrono::Utc::now().to_rfc3339(),
        lanes,
        focused_lane: crate::state::read_focused_lane(),
        focused_claude_session: crate::state::read_claude_cursor(),
    }
}

/// `(session, pane, seconds_remaining)` for every `running` shell record
/// that exists right now but hasn't crossed `shell_notice_secs()` yet - so
/// its signal is correctly absent from `live_snapshot`'s output.
/// Nothing re-diffs a fact that hasn't itself changed, so without this
/// there'd be no second look once the floor actually passes - a command
/// that's still running at exactly the 3s mark would just never get a chip
/// until something unrelated happens to trigger a refetch. The caller
/// (switch-ui's Tauri backend) arms a one-shot timer per key this returns;
/// `seconds_remaining` is clamped to 0 defensively but should never
/// actually be negative, since this is the same elapsed-time check
/// `command_signal` itself uses to decide visibility.
pub fn pending_running_commands(cfg: &Config, shell_records: &[CommandRecord]) -> Vec<(String, u32, i64)> {
    let now: chrono::DateTime<chrono::FixedOffset> = chrono::Utc::now().into();
    cfg.lanes
        .iter()
        .filter_map(|lane| lane.terminal_session())
        .flat_map(|session| command_records(shell_records, session))
        .filter(|rec| rec.state == "running")
        .filter(|rec| !crate::is_shell_signal_ignored(rec.cmd.as_deref(), rec.argv0.as_deref(), &cfg.shell_ignored_commands))
        .filter_map(|rec| {
            let started = chrono::DateTime::parse_from_rfc3339(rec.started_at.as_deref()?).ok()?;
            let elapsed = (now - started).num_seconds();
            let remaining = crate::shell_notice_secs() - elapsed;
            (remaining > 0).then_some((rec.session, rec.pane, remaining))
        })
        .collect()
}

fn lane_snapshot(lane: &crate::model::Lane, shell_ignored_commands: &[String], live: &LiveData) -> LaneSnapshot {
    let empty_panes = Vec::new();
    let nav_session = lane.scope.iter().find_map(|el| el.zellij_session_name());
    let mut facets: Vec<FacetSnapshot> = lane
        .scope
        .iter()
        .map(|el| {
            if let Some(session) = el.zellij_session_name() {
                let running = live.running_sessions.contains(session);
                let live_panes = live.panes_by_session.get(session).unwrap_or(&empty_panes);
                let claude = if running { claude_signals(live.claude_sessions, live_panes, session) } else { vec![] };
                let needs_you = live
                    .claude_sessions
                    .iter()
                    .any(|cs| cs.zellij_session.as_deref() == Some(session) && matches!(cs.state.as_str(), "idle" | "permission_pending"));
                let panes = if running { panes_from_zellij(live_panes, needs_you) } else { vec![] };
                FacetSnapshot::Terminal { session: session.to_string(), running, panes, signals: claude }
            } else {
                let raw = el.repo_path().unwrap_or_default();
                let resolved = resolve_path(raw);
                let mut signals = repo_signals(live.repo_status, &resolved, raw, nav_session);
                let empty_checks = Vec::new();
                let checks = live.build_checks.get(&resolved).unwrap_or(&empty_checks);
                signals.extend(build_signals(checks, &resolved));
                FacetSnapshot::Repo { path: raw.to_string(), signals }
            }
        })
        .collect();
    facets.extend(lane.targets.iter().map(|t| FacetSnapshot::Target { driver: t.driver.name().to_string() }));
    facets.extend(lane.targets.iter().find_map(|t| match &t.driver {
        crate::model::TargetDriver::Boards { project } => {
            Some(FacetSnapshot::Board { project: project.clone(), signals: board_signals(live.inbox_counts, project) })
        }
        _ => None,
    }));

    // "Reachable" is whether a WezTerm tab is showing the lane's session
    // right now - read live from `drivers::wezterm::sessions_with_tabs`
    // (PLAN-retire-lensd.md step 4b), not Lanes' cached tab id, and not
    // lensd's derived `lanes_tab` relation anymore either.
    let has_tab = lane.terminal_session().is_some_and(|s| live.tabbed_sessions.contains(s));
    let reachable = facets.iter().any(|f| matches!(f, FacetSnapshot::Terminal { .. })).then_some(has_tab);

    if let Some(FacetSnapshot::Terminal { session, signals, .. }) =
        facets.iter_mut().find(|f| matches!(f, FacetSnapshot::Terminal { .. }))
    {
        // Both conditions below are now derived straight from
        // `tabbed_sessions`/`running_sessions` (steps 4a/4b) rather than
        // lensd's `lanes_session_missing`/`lanes_session_not_running` -
        // the same facts, read directly instead of through the Mirror.
        // `active` still gates SessionMissing client-side: a session being
        // missing only matters for a lane still in the cycling set, not
        // one the user has deliberately deactivated.
        if lane.active && !has_tab {
            signals.push(Signal::new(
                SignalReason::Lanes(LanesReason::SessionMissing),
                None,
                Some(format!("no WezTerm tab showing zellij session \"{session}\"")),
            ));
        }
        if !live.running_sessions.contains(session.as_str()) {
            signals.push(Signal::new(
                SignalReason::Lanes(LanesReason::SessionNotRunning),
                None,
                Some(format!("expected zellij session \"{session}\" to be running")),
            ));
        }
        for rec in command_records(live.shell_records, session) {
            if crate::state::is_signal_dismissed(&rec.occurrence_id()) {
                continue;
            }
            if crate::is_shell_signal_ignored(rec.cmd.as_deref(), rec.argv0.as_deref(), shell_ignored_commands) {
                continue;
            }
            if let Some(sig) = crate::command_signal(&rec, chrono::Utc::now().into()) {
                signals.push(sig);
            }
        }
    }

    let has_claude_signal =
        facets.iter().any(|f| f.signals().iter().any(|sig| sig.kind() == crate::model::SignalKind::ClaudeSession));
    let cyclable = crate::lane_cyclable(lane.active, reachable, has_claude_signal);
    crate::correct_signals(&mut facets, cyclable);

    LaneSnapshot { id: lane.id.clone(), name: lane.name.clone(), active: lane.active, cyclable, facets }
}

fn resolve_path(path: &str) -> String {
    let expanded = crate::expand_tilde(path);
    let trimmed = expanded.trim_end_matches('/');
    std::fs::canonicalize(trimmed).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn boards_inbox_counts_at_reads_a_real_boards_instances_facts() {
        // Short /tmp path, not std::env::temp_dir()'s long /var/folders/...
        // one - macOS caps unix socket paths at ~104 bytes (same reason
        // boards' own appctl test uses a short /tmp path).
        let state = std::path::PathBuf::from(format!("/tmp/lic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        let handle = appctl::start(appctl::config!("boards").state_dir(&state).facts(&[("inbox_count", &["project", "count"])])).expect("start appctl");
        handle
            .facts()
            .replace("inbox_count", [vec!["Infra".into(), 3i64.into()], vec!["Lanes".into(), 0i64.into()]])
            .unwrap();

        // The facts server applies a replace asynchronously; poll briefly
        // rather than racing it (same pattern other appctl-backed tests in
        // this codebase use).
        let mut counts = HashMap::new();
        for _ in 0..50 {
            counts = boards_inbox_counts_at(&state);
            if !counts.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(counts.get("Infra"), Some(&3));
        assert_eq!(counts.get("Lanes"), Some(&0));

        handle.shutdown();
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn boards_inbox_counts_at_is_empty_when_boards_is_not_running() {
        let state = std::env::temp_dir().join(format!("lanes-inbox-counts-test-absent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        assert_eq!(boards_inbox_counts_at(&state), HashMap::new());
    }

    fn cfg_with(lane: crate::model::Lane) -> Config {
        Config {
            drivers: None,
            monitors: HashMap::new(),
            order: None,
            indented: Vec::new(),
            cycle_policy: crate::model::CyclePolicy::default(),
            contexts: Vec::new(),
            lanes: vec![lane],
            boards_targets: Vec::new(),
            context_ids: Vec::new(),
            shell_ignored_commands: Vec::new(),
        }
    }

    fn lane() -> crate::model::Lane {
        crate::model::Lane {
            id: "l".into(),
            name: "L".into(),
            active: true,
            scope: vec![
                crate::scope::ScopeElement::zellij_session("z"),
                crate::scope::ScopeElement::repo("/p/infra"),
            ],
            targets: vec![],
            layout_template: None,
        }
    }

    fn lane_with_boards_target(project: &str) -> crate::model::Lane {
        crate::model::Lane {
            targets: vec![crate::model::Target {
                driver: crate::model::TargetDriver::Boards { project: project.to_string() },
                monitor: None,
                position: None,
                position_solo: None,
                launch: false,
                raise: false,
                minimized: None,
                primary: false,
            }],
            ..lane()
        }
    }

    fn board_facet(snap: &LanewiseSnapshot) -> (&str, &Vec<Signal>) {
        snap.lanes[0]
            .facets
            .iter()
            .find_map(|f| match f {
                FacetSnapshot::Board { project, signals } => Some((project.as_str(), signals)),
                _ => None,
            })
            .expect("expected a Board facet")
    }

    /// A `drivers::claude::ClaudeSession` for tests - built directly rather
    /// than through a daemon's derived row (PLAN-retire-lensd.md step 3).
    fn claude_session(session_id: &str, state: &str, zellij_session: &str, pane_id: u32) -> crate::drivers::claude::ClaudeSession {
        crate::drivers::claude::ClaudeSession {
            session_id: session_id.to_string(),
            zellij_session: Some(zellij_session.to_string()),
            state: state.to_string(),
            zellij_pane_id: Some(pane_id),
        }
    }

    /// A `drivers::zellij::LivePane` for tests, built directly rather than
    /// through a daemon's derived row (PLAN-retire-lensd.md step 4a).
    fn live_pane(id: u32, focused: bool, cwd: Option<&str>, command: Option<&str>) -> crate::drivers::zellij::LivePane {
        crate::drivers::zellij::LivePane { id, focused, cwd: cwd.map(str::to_string), command: command.map(str::to_string) }
    }

    /// A `drivers::shell::CommandRecord` for tests, built directly rather
    /// than through a daemon's derived row (PLAN-retire-lensd.md step 5).
    #[allow(clippy::too_many_arguments)]
    fn cmd_record(session: &str, pane: u32, state: &str, started_at: &str, cmd: Option<&str>, ended_at: Option<&str>, exit_code: Option<i32>) -> CommandRecord {
        CommandRecord {
            v: 1,
            shell: None,
            session: session.to_string(),
            pane,
            argv0: None,
            cmd: cmd.map(str::to_string),
            started_at: Some(started_at.to_string()),
            state: state.to_string(),
            ended_at: ended_at.map(str::to_string),
            exit_code,
        }
    }

    fn terminal(snap: &LanewiseSnapshot) -> (&bool, &Vec<PaneSnapshot>, &Vec<Signal>) {
        match &snap.lanes[0].facets[0] {
            FacetSnapshot::Terminal { running, panes, signals, .. } => (running, panes, signals),
            other => panic!("expected terminal, got {}", serde_json::to_string(&other.signals().len()).unwrap()),
        }
    }

    #[test]
    fn a_reachable_lane_with_an_idle_claude_has_a_ready_cyclable_signal_in_screen_order() {
        let sessions = vec![claude_session("s-low", "running", "z", 7), claude_session("s-high", "idle", "z", 3)];
        // pane 3 (focused, cwd /a) is in reading order before pane 7.
        let panes = vec![live_pane(3, true, Some("/a"), Some("claude")), live_pane(7, false, None, Some("claude"))];
        let running = HashSet::from(["z".to_string()]);
        let panes_by_session = HashMap::from([("z".to_string(), panes)]);
        let tabbed = HashSet::from(["z".to_string()]);
        let empty_counts = HashMap::new();
        let live = LiveData {
            inbox_counts: &empty_counts,
            claude_sessions: &sessions,
            running_sessions: &running,
            panes_by_session: &panes_by_session,
            tabbed_sessions: &tabbed,
            shell_records: &[],
            repo_status: &HashMap::new(),
            build_checks: &HashMap::new(),
        };
        let snap = live_snapshot(&cfg_with(lane()), &live);
        let (running, panes, signals) = terminal(&snap);
        assert!(*running);
        assert_eq!(panes.len(), 2);
        assert!(panes[0].focused && panes[0].cwd.as_deref() == Some("/a"), "top pane first");
        assert!(matches!(panes[0].kind, PaneKind::ClaudeSession { awaiting: true }));

        let ids: Vec<_> = signals
            .iter()
            .map(|sg| match &sg.action {
                Some(SignalAction::SwitchClaudeSession { session_id }) => session_id.as_str(),
                _ => "?",
            })
            .collect();
        assert_eq!(ids, ["s-high", "s-low"]);
        assert!(snap.lanes[0].cyclable);
        assert!(matches!(signals[0].reason, SignalReason::ClaudeSession(ClaudeSessionReason::Ready)), "idle in a cyclable lane is Ready");
        assert!(signals[0].cyclable && signals[0].visible);
        assert!(matches!(signals[1].reason, SignalReason::ClaudeSession(ClaudeSessionReason::Active)));
    }

    #[test]
    fn a_lane_without_a_wezterm_tab_is_session_missing_and_not_cyclable() {
        let sessions = vec![claude_session("s-low", "running", "z", 7), claude_session("s-high", "idle", "z", 3)];
        let running = HashSet::from(["z".to_string()]);
        let empty_counts = HashMap::new();
        let empty_panes = HashMap::new();
        let live = LiveData {
            inbox_counts: &empty_counts,
            claude_sessions: &sessions,
            running_sessions: &running,
            panes_by_session: &empty_panes,
            tabbed_sessions: &HashSet::new(),
            shell_records: &[],
            repo_status: &HashMap::new(),
            build_checks: &HashMap::new(),
        };
        let snap = live_snapshot(&cfg_with(lane()), &live);
        let (_, _, signals) = terminal(&snap);
        assert!(!snap.lanes[0].cyclable);
        assert!(signals.iter().any(|sg| matches!(sg.reason, SignalReason::Lanes(LanesReason::SessionMissing)) && sg.visible));
        assert!(
            signals.iter().any(|sg| matches!(sg.reason, SignalReason::ClaudeSession(ClaudeSessionReason::Awaiting))),
            "not Ready when no cycle can land there"
        );
    }

    #[test]
    fn a_session_that_is_not_running_has_no_panes_and_a_not_running_signal() {
        let snap = live_snapshot(&cfg_with(lane()), &LiveData {
            inbox_counts: &HashMap::new(),
            claude_sessions: &[],
            running_sessions: &HashSet::new(),
            panes_by_session: &HashMap::new(),
            tabbed_sessions: &HashSet::new(),
            shell_records: &[],
            repo_status: &HashMap::new(),
            build_checks: &HashMap::new(),
        });
        let (running, panes, signals) = terminal(&snap);
        assert!(!*running && panes.is_empty());
        assert!(signals.iter().any(|sg| matches!(sg.reason, SignalReason::Lanes(LanesReason::SessionNotRunning))));
    }

    #[test]
    fn a_nonempty_inbox_count_becomes_a_boards_signal() {
        let counts = HashMap::from([("Boards".to_string(), 3)]);
        let snap = live_snapshot(&cfg_with(lane_with_boards_target("Boards")), &LiveData {
            inbox_counts: &counts,
            claude_sessions: &[],
            running_sessions: &HashSet::new(),
            panes_by_session: &HashMap::new(),
            tabbed_sessions: &HashSet::new(),
            shell_records: &[],
            repo_status: &HashMap::new(),
            build_checks: &HashMap::new(),
        });
        let (project, signals) = board_facet(&snap);
        assert_eq!(project, "Boards");
        assert_eq!(signals.len(), 1);
        assert!(matches!(signals[0].reason, SignalReason::Boards(crate::model::BoardsReason::InboxNonEmpty)));
        assert_eq!(signals[0].detail.as_deref(), Some("3"));
    }

    #[test]
    fn a_zero_inbox_count_produces_no_signal() {
        let counts = HashMap::from([("Boards".to_string(), 0)]);
        let snap = live_snapshot(&cfg_with(lane_with_boards_target("Boards")), &LiveData {
            inbox_counts: &counts,
            claude_sessions: &[],
            running_sessions: &HashSet::new(),
            panes_by_session: &HashMap::new(),
            tabbed_sessions: &HashSet::new(),
            shell_records: &[],
            repo_status: &HashMap::new(),
            build_checks: &HashMap::new(),
        });
        let (project, signals) = board_facet(&snap);
        assert_eq!(project, "Boards");
        assert!(signals.is_empty());
    }

    #[test]
    fn no_inbox_count_row_at_all_still_yields_the_board_facet_with_no_signal() {
        let snap = live_snapshot(&cfg_with(lane_with_boards_target("Boards")), &LiveData {
            inbox_counts: &HashMap::new(),
            claude_sessions: &[],
            running_sessions: &HashSet::new(),
            panes_by_session: &HashMap::new(),
            tabbed_sessions: &HashSet::new(),
            shell_records: &[],
            repo_status: &HashMap::new(),
            build_checks: &HashMap::new(),
        });
        let (project, signals) = board_facet(&snap);
        assert_eq!(project, "Boards");
        assert!(signals.is_empty());
    }

    #[test]
    fn a_lane_with_no_boards_target_has_no_board_facet() {
        let snap = live_snapshot(&cfg_with(lane()), &LiveData {
            inbox_counts: &HashMap::new(),
            claude_sessions: &[],
            running_sessions: &HashSet::new(),
            panes_by_session: &HashMap::new(),
            tabbed_sessions: &HashSet::new(),
            shell_records: &[],
            repo_status: &HashMap::new(),
            build_checks: &HashMap::new(),
        });
        assert!(!snap.lanes[0].facets.iter().any(|f| matches!(f, FacetSnapshot::Board { .. })));
    }

    #[test]
    fn finished_commands_carry_their_detail_and_dismiss_id() {
        let records = vec![
            cmd_record("z", 3, "failed", "2026-01-01T00:00:00Z", Some("cargo test"), Some("2026-01-01T00:00:07Z"), Some(101)),
            // Long since past shell_notice_secs() (real time has moved on
            // from this fixed timestamp), so this one does become a signal
            // too - see running_command_only_becomes_a_signal_past_the_
            // notice_floor for the still-fresh case.
            cmd_record("z", 7, "running", "2026-01-01T00:00:00Z", Some("vite"), None, None),
        ];
        let snap = live_snapshot(&cfg_with(lane()), &LiveData {
            inbox_counts: &HashMap::new(),
            claude_sessions: &[],
            running_sessions: &HashSet::new(),
            panes_by_session: &HashMap::new(),
            tabbed_sessions: &HashSet::new(),
            shell_records: &records,
            repo_status: &HashMap::new(),
            build_checks: &HashMap::new(),
        });
        let (_, _, signals) = terminal(&snap);
        let cmds: Vec<_> = signals.iter().filter(|sg| sg.kind() == crate::model::SignalKind::Command).collect();
        assert_eq!(cmds.len(), 2);
        let failed = cmds.iter().find(|sg| matches!(sg.reason, SignalReason::Command(crate::model::CommandReason::Failed))).unwrap();
        assert_eq!(failed.detail.as_deref(), Some("cargo test · exit 101 · 7s"));
        assert_eq!(failed.dismiss_id.as_deref(), Some("command:z--3--2026-01-01T00:00:00Z"));
        assert!(failed.visible && !failed.cyclable);
        let running = cmds.iter().find(|sg| matches!(sg.reason, SignalReason::Command(crate::model::CommandReason::Running))).unwrap();
        assert!(running.visible && !running.cyclable);
    }

    #[test]
    fn running_command_only_becomes_a_signal_past_the_notice_floor() {
        let just_started = chrono::Utc::now().to_rfc3339();
        let records = vec![cmd_record("z", 7, "running", &just_started, None, None, None)];
        let snap = live_snapshot(&cfg_with(lane()), &LiveData {
            inbox_counts: &HashMap::new(),
            claude_sessions: &[],
            running_sessions: &HashSet::new(),
            panes_by_session: &HashMap::new(),
            tabbed_sessions: &HashSet::new(),
            shell_records: &records,
            repo_status: &HashMap::new(),
            build_checks: &HashMap::new(),
        });
        let (_, _, signals) = terminal(&snap);
        assert!(
            !signals.iter().any(|sg| sg.kind() == crate::model::SignalKind::Command),
            "just started - under the notice floor, no chip yet"
        );
    }

    #[test]
    fn lane_snapshot_honors_shell_ignored_commands_not_just_gather_lanes() {
        // Regression: lane_snapshot (the actual live-dashboard path via
        // live_snapshot) used to build Command signals through its
        // own direct crate::command_signal call, bypassing
        // is_shell_signal_ignored entirely - a config entry excluding a
        // command worked for `lanes snapshot` (gather_lanes) but not for
        // the running switch-ui app, which only ever calls this path.
        let records =
            vec![cmd_record("z", 7, "failed", "2026-01-01T00:00:00Z", Some("claude --resume boards"), Some("2026-01-01T00:00:07Z"), Some(1))];
        let mut cfg = cfg_with(lane());
        cfg.shell_ignored_commands = vec!["claude*".to_string()];
        let snap = live_snapshot(&cfg, &LiveData {
            inbox_counts: &HashMap::new(),
            claude_sessions: &[],
            running_sessions: &HashSet::new(),
            panes_by_session: &HashMap::new(),
            tabbed_sessions: &HashSet::new(),
            shell_records: &records,
            repo_status: &HashMap::new(),
            build_checks: &HashMap::new(),
        });
        let (_, _, signals) = terminal(&snap);
        assert!(!signals.iter().any(|sg| sg.kind() == crate::model::SignalKind::Command));
    }

    #[test]
    fn pending_running_commands_excludes_an_ignored_command_even_under_the_floor() {
        let just_started = chrono::Utc::now().to_rfc3339();
        let records = vec![cmd_record("z", 7, "running", &just_started, Some("claude --resume boards"), None, None)];
        let mut cfg = cfg_with(lane());
        cfg.shell_ignored_commands = vec!["claude*".to_string()];
        assert!(pending_running_commands(&cfg, &records).is_empty());
    }

    #[test]
    fn pending_running_commands_finds_fresh_running_records_under_the_floor() {
        let just_started = chrono::Utc::now().to_rfc3339();
        let records = vec![cmd_record("z", 7, "running", &just_started, None, None, None)];
        let cfg = cfg_with(lane());
        let pending = pending_running_commands(&cfg, &records);
        assert_eq!(pending.len(), 1);
        let (session, pane, remaining) = &pending[0];
        assert_eq!(session, "z");
        assert_eq!(*pane, 7);
        assert!((1..=3).contains(remaining), "remaining={remaining}");
    }

    #[test]
    fn pending_running_commands_excludes_one_already_past_the_floor() {
        // Started well over shell_notice_secs() ago - already a real
        // signal, not pending.
        let records = vec![cmd_record("z", 7, "running", "2026-01-01T00:00:00Z", None, None, None)];
        let cfg = cfg_with(lane());
        assert!(pending_running_commands(&cfg, &records).is_empty());
    }

    #[test]
    fn pending_running_commands_ignores_done_and_failed_records() {
        let just_started = chrono::Utc::now().to_rfc3339();
        let records = vec![cmd_record("z", 7, "done", &just_started, None, None, None)];
        let cfg = cfg_with(lane());
        assert!(pending_running_commands(&cfg, &records).is_empty());
    }

    #[test]
    fn repo_facets_get_dirty_and_branch_signals_that_focus_the_lanes_session() {
        let repo_status = HashMap::from([(
            "/p/infra".to_string(),
            crate::drivers::git::RepoStatus { dirty: true, branch: Some("feat".to_string()), default_branch: Some("main".to_string()) },
        )]);
        let snap = live_snapshot(&cfg_with(lane()), &LiveData {
            inbox_counts: &HashMap::new(),
            claude_sessions: &[],
            running_sessions: &HashSet::new(),
            panes_by_session: &HashMap::new(),
            tabbed_sessions: &HashSet::new(),
            shell_records: &[],
            repo_status: &repo_status,
            build_checks: &HashMap::new(),
        });
        let FacetSnapshot::Repo { path, signals } = &snap.lanes[0].facets[1] else { panic!("repo facet") };
        assert_eq!(path, "/p/infra");
        assert_eq!(signals.len(), 2);
        assert!(matches!(signals[0].reason, SignalReason::Repo(RepoReason::PendingCommit)));
        assert_eq!(signals[1].detail.as_deref(), Some("on \"feat\", default is \"main\""));
        assert!(matches!(&signals[0].action, Some(SignalAction::FocusRepoPane { session, path }) if session == "z" && path == "/p/infra"));
    }

    fn source_tree(repo: &str, path: &str) -> appctl::build::SourceTree {
        appctl::build::SourceTree {
            repo: repo.to_string(),
            path: path.to_string(),
            commit: None,
            tree: String::new(),
            dirty: false,
            exclude: Vec::new(),
        }
    }

    fn changed(now_repo: &str, now_path: &str) -> appctl::build::Changed {
        appctl::build::Changed { built: source_tree(now_repo, now_path), now: source_tree(now_repo, now_path) }
    }

    #[test]
    fn stale_builds_get_visible_warning_chips_on_the_owning_repo_facet() {
        // Two of the app's own sources changed, plus one in another repo.
        let check = appctl::build::Check {
            source: appctl::build::Source::Changed {
                changed: vec![changed("/p/infra", "src-tauri"), changed("/s/dev-infra", "crates/appctl")],
            },
            rebuilt_since_launch: Some(true),
        };
        let build_checks = HashMap::from([("/p/infra".to_string(), vec![("boards".to_string(), "dev-main".to_string(), check)])]);
        let snap = live_snapshot(&cfg_with(lane()), &LiveData {
            inbox_counts: &HashMap::new(),
            claude_sessions: &[],
            running_sessions: &HashSet::new(),
            panes_by_session: &HashMap::new(),
            tabbed_sessions: &HashSet::new(),
            shell_records: &[],
            repo_status: &HashMap::new(),
            build_checks: &build_checks,
        });
        let FacetSnapshot::Repo { signals, .. } = &snap.lanes[0].facets[1] else { panic!("repo facet") };
        let builds: Vec<&Signal> = signals.iter().filter(|s| s.kind() == crate::model::SignalKind::Build).collect();
        assert_eq!(builds.len(), 2, "{:?}", builds.iter().map(|s| &s.detail).collect::<Vec<_>>());

        assert!(matches!(builds[0].reason, SignalReason::Build(crate::model::BuildReason::SourceChanged)));
        assert_eq!(builds[0].detail.as_deref(), Some("boards dev-main · dev-infra/crates/appctl, src-tauri"));
        assert!(matches!(builds[1].reason, SignalReason::Build(crate::model::BuildReason::RebuiltSinceLaunch)));
        assert_eq!(builds[1].detail.as_deref(), Some("boards dev-main"));
        for chip in builds {
            assert_eq!(chip.urgency, crate::model::Urgency::Warning);
            assert!(chip.visible, "shown whether or not the lane is cyclable");
            assert!(chip.action.is_none());
        }
    }
}
