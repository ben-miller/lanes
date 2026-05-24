#[tauri::command]
fn get_snapshot() -> serde_json::Value {
    let cfg = lanes::config::Config::load();
    let snapshot = lanes::gather_lanes(&cfg);
    serde_json::to_value(&snapshot).unwrap()
}

#[tauri::command]
fn execute_action(action: lanes::model::SignalAction) -> Result<(), String> {
    match action {
        lanes::model::SignalAction::FocusRepoPane { session, path } => {
            lanes::navigate_to_repo_pane(&session, &path)
        }
        lanes::model::SignalAction::SwitchClaudeSession { session_id: _ } => {
            Err("SwitchClaudeSession not yet implemented".to_string())
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![get_snapshot, execute_action])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
