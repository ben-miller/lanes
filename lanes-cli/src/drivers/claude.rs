use std::fs;
use std::path::Path;

use serde::Deserialize;

#[derive(Deserialize)]
struct ActiveSession {
    session_id: String,
    zellij_session: Option<String>,
    pid: Option<u32>,
}

/// A live Claude Code session, for ordering/cycling between them -
/// cycle_claude_session() is the only consumer, and only needs enough to
/// sort sessions and identify one to switch to.
pub struct ClaudeSession {
    pub session_id: String,
    pub zellij_session: Option<String>,
}

pub fn enumerate() -> Vec<ClaudeSession> {
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

fn load_session(path: &Path, live_zellij_sessions: &std::collections::HashSet<String>) -> Option<ClaudeSession> {
    let data = fs::read_to_string(path).ok()?;
    let s: ActiveSession = serde_json::from_str(&data).ok()?;

    let zellij_session = s.zellij_session.clone().unwrap_or_default();
    if !crate::session_is_live(&zellij_session, live_zellij_sessions, s.pid) {
        return None;
    }

    Some(ClaudeSession {
        session_id: s.session_id,
        zellij_session: s.zellij_session,
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
