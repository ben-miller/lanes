pub mod config;
mod drivers;
pub mod model;
pub mod zone;

use model::{Observed, Snapshot};
use std::collections::{HashMap, HashSet};

pub fn gather() -> Snapshot {
    let cfg = config::Config::load();
    let mut resources: Vec<Observed> = Vec::new();

    if cfg.driver_enabled("claude") {
        resources.extend(drivers::claude::enumerate());
    }
    if cfg.driver_enabled("zellij") {
        resources.extend(drivers::zellij::enumerate());
    }
    if cfg.driver_enabled("brotab") {
        resources.extend(drivers::browser::enumerate());
    }

    correlate(&mut resources, &cfg);
    Snapshot {
        taken_at: chrono::Utc::now().to_rfc3339(),
        resources,
    }
}

pub fn gather_lanes(cfg: &config::Config) -> model::LanewiseSnapshot {
    let running = running_zellij_sessions();
    let claude = claude_sessions_by_zellij();

    let lanes = cfg.lanes.iter().map(|lane| {
        let facets = lane.facets.iter().map(|facet| match facet {
            model::Facet::Terminal { session } => {
                let is_running = running.contains(session.as_str());
                let (panes, signals) = if is_running {
                    build_terminal_state(session, &claude)
                } else {
                    (vec![], vec![])
                };
                model::FacetSnapshot::Terminal {
                    session: session.clone(),
                    running: is_running,
                    panes,
                    signals,
                }
            }
            model::Facet::Window { path, zone } => model::FacetSnapshot::Window {
                path: path.clone(),
                zone: zone.clone(),
            },
            model::Facet::Repo { path } => {
                let expanded = expand_tilde(path);
                let signals = git_signals(&expanded, lane.terminal_session());
                model::FacetSnapshot::Repo { path: path.clone(), signals }
            }
        }).collect();

        model::LaneSnapshot { id: lane.id.clone(), name: lane.name.clone(), facets }
    }).collect();

    model::LanewiseSnapshot {
        taken_at: chrono::Utc::now().to_rfc3339(),
        lanes,
    }
}

struct ClaudeRef {
    session_id: String,
    awaiting: bool,
}

fn claude_sessions_by_zellij() -> HashMap<String, Vec<ClaudeRef>> {
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = std::path::PathBuf::from(home).join(".claude").join("active-sessions");
    let mut map: HashMap<String, Vec<ClaudeRef>> = HashMap::new();

    let Ok(entries) = std::fs::read_dir(&dir) else { return map; };
    for entry in entries.filter_map(|e| e.ok()) {
        if entry.path().extension().map_or(true, |e| e != "json") { continue; }
        let Ok(data) = std::fs::read_to_string(entry.path()) else { continue; };
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&data) else { continue; };
        let Some(zs) = val["zellij_session"].as_str() else { continue; };
        let session_id = val["session_id"].as_str().unwrap_or("").to_string();
        let awaiting = val["state"].as_str().unwrap_or("") == "idle";
        map.entry(zs.to_string()).or_default().push(ClaudeRef { session_id, awaiting });
    }
    map
}

fn build_terminal_state(
    session: &str,
    claude: &HashMap<String, Vec<ClaudeRef>>,
) -> (Vec<model::PaneSnapshot>, Vec<model::Signal>) {
    let Some((shape, _)) = drivers::zellij::layout_for_session(session) else {
        return (vec![], vec![]);
    };

    let claude_refs = claude.get(session);
    let any_awaiting = claude_refs.map_or(false, |refs| refs.iter().any(|r| r.awaiting));

    let panes = shape.tabs.iter().flat_map(|tab| {
        tab.panes.iter().map(|pane| {
            let kind = match pane.command.as_deref() {
                Some("claude") => model::PaneKind::ClaudeSession { awaiting: any_awaiting },
                other => model::PaneKind::from_command(other),
            };
            model::PaneSnapshot { focused: pane.focused, cwd: pane.cwd.clone(), kind }
        })
    }).collect();

    let signals = claude_refs.map_or(vec![], |refs| {
        refs.iter()
            .filter(|r| r.awaiting)
            .map(|r| model::Signal {
                reason: model::SignalReason::ClaudeSessionAwaiting,
                action: Some(model::SignalAction::SwitchClaudeSession {
                    session_id: r.session_id.clone(),
                }),
            })
            .collect()
    });

    (panes, signals)
}

fn running_zellij_sessions() -> HashSet<String> {
    let Ok(out) = std::process::Command::new("zellij")
        .args(["list-sessions", "--short"])
        .output()
    else {
        return HashSet::new();
    };
    if !out.status.success() { return HashSet::new(); }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

fn git_signals(path: &str, session: Option<&str>) -> Vec<model::Signal> {
    let Ok(out) = std::process::Command::new("git")
        .args(["-C", path, "status", "--porcelain"])
        .output()
    else {
        return vec![];
    };
    if out.status.success() && !out.stdout.is_empty() {
        let action = session.map(|s| model::SignalAction::FocusRepoPane {
            session: s.to_string(),
            path: path.to_string(),
        });
        vec![model::Signal { reason: model::SignalReason::PendingCommit, action }]
    } else {
        vec![]
    }
}

pub fn navigate_to_repo_pane(session: &str, path: &str) -> Result<(), String> {
    // Look up display name from config for WezTerm tab matching
    let cfg = config::Config::load();
    let display_name = cfg.lanes.iter()
        .find(|l| l.terminal_session() == Some(session))
        .map(|l| l.display_name().to_string());

    // Activate the WezTerm tab for this session
    activate_wezterm_tab(session, display_name.as_deref())?;

    // Navigate within Zellij to the right tab
    let Some((shape, _)) = drivers::zellij::layout_for_session(session) else {
        return Ok(());
    };

    let zellij_tab = shape.tabs.iter().find(|tab| {
        tab.panes.iter().any(|p| p.cwd.as_deref() == Some(path))
    }).map(|t| t.name.clone());

    if let Some(tab) = zellij_tab {
        std::process::Command::new("/opt/homebrew/bin/zellij")
            .args(["--session", session, "action", "go-to-tab-name", &tab])
            .output()
            .map_err(|e| e.to_string())?;
    } else {
        std::process::Command::new("/opt/homebrew/bin/zellij")
            .args(["--session", session, "action", "new-tab", "--cwd", path])
            .output()
            .map_err(|e| e.to_string())?;
    }

    Ok(())
}

fn wezterm_socket() -> Option<String> {
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

fn activate_wezterm_tab(session: &str, display_name: Option<&str>) -> Result<(), String> {
    let sock = wezterm_socket();

    let mut cmd = std::process::Command::new("/opt/homebrew/bin/wezterm");
    cmd.args(["cli", "list", "--format", "json"]);
    if let Some(ref s) = sock {
        cmd.env("WEZTERM_UNIX_SOCKET", s);
    }
    let output = cmd.output().map_err(|e| format!("wezterm cli list: {}", e))?;
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("wezterm cli list parse: {}", e))?;

    let tab_id = json.as_array()
        .and_then(|panes| {
            panes.iter().find(|p| {
                p["tab_title"].as_str().map_or(false, |t| {
                    t == session || display_name.map_or(false, |dn| t == dn)
                })
            })
        })
        .and_then(|p| p["tab_id"].as_u64());

    let Some(id) = tab_id else {
        return Err(format!("no WezTerm tab found for session '{}'", session));
    };

    std::process::Command::new("open").args(["-a", "WezTerm"]).output().ok();

    let mut cmd = std::process::Command::new("/opt/homebrew/bin/wezterm");
    cmd.args(["cli", "activate-tab", "--tab-id", &id.to_string()]);
    if let Some(ref s) = sock {
        cmd.env("WEZTERM_UNIX_SOCKET", s);
    }
    cmd.output().map_err(|e| format!("wezterm activate-tab: {}", e))?;

    Ok(())
}

fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{}/{}", home, rest)
    } else {
        path.to_string()
    }
}

fn correlate(resources: &mut Vec<Observed>, cfg: &config::Config) {
    let lane_names = cfg.zellij_lane_names();

    let zellij_cwds: std::collections::HashMap<String, String> = resources
        .iter()
        .filter_map(|r| {
            if let model::Selector::Terminal(sel) = &r.selector {
                if sel.driver == "zellij" {
                    return r.cwd.as_ref().map(|cwd| (sel.id.clone(), cwd.clone()));
                }
            }
            None
        })
        .collect();

    for resource in resources.iter_mut() {
        if let model::Selector::Terminal(sel) = &resource.selector {
            if sel.driver != "claude" {
                continue;
            }
        } else {
            continue;
        }

        let extra = resource.extra.as_object_mut().unwrap();

        if let Some(zs) = extra.get("zellij_session").and_then(|v| v.as_str()) {
            let zs = zs.to_string();
            if let Some(zcwd) = zellij_cwds.get(&zs) {
                if resource.cwd.as_deref() == Some(zcwd.as_str()) {
                    extra.insert("zellij_cwd_match".to_string(), serde_json::json!(true));
                }
            }
            if let Some(lane) = lane_names.get(&zs) {
                extra.insert("lane".to_string(), serde_json::json!(lane));
            }
        }
    }

    for resource in resources.iter_mut() {
        if let model::Selector::Terminal(sel) = &resource.selector {
            if sel.driver == "zellij" {
                if let Some(lane) = lane_names.get(&sel.id) {
                    if let Some(extra) = resource.extra.as_object_mut() {
                        extra.insert("lane".to_string(), serde_json::json!(lane));
                    }
                }
            }
        }
    }
}
