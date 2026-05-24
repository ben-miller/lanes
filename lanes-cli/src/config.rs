use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::model::{Facet, Lane};

#[derive(Clone)]
pub struct MonitorConfig {
    pub uuid: Option<String>,
    pub name: Option<String>,
}

pub struct Config {
    /// Drivers to run. None means all drivers.
    pub drivers: Option<Vec<String>>,
    /// Monitor handle -> config, from lanes.toml [monitors.*].
    pub monitors: HashMap<String, MonitorConfig>,
    /// Discovered lane definitions.
    pub lanes: Vec<Lane>,
}

impl Config {
    pub fn load() -> Self {
        let (drivers, monitors) = load_global_config();
        let lanes = load_lanes();
        Self { drivers, monitors, lanes }
    }

    pub fn monitor_uuid(&self, handle: &str) -> Option<&str> {
        self.monitors.get(handle)?.uuid.as_deref()
    }

    pub fn driver_enabled(&self, name: &str) -> bool {
        match &self.drivers {
            None => true,
            Some(list) => list.iter().any(|d| d == name),
        }
    }

    /// Returns a map of zellij session name -> lane display name,
    /// derived from Terminal facets across all lanes.
    pub fn zellij_lane_names(&self) -> HashMap<String, String> {
        self.lanes
            .iter()
            .filter_map(|lane| {
                lane.terminal_session()
                    .map(|s| (s.to_string(), lane.display_name().to_string()))
            })
            .collect()
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            drivers: None,
            monitors: HashMap::new(),
            lanes: Vec::new(),
        }
    }
}

pub fn config_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".config").join("lanes")
}

// --- Deserialization helpers ---

#[derive(Deserialize)]
struct GlobalConfig {
    #[serde(default)]
    drivers: Option<Vec<String>>,
    #[serde(default)]
    monitors: HashMap<String, MonitorConfigRaw>,
}

#[derive(Deserialize)]
struct MonitorConfigRaw {
    uuid: Option<String>,
    name: Option<String>,
}

#[derive(Deserialize)]
struct LaneFile {
    lane: LaneHeader,
    #[serde(default)]
    facets: Vec<Facet>,
}

#[derive(Deserialize)]
struct LaneHeader {
    id: String,
    name: Option<String>,
}

// --- Loaders ---

fn load_global_config() -> (Option<Vec<String>>, HashMap<String, MonitorConfig>) {
    let home = std::env::var("HOME").unwrap_or_default();
    let path = PathBuf::from(home).join(".config").join("lanes.toml");
    let content = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return (None, HashMap::new()),
    };
    let cfg: GlobalConfig = match toml::from_str(&content) {
        Ok(c) => c,
        Err(_) => return (None, HashMap::new()),
    };
    let monitors = cfg.monitors.into_iter()
        .map(|(k, v)| (k, MonitorConfig { uuid: v.uuid, name: v.name }))
        .collect();
    (cfg.drivers, monitors)
}

fn load_lanes() -> Vec<Lane> {
    let dir = config_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut lanes: Vec<Lane> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let s = name.to_string_lossy();
            s.ends_with(".toml") && s != "config.toml"
        })
        .filter_map(|e| {
            let path = e.path();
            let content = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(err) => { eprintln!("warning: could not read {:?}: {}", path, err); return None; }
            };
            let file: LaneFile = match toml::from_str(&content) {
                Ok(f) => f,
                Err(err) => { eprintln!("warning: could not parse {:?}: {}", path, err); return None; }
            };
            if file.lane.id.contains(char::is_whitespace) {
                eprintln!(
                    "warning: skipping lane file {:?} — id {:?} must not contain spaces",
                    e.file_name(),
                    file.lane.id
                );
                return None;
            }
            Some(Lane {
                id: file.lane.id,
                name: file.lane.name,
                facets: file.facets,
            })
        })
        .collect();

    lanes.sort_by(|a, b| a.id.cmp(&b.id));
    lanes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_global_drivers() {
        let cfg: super::GlobalConfig =
            toml::from_str(r#"drivers = ["zellij", "claude"]"#).unwrap();
        assert_eq!(
            cfg.drivers,
            Some(vec!["zellij".to_string(), "claude".to_string()])
        );
    }

    #[test]
    fn parses_lane_file_terminal_facet() {
        let content = r#"
[lane]
id = "sheetwork"

[[facets]]
kind = "terminal"
session = "sheetwork"
"#;
        let file: LaneFile = toml::from_str(content).unwrap();
        assert_eq!(file.lane.id, "sheetwork");
        assert_eq!(file.facets.len(), 1);
        assert!(matches!(&file.facets[0], Facet::Terminal { session } if session == "sheetwork"));
    }

    #[test]
    fn parses_lane_file_with_name_and_window_facet() {
        let content = r#"
[lane]
id = "lanes-dev"
name = "lanes dev"

[[facets]]
kind = "terminal"
session = "lanes"

[[facets]]
kind = "window"
path = "app:com.jetbrains.intellij / window"
zone = "main:1-2/3"
"#;
        let file: LaneFile = toml::from_str(content).unwrap();
        assert_eq!(file.lane.id, "lanes-dev");
        assert_eq!(file.lane.name.as_deref(), Some("lanes dev"));
        assert_eq!(file.facets.len(), 2);
        assert!(matches!(&file.facets[1], Facet::Window { path, zone } if path.contains("intellij") && zone == "main:1-2/3"));
    }

    #[test]
    fn driver_enabled_with_list() {
        let cfg = Config {
            drivers: Some(vec!["zellij".to_string(), "claude".to_string()]),
            monitors: HashMap::new(),
            lanes: Vec::new(),
        };
        assert!(cfg.driver_enabled("zellij"));
        assert!(cfg.driver_enabled("claude"));
        assert!(!cfg.driver_enabled("brotab"));
    }

    #[test]
    fn driver_enabled_without_list() {
        let cfg = Config::default();
        assert!(cfg.driver_enabled("zellij"));
        assert!(cfg.driver_enabled("brotab"));
    }

    #[test]
    fn zellij_lane_names_derived_from_facets() {
        use crate::model::Facet;
        let cfg = Config {
            drivers: None,
            monitors: HashMap::new(),
            lanes: vec![
                Lane {
                    id: "sheetwork".to_string(),
                    name: None,
                    facets: vec![Facet::Terminal {
                        session: "sheetwork".to_string(),
                    }],
                },
                Lane {
                    id: "lanes-dev".to_string(),
                    name: Some("lanes dev".to_string()),
                    facets: vec![Facet::Terminal {
                        session: "lanes".to_string(),
                    }],
                },
            ],
        };
        let names = cfg.zellij_lane_names();
        assert_eq!(names.get("sheetwork").map(|s| s.as_str()), Some("sheetwork"));
        assert_eq!(names.get("lanes").map(|s| s.as_str()), Some("lanes dev"));
    }
}
