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

pub fn read_current_lane() -> Option<String> {
    current_lane_from(&load_doc())
}

fn current_lane_from(doc: &KdlDocument) -> Option<String> {
    doc.get("current-lane")
        .and_then(|n| n.get(0))
        .and_then(|v| v.as_string())
        .map(|s| s.to_string())
}

pub fn set_current_lane(id: &str) {
    let mut doc = load_doc();
    put_current_lane(&mut doc, id);
    save_doc(&doc);
}

fn put_current_lane(doc: &mut KdlDocument, id: &str) {
    doc.nodes_mut().retain(|n| n.name().value() != "current-lane");
    let mut node = KdlNode::new("current-lane");
    node.push(id);
    doc.nodes_mut().push(node);
}

/// Cached WezTerm tab ID for a Zellij session, so navigation doesn't have to
/// re-derive the tab by matching titles (which aren't guaranteed to relate to
/// the session name at all - see `lib::activate_wezterm_tab`). Populated once
/// a tab is found by whatever means, then reused until the cached tab no
/// longer exists.
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
    fn round_trips_current_lane() {
        let mut doc = KdlDocument::new();
        assert_eq!(current_lane_from(&doc), None);
        put_current_lane(&mut doc, "sheetwork1");
        assert_eq!(current_lane_from(&doc), Some("sheetwork1".to_string()));
        put_current_lane(&mut doc, "infra");
        assert_eq!(current_lane_from(&doc), Some("infra".to_string()));
        // overwriting current-lane shouldn't leave a duplicate node behind
        assert_eq!(doc.nodes().iter().filter(|n| n.name().value() == "current-lane").count(), 1);
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
    fn current_lane_and_tab_ids_coexist_without_clobbering_each_other() {
        let mut doc = KdlDocument::new();
        put_current_lane(&mut doc, "lanes-dev");
        put_wezterm_tab_id(&mut doc, "lanes", 4);
        put_wezterm_tab_id(&mut doc, "infra", 3);
        assert_eq!(current_lane_from(&doc), Some("lanes-dev".to_string()));
        assert_eq!(wezterm_tab_id_from(&doc, "lanes"), Some(4));
        assert_eq!(wezterm_tab_id_from(&doc, "infra"), Some(3));

        // updating current-lane later shouldn't touch the tab-id mappings
        put_current_lane(&mut doc, "infra");
        assert_eq!(wezterm_tab_id_from(&doc, "lanes"), Some(4));
        assert_eq!(wezterm_tab_id_from(&doc, "infra"), Some(3));
    }
}
