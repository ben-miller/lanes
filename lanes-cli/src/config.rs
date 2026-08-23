use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::model::{Lane, WindowPlacement};
use crate::scope::ScopeElement;

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

    /// The lane whose Terminal facet session matches the given Zellij session
    /// name, if any.
    pub fn lane_for_session(&self, session: &str) -> Option<&Lane> {
        self.lanes.iter().find(|l| l.terminal_session() == Some(session))
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
    scope: Vec<ScopeElementRaw>,
    #[serde(default)]
    windows: Vec<WindowPlacement>,
}

#[derive(Deserialize)]
struct LaneHeader {
    id: String,
    name: String,
    #[serde(default = "default_true")]
    active: bool,
}

fn default_true() -> bool {
    true
}

/// TOML-facing shape for a scope element - human-friendly fields
/// (`session`, `path`) rather than making config authors hand-write the
/// real ScopeElement's URI locators. Converted to the real type on load,
/// same pattern as LaneFile/LaneHeader vs. the real Lane.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ScopeElementRaw {
    ZellijSession { session: String },
    Repo { path: String },
}

impl From<ScopeElementRaw> for ScopeElement {
    fn from(raw: ScopeElementRaw) -> Self {
        match raw {
            ScopeElementRaw::ZellijSession { session } => ScopeElement::zellij_session(&session),
            ScopeElementRaw::Repo { path } => ScopeElement::repo(&path),
        }
    }
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
                active: file.lane.active,
                scope: file.scope.into_iter().map(ScopeElement::from).collect(),
                windows: file.windows,
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
    fn parses_lane_file_zellij_session_scope_element() {
        let content = r#"
[lane]
id = "sheetwork"
name = "Sheetwork"

[[scope]]
kind = "zellij_session"
session = "sheetwork"
"#;
        let file: LaneFile = toml::from_str(content).unwrap();
        assert_eq!(file.lane.id, "sheetwork");
        assert_eq!(file.lane.name, "Sheetwork");
        assert_eq!(file.scope.len(), 1);
        assert!(matches!(&file.scope[0], ScopeElementRaw::ZellijSession { session } if session == "sheetwork"));
    }

    #[test]
    fn lane_active_defaults_true_when_absent() {
        let content = r#"
[lane]
id = "sheetwork"
name = "Sheetwork"
"#;
        let file: LaneFile = toml::from_str(content).unwrap();
        assert!(file.lane.active);
    }

    #[test]
    fn lane_active_false_is_parsed() {
        let content = r#"
[lane]
id = "sheetwork"
name = "Sheetwork"
active = false
"#;
        let file: LaneFile = toml::from_str(content).unwrap();
        assert!(!file.lane.active);
    }

    #[test]
    fn lane_file_without_name_fails_to_parse() {
        let content = r#"
[lane]
id = "sheetwork"

[[scope]]
kind = "zellij_session"
session = "sheetwork"
"#;
        let result: Result<LaneFile, _> = toml::from_str(content);
        assert!(result.is_err());
    }

    #[test]
    fn parses_lane_file_with_name_and_window_placement() {
        let content = r#"
[lane]
id = "lanes-dev"
name = "lanes dev"

[[scope]]
kind = "zellij_session"
session = "lanes"

[[windows]]
path = "app:com.jetbrains.intellij / window"
zone = "main:1-2/3"
"#;
        let file: LaneFile = toml::from_str(content).unwrap();
        assert_eq!(file.lane.id, "lanes-dev");
        assert_eq!(file.lane.name, "lanes dev");
        assert_eq!(file.scope.len(), 1);
        assert_eq!(file.windows.len(), 1);
        assert!(file.windows[0].path.contains("intellij"));
        assert_eq!(file.windows[0].zone, "main:1-2/3");
    }

    #[test]
    fn scope_element_raw_converts_to_real_scope_element() {
        let el: ScopeElement = ScopeElementRaw::ZellijSession { session: "infra".to_string() }.into();
        assert_eq!(el.zellij_session_name(), Some("infra"));

        let el: ScopeElement = ScopeElementRaw::Repo { path: "~/src/infra".to_string() }.into();
        assert_eq!(el.repo_path(), Some("~/src/infra"));
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
    fn zellij_lane_names_derived_from_scope() {
        let cfg = Config {
            drivers: None,
            monitors: HashMap::new(),
            lanes: vec![
                Lane {
                    id: "sheetwork".to_string(),
                    name: "Sheetwork".to_string(),
                    active: true,
                    scope: vec![ScopeElement::zellij_session("sheetwork")],
                    windows: vec![],
                },
                Lane {
                    id: "lanes-dev".to_string(),
                    name: "lanes dev".to_string(),
                    active: true,
                    scope: vec![ScopeElement::zellij_session("lanes")],
                    windows: vec![],
                },
            ],
        };
        let names = cfg.zellij_lane_names();
        assert_eq!(names.get("sheetwork").map(|s| s.as_str()), Some("Sheetwork"));
        assert_eq!(names.get("lanes").map(|s| s.as_str()), Some("lanes dev"));
    }

    #[test]
    fn lane_for_session_finds_lane_by_zellij_session_scope_element() {
        let cfg = Config {
            drivers: None,
            monitors: HashMap::new(),
            lanes: vec![
                Lane {
                    id: "sheetwork1".to_string(),
                    name: "Sheetwork 1".to_string(),
                    active: true,
                    scope: vec![ScopeElement::zellij_session("sheetwork1")],
                    windows: vec![],
                },
                Lane {
                    id: "lanes-dev".to_string(),
                    name: "lanes dev".to_string(),
                    active: true,
                    scope: vec![ScopeElement::zellij_session("lanes")],
                    windows: vec![],
                },
            ],
        };
        assert_eq!(cfg.lane_for_session("lanes").map(|l| l.id.as_str()), Some("lanes-dev"));
        assert_eq!(cfg.lane_for_session("sheetwork1").map(|l| l.id.as_str()), Some("sheetwork1"));
        assert!(cfg.lane_for_session("job-hunting").is_none());
    }
}
