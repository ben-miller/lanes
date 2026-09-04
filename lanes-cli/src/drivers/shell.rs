//! Reads the command-lifecycle records written by the shell hook
//! (`lanes shell-init <shell>`) to `~/.local/state/lanes/shell/`. This
//! driver is deliberately shell-agnostic: everything shell-specific
//! (`fish_preexec`, `$CMD_DURATION`, bash-preexec, …) lives in the emitted
//! hook, which normalises to the one record schema below. A bash or zsh
//! hook writing the same schema feeds this same driver unchanged.
//!
//! Every read is lenient - a malformed or wrong-version file is skipped
//! with a warning, never propagated - so a bad record can't break a
//! `gather_lanes()` refresh. See PLAN-shell-signal.md.

use serde::Deserialize;

/// The hook↔driver contract version. A record written by an older or newer
/// hook is skipped rather than mis-parsed.
pub const SUPPORTED_V: u32 = 1;

/// One record per pane, at `<state_dir>/shell/<session>--<pane>.json`. A
/// pane runs one foreground command at a time, so the file is overwritten
/// in place as that command's state changes.
#[derive(Debug, Clone, Deserialize)]
pub struct CommandRecord {
    #[serde(default)]
    pub v: u32,
    /// Which shell wrote this - kept for the schema and diagnostics
    /// (doctor / logs); nothing in the driver branches on it.
    #[serde(default)]
    #[allow(dead_code)]
    pub shell: Option<String>,
    pub session: String,
    /// Numeric `$ZELLIJ_PANE_ID`, the same id space `zellij action
    /// list-panes` reports (see drivers::zellij).
    pub pane: u32,
    #[serde(default)]
    pub argv0: Option<String>,
    #[serde(default)]
    pub cmd: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    /// `"running"` | `"done"` | `"failed"`. Phase 1 only acts on the latter
    /// two.
    pub state: String,
    #[serde(default)]
    pub ended_at: Option<String>,
    #[serde(default)]
    pub exit_code: Option<i32>,
}

impl CommandRecord {
    /// Stable per-occurrence id, used as the key for `signal-dismissed`
    /// (see state.rs). Carries `started_at` so dismissing one run never
    /// suppresses a later run in the same pane.
    pub fn occurrence_id(&self) -> String {
        format!(
            "command:{}--{}--{}",
            self.session,
            self.pane,
            self.started_at.as_deref().unwrap_or("unknown")
        )
    }

    /// Whole seconds between `started_at` and `ended_at`, when both parse as
    /// RFC3339. `None` if either is missing or unparseable.
    pub fn duration_secs(&self) -> Option<i64> {
        let start = chrono::DateTime::parse_from_rfc3339(self.started_at.as_deref()?).ok()?;
        let end = chrono::DateTime::parse_from_rfc3339(self.ended_at.as_deref()?).ok()?;
        Some((end - start).num_seconds().max(0))
    }
}

/// Every readable, current-version record in the shell state dir. Order is
/// filesystem order and not meaningful - callers key by `session`/`pane`.
pub fn enumerate() -> Vec<CommandRecord> {
    let dir = crate::logging::state_dir().join("shell");
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| load_record(&e.path()))
        .collect()
}

fn load_record(path: &std::path::Path) -> Option<CommandRecord> {
    let data = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<CommandRecord>(&data) {
        Ok(rec) if rec.v == SUPPORTED_V => Some(rec),
        Ok(rec) => {
            crate::logging::append_line(
                "switch-ui.log",
                "warn",
                &format!("shell driver: unsupported record v={} in {:?}", rec.v, path),
            );
            None
        }
        Err(err) => {
            crate::logging::append_line(
                "switch-ui.log",
                "warn",
                &format!("shell driver: unreadable record {:?}: {err}", path),
            );
            None
        }
    }
}

/// `3m12s` / `47s` / `1h04m` - compact, for a signal's detail line.
pub fn fmt_duration(secs: i64) -> String {
    let secs = secs.max(0);
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(json: &str) -> Result<CommandRecord, serde_json::Error> {
        serde_json::from_str(json)
    }

    #[test]
    fn parses_a_full_done_record() {
        let r = rec(r#"{
            "v": 1, "shell": "fish", "session": "lanes", "pane": 3,
            "argv0": "cargo", "cmd": "cargo test --workspace",
            "started_at": "2026-09-04T10:00:00Z", "state": "done",
            "ended_at": "2026-09-04T10:03:12Z", "exit_code": 0
        }"#).unwrap();
        assert_eq!(r.session, "lanes");
        assert_eq!(r.pane, 3);
        assert_eq!(r.state, "done");
        assert_eq!(r.duration_secs(), Some(192));
        assert_eq!(r.occurrence_id(), "command:lanes--3--2026-09-04T10:00:00Z");
    }

    #[test]
    fn missing_required_field_fails_to_parse() {
        // no `session` - load_record would skip this and warn
        assert!(rec(r#"{"v":1,"pane":3,"state":"done"}"#).is_err());
    }

    #[test]
    fn duration_is_none_without_both_timestamps() {
        let r = rec(r#"{"v":1,"session":"x","pane":1,"state":"running"}"#).unwrap();
        assert_eq!(r.duration_secs(), None);
    }

    #[test]
    fn fmt_duration_shapes() {
        assert_eq!(fmt_duration(9), "9s");
        assert_eq!(fmt_duration(47), "47s");
        assert_eq!(fmt_duration(192), "3m12s");
        assert_eq!(fmt_duration(3720), "1h02m");
    }
}
