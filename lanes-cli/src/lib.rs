pub mod config;
mod drivers;
pub mod model;
pub mod state;
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
    let claude = claude_sessions_by_zellij(&running);

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
        current_lane: state::read_current_lane(),
    }
}

struct ClaudeRef {
    session_id: String,
    state: String, // "idle" | "running" | "permission_pending"
}

fn claude_sessions_by_zellij(live_zellij_sessions: &HashSet<String>) -> HashMap<String, Vec<ClaudeRef>> {
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = std::path::PathBuf::from(home).join(".claude").join("active-sessions");
    let mut map: HashMap<String, Vec<ClaudeRef>> = HashMap::new();

    let Ok(entries) = std::fs::read_dir(&dir) else { return map; };
    for entry in entries.filter_map(|e| e.ok()) {
        if entry.path().extension().map_or(true, |e| e != "json") { continue; }
        let Ok(data) = std::fs::read_to_string(entry.path()) else { continue; };
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&data) else { continue; };
        let Some(zs) = val["zellij_session"].as_str() else { continue; };
        let pid = val["pid"].as_u64().map(|p| p as u32);
        if !session_is_live(zs, live_zellij_sessions, pid) { continue; }
        let session_id = val["session_id"].as_str().unwrap_or("").to_string();
        let raw_state = val["state"].as_str().unwrap_or("running");
        let state = if raw_state == "permission_pending" {
            let stale = entry.path().metadata()
                .and_then(|m| m.modified())
                .ok().and_then(|t| t.elapsed().ok())
                .map_or(false, |age| age.as_secs() >= 1);
            if stale { "idle".to_string() } else { raw_state.to_string() }
        } else {
            raw_state.to_string()
        };
        map.entry(zs.to_string()).or_default().push(ClaudeRef { session_id, state });
    }
    map
}

/// Whether a registry entry for a Claude session still refers to something actually
/// running, rather than a file orphaned by a session that ended without firing
/// `SessionEnd` (crash, force-quit, killed pane).
///
/// Sessions living in a Zellij pane are verified against currently running Zellij
/// sessions - reliable, no guessing. Sessions started outside Zellij have no such
/// anchor, so we fall back to checking that the recorded PID is both alive and is
/// actually a `claude` process - a bare `kill -0` isn't enough since PIDs get
/// reused, so a dead session's orphaned PID could later collide with an unrelated
/// process.
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
        return live_zellij_sessions.contains(zellij_session);
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

fn build_terminal_state(
    session: &str,
    claude: &HashMap<String, Vec<ClaudeRef>>,
) -> (Vec<model::PaneSnapshot>, Vec<model::Signal>) {
    let Some((shape, _)) = drivers::zellij::layout_for_session(session) else {
        return (vec![], vec![]);
    };

    let claude_refs = claude.get(session);
    let needs_attention = claude_refs.map_or(false, |refs| {
        refs.iter().any(|r| matches!(r.state.as_str(), "idle" | "permission_pending"))
    });

    let panes = shape.tabs.iter().flat_map(|tab| {
        tab.panes.iter().map(|pane| {
            let kind = match pane.command.as_deref() {
                Some("claude") => model::PaneKind::ClaudeSession { awaiting: needs_attention },
                other => model::PaneKind::from_command(other),
            };
            model::PaneSnapshot { focused: pane.focused, cwd: pane.cwd.clone(), kind }
        })
    }).collect();

    let signals = claude_refs.map_or(vec![], |refs| {
        refs.iter()
            .map(|r| model::Signal {
                reason: match r.state.as_str() {
                    "idle" => model::SignalReason::ClaudeSessionAwaiting,
                    "permission_pending" => model::SignalReason::ClaudeSessionPermission,
                    _ => model::SignalReason::ClaudeSessionActive,
                },
                action: Some(model::SignalAction::SwitchClaudeSession {
                    session_id: r.session_id.clone(),
                }),
            })
            .collect()
    });

    (panes, signals)
}

pub(crate) fn running_zellij_sessions() -> HashSet<String> {
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
    let wezterm_tab_id = val["wezterm_tab_id"].as_u64();

    if let Some(tab_id) = wezterm_tab_id {
        std::process::Command::new("open").args(["-a", "WezTerm"]).output().ok();
        let sock = wezterm_socket();
        let mut cmd = std::process::Command::new("/opt/homebrew/bin/wezterm");
        cmd.args(["cli", "activate-tab", "--tab-id", &tab_id.to_string()]);
        if let Some(ref s) = sock {
            cmd.env("WEZTERM_UNIX_SOCKET", s);
        }
        cmd.output().map_err(|e| format!("wezterm activate-tab: {}", e))?;
    }

    if !zellij_session.is_empty() {
        if let Some(pane_id) = zellij_pane_id {
            std::process::Command::new("/opt/homebrew/bin/zellij")
                .args(["--session", &zellij_session, "action", "focus-pane-id", &pane_id.to_string()])
                .output()
                .map_err(|e| format!("zellij focus-pane-id: {}", e))?;
        }
    }

    Ok(())
}

pub fn navigate_to_repo_pane(session: &str, path: &str) -> Result<(), String> {
    // Activate the WezTerm tab for this session
    activate_wezterm_tab(session, true)?;

    // Navigate within Zellij to the right tab
    let Some((shape, _)) = drivers::zellij::layout_for_session(session) else {
        return Ok(());
    };

    let target_tab = shape.tabs.iter().find(|tab| {
        tab.panes.iter().any(|p| p.cwd.as_deref() == Some(path))
    });

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

fn activate_wezterm_tab(session: &str, focus: bool) -> Result<(), String> {
    let cached = state::get_wezterm_tab_id(session).ok_or_else(|| {
        format!("no cached WezTerm tab for session '{}' - run `lanes tabs set {} <id>`", session, session)
    })?;

    let sock = wezterm_socket();

    let mut cmd = std::process::Command::new("/opt/homebrew/bin/wezterm");
    cmd.args(["cli", "list", "--format", "json"]);
    if let Some(ref s) = sock {
        cmd.env("WEZTERM_UNIX_SOCKET", s);
    }
    let output = cmd.output().map_err(|e| format!("wezterm cli list: {}", e))?;
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("wezterm cli list parse: {}", e))?;
    let live_tab_ids: HashSet<u64> = json.as_array()
        .map(|panes| panes.iter().filter_map(|p| p["tab_id"].as_u64()).collect())
        .unwrap_or_default();

    if !live_tab_ids.contains(&cached) {
        return Err(format!(
            "cached WezTerm tab {} for session '{}' no longer exists - run `lanes tabs set {} <id>`",
            cached, session, session
        ));
    }

    if focus {
        std::process::Command::new("open").args(["-a", "WezTerm"]).output().ok();
    }

    let mut cmd = std::process::Command::new("/opt/homebrew/bin/wezterm");
    cmd.args(["cli", "activate-tab", "--tab-id", &cached.to_string()]);
    if let Some(ref s) = sock {
        cmd.env("WEZTERM_UNIX_SOCKET", s);
    }
    cmd.output().map_err(|e| format!("wezterm activate-tab: {}", e))?;

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

pub fn activate_lane(lane_id: &str, focus: bool) {
    let cfg = config::Config::load();
    let lane = match cfg.lanes.iter().find(|l| l.id == lane_id) {
        Some(l) => l,
        None => {
            eprintln!("error: lane not found: {}", lane_id);
            return;
        }
    };

    for facet in &lane.facets {
        match facet {
            model::Facet::Terminal { session } => {
                if let Err(e) = activate_wezterm_tab(session, focus) {
                    eprintln!("warning: {}", e);
                }
            }
            model::Facet::Window { path, zone } => {
                if let Err(e) = activate_window_facet(path, zone, &cfg) {
                    eprintln!("warning: {}", e);
                }
            }
            model::Facet::Repo { .. } => {}
        }
    }

    state::set_current_lane(lane_id);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bundle_id() {
        assert_eq!(
            parse_bundle_id("app:com.github.wez.wezterm / window"),
            Some("com.github.wez.wezterm".to_string())
        );
    }

    #[test]
    fn zellij_backed_session_live_iff_session_running() {
        let live: HashSet<String> = ["lanes".to_string()].into_iter().collect();
        assert!(session_is_live_with("lanes", &live, None, |_| unreachable!("should not need pid lookup")));
        assert!(!session_is_live_with("job-hunting", &live, None, |_| unreachable!("should not need pid lookup")));
    }

    #[test]
    fn zellij_backed_session_ignores_pid_entirely() {
        // Even a "live" pid shouldn't matter once the Zellij session itself is gone -
        // the pane is the source of truth for sessions that were ever pane-attached.
        let live: HashSet<String> = HashSet::new();
        assert!(!session_is_live_with("lanes", &live, Some(123), |_| Some("claude".to_string())));
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
