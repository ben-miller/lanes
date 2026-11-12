//! Which Zellij sessions currently have an open WezTerm tab - ported from
//! lensd's `source::wezterm` (PLAN-retire-lensd.md step 4b), not
//! `infra_zellij.py`'s tab-title matching: WezTerm tab titles are
//! arbitrary (the user, or any tool, can set one to anything), so matching
//! by title is a heuristic that can silently miss or misattribute a tab.
//! The real link is structural: each WezTerm pane has a tty, and the
//! process actually running on that tty is `zellij attach <session>` (or
//! `zellij a <session>`) when that pane is showing a Zellij session. So a
//! tab "has" a session exactly when one of its panes' ttys is running that
//! attach process - true regardless of what the tab's title says.
//!
//! The live-GUI-socket lookup this needs (`wezterm_socket`, avoiding a
//! dead `gui-sock-*` file with a newer mtime than the live one - see
//! `BUG-wezterm-stale-socket.md`) already exists in `crate::wezterm_socket`
//! and is reused here rather than duplicated a third time (lensd
//! independently hit and fixed the same bug once already).

use std::collections::{HashMap, HashSet};
use std::process::Command;

use serde::Deserialize;

#[derive(Deserialize)]
struct RawPane {
    #[serde(default)]
    tty_name: Option<String>,
}

/// Every Zellij session currently shown by some open WezTerm tab - one
/// `wezterm cli list` call and one `ps` call cover every lane at once
/// (PLAN-retire-lensd.md step 4b: "one function covering every lane," not
/// N calls per snapshot). No live WezTerm GUI at all is a real, empty
/// answer (no tabs), not an error.
pub fn sessions_with_tabs() -> HashSet<String> {
    let Some(panes) = list_panes() else {
        return HashSet::new();
    };
    let clients = attach_clients();
    panes
        .into_iter()
        .filter_map(|p| p.tty_name.and_then(|t| clients.get(&normalize_tty(&t)).cloned()))
        .collect()
}

fn list_panes() -> Option<Vec<RawPane>> {
    let socket = crate::wezterm_socket()?;
    let out = Command::new("wezterm")
        .args(["cli", "--no-auto-start", "list", "--format", "json"])
        .env("WEZTERM_UNIX_SOCKET", socket)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

fn normalize_tty(tty: &str) -> String {
    tty.trim_start_matches("/dev/").to_string()
}

fn attach_clients() -> HashMap<String, String> {
    let Ok(out) = Command::new("ps").args(["-axo", "tty=,command="]).output() else {
        return HashMap::new();
    };
    parse_attach_clients(&String::from_utf8_lossy(&out.stdout))
}

/// `tty -> zellij session` for every `zellij attach [flags] <session>` (or
/// `zellij a <session>`) client in `ps`'s output.
fn parse_attach_clients(ps: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in ps.lines() {
        let mut words = line.split_whitespace();
        let Some(tty) = words.next() else { continue };
        let rest: Vec<&str> = words.collect();
        let Some(zi) = rest.iter().position(|w| w.rsplit('/').next() == Some("zellij")) else { continue };
        let Some(ai) = rest[zi + 1..].iter().position(|w| matches!(*w, "attach" | "a")) else { continue };
        if let Some(session) = rest[zi + 1 + ai + 1..].iter().find(|w| !w.starts_with('-')) {
            out.entry(tty.to_string()).or_insert_with(|| session.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(tty: Option<&str>) -> RawPane {
        RawPane { tty_name: tty.map(str::to_string) }
    }

    const PS: &str = "ttys007  /opt/homebrew/bin/zellij attach --create sheetwork-planner\n\
                      ttys009  /opt/homebrew/bin/zellij attach lanes\n\
                      ttys050  -fish\n\
                      ??       /opt/homebrew/bin/zellij --server /tmp/zellij/infra\n";

    #[test]
    fn parse_attach_clients_matches_attach_and_its_short_form() {
        let clients = parse_attach_clients(PS);
        assert_eq!(clients.get("ttys007").map(String::as_str), Some("sheetwork-planner"));
        assert_eq!(clients.get("ttys009").map(String::as_str), Some("lanes"));
    }

    #[test]
    fn parse_attach_clients_ignores_non_attach_processes() {
        let clients = parse_attach_clients(PS);
        assert!(!clients.contains_key("ttys050"), "a plain shell, not an attach client");
        assert!(!clients.values().any(|s| s == "infra"), "a zellij --server, not an attach client");
    }

    #[test]
    fn parse_attach_clients_handles_short_a_form() {
        let ps = "ttys011  /opt/homebrew/bin/zellij a boards-dev\n";
        let clients = parse_attach_clients(ps);
        assert_eq!(clients.get("ttys011").map(String::as_str), Some("boards-dev"));
    }

    #[test]
    fn normalize_tty_strips_the_dev_prefix() {
        assert_eq!(normalize_tty("/dev/ttys007"), "ttys007");
        assert_eq!(normalize_tty("ttys007"), "ttys007");
    }

    fn sessions_with_tabs_from(panes: Vec<RawPane>, clients: &HashMap<String, String>) -> HashSet<String> {
        panes
            .into_iter()
            .filter_map(|p| p.tty_name.and_then(|t| clients.get(&normalize_tty(&t)).cloned()))
            .collect()
    }

    #[test]
    fn sessions_with_tabs_links_a_pane_to_its_sessions_via_tty() {
        let panes = vec![pane(Some("/dev/ttys007")), pane(Some("/dev/ttys009")), pane(Some("/dev/ttys050"))];
        let clients = parse_attach_clients(PS);
        let sessions = sessions_with_tabs_from(panes, &clients);
        assert_eq!(sessions, HashSet::from(["sheetwork-planner".to_string(), "lanes".to_string()]));
    }

    #[test]
    fn sessions_with_tabs_is_empty_when_no_pane_has_a_tty() {
        let panes = vec![pane(None)];
        let clients = parse_attach_clients(PS);
        assert!(sessions_with_tabs_from(panes, &clients).is_empty());
    }
}
