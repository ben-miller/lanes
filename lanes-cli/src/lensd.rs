//! Spike: read Lanes' rule set from lensd's socket and compare the signals it
//! implies against `gather_lanes()`. lensd is not wired into the UI yet; this
//! exists to find which facts are missing before swapping anything.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use crate::model::{FacetSnapshot, LanewiseSnapshot, RepoReason, SignalAction, SignalReason};

pub type Row = Vec<Value>;

/// The current rows per relation, rebuilt from a snapshot and kept current by diffs.
#[derive(Default, Debug)]
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
