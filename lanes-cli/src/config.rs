use std::collections::HashMap;
use std::path::PathBuf;

pub struct Config {
    /// Drivers to run. None means all drivers.
    pub drivers: Option<Vec<String>>,
    /// zellij_session -> lane_name
    pub lane_names: HashMap<String, String>,
}

impl Config {
    pub fn load() -> Self {
        let path = registry_path();
        let content = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => return Self::default(),
        };
        parse(&content)
    }

    pub fn driver_enabled(&self, name: &str) -> bool {
        match &self.drivers {
            None => true,
            Some(list) => list.iter().any(|d| d == name),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            drivers: None,
            lane_names: HashMap::new(),
        }
    }
}

pub fn registry_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home)
        .join(".config")
        .join("lanes")
        .join("registry.toml")
}

fn parse(content: &str) -> Config {
    let mut drivers: Option<Vec<String>> = None;
    let mut lane_names: HashMap<String, String> = HashMap::new();
    let mut current_name: Option<String> = None;
    let mut current_zellij: Option<String> = None;

    for line in content.lines() {
        let line = line.trim();

        if line == "[[lanes]]" {
            if let (Some(name), Some(zs)) = (current_name.take(), current_zellij.take()) {
                lane_names.insert(zs, name);
            }
            continue;
        }

        if line.starts_with("drivers = [") {
            drivers = Some(parse_string_array(line));
            continue;
        }

        if let Some(val) = toml_str_value(line, "name") {
            current_name = Some(val);
        } else if let Some(val) = toml_str_value(line, "zellij_session") {
            current_zellij = Some(val);
        }
    }

    if let (Some(name), Some(zs)) = (current_name, current_zellij) {
        lane_names.insert(zs, name);
    }

    Config {
        drivers,
        lane_names,
    }
}

// Parse `drivers = ["a", "b", "c"]` -> vec!["a", "b", "c"]
fn parse_string_array(line: &str) -> Vec<String> {
    let start = match line.find('[') {
        Some(i) => i + 1,
        None => return vec![],
    };
    let end = match line.find(']') {
        Some(i) => i,
        None => return vec![],
    };
    line[start..end]
        .split(',')
        .filter_map(|s| {
            let s = s.trim().trim_matches('"');
            if s.is_empty() {
                None
            } else {
                Some(s.to_string())
            }
        })
        .collect()
}

fn toml_str_value(line: &str, key: &str) -> Option<String> {
    let prefix = format!("{} = \"", key);
    if !line.starts_with(&prefix) {
        return None;
    }
    let rest = &line[prefix.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const REGISTRY: &str = r#"
drivers = ["zellij", "claude"]

[[lanes]]
name = "sheetwork"
zellij_session = "sheetwork"
position = 0

[[lanes]]
name = "lanes dev"
zellij_session = "lanes"
position = 1
"#;

    #[test]
    fn parses_drivers_list() {
        let cfg = parse(REGISTRY);
        assert_eq!(
            cfg.drivers,
            Some(vec!["zellij".to_string(), "claude".to_string()])
        );
    }

    #[test]
    fn parses_lane_names() {
        let cfg = parse(REGISTRY);
        assert_eq!(
            cfg.lane_names.get("sheetwork").map(|s| s.as_str()),
            Some("sheetwork")
        );
        assert_eq!(
            cfg.lane_names.get("lanes").map(|s| s.as_str()),
            Some("lanes dev")
        );
    }

    #[test]
    fn driver_enabled_with_list() {
        let cfg = parse(REGISTRY);
        assert!(cfg.driver_enabled("zellij"));
        assert!(cfg.driver_enabled("claude"));
        assert!(!cfg.driver_enabled("brotab"));
    }

    #[test]
    fn driver_enabled_without_list() {
        let cfg = parse("[[lanes]]\nname = \"foo\"\nzellij_session = \"foo\"\n");
        assert!(cfg.driver_enabled("zellij"));
        assert!(cfg.driver_enabled("brotab"));
    }

    #[test]
    fn missing_file_gives_default() {
        let cfg = Config::default();
        assert!(cfg.drivers.is_none());
        assert!(cfg.lane_names.is_empty());
        assert!(cfg.driver_enabled("anything"));
    }
}
