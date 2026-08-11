use std::fs;
use std::path::Path;

use serde::Deserialize;
use serde_json::json;

use crate::model::*;

#[derive(Deserialize)]
struct ActiveSession {
    session_id: String,
    zellij_session: Option<String>,
    zellij_pane_id: Option<i64>,
    cwd: String,
    pid: Option<u32>,
}

pub fn enumerate() -> Vec<Observed> {
    let registry_dir = dirs::active_sessions_dir();
    let entries = match fs::read_dir(&registry_dir) {
        Ok(e) => e,
        Err(_) => return vec![],
    };

    let live_zellij_sessions = crate::running_zellij_sessions();

    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map_or(false, |x| x == "json"))
        .filter_map(|e| load_session(&e.path(), &live_zellij_sessions))
        .collect()
}

fn load_session(path: &Path, live_zellij_sessions: &std::collections::HashSet<String>) -> Option<Observed> {
    let data = fs::read_to_string(path).ok()?;
    let s: ActiveSession = serde_json::from_str(&data).ok()?;

    let zellij_session = s.zellij_session.clone().unwrap_or_default();
    if !crate::session_is_live(&zellij_session, live_zellij_sessions, s.pid) {
        return None;
    }

    let mut extra = json!({});
    if let Some(zs) = &s.zellij_session {
        extra["zellij_session"] = json!(zs);
    }
    if let Some(pid) = s.zellij_pane_id {
        extra["zellij_pane_id"] = json!(pid);
    }

    Some(Observed {
        selector: Selector::Terminal(TerminalSel {
            driver: "claude".to_string(),
            id: s.session_id.clone(),
        }),
        locator: s.session_id.clone(),
        cwd: Some(s.cwd),
        extra,
    })
}

mod dirs {
    use std::path::PathBuf;

    pub fn claude_dir() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_default();
        PathBuf::from(home).join(".claude")
    }

    pub fn active_sessions_dir() -> PathBuf {
        claude_dir().join("active-sessions")
    }
}
