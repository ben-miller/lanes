use std::fs;
use std::path::Path;

use serde::Deserialize;

#[derive(Deserialize)]
struct ActiveSession {
    session_id: String,
    zellij_session: Option<String>,
    pid: Option<u32>,
    state: Option<String>,
    cwd: Option<String>,
}

/// A live Claude Code session. `state` is the raw registry value ("idle" |
/// "busy" | "running" | "permission_pending" | ...), defaulting to "running"
/// when absent - interpreting it into a signal/reason beyond that is a
/// policy decision that belongs with whatever's doing the interpreting (see
/// scope::observe), not the driver. `permission_pending` is only ever
/// cleared by the Stop/StopFailure hooks writing "idle" back once the
/// assistant's turn completes, so as long as `session_is_live` says the
/// process is still around, an old mtime just means the prompt is still
/// sitting there unanswered, not that the write was abandoned.
pub struct ClaudeSession {
    pub session_id: String,
    pub zellij_session: Option<String>,
    pub state: String,
}

pub fn enumerate() -> Vec<ClaudeSession> {
    let registry_dir = dirs::active_sessions_dir();
    let entries = match fs::read_dir(&registry_dir) {
        Ok(e) => e,
        Err(_) => return vec![],
    };

    let live_zellij_sessions = crate::drivers::zellij::running_sessions();

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

    let state = s.state.unwrap_or_else(|| "running".to_string());

    Some(ClaudeSession {
        session_id: s.session_id,
        zellij_session: s.zellij_session,
        state,
    })
}

/// A registry entry whose process is still genuinely alive, but whose
/// recorded zellij_session no longer matches any currently running Zellij
/// session.
pub struct RenamedCandidate {
    pub session_id: String,
    pub old_zellij_session: String,
    pub cwd: Option<String>,
    pub pid: u32,
}

/// Distinct from a fully dead entry, which `session_is_live` (and thus
/// `enumerate()`) already filters out entirely: here the recorded pid is
/// still a genuine, running `claude` process, just tagged with a Zellij
/// session name that doesn't exist anymore - most likely because that
/// session got renamed after this Claude session started
/// (session-start.sh captures $ZELLIJ_SESSION_NAME once, at hook time, and
/// never refreshes it). A live process silently invisible to every lane
/// until this is noticed and fixed - see cmd::doctor.
pub fn possibly_renamed_sessions() -> Vec<RenamedCandidate> {
    let registry_dir = dirs::active_sessions_dir();
    let entries = match fs::read_dir(&registry_dir) {
        Ok(e) => e,
        Err(_) => return vec![],
    };
    let live_zellij_sessions = crate::drivers::zellij::running_sessions();

    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map_or(false, |x| x == "json"))
        .filter_map(|e| check_renamed(&e.path(), &live_zellij_sessions))
        .collect()
}

fn check_renamed(path: &Path, live_zellij_sessions: &std::collections::HashSet<String>) -> Option<RenamedCandidate> {
    let data = fs::read_to_string(path).ok()?;
    let s: ActiveSession = serde_json::from_str(&data).ok()?;

    let zellij_session = s.zellij_session.clone().unwrap_or_default();
    if zellij_session.is_empty() || live_zellij_sessions.contains(&zellij_session) {
        return None;
    }

    let pid = s.pid?;
    let cmd = crate::process_command(pid)?;
    if !crate::is_claude_command(&cmd) {
        return None;
    }

    Some(RenamedCandidate {
        session_id: s.session_id,
        old_zellij_session: zellij_session,
        cwd: s.cwd,
        pid,
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
