use std::process::Command;

use serde_json::json;

use crate::model::*;

pub fn enumerate() -> Vec<Observed> {
    // Check brotab is present and connected before attempting anything
    let check = Command::new("bt").args(["clients"]).output();
    match check {
        Err(_) => {
            eprintln!("gather: brotab (bt) not found — browser driver skipped");
            return vec![];
        }
        Ok(out) if out.stdout.is_empty() => {
            eprintln!("gather: brotab reports no connected browsers — browser driver skipped");
            return vec![];
        }
        Ok(_) => {}
    }

    let list_out = match Command::new("bt").args(["list"]).output() {
        Ok(o) => o,
        Err(_) => return vec![],
    };

    let stdout = String::from_utf8_lossy(&list_out.stdout);
    let tabs: Vec<BrowserTabInfo> = stdout.lines().filter_map(parse_bt_line).collect();

    if tabs.is_empty() {
        return vec![];
    }

    // One Observed per browser window, with shape holding all its tabs
    let mut windows: std::collections::HashMap<String, Vec<BrowserTabInfo>> =
        std::collections::HashMap::new();
    for tab in &tabs {
        windows
            .entry(tab.window_id.clone())
            .or_default()
            .push(tab.clone());
    }

    windows
        .into_iter()
        .map(|(window_id, window_tabs)| {
            let first = &window_tabs[0];
            Observed {
                selector: Selector::Browser(BrowserSel {
                    url: first.url.clone(),
                    profile: None,
                }),
                locator: window_id.clone(),
                label: None,
                shape: Some(Shape::Browser(BrowserShape { tabs: window_tabs })),
                state: None,
                cwd: None,
                worktree_path: None,
                extra: json!({"window_id": window_id}),
            }
        })
        .collect()
}

// bt list line format: "prefix.window_id.tab_id\tTitle\tURL"
pub(crate) fn parse_bt_line(line: &str) -> Option<BrowserTabInfo> {
    let parts: Vec<&str> = line.splitn(3, '\t').collect();
    if parts.len() < 3 {
        return None;
    }
    let id_parts: Vec<&str> = parts[0].splitn(3, '.').collect();
    if id_parts.len() < 3 {
        return None;
    }
    Some(BrowserTabInfo {
        window_id: id_parts[1].to_string(),
        tab_id: id_parts[2].to_string(),
        title: parts[1].to_string(),
        url: parts[2].to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bt_list_line() {
        let line = "ff.1.42\tGitHub - balta2ar/brotab\thttps://github.com/balta2ar/brotab";
        let tab = parse_bt_line(line).unwrap();
        assert_eq!(tab.window_id, "1");
        assert_eq!(tab.tab_id, "42");
        assert_eq!(tab.title, "GitHub - balta2ar/brotab");
        assert_eq!(tab.url, "https://github.com/balta2ar/brotab");
    }

    #[test]
    fn rejects_malformed_lines() {
        assert!(parse_bt_line("no tabs here").is_none());
        assert!(parse_bt_line("ff.1\tTitle\tURL").is_none()); // only 2 id parts
        assert!(parse_bt_line("").is_none());
    }
}
