use std::collections::{BTreeMap, HashMap, HashSet};
use std::process::Command;

use serde::Deserialize;

use crate::model::*;

pub fn layout_for_session(session: &str) -> Option<(TerminalShape, Option<String>)> {
    dump_layout(session)
}

#[derive(Deserialize)]
struct RawPaneInfo {
    id: u32,
    is_plugin: bool,
    is_focused: bool,
    tab_position: usize,
    tab_name: String,
    pane_x: i64,
    pane_y: i64,
    #[serde(default)]
    pane_command: Option<String>,
    #[serde(default)]
    pane_cwd: Option<String>,
}

fn list_panes(session: &str) -> Vec<RawPaneInfo> {
    let Ok(out) = Command::new("/opt/homebrew/bin/zellij")
        .args(["--session", session, "action", "list-panes", "--all", "--json"])
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    serde_json::from_slice(&out.stdout).unwrap_or_default()
}

/// Terminal pane id -> (tab_position, pane_y, pane_x), i.e. this pane's
/// on-screen reading-order position: which Zellij tab it's in, then top-to-
/// bottom/left-to-right within that tab. The id is the same numeric
/// `$ZELLIJ_PANE_ID` a Claude session's SessionStart hook captures at
/// startup (see `drivers::claude::ClaudeSession::zellij_pane_id`) - plugin
/// panes (tab bar, etc.) have their own separate id namespace and are
/// excluded here so they can't collide with a terminal pane's id.
pub fn pane_positions(session: &str) -> HashMap<u32, (usize, i64, i64)> {
    positions_from_panes(&list_panes(session))
}

fn positions_from_panes(panes: &[RawPaneInfo]) -> HashMap<u32, (usize, i64, i64)> {
    panes.iter()
        .filter(|p| !p.is_plugin)
        .map(|p| (p.id, (p.tab_position, p.pane_y, p.pane_x)))
        .collect()
}

/// A session's TerminalShape (for the UI's per-pane display) and every
/// pane's on-screen position (see `pane_positions`), from a single
/// `list-panes` call - `gather_lanes()` used to make a separate
/// `dump-layout` call for the shape alone, on top of `list-panes` for
/// position, doubling the per-session subprocess round-trips on every
/// refresh (measured ~600-700ms total vs ~220ms for the shape alone; see
/// perf.log's `gather_lanes.subprocess_batch_done`). `list-panes --all
/// --json` already carries everything `dump-layout`'s KDL gave us
/// (command, cwd, focus) plus real geometry and pane ids, so one call
/// produces both.
pub fn shape_and_positions_for_session(session: &str) -> (TerminalShape, HashMap<u32, (usize, i64, i64)>) {
    let panes = list_panes(session);
    (shape_from_panes(&panes), positions_from_panes(&panes))
}

fn shape_from_panes(panes: &[RawPaneInfo]) -> TerminalShape {
    let mut by_tab: BTreeMap<usize, TabInfo> = BTreeMap::new();
    for p in panes.iter().filter(|p| !p.is_plugin) {
        let tab = by_tab.entry(p.tab_position).or_insert_with(|| TabInfo {
            name: p.tab_name.clone(),
            focused: false,
            panes: Vec::new(),
        });
        if p.is_focused {
            tab.focused = true;
        }
        tab.panes.push(PaneInfo { command: p.pane_command.clone(), focused: p.is_focused, cwd: p.pane_cwd.clone() });
    }
    TerminalShape { cwd: None, tabs: by_tab.into_values().collect() }
}

/// `--short` lists a session name regardless of whether it's actually
/// attachable - a session whose pane died without the session itself being
/// killed shows up there identically to a genuinely running one, just
/// marked "(EXITED - attach to resurrect)" in the fuller listing. Using
/// `--no-formatting` instead (plain text, no ANSI color codes, but keeps
/// the EXITED annotation `--short` throws away) so exited-but-not-dead
/// sessions can actually be told apart from live ones.
pub fn running_sessions() -> HashSet<String> {
    let Ok(out) = Command::new("/opt/homebrew/bin/zellij")
        .args(["list-sessions", "--no-formatting"])
        .output()
    else {
        return HashSet::new();
    };
    if !out.status.success() { return HashSet::new(); }
    parse_running_sessions(&String::from_utf8_lossy(&out.stdout))
}

fn parse_running_sessions(text: &str) -> HashSet<String> {
    text.lines()
        .filter(|l| !l.contains("(EXITED"))
        .filter_map(|l| l.split(" [Created").next())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn dump_layout(session: &str) -> Option<(TerminalShape, Option<String>)> {
    let output = Command::new("/opt/homebrew/bin/zellij")
        .args(["--session", session, "action", "dump-layout"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    Some(parse_kdl_layout(&text))
}

fn parse_kdl_layout(kdl: &str) -> (TerminalShape, Option<String>) {
    let mut session_cwd: Option<String> = None;
    let mut tabs: Vec<TabInfo> = Vec::new();

    // depth tracking: 0 = outside layout, 1 = inside layout {}, 2+ = inside tab/pane
    let mut depth: usize = 0;
    let mut current_tab: Option<TabInfo> = None;
    let mut in_tab = false;
    let mut tab_depth: usize = 0;

    for line in kdl.lines() {
        let trimmed = line.trim();

        // Count braces to track depth
        let opens = trimmed.chars().filter(|&c| c == '{').count();
        let closes = trimmed.chars().filter(|&c| c == '}').count();

        // Session-level cwd: depth 1, `cwd "..."` line
        if depth == 1 && !in_tab {
            if let Some(val) = kdl_arg(trimmed, "cwd") {
                session_cwd = Some(val);
            }
        }

        // Tab start: depth 1
        if depth == 1 && trimmed.starts_with("tab ") && opens > 0 {
            let name = kdl_prop(trimmed, "name").unwrap_or_else(|| "unnamed".to_string());
            let focused = trimmed.contains("focus=true");
            current_tab = Some(TabInfo {
                name,
                focused,
                panes: Vec::new(),
            });
            in_tab = true;
            tab_depth = depth;
        }

        // Panes inside a tab — skip plugin/borderless UI panes and split containers
        if in_tab && depth > tab_depth && trimmed.starts_with("pane ") {
            let borderless = trimmed.contains("borderless=true");
            let is_split = trimmed.contains("split_direction=");
            let cmd = kdl_prop(trimmed, "command");
            // A pane that opens a block but has no command is a split container, not a leaf
            let is_container = opens > 0 && cmd.is_none();
            if !borderless && !is_split && !is_container {
                let focused = trimmed.contains("focus=true");
                let cwd = kdl_prop(trimmed, "cwd");
                if let Some(tab) = current_tab.as_mut() {
                    tab.panes.push(PaneInfo { command: cmd, focused, cwd });
                }
            }
        }

        // Update depth after processing the line
        depth = depth.saturating_add(opens).saturating_sub(closes);

        // If we've closed back to tab_depth, the tab block ended
        if in_tab && depth <= tab_depth && closes > 0 {
            if let Some(tab) = current_tab.take() {
                tabs.push(tab);
            }
            in_tab = false;
        }
    }

    // Resolve pane cwds: inherit session cwd if absent, resolve relative paths against it
    if let Some(ref scwd) = session_cwd {
        for tab in &mut tabs {
            for pane in &mut tab.panes {
                pane.cwd = Some(match &pane.cwd {
                    None => scwd.clone(),
                    Some(pcwd) if !pcwd.starts_with('/') => format!("{}/{}", scwd, pcwd),
                    Some(pcwd) => pcwd.clone(),
                });
            }
        }
    }

    (
        TerminalShape {
            cwd: session_cwd.clone(),
            tabs,
        },
        session_cwd,
    )
}

// Extract a space-separated quoted argument: `node "value"`
fn kdl_arg(line: &str, node: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with(node) {
        return None;
    }
    let rest = trimmed[node.len()..].trim_start();
    extract_quoted(rest)
}

// Extract a key="value" property from anywhere in the line
fn kdl_prop(line: &str, key: &str) -> Option<String> {
    let needle = format!("{}=\"", key);
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn extract_quoted(s: &str) -> Option<String> {
    let s = s.trim_start();
    if !s.starts_with('"') {
        return None;
    }
    let inner = &s[1..];
    let end = inner.find('"')?;
    Some(inner[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_pane(id: u32, is_plugin: bool, is_focused: bool, tab_position: usize, tab_name: &str, command: Option<&str>, cwd: Option<&str>) -> RawPaneInfo {
        RawPaneInfo {
            id,
            is_plugin,
            is_focused,
            tab_position,
            tab_name: tab_name.to_string(),
            pane_x: 0,
            pane_y: 0,
            pane_command: command.map(str::to_string),
            pane_cwd: cwd.map(str::to_string),
        }
    }

    #[test]
    fn positions_from_panes_excludes_plugins_and_keys_by_id() {
        let panes = vec![
            raw_pane(0, true, false, 0, "Tab #1", None, None),
            raw_pane(0, false, true, 0, "Tab #1", Some("claude"), Some("/a")),
            raw_pane(3, false, false, 1, "Tab #2", Some("claude"), Some("/a")),
        ];
        let positions = positions_from_panes(&panes);
        assert_eq!(positions.len(), 2);
        assert!(positions.contains_key(&0));
        assert!(positions.contains_key(&3));
    }

    #[test]
    fn shape_from_panes_groups_by_tab_position_excluding_plugins() {
        let panes = vec![
            raw_pane(0, true, false, 0, "Tab #1", None, None),
            raw_pane(0, false, true, 0, "Tab #1", Some("claude"), Some("/a")),
            raw_pane(1, false, false, 0, "Tab #1", None, Some("/b")),
            raw_pane(3, false, false, 1, "Tab #2", Some("claude"), Some("/a")),
        ];
        let shape = shape_from_panes(&panes);
        assert_eq!(shape.tabs.len(), 2);
        assert_eq!(shape.tabs[0].name, "Tab #1");
        assert_eq!(shape.tabs[0].panes.len(), 2);
        assert_eq!(shape.tabs[0].panes[0].command.as_deref(), Some("claude"));
        assert!(shape.tabs[0].panes[0].focused);
        assert_eq!(shape.tabs[1].name, "Tab #2");
        assert_eq!(shape.tabs[1].panes.len(), 1);
    }

    const LAYOUT: &str = r#"layout {
    cwd "/Users/bmiller/src/projects/sheetwork"
    tab name="Tab #1" focus=true hide_floating_panes=true {
        pane size=1 borderless=true {
            plugin location="zellij:tab-bar"
        }
        pane split_direction="vertical" {
            pane command="claude" focus=true size="50%" {
                args "--resume" "sheetwork"
                start_suspended true
            }
            pane size="50%"
        }
    }
    new_tab_template {
        pane size=1 borderless=true {
            plugin location="zellij:tab-bar"
        }
        pane
    }
}"#;

    #[test]
    fn parses_session_cwd() {
        let (shape, cwd) = parse_kdl_layout(LAYOUT);
        assert_eq!(
            cwd.as_deref(),
            Some("/Users/bmiller/src/projects/sheetwork")
        );
        assert_eq!(
            shape.cwd.as_deref(),
            Some("/Users/bmiller/src/projects/sheetwork")
        );
    }

    #[test]
    fn parses_tab_name_and_focus() {
        let (shape, _) = parse_kdl_layout(LAYOUT);
        assert_eq!(shape.tabs.len(), 1);
        assert_eq!(shape.tabs[0].name, "Tab #1");
        assert!(shape.tabs[0].focused);
    }

    #[test]
    fn parses_pane_command() {
        let (shape, _) = parse_kdl_layout(LAYOUT);
        assert_eq!(shape.tabs[0].panes.len(), 2);
        assert_eq!(shape.tabs[0].panes[0].command.as_deref(), Some("claude"));
        assert!(shape.tabs[0].panes[0].focused);
        assert_eq!(shape.tabs[0].panes[1].command, None);
    }

    #[test]
    fn pane_inherits_session_cwd() {
        let (shape, _) = parse_kdl_layout(LAYOUT);
        assert_eq!(
            shape.tabs[0].panes[0].cwd.as_deref(),
            Some("/Users/bmiller/src/projects/sheetwork")
        );
        assert_eq!(
            shape.tabs[0].panes[1].cwd.as_deref(),
            Some("/Users/bmiller/src/projects/sheetwork")
        );
    }

    #[test]
    fn pane_resolves_relative_cwd() {
        let layout = r#"layout {
    cwd "/Users/bmiller"
    tab name="Tab #1" focus=true hide_floating_panes=true {
        pane size=1 borderless=true {
            plugin location="zellij:tab-bar"
        }
        pane cwd="src/projects/sheetwork" focus=true
    }
}"#;
        let (shape, _) = parse_kdl_layout(layout);
        assert_eq!(
            shape.tabs[0].panes[0].cwd.as_deref(),
            Some("/Users/bmiller/src/projects/sheetwork")
        );
    }

    #[test]
    fn new_tab_template_not_included() {
        let (shape, _) = parse_kdl_layout(LAYOUT);
        assert_eq!(shape.tabs.len(), 1);
    }

    #[test]
    fn nested_split_container_not_included() {
        // A pane with a block but no command is a split container — should not appear as a leaf
        let layout = r#"layout {
    cwd "/Users/bmiller/src/projects/lanes"
    tab name="Tab #1" focus=true hide_floating_panes=true {
        pane size=1 borderless=true {
            plugin location="zellij:tab-bar"
        }
        pane split_direction="vertical" {
            pane command="claude" focus=true size="50%" {
                args "--resume" "lanes"
                start_suspended true
            }
            pane size="50%" {
                pane command="npm" cwd="ui" size="50%" {
                    args "run" "tauri" "dev"
                    start_suspended true
                }
                pane size="50%"
            }
        }
    }
}"#;
        let (shape, _) = parse_kdl_layout(layout);
        assert_eq!(shape.tabs[0].panes.len(), 3);
        assert_eq!(shape.tabs[0].panes[0].command.as_deref(), Some("claude"));
        assert_eq!(shape.tabs[0].panes[1].command.as_deref(), Some("npm"));
        assert_eq!(shape.tabs[0].panes[2].command, None);
    }

    #[test]
    fn kdl_prop_extracts_value() {
        assert_eq!(
            kdl_prop(r#"tab name="My Tab" focus=true"#, "name"),
            Some("My Tab".to_string())
        );
        assert_eq!(
            kdl_prop(r#"pane command="claude" size="50%""#, "command"),
            Some("claude".to_string())
        );
        assert_eq!(kdl_prop("pane size=1 borderless=true", "command"), None);
    }

    #[test]
    fn kdl_arg_extracts_value() {
        assert_eq!(
            kdl_arg(r#"cwd "/some/path""#, "cwd"),
            Some("/some/path".to_string())
        );
        assert_eq!(kdl_arg(r#"tab name="foo""#, "cwd"), None);
    }

    #[test]
    fn parse_running_sessions_excludes_exited() {
        let text = "infra [Created 4days ago] \n\
                     spinner [Created 4h ago] (EXITED - attach to resurrect)\n\
                     lanes [Created 4days ago] (current)\n";
        let running = parse_running_sessions(text);
        assert!(running.contains("infra"));
        assert!(running.contains("lanes"));
        assert!(!running.contains("spinner"));
    }

    #[test]
    fn parse_running_sessions_handles_names_with_spaces() {
        let text = "sheetwork planner [Created 4days ago] \n";
        let running = parse_running_sessions(text);
        assert!(running.contains("sheetwork planner"));
    }

}
