use std::process::Command;

use serde_json::json;

use crate::model::*;

pub fn enumerate() -> Vec<Observed> {
    let output = match Command::new("zellij")
        .args(["list-sessions", "--no-formatting"])
        .output()
    {
        Ok(o) => o,
        Err(_) => {
            eprintln!("gather: zellij not found");
            return vec![];
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| parse_session_line(line))
        .collect()
}

// Line format: "session-name [Created Xdays Yh ...] (EXITED - ...)" or without (EXITED)
fn parse_session_line(line: &str) -> Option<Observed> {
    let name = line.split_whitespace().next()?.to_string();
    let exited = line.contains("(EXITED");

    let (shape, cwd) = if !exited {
        match dump_layout(&name) {
            Some((s, c)) => (Some(s), c),
            None => (None, None),
        }
    } else {
        (None, None)
    };

    let status = if exited { Status::Gone } else { Status::Idle };

    Some(Observed {
        selector: Selector::Terminal(TerminalSel {
            driver: "zellij".to_string(),
            id: name.clone(),
        }),
        locator: name.clone(),
        label: None,
        shape: shape.map(Shape::Terminal),
        state: Some(State {
            status,
            detail: Some(DriverState::Repl(ReplState {
                activity: if exited {
                    "exited".to_string()
                } else {
                    "idle".to_string()
                },
            })),
        }),
        cwd,
        worktree_path: None,
        extra: json!({}),
    })
}

pub fn layout_for_session(session: &str) -> Option<(TerminalShape, Option<String>)> {
    dump_layout(session)
}

fn dump_layout(session: &str) -> Option<(TerminalShape, Option<String>)> {
    let output = Command::new("zellij")
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

        // Panes inside a tab — skip plugin/borderless UI panes
        if in_tab && depth > tab_depth && trimmed.starts_with("pane ") {
            let borderless = trimmed.contains("borderless=true");
            let is_split = trimmed.contains("split_direction=");
            if !borderless && !is_split {
                let cmd = kdl_prop(trimmed, "command");
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
        // new_tab_template is not a real tab and should not appear
        assert_eq!(shape.tabs.len(), 1);
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
}
