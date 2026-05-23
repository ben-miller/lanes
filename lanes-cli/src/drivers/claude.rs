use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::json;

use crate::model::*;

#[derive(Deserialize)]
struct ActiveSession {
    session_id: String,
    zellij_session: Option<String>,
    zellij_pane_id: Option<i64>,
    wezterm_tab_id: Option<i64>,
    cwd: String,
    started_at: Option<String>,
    state: Option<String>,
}

pub fn enumerate() -> Vec<Observed> {
    let registry_dir = dirs::active_sessions_dir();
    let entries = match fs::read_dir(&registry_dir) {
        Ok(e) => e,
        Err(_) => return vec![],
    };

    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map_or(false, |x| x == "json"))
        .filter_map(|e| load_session(&e.path()))
        .collect()
}

fn load_session(path: &Path) -> Option<Observed> {
    let data = fs::read_to_string(path).ok()?;
    let s: ActiveSession = serde_json::from_str(&data).ok()?;

    let activity = s.state.clone().unwrap_or_else(|| "unknown".to_string());
    let status = match activity.as_str() {
        "idle" => Status::Idle,
        "busy" | "running" => Status::Busy,
        _ => Status::Idle,
    };

    let label = ai_title_for(&s.cwd, &s.session_id);

    let mut extra = json!({});
    if let Some(zs) = &s.zellij_session {
        extra["zellij_session"] = json!(zs);
    }
    if let Some(pid) = s.zellij_pane_id {
        extra["zellij_pane_id"] = json!(pid);
    }
    if let Some(tid) = s.wezterm_tab_id {
        extra["wezterm_tab_id"] = json!(tid);
    }
    if let Some(started) = &s.started_at {
        extra["started_at"] = json!(started);
    }

    Some(Observed {
        selector: Selector::Terminal(TerminalSel {
            driver: "claude".to_string(),
            id: s.session_id.clone(),
        }),
        locator: s.session_id.clone(),
        label,
        shape: None,
        state: Some(State {
            status,
            detail: Some(DriverState::Claude(ClaudeState { activity })),
        }),
        cwd: Some(s.cwd),
        worktree_path: None,
        extra,
    })
}

fn ai_title_for(cwd: &str, session_id: &str) -> Option<String> {
    let slug = cwd.replace('/', "-");
    let path = PathBuf::from(dirs::claude_dir())
        .join("projects")
        .join(&slug)
        .join(format!("{}.jsonl", session_id));

    let file = fs::File::open(&path).ok()?;
    let reader = BufReader::new(file);

    let mut last_title: Option<String> = None;
    for line in reader.lines() {
        let Ok(line) = line else { continue };
        let Ok(record) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if record.get("type").and_then(|v| v.as_str()) == Some("aiTitle") {
            if let Some(title) = record.get("aiTitle").and_then(|v| v.as_str()) {
                last_title = Some(title.to_string());
            }
        }
    }
    last_title
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
