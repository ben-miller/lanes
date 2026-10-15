//! Client for lensd's `lanes` socket. [`Client`] keeps a [`Mirror`] of the
//! rule set's rows current on a background thread and reconnects when lensd
//! goes away. The rest of the file is the spike that compares what lensd
//! implies against `gather_lanes()` (`lensd-compare`), removed with it.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use crate::config::Config;
use crate::drivers::shell::CommandRecord;
use crate::model::{
    ClaudeSessionReason, FacetSnapshot, LaneSnapshot, LanesReason, LanewiseSnapshot, PaneKind, PaneSnapshot, RepoReason,
    Signal, SignalAction, SignalReason,
};

pub type Row = Vec<Value>;

/// The current rows per relation, rebuilt from a snapshot and kept current by diffs.
#[derive(Default, Debug, Clone)]
pub struct Mirror {
    pub rels: HashMap<String, HashSet<Row>>,
    pub epoch: Option<u64>,
}

impl Mirror {
    /// Applies one newline-delimited protocol message. Unknown messages are ignored.
    pub fn apply(&mut self, line: &str) {
        let Ok(msg) = serde_json::from_str::<Value>(line) else { return };
        if let Some(snap) = msg.get("snapshot") {
            self.rels.clear();
            self.epoch = snap.get("epoch").and_then(Value::as_u64);
            for row in snap.get("rows").and_then(Value::as_array).into_iter().flatten() {
                if let Some((rel, values)) = relation_values(row) {
                    self.rels.entry(rel).or_default().insert(values);
                }
            }
        } else if let Some(diff) = msg.get("diff") {
            self.epoch = diff.get("epoch").and_then(Value::as_u64);
            for ch in diff.get("changes").and_then(Value::as_array).into_iter().flatten() {
                let Some((rel, values)) = relation_values(ch) else { continue };
                let rows = self.rels.entry(rel).or_default();
                if ch.get("weight").and_then(Value::as_i64).unwrap_or(0) > 0 {
                    rows.insert(values);
                } else {
                    rows.remove(&values);
                }
            }
        }
    }

    fn rows(&self, rel: &str) -> impl Iterator<Item = &Row> {
        self.rels.get(rel).into_iter().flatten()
    }
}

fn relation_values(v: &Value) -> Option<(String, Row)> {
    Some((v.get("relation")?.as_str()?.to_string(), v.get("values")?.as_array()?.clone()))
}

pub fn default_socket() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local/state/lensd/lanes.sock")
}

#[derive(Default)]
struct Shared {
    mirror: Mirror,
    connected: bool,
    version: u64,
}

/// A live view of lensd's `lanes` stream. Not connected means lensd is not
/// running (or restarting): there is no data, and no fallback.
#[derive(Clone)]
pub struct Client {
    shared: Arc<Mutex<Shared>>,
}

const BACKOFF_START: Duration = Duration::from_millis(100);
const BACKOFF_MAX: Duration = Duration::from_secs(5);

impl Client {
    /// Starts the background connection. `on_change` runs on that thread after
    /// every snapshot, diff and connect/disconnect, with no lock held.
    pub fn start(path: PathBuf, on_change: impl Fn() + Send + 'static) -> Client {
        Self::start_with_backoff(path, BACKOFF_START, BACKOFF_MAX, on_change)
    }

    pub fn start_with_backoff(
        path: PathBuf,
        backoff_start: Duration,
        backoff_max: Duration,
        on_change: impl Fn() + Send + 'static,
    ) -> Client {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let client = Client { shared: shared.clone() };
        std::thread::spawn(move || {
            let mut backoff = backoff_start;
            loop {
                if let Ok(stream) = UnixStream::connect(&path) {
                    if stream_into(stream, &shared, &on_change) {
                        backoff = backoff_start;
                    }
                    let mut g = shared.lock().unwrap();
                    g.mirror = Mirror::default();
                    g.connected = false;
                    g.version += 1;
                    drop(g);
                    on_change();
                }
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(backoff_max);
            }
        });
        client
    }

    /// The current rows, or `None` while not connected.
    pub fn current(&self) -> Option<Mirror> {
        let g = self.shared.lock().unwrap();
        g.connected.then(|| g.mirror.clone())
    }

    pub fn connected(&self) -> bool {
        self.shared.lock().unwrap().connected
    }

    /// Increases on every change, including connect and disconnect.
    pub fn version(&self) -> u64 {
        self.shared.lock().unwrap().version
    }
}

/// Reads one connection until it ends. Returns whether a snapshot arrived.
fn stream_into(stream: UnixStream, shared: &Mutex<Shared>, on_change: &impl Fn()) -> bool {
    let mut got_snapshot = false;
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        {
            let mut g = shared.lock().unwrap();
            g.mirror.apply(&line);
            g.connected = g.mirror.epoch.is_some();
            got_snapshot |= g.connected;
            g.version += 1;
        }
        on_change();
    }
    got_snapshot
}

/// Connects and reads until the snapshot has arrived.
pub fn read_snapshot(path: &std::path::Path, timeout: Duration) -> std::io::Result<Mirror> {
    let stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(timeout))?;
    let mut mirror = Mirror::default();
    for line in BufReader::new(stream).lines() {
        let line = line?;
        mirror.apply(&line);
        if mirror.epoch.is_some() {
            return Ok(mirror);
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "socket closed before a snapshot"))
}

fn int(v: &Value) -> i64 {
    v.as_i64().unwrap_or(-1)
}

/// Rows of `rel` whose first column is `lane` and second is `session`.
fn rows_for<'a>(m: &'a Mirror, rel: &str, lane: &'a str, session: &'a str) -> impl Iterator<Item = &'a Row> {
    m.rows(rel).filter(move |r| s(&r[0]) == lane && s(&r[1]) == session)
}

/// `(pane) -> value` for one of the `lanes_pane_*` / `lanes_command_*` side relations.
fn by_pane<'a>(m: &'a Mirror, rel: &str, lane: &'a str, session: &'a str) -> HashMap<i64, &'a Value> {
    rows_for(m, rel, lane, session).map(|r| (int(&r[2]), &r[3])).collect()
}

fn panes_from_mirror(m: &Mirror, lane: &str, session: &str, any_claude_needs_you: bool) -> Vec<PaneSnapshot> {
    let cmds = by_pane(m, "lanes_pane_cmd", lane, session);
    let cwds = by_pane(m, "lanes_pane_cwd", lane, session);
    let focused: HashSet<i64> = rows_for(m, "lanes_pane_focused", lane, session).map(|r| int(&r[2])).collect();
    let mut panes: Vec<_> = rows_for(m, "lanes_pane", lane, session)
        .map(|r| ((int(&r[3]), int(&r[6]), int(&r[5])), int(&r[2])))
        .collect();
    panes.sort();
    panes
        .into_iter()
        .map(|(_, id)| {
            let command = cmds.get(&id).map(|v| s(v));
            let kind = match command {
                Some("claude") => PaneKind::ClaudeSession { awaiting: any_claude_needs_you },
                other => PaneKind::from_command(other),
            };
            PaneSnapshot { focused: focused.contains(&id), cwd: cwds.get(&id).map(|v| s(v).to_string()), kind }
        })
        .collect()
}

fn command_records(m: &Mirror, lane: &str, session: &str) -> Vec<CommandRecord> {
    let cmds = by_pane(m, "lanes_command_cmd", lane, session);
    let ended = by_pane(m, "lanes_command_ended", lane, session);
    let exits = by_pane(m, "lanes_command_exit", lane, session);
    rows_for(m, "lanes_command", lane, session)
        .map(|r| {
            let pane = int(&r[2]);
            CommandRecord {
                v: 1,
                shell: None,
                session: session.to_string(),
                pane: pane.max(0) as u32,
                argv0: None,
                cmd: cmds.get(&pane).map(|v| s(v).to_string()),
                started_at: Some(s(&r[4]).to_string()),
                state: s(&r[3]).to_string(),
                ended_at: ended.get(&pane).map(|v| s(v).to_string()),
                exit_code: exits.get(&pane).and_then(|v| v.as_i64()).map(|c| c as i32),
            }
        })
        .collect()
}

fn claude_signals(m: &Mirror, lane: &str, session: &str) -> Vec<Signal> {
    let positions: HashMap<i64, (i64, i64, i64)> = rows_for(m, "lanes_pane", lane, session)
        .map(|r| (int(&r[2]), (int(&r[3]), int(&r[6]), int(&r[5]))))
        .collect();
    let mut rows: Vec<&Row> = m.rows("lanes_claude").filter(|r| s(&r[0]) == lane && s(&r[4]) == session).collect();
    // Same reading order as the on-screen panes, as gather_lanes() does.
    rows.sort_by_key(|r| (positions.get(&int(&r[3])).copied().unwrap_or((i64::MAX, i64::MAX, i64::MAX)), s(&r[1]).to_string()));
    rows.into_iter()
        .map(|r| {
            let reason = match s(&r[2]) {
                "idle" => ClaudeSessionReason::Awaiting,
                "permission_pending" => ClaudeSessionReason::Permission,
                _ => ClaudeSessionReason::Active,
            };
            Signal::new(
                SignalReason::ClaudeSession(reason),
                Some(SignalAction::SwitchClaudeSession { session_id: s(&r[1]).to_string() }),
                None,
            )
        })
        .collect()
}

fn repo_signals(m: &Mirror, lane: &str, resolved_path: &str, raw_path: &str, session: Option<&str>) -> Vec<Signal> {
    let action = || session.map(|session| SignalAction::FocusRepoPane { session: session.to_string(), path: raw_path.to_string() });
    let mut out = Vec::new();
    if m.rows("lanes_repo_dirty").any(|r| s(&r[0]) == lane && s(&r[2]) == resolved_path) {
        out.push(Signal::new(SignalReason::Repo(RepoReason::PendingCommit), action(), None));
    }
    if let Some(r) = m.rows("lanes_repo_branch").find(|r| s(&r[0]) == lane && s(&r[4]) == resolved_path) {
        out.push(Signal::new(
            SignalReason::Repo(RepoReason::NonDefaultBranch),
            action(),
            Some(format!("on \"{}\", default is \"{}\"", s(&r[2]), s(&r[3]))),
        ));
    }
    out
}

/// The UI snapshot built from lensd's rows. Lane definitions come from Lanes'
/// config; signals and live state come from the mirror. Lanes' own policy
/// (dismissals, disabled sessions, `cyclable`, `visible`, lifecycle) is
/// applied here, never by lensd.
pub fn snapshot_from_mirror(cfg: &Config, m: &Mirror) -> LanewiseSnapshot {
    let lanes = cfg.lanes.iter().map(|lane| lane_from_mirror(lane, m)).collect();
    LanewiseSnapshot {
        taken_at: chrono::Utc::now().to_rfc3339(),
        lanes,
        focused_lane: crate::state::read_focused_lane(),
        focused_claude_session: crate::state::read_claude_cursor(),
    }
}

/// `(session, pane, seconds_remaining)` for every `running` shell record
/// that exists right now but hasn't crossed `shell_notice_secs()` yet - so
/// its signal is correctly absent from `snapshot_from_mirror`'s output.
/// Nothing re-diffs a fact that hasn't itself changed, so without this
/// there'd be no second look once the floor actually passes - a command
/// that's still running at exactly the 3s mark would just never get a chip
/// until something unrelated happens to trigger a refetch. The caller
/// (switch-ui's Tauri backend) arms a one-shot timer per key this returns;
/// `seconds_remaining` is clamped to 0 defensively but should never
/// actually be negative, since this is the same elapsed-time check
/// `command_signal` itself uses to decide visibility.
pub fn pending_running_commands(cfg: &Config, m: &Mirror) -> Vec<(String, u32, i64)> {
    let now: chrono::DateTime<chrono::FixedOffset> = chrono::Utc::now().into();
    cfg.lanes
        .iter()
        .filter_map(|lane| Some((lane.id.as_str(), lane.terminal_session()?)))
        .flat_map(|(lane_id, session)| command_records(m, lane_id, session))
        .filter(|rec| rec.state == "running")
        .filter_map(|rec| {
            let started = chrono::DateTime::parse_from_rfc3339(rec.started_at.as_deref()?).ok()?;
            let elapsed = (now - started).num_seconds();
            let remaining = crate::shell_notice_secs() - elapsed;
            (remaining > 0).then_some((rec.session, rec.pane, remaining))
        })
        .collect()
}

fn lane_from_mirror(lane: &crate::model::Lane, m: &Mirror) -> LaneSnapshot {
    let nav_session = lane.scope.iter().find_map(|el| el.zellij_session_name());
    let mut facets: Vec<FacetSnapshot> = lane
        .scope
        .iter()
        .map(|el| {
            if let Some(session) = el.zellij_session_name() {
                let running = rows_for(m, "lanes_session_running", &lane.id, session).next().is_some();
                let claude = if running { claude_signals(m, &lane.id, session) } else { vec![] };
                let needs_you = m
                    .rows("lanes_claude")
                    .any(|r| s(&r[0]) == lane.id && s(&r[4]) == session && matches!(s(&r[2]), "idle" | "permission_pending"));
                let panes = if running { panes_from_mirror(m, &lane.id, session, needs_you) } else { vec![] };
                FacetSnapshot::Terminal { session: session.to_string(), running, panes, signals: claude }
            } else {
                let raw = el.repo_path().unwrap_or_default();
                let signals = repo_signals(m, &lane.id, &resolve_path(raw), raw, nav_session);
                FacetSnapshot::Repo { path: raw.to_string(), signals }
            }
        })
        .collect();
    facets.extend(lane.targets.iter().map(|t| FacetSnapshot::Target { driver: t.driver.name().to_string() }));

    // "Reachable" is now whether a WezTerm tab is showing the lane's session
    // right now (lensd's wezterm source), not Lanes' cached tab id.
    let has_tab = m.rows("lanes_tab").any(|r| s(&r[0]) == lane.id);
    let reachable = facets.iter().any(|f| matches!(f, FacetSnapshot::Terminal { .. })).then_some(has_tab);
    let terminal_running = crate::lane_terminal_running(&facets);

    if let Some(FacetSnapshot::Terminal { session, signals, .. }) =
        facets.iter_mut().find(|f| matches!(f, FacetSnapshot::Terminal { .. }))
    {
        if crate::lane_session_missing(lane.active, reachable) {
            signals.push(Signal::new(
                SignalReason::Lanes(LanesReason::SessionMissing),
                None,
                Some(format!("no WezTerm tab showing zellij session \"{session}\"")),
            ));
        }
        if terminal_running == Some(false) {
            signals.push(Signal::new(
                SignalReason::Lanes(LanesReason::SessionNotRunning),
                None,
                Some(format!("expected zellij session \"{session}\" to be running")),
            ));
        }
        for rec in command_records(m, &lane.id, session) {
            if crate::state::is_signal_dismissed(&rec.occurrence_id()) {
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

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

fn claude_reason(state: &str) -> &'static str {
    match state {
        "idle" => "awaiting",
        "permission_pending" => "permission",
        _ => "active",
    }
}

/// Comparable signal keys implied by lensd's rows. `is_dismissed` is Lanes' own filter.
pub fn keys_from_mirror(m: &Mirror, is_dismissed: impl Fn(&str) -> bool) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for r in m.rows("lanes_claude") {
        keys.insert(format!("{}|claude_session|{}|{}", s(&r[0]), claude_reason(s(&r[2])), s(&r[1])));
    }
    for r in m.rows("lanes_command") {
        let reason = match s(&r[3]) {
            "done" => "done",
            "failed" => "failed",
            _ => continue,
        };
        let (session, pane, started) = (s(&r[1]), r[2].as_i64().unwrap_or(-1), s(&r[4]));
        if is_dismissed(&format!("command:{session}--{pane}--{started}")) {
            continue;
        }
        keys.insert(format!("{}|command|{reason}|{session}:{pane}", s(&r[0])));
    }
    for r in m.rows("lanes_repo_dirty") {
        keys.insert(format!("{}|repo|pending_commit|{}", s(&r[0]), s(&r[1])));
    }
    for r in m.rows("lanes_repo_branch") {
        keys.insert(format!("{}|repo|non_default_branch|{}", s(&r[0]), s(&r[1])));
    }
    let running: HashSet<(&str, &str)> = m.rows("lanes_session_running").map(|r| (s(&r[0]), s(&r[1]))).collect();
    for r in m.rows("lanes_session") {
        if !running.contains(&(s(&r[0]), s(&r[1]))) {
            keys.insert(format!("{}|lanes|session_not_running|", s(&r[0])));
        }
    }
    keys
}

/// Resolved path -> repo name for the lane repos lensd actually watches
/// (those also in its own config).
pub fn watched_repos(m: &Mirror) -> HashMap<String, String> {
    m.rows("lanes_repo_watched").map(|r| (s(&r[2]).to_string(), s(&r[1]).to_string())).collect()
}

fn resolve_path(path: &str) -> String {
    let expanded = crate::expand_tilde(path);
    let trimmed = expanded.trim_end_matches('/');
    std::fs::canonicalize(trimmed).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| trimmed.to_string())
}

/// The same keys from `gather_lanes()`, limited to the kinds lensd covers so far.
/// Repo signals are kept only for repos in `watched`; the rest are returned
/// separately since lensd cannot know about them.
/// `Ready` is `Awaiting` upgraded by lane cyclability, which lensd does not model.
pub fn keys_from_snapshot(snap: &LanewiseSnapshot, watched: &HashMap<String, String>) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut keys = BTreeSet::new();
    let mut unwatched = BTreeSet::new();
    for lane in &snap.lanes {
        for facet in &lane.facets {
            if let FacetSnapshot::Repo { path, signals } = facet {
                let resolved = resolve_path(path);
                for sig in signals {
                    let reason = match &sig.reason {
                        SignalReason::Repo(RepoReason::PendingCommit) => "pending_commit",
                        SignalReason::Repo(RepoReason::NonDefaultBranch) => "non_default_branch",
                        _ => continue,
                    };
                    match watched.get(&resolved) {
                        Some(name) => {
                            keys.insert(format!("{}|repo|{reason}|{name}", lane.id));
                        }
                        None => {
                            unwatched.insert(path.clone());
                        }
                    }
                }
                continue;
            }
            for sig in facet.signals() {
                let v = serde_json::to_value(sig).unwrap_or_default();
                let kind = s(&v["kind"]);
                let mut reason = s(&v["reason"]).to_string();
                let subject = match (kind, &sig.action) {
                    ("claude_session", Some(SignalAction::SwitchClaudeSession { session_id })) => {
                        if reason == "ready" {
                            reason = "awaiting".into();
                        }
                        session_id.clone()
                    }
                    ("command", Some(SignalAction::FocusPane { session, pane })) => format!("{session}:{pane}"),
                    ("lanes", _) if reason == "session_not_running" => String::new(),
                    _ => continue,
                };
                keys.insert(format!("{}|{kind}|{reason}|{subject}", lane.id));
            }
        }
    }
    (keys, unwatched)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mirror(rows: Vec<Value>) -> Mirror {
        let mut m = Mirror::default();
        m.apply(&json!({"snapshot": {"epoch": 1, "rows": rows}}).to_string());
        m
    }

    fn row(rel: &str, values: Value) -> Value {
        json!({"relation": rel, "values": values})
    }

    #[test]
    fn diffs_add_and_remove_rows_after_a_snapshot() {
        let mut m = mirror(vec![row("lanes_claude", json!(["l", "s1", "idle"]))]);
        m.apply(&json!({"diff": {"epoch": 2, "changes": [
            {"relation": "lanes_claude", "values": ["l", "s1", "idle"], "weight": -1},
            {"relation": "lanes_claude", "values": ["l", "s1", "permission_pending"], "weight": 1}]}}).to_string());
        assert_eq!(m.epoch, Some(2));
        assert_eq!(
            keys_from_mirror(&m, |_| false),
            BTreeSet::from(["l|claude_session|permission|s1".to_string()])
        );
    }

    #[test]
    fn dismissed_commands_are_filtered_and_running_ones_ignored() {
        let m = mirror(vec![
            row("lanes_command", json!(["l", "z", 3, "failed", "T1"])),
            row("lanes_command", json!(["l", "z", 4, "failed", "T2"])),
            row("lanes_command", json!(["l", "z", 5, "running", "T3"])),
        ]);
        let keys = keys_from_mirror(&m, |id| id == "command:z--3--T1");
        assert_eq!(keys, BTreeSet::from(["l|command|failed|z:4".to_string()]));
    }

    #[test]
    fn repo_rows_become_repo_keys_and_watched_names_are_listed() {
        let m = mirror(vec![
            row("lanes_repo_dirty", json!(["l", "infra"])),
            row("lanes_repo_branch", json!(["l", "infra", "feat", "main"])),
            row("lanes_repo_watched", json!(["l", "infra", "/p/infra"])),
        ]);
        assert_eq!(
            keys_from_mirror(&m, |_| false),
            BTreeSet::from(["l|repo|pending_commit|infra".to_string(), "l|repo|non_default_branch|infra".to_string()])
        );
        assert_eq!(watched_repos(&m), HashMap::from([("/p/infra".to_string(), "infra".to_string())]));
    }

    #[test]
    fn a_lane_session_that_is_not_running_is_a_session_not_running_key() {
        let m = mirror(vec![
            row("lanes_session", json!(["a", "up"])),
            row("lanes_session", json!(["b", "down"])),
            row("lanes_session_running", json!(["a", "up"])),
        ]);
        assert_eq!(keys_from_mirror(&m, |_| false), BTreeSet::from(["b|lanes|session_not_running|".to_string()]));
    }

    fn socket_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lanes-lensd-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("t.sock")
    }

    fn wait_until(what: &str, cond: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !cond() {
            assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn send(c: &mut UnixStream, line: &str) {
        use std::io::Write;
        writeln!(c, "{line}").unwrap();
    }

    #[test]
    fn client_follows_diffs_reports_disconnect_and_reconnects_with_a_fresh_snapshot() {
        let path = socket_path("client");
        let notified = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let n = notified.clone();
        // Started before the socket exists: not connected, and keeps retrying.
        let client = Client::start_with_backoff(path.clone(), Duration::from_millis(20), Duration::from_millis(50), move || {
            n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        std::thread::sleep(Duration::from_millis(100));
        assert!(!client.connected());
        assert!(client.current().is_none());

        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let (mut c, _) = listener.accept().unwrap();
        send(&mut c, r#"{"hello":{"protocol":1,"set":"lanes"}}"#);
        send(&mut c, r#"{"snapshot":{"epoch":1,"rows":[{"relation":"lanes_session","values":["a","z"]}]}}"#);
        wait_until("snapshot", || client.connected());
        assert_eq!(client.current().unwrap().rels["lanes_session"].len(), 1);

        let v = client.version();
        send(&mut c, r#"{"diff":{"epoch":2,"changes":[{"relation":"lanes_session","values":["b","y"],"weight":1}]}}"#);
        wait_until("diff", || client.version() > v);
        assert_eq!(client.current().unwrap().rels["lanes_session"].len(), 2);
        assert!(notified.load(std::sync::atomic::Ordering::SeqCst) >= 2);

        // lensd goes away: no data, not a stale mirror.
        drop(c);
        wait_until("disconnect", || !client.connected());
        assert!(client.current().is_none());

        // It comes back: a fresh snapshot replaces everything, nothing doubled.
        let (mut c, _) = listener.accept().unwrap();
        send(&mut c, r#"{"snapshot":{"epoch":9,"rows":[{"relation":"lanes_session","values":["c","x"]}]}}"#);
        wait_until("reconnect", || client.connected());
        let m = client.current().unwrap();
        assert_eq!(m.epoch, Some(9));
        assert_eq!(m.rels["lanes_session"], HashSet::from([vec![Value::from("c"), Value::from("x")]]));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_connection_that_closes_before_a_snapshot_is_not_connected() {
        let path = socket_path("nosnap");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let client = Client::start_with_backoff(path.clone(), Duration::from_millis(20), Duration::from_millis(50), || {});
        let (mut c, _) = listener.accept().unwrap();
        send(&mut c, r#"{"hello":{"protocol":1,"set":"lanes"}}"#);
        drop(c);
        std::thread::sleep(Duration::from_millis(100));
        assert!(!client.connected(), "hello alone is not data");
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    fn cfg_with(lane: crate::model::Lane) -> Config {
        Config { drivers: None, monitors: HashMap::new(), order: None, cycle_policy: crate::model::CyclePolicy::default(), lanes: vec![lane], boards_targets: Vec::new(), context_ids: Vec::new() }
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

    fn live_rows() -> Vec<Value> {
        vec![
            row("lanes_session", json!(["l", "z"])),
            row("lanes_session_running", json!(["l", "z"])),
            row("lanes_tab", json!(["l", 4])),
            // pane 7 is lower on screen than pane 3, so its session sorts second.
            row("lanes_pane", json!(["l", "z", 7, 0, "T", 0, 20])),
            row("lanes_pane", json!(["l", "z", 3, 0, "T", 0, 1])),
            row("lanes_pane_cmd", json!(["l", "z", 3, "claude"])),
            row("lanes_pane_cmd", json!(["l", "z", 7, "claude"])),
            row("lanes_pane_cwd", json!(["l", "z", 3, "/a"])),
            row("lanes_pane_focused", json!(["l", "z", 3])),
            row("lanes_claude", json!(["l", "s-low", "running", 7, "z"])),
            row("lanes_claude", json!(["l", "s-high", "idle", 3, "z"])),
        ]
    }

    fn terminal(snap: &LanewiseSnapshot) -> (&bool, &Vec<PaneSnapshot>, &Vec<Signal>) {
        match &snap.lanes[0].facets[0] {
            FacetSnapshot::Terminal { running, panes, signals, .. } => (running, panes, signals),
            other => panic!("expected terminal, got {}", serde_json::to_string(&other.signals().len()).unwrap()),
        }
    }

    #[test]
    fn a_reachable_lane_with_an_idle_claude_has_a_ready_cyclable_signal_in_screen_order() {
        let snap = snapshot_from_mirror(&cfg_with(lane()), &mirror(live_rows()));
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
        let rows: Vec<Value> = live_rows().into_iter().filter(|r| r["relation"] != "lanes_tab").collect();
        let snap = snapshot_from_mirror(&cfg_with(lane()), &mirror(rows));
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
        let rows = vec![row("lanes_session", json!(["l", "z"])), row("lanes_tab", json!(["l", 4]))];
        let snap = snapshot_from_mirror(&cfg_with(lane()), &mirror(rows));
        let (running, panes, signals) = terminal(&snap);
        assert!(!*running && panes.is_empty());
        assert!(signals.iter().any(|sg| matches!(sg.reason, SignalReason::Lanes(LanesReason::SessionNotRunning))));
    }

    #[test]
    fn finished_commands_carry_their_detail_and_dismiss_id() {
        let mut rows = live_rows();
        rows.extend([
            row("lanes_command", json!(["l", "z", 3, "failed", "2026-01-01T00:00:00Z"])),
            row("lanes_command_cmd", json!(["l", "z", 3, "cargo test"])),
            row("lanes_command_exit", json!(["l", "z", 3, 101])),
            row("lanes_command_ended", json!(["l", "z", 3, "2026-01-01T00:00:07Z"])),
            // Long since past shell_notice_secs() (real time has moved on
            // from this fixed timestamp), so this one does become a signal
            // too - see running_command_only_becomes_a_signal_past_the_
            // notice_floor for the still-fresh case.
            row("lanes_command", json!(["l", "z", 7, "running", "2026-01-01T00:00:00Z"])),
            row("lanes_command_cmd", json!(["l", "z", 7, "vite"])),
        ]);
        let snap = snapshot_from_mirror(&cfg_with(lane()), &mirror(rows));
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
        let mut rows = live_rows();
        let just_started = chrono::Utc::now().to_rfc3339();
        rows.extend([row("lanes_command", json!(["l", "z", 7, "running", just_started]))]);
        let snap = snapshot_from_mirror(&cfg_with(lane()), &mirror(rows));
        let (_, _, signals) = terminal(&snap);
        assert!(
            !signals.iter().any(|sg| sg.kind() == crate::model::SignalKind::Command),
            "just started - under the notice floor, no chip yet"
        );
    }

    #[test]
    fn pending_running_commands_finds_fresh_running_records_under_the_floor() {
        let mut rows = live_rows();
        let just_started = chrono::Utc::now().to_rfc3339();
        rows.extend([row("lanes_command", json!(["l", "z", 7, "running", just_started]))]);
        let cfg = cfg_with(lane());
        let pending = pending_running_commands(&cfg, &mirror(rows));
        assert_eq!(pending.len(), 1);
        let (session, pane, remaining) = &pending[0];
        assert_eq!(session, "z");
        assert_eq!(*pane, 7);
        assert!((1..=3).contains(remaining), "remaining={remaining}");
    }

    #[test]
    fn pending_running_commands_excludes_one_already_past_the_floor() {
        let mut rows = live_rows();
        // Started well over shell_notice_secs() ago - already a real
        // signal, not pending.
        rows.extend([row("lanes_command", json!(["l", "z", 7, "running", "2026-01-01T00:00:00Z"]))]);
        let cfg = cfg_with(lane());
        assert!(pending_running_commands(&cfg, &mirror(rows)).is_empty());
    }

    #[test]
    fn pending_running_commands_ignores_done_and_failed_records() {
        let mut rows = live_rows();
        let just_started = chrono::Utc::now().to_rfc3339();
        rows.extend([row("lanes_command", json!(["l", "z", 7, "done", just_started]))]);
        let cfg = cfg_with(lane());
        assert!(pending_running_commands(&cfg, &mirror(rows)).is_empty());
    }

    #[test]
    fn repo_facets_get_dirty_and_branch_signals_that_focus_the_lanes_session() {
        let mut rows = live_rows();
        rows.extend([
            row("lanes_repo_dirty", json!(["l", "infra", "/p/infra"])),
            row("lanes_repo_branch", json!(["l", "infra", "feat", "main", "/p/infra"])),
            row("lanes_repo_dirty", json!(["other", "x", "/p/infra"])),
        ]);
        let snap = snapshot_from_mirror(&cfg_with(lane()), &mirror(rows));
        let FacetSnapshot::Repo { path, signals } = &snap.lanes[0].facets[1] else { panic!("repo facet") };
        assert_eq!(path, "/p/infra");
        assert_eq!(signals.len(), 2);
        assert!(matches!(signals[0].reason, SignalReason::Repo(RepoReason::PendingCommit)));
        assert_eq!(signals[1].detail.as_deref(), Some("on \"feat\", default is \"main\""));
        assert!(matches!(&signals[0].action, Some(SignalAction::FocusRepoPane { session, path }) if session == "z" && path == "/p/infra"));
    }

    #[test]
    fn socket_snapshot_round_trip() {
        let dir = std::env::temp_dir().join(format!("lanes-lensd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            use std::io::Write;
            let (mut c, _) = listener.accept().unwrap();
            writeln!(c, r#"{{"hello":{{"protocol":1,"set":"lanes"}}}}"#).unwrap();
            writeln!(c, r#"{{"snapshot":{{"epoch":7,"rows":[{{"relation":"lanes_session","values":["a","z"]}}]}}}}"#).unwrap();
        });
        let m = read_snapshot(&path, Duration::from_secs(5)).unwrap();
        server.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(m.epoch, Some(7));
        assert_eq!(m.rels["lanes_session"].len(), 1);
    }
}
