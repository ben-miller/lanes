pub mod config;
mod drivers;
pub mod model;
pub mod zone;

use model::{Observed, Snapshot};
use std::collections::HashSet;

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

    let lanes = cfg.lanes.iter().map(|lane| {
        let facets = lane.facets.iter().map(|facet| match facet {
            model::Facet::Terminal { session } => model::FacetSnapshot::Terminal {
                session: session.clone(),
                running: running.contains(session.as_str()),
            },
            model::Facet::Window { path, zone } => model::FacetSnapshot::Window {
                path: path.clone(),
                zone: zone.clone(),
            },
            model::Facet::Repo { path } => {
                let expanded = expand_tilde(path);
                let signals = git_signals(&expanded);
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

fn git_signals(path: &str) -> Vec<model::Signal> {
    let Ok(out) = std::process::Command::new("git")
        .args(["-C", path, "status", "--porcelain"])
        .output()
    else {
        return vec![];
    };
    if out.status.success() && !out.stdout.is_empty() {
        vec![model::Signal { reason: model::SignalReason::PendingCommit }]
    } else {
        vec![]
    }
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
