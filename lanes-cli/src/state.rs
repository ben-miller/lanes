use std::path::PathBuf;

fn state_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local/state/lanes/state.kdl")
}

pub fn read_current_lane() -> Option<String> {
    let contents = std::fs::read_to_string(state_path()).ok()?;
    let doc: kdl::KdlDocument = contents.parse().ok()?;
    doc.get("current-lane")
        .and_then(|n| n.get(0))
        .and_then(|v| v.as_string())
        .map(|s| s.to_string())
}

pub fn set_current_lane(id: &str) {
    let path = state_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let contents = format!("current-lane {:?}\n", id);
    std::fs::write(&path, contents).ok();
}
