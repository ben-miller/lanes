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

// state.kdl mostly holds flat scalar nodes (`name value`) - one value per
// name, last write wins. These getters plus `put_scalar` are the single
// implementation of that pattern; every field below is a thin, named
// wrapper around them.

fn get_scalar_str(doc: &KdlDocument, name: &str) -> Option<String> {
    doc.get(name).and_then(|n| n.get(0)).and_then(|v| v.as_string()).map(|s| s.to_string())
}

fn get_scalar_bool(doc: &KdlDocument, name: &str) -> bool {
    doc.get(name).and_then(|n| n.get(0)).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn put_scalar(doc: &mut KdlDocument, name: &str, value: impl Into<kdl::KdlEntry>) {
    doc.nodes_mut().retain(|n| n.name().value() != name);
    let mut node = KdlNode::new(name);
    node.push(value);
    doc.nodes_mut().push(node);
}

pub fn read_focused_lane() -> Option<String> {
    get_scalar_str(&load_doc(), "focused-lane")
}

pub fn set_focused_lane(id: &str) {
    let mut doc = load_doc();
    put_scalar(&mut doc, "focused-lane", id);
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

fn write_scalar_opt(doc: &mut KdlDocument, name: &str, value: Option<&str>) {
    match value {
        Some(v) => put_scalar(doc, name, v),
        None => { doc.nodes_mut().retain(|n| n.name().value() != name); }
    }
}

/// One load/mutate/save round-trip for both fields, instead of the two
/// separate set_claude_cursor/set_focused_lane round-trips this replaced -
/// switching sessions always touches both together, and writing them
/// separately meant two file-system change events (and two Lanes Switch UI
/// refreshes) for what's really one atomic action. `None` removes a field
/// entirely rather than writing a stale value; used both to record a normal
/// switch (cursor is always Some there) and, with the previous values, to
/// undo an optimistic write if the switch it was written ahead of turns out
/// to have failed partway through (where cursor may legitimately be None -
/// e.g. the very first switch ever, before state.kdl had one at all).
pub fn write_claude_cursor_and_lane(cursor: Option<&str>, lane: Option<&str>) {
    let mut doc = load_doc();
    write_scalar_opt(&mut doc, "claude-cursor", cursor);
    write_scalar_opt(&mut doc, "focused-lane", lane);
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

/// Whether Lanes Switch's dashboard should include inactive lanes (see
/// model::Lane.active) alongside the active ones. Same persisted-toggle
/// pattern as switch-pinned - Lanes Switch's tray menu just flips this,
/// defaulting to false so inactive lanes stay out of the way by default.
pub fn read_show_inactive() -> bool {
    get_scalar_bool(&load_doc(), "show-inactive")
}

pub fn set_show_inactive(show: bool) {
    let mut doc = load_doc();
    put_scalar(&mut doc, "show-inactive", show);
    save_doc(&doc);
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
        assert_eq!(get_scalar_str(&doc, "focused-lane"), None);
        put_scalar(&mut doc, "focused-lane", "sheetwork1");
        assert_eq!(get_scalar_str(&doc, "focused-lane"), Some("sheetwork1".to_string()));
        put_scalar(&mut doc, "focused-lane", "infra");
        assert_eq!(get_scalar_str(&doc, "focused-lane"), Some("infra".to_string()));
        // overwriting shouldn't leave a duplicate node behind
        assert_eq!(doc.nodes().iter().filter(|n| n.name().value() == "focused-lane").count(), 1);
    }

    #[test]
    fn scalar_bool_defaults_false_when_absent() {
        let doc = KdlDocument::new();
        assert_eq!(get_scalar_bool(&doc, "switch-pinned"), false);
    }

    #[test]
    fn different_scalar_fields_coexist_without_clobbering_each_other() {
        let mut doc = KdlDocument::new();
        put_scalar(&mut doc, "focused-lane", "lanes-dev");
        put_scalar(&mut doc, "switch-pinned", true);
        assert_eq!(get_scalar_str(&doc, "focused-lane"), Some("lanes-dev".to_string()));
        assert_eq!(get_scalar_bool(&doc, "switch-pinned"), true);

        // updating one field later shouldn't touch the other
        put_scalar(&mut doc, "focused-lane", "infra");
        assert_eq!(get_scalar_bool(&doc, "switch-pinned"), true);
    }

    #[test]
    fn write_scalar_opt_sets_value_when_some() {
        let mut doc = KdlDocument::new();
        write_scalar_opt(&mut doc, "focused-lane", Some("infra"));
        assert_eq!(get_scalar_str(&doc, "focused-lane"), Some("infra".to_string()));
    }

    #[test]
    fn write_scalar_opt_removes_node_when_none() {
        let mut doc = KdlDocument::new();
        put_scalar(&mut doc, "focused-lane", "infra");
        write_scalar_opt(&mut doc, "focused-lane", None);
        assert_eq!(get_scalar_str(&doc, "focused-lane"), None);
        assert_eq!(doc.nodes().iter().filter(|n| n.name().value() == "focused-lane").count(), 0);
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
        put_scalar(&mut doc, "focused-lane", "lanes-dev");
        put_wezterm_tab_id(&mut doc, "lanes", 4);
        put_wezterm_tab_id(&mut doc, "infra", 3);
        assert_eq!(get_scalar_str(&doc, "focused-lane"), Some("lanes-dev".to_string()));
        assert_eq!(wezterm_tab_id_from(&doc, "lanes"), Some(4));
        assert_eq!(wezterm_tab_id_from(&doc, "infra"), Some(3));

        // updating focused-lane later shouldn't touch the tab-id mappings
        put_scalar(&mut doc, "focused-lane", "infra");
        assert_eq!(wezterm_tab_id_from(&doc, "lanes"), Some(4));
        assert_eq!(wezterm_tab_id_from(&doc, "infra"), Some(3));
    }
}
