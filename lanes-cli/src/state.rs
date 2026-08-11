use std::path::PathBuf;

use kdl::{KdlDocument, KdlNode};

fn state_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local/state/lanes/state.kdl")
}

fn load_doc() -> KdlDocument {
    std::fs::read_to_string(state_path())
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(KdlDocument::new)
}

fn save_doc(doc: &KdlDocument) {
    let path = state_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(path, doc.to_string()).ok();
}

fn now_millis() -> i128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i128)
        .unwrap_or(0)
}

// state.kdl mostly holds flat scalar nodes (`name value`) - one value per
// name, last write wins. These three getters plus `put_scalar` are the
// single implementation of that pattern; every field below is a thin,
// named wrapper around them.

fn get_scalar_str(doc: &KdlDocument, name: &str) -> Option<String> {
    doc.get(name).and_then(|n| n.get(0)).and_then(|v| v.as_string()).map(|s| s.to_string())
}

fn get_scalar_bool(doc: &KdlDocument, name: &str) -> bool {
    doc.get(name).and_then(|n| n.get(0)).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn get_scalar_int(doc: &KdlDocument, name: &str) -> i128 {
    doc.get(name).and_then(|n| n.get(0)).and_then(|v| v.as_integer()).unwrap_or(0)
}

fn put_scalar(doc: &mut KdlDocument, name: &str, value: impl Into<kdl::KdlEntry>) {
    doc.nodes_mut().retain(|n| n.name().value() != name);
    let mut node = KdlNode::new(name);
    node.push(value);
    doc.nodes_mut().push(node);
}

pub fn read_current_lane() -> Option<String> {
    get_scalar_str(&load_doc(), "current-lane")
}

pub fn set_current_lane(id: &str) {
    let mut doc = load_doc();
    put_scalar(&mut doc, "current-lane", id);
    save_doc(&doc);
}

pub fn read_claude_cursor() -> Option<String> {
    get_scalar_str(&load_doc(), "claude-cursor")
}

pub fn set_claude_cursor(session_id: &str) {
    let mut doc = load_doc();
    put_scalar(&mut doc, "claude-cursor", session_id);
    save_doc(&doc);
}

pub fn read_switch_pinned() -> bool {
    get_scalar_bool(&load_doc(), "switch-pinned")
}

pub fn set_switch_pinned(pinned: bool) {
    let mut doc = load_doc();
    put_scalar(&mut doc, "switch-pinned", pinned);
    save_doc(&doc);
}

fn request_switch_pulse(name: &str) {
    let mut doc = load_doc();
    put_scalar(&mut doc, name, now_millis());
    save_doc(&doc);
}

/// Monotonic pulse the running Lanes Switch app watches for and reacts to by
/// showing+focusing its window, without touching the pin/always-on-top state.
/// Needed because the window is only actually created on-screen via Tauri's
/// own `show()` call - an external process (Hammerspoon) can't order a
/// hidden NSWindow back on-screen itself, so J/K request a show this way
/// instead.
pub fn request_switch_show() {
    request_switch_pulse("switch-show-requested");
}

pub fn read_switch_show_requested() -> i128 {
    get_scalar_int(&load_doc(), "switch-show-requested")
}

/// Same pulse pattern as `request_switch_show`, in the other direction: used
/// when Control-Option is released after a J/K-triggered show, so the
/// window disappears the way Cmd+Tab's selector does on modifier release.
pub fn request_switch_hide() {
    request_switch_pulse("switch-hide-requested");
}

pub fn read_switch_hide_requested() -> i128 {
    get_scalar_int(&load_doc(), "switch-hide-requested")
}

/// Cached WezTerm tab ID for a Zellij session, so navigation doesn't have to
/// re-derive the tab by matching titles (which aren't guaranteed to relate to
/// the session name at all - see `lib::activate_wezterm_tab`). Populated once
/// a tab is found by whatever means, then reused until the cached tab no
/// longer exists. Keyed by session, so it doesn't fit the flat-scalar
/// pattern above.
pub fn get_wezterm_tab_id(session: &str) -> Option<u64> {
    wezterm_tab_id_from(&load_doc(), session)
}

fn wezterm_tab_id_from(doc: &KdlDocument, session: &str) -> Option<u64> {
    doc.nodes().iter()
        .find(|n| {
            n.name().value() == "wezterm-tab-id"
                && n.get("session").and_then(|v| v.as_string()) == Some(session)
        })
        .and_then(|n| n.get("id"))
        .and_then(|v| v.as_integer())
        .map(|i| i as u64)
}

pub fn set_wezterm_tab_id(session: &str, id: u64) {
    let mut doc = load_doc();
    put_wezterm_tab_id(&mut doc, session, id);
    save_doc(&doc);
}

fn put_wezterm_tab_id(doc: &mut KdlDocument, session: &str, id: u64) {
    doc.nodes_mut().retain(|n| {
        !(n.name().value() == "wezterm-tab-id"
            && n.get("session").and_then(|v| v.as_string()) == Some(session))
    });
    let mut node = KdlNode::new("wezterm-tab-id");
    node.insert("session", session);
    node.insert("id", id as i128);
    doc.nodes_mut().push(node);
}

pub fn all_wezterm_tab_ids() -> Vec<(String, u64)> {
    load_doc().nodes().iter()
        .filter(|n| n.name().value() == "wezterm-tab-id")
        .filter_map(|n| {
            let session = n.get("session").and_then(|v| v.as_string())?.to_string();
            let id = n.get("id").and_then(|v| v.as_integer())? as u64;
            Some((session, id))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_round_trips_and_overwrites_without_duplicating() {
        let mut doc = KdlDocument::new();
        assert_eq!(get_scalar_str(&doc, "current-lane"), None);
        put_scalar(&mut doc, "current-lane", "sheetwork1");
        assert_eq!(get_scalar_str(&doc, "current-lane"), Some("sheetwork1".to_string()));
        put_scalar(&mut doc, "current-lane", "infra");
        assert_eq!(get_scalar_str(&doc, "current-lane"), Some("infra".to_string()));
        // overwriting shouldn't leave a duplicate node behind
        assert_eq!(doc.nodes().iter().filter(|n| n.name().value() == "current-lane").count(), 1);
    }

    #[test]
    fn scalar_bool_and_int_default_when_absent() {
        let doc = KdlDocument::new();
        assert_eq!(get_scalar_bool(&doc, "switch-pinned"), false);
        assert_eq!(get_scalar_int(&doc, "switch-show-requested"), 0);
    }

    #[test]
    fn different_scalar_fields_coexist_without_clobbering_each_other() {
        let mut doc = KdlDocument::new();
        put_scalar(&mut doc, "current-lane", "lanes-dev");
        put_scalar(&mut doc, "switch-pinned", true);
        assert_eq!(get_scalar_str(&doc, "current-lane"), Some("lanes-dev".to_string()));
        assert_eq!(get_scalar_bool(&doc, "switch-pinned"), true);

        // updating one field later shouldn't touch the other
        put_scalar(&mut doc, "current-lane", "infra");
        assert_eq!(get_scalar_bool(&doc, "switch-pinned"), true);
    }

    #[test]
    fn round_trips_wezterm_tab_id() {
        let mut doc = KdlDocument::new();
        assert_eq!(wezterm_tab_id_from(&doc, "infra"), None);
        put_wezterm_tab_id(&mut doc, "infra", 3);
        assert_eq!(wezterm_tab_id_from(&doc, "infra"), Some(3));
        put_wezterm_tab_id(&mut doc, "infra", 7);
        assert_eq!(wezterm_tab_id_from(&doc, "infra"), Some(7));
        assert_eq!(doc.nodes().iter().filter(|n| n.name().value() == "wezterm-tab-id").count(), 1);
    }

    #[test]
    fn tab_id_mappings_are_independent_per_session() {
        let mut doc = KdlDocument::new();
        put_wezterm_tab_id(&mut doc, "infra", 3);
        put_wezterm_tab_id(&mut doc, "sheetwork1", 1);
        assert_eq!(wezterm_tab_id_from(&doc, "infra"), Some(3));
        assert_eq!(wezterm_tab_id_from(&doc, "sheetwork1"), Some(1));
        assert_eq!(wezterm_tab_id_from(&doc, "sheetwork2"), None);
    }

    #[test]
    fn tab_ids_coexist_with_scalar_fields_without_clobbering_each_other() {
        let mut doc = KdlDocument::new();
        put_scalar(&mut doc, "current-lane", "lanes-dev");
        put_wezterm_tab_id(&mut doc, "lanes", 4);
        put_wezterm_tab_id(&mut doc, "infra", 3);
        assert_eq!(get_scalar_str(&doc, "current-lane"), Some("lanes-dev".to_string()));
        assert_eq!(wezterm_tab_id_from(&doc, "lanes"), Some(4));
        assert_eq!(wezterm_tab_id_from(&doc, "infra"), Some(3));

        // updating current-lane later shouldn't touch the tab-id mappings
        put_scalar(&mut doc, "current-lane", "infra");
        assert_eq!(wezterm_tab_id_from(&doc, "lanes"), Some(4));
        assert_eq!(wezterm_tab_id_from(&doc, "infra"), Some(3));
    }
}
