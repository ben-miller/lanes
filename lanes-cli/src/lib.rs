pub mod config;
mod drivers;
pub mod model;
pub mod zone;

use model::{Observed, Snapshot};

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
