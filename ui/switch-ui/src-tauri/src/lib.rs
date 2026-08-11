use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::{RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tauri::menu::{CheckMenuItem, MenuBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{Emitter, Manager};

#[tauri::command]
fn get_snapshot() -> serde_json::Value {
    let cfg = lanes::config::Config::load();
    let snapshot = lanes::gather_lanes(&cfg);
    serde_json::to_value(&snapshot).unwrap()
}

#[tauri::command]
fn set_current_lane(lane_id: String) {
    lanes::state::set_current_lane(&lane_id);
}

#[tauri::command]
fn activate_lane(lane_id: String) {
    lanes::activate_lane(&lane_id, false);
}

#[tauri::command]
fn execute_action(action: lanes::model::SignalAction) -> Result<(), String> {
    match action {
        lanes::model::SignalAction::FocusRepoPane { session, path } => {
            lanes::navigate_to_repo_pane(&session, &path)
        }
        lanes::model::SignalAction::SwitchClaudeSession { session_id } => {
            lanes::switch_claude_session(&session_id)
        }
    }
}

fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{}/{}", home, rest)
    } else {
        path.to_string()
    }
}

struct RepoWatch {
    root: PathBuf,
    gitignore: Gitignore,
}

fn build_repo_watch(root: PathBuf) -> RepoWatch {
    let mut builder = GitignoreBuilder::new(&root);
    builder.add(root.join(".gitignore"));
    let home = std::env::var("HOME").unwrap_or_default();
    let global = PathBuf::from(home).join(".config/git/ignore");
    if global.exists() {
        builder.add(global);
    }
    let gitignore = builder.build().unwrap_or(Gitignore::empty());
    RepoWatch { root, gitignore }
}

fn is_relevant_change(
    watches: &[RepoWatch],
    sessions_dir: &Path,
    state_dir: &Path,
    lanes_config_dir: &Path,
    global_config_path: &Path,
    path: &Path,
) -> bool {
    if path.starts_with(sessions_dir)
        || path.starts_with(state_dir)
        || path.starts_with(lanes_config_dir)
        || path == global_config_path
    {
        return true;
    }
    for watch in watches {
        if !path.starts_with(&watch.root) {
            continue;
        }
        let git_dir = watch.root.join(".git");
        if path.starts_with(&git_dir) {
            // Only care about index (staging) and refs (commits) inside .git/
            let rel = match path.strip_prefix(&git_dir) {
                Ok(r) => r,
                Err(_) => return false,
            };
            return rel == Path::new("index") || rel.starts_with("refs/");
        }
        // Working tree: pass through unless gitignored
        return !watch.gitignore.matched_path_or_any_parents(path, path.is_dir()).is_ignore();
    }
    false
}

fn apply_pin(app: &tauri::AppHandle, pin_item: &CheckMenuItem<tauri::Wry>, pinned: bool) {
    let _ = pin_item.set_checked(pinned);
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.set_always_on_top(pinned);
        if pinned {
            let _ = win.show();
            let _ = win.set_focus();
        } else {
            let _ = win.hide();
        }
    }
}

fn watch_paths(handle: tauri::AppHandle, pin_item: CheckMenuItem<tauri::Wry>) {
    let home = std::env::var("HOME").unwrap_or_default();
    let sessions_dir = PathBuf::from(&home).join(".claude").join("active-sessions");
    let state_dir = PathBuf::from(&home).join(".local/state/lanes");
    let lanes_config_dir = lanes::config::config_dir();
    let global_config_path = PathBuf::from(&home).join(".config").join("lanes.toml");

    let cfg = lanes::config::Config::load();
    let repo_watches: Vec<RepoWatch> = cfg.lanes.iter()
        .flat_map(|lane| lane.facets.iter())
        .filter_map(|facet| {
            if let lanes::model::Facet::Repo { path } = facet {
                let p = PathBuf::from(expand_tilde(path));
                if p.exists() { Some(build_repo_watch(p)) } else { None }
            } else {
                None
            }
        })
        .collect();

    std::thread::spawn(move || {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut watcher = match notify::recommended_watcher(tx) {
            Ok(w) => w,
            Err(_) => return,
        };

        watcher.watch(&sessions_dir, RecursiveMode::NonRecursive).ok();
        if state_dir.exists() {
            watcher.watch(&state_dir, RecursiveMode::NonRecursive).ok();
        }
        if lanes_config_dir.exists() {
            watcher.watch(&lanes_config_dir, RecursiveMode::NonRecursive).ok();
        }
        if global_config_path.exists() {
            watcher.watch(&global_config_path, RecursiveMode::NonRecursive).ok();
        }
        for rw in &repo_watches {
            watcher.watch(&rw.root, RecursiveMode::Recursive).ok();
        }

        let debounce = Duration::from_millis(100);
        let mut last_emit: Option<Instant> = None;
        let mut last_pinned = lanes::state::read_switch_pinned();
        let mut last_show_requested = lanes::state::read_switch_show_requested();
        let mut last_hide_requested = lanes::state::read_switch_hide_requested();

        for res in rx {
            if let Ok(event) = res {
                use notify::EventKind::*;
                if !matches!(event.kind, Create(_) | Modify(_) | Remove(_)) {
                    continue;
                }
                if event.paths.iter().any(|p| p.starts_with(&state_dir)) {
                    let pinned = lanes::state::read_switch_pinned();
                    if pinned != last_pinned {
                        last_pinned = pinned;
                        apply_pin(&handle, &pin_item, pinned);
                    }

                    let show_requested = lanes::state::read_switch_show_requested();
                    if show_requested != last_show_requested {
                        last_show_requested = show_requested;
                        if let Some(win) = handle.get_webview_window("main") {
                            let _ = win.show();
                            let _ = win.set_focus();
                        }
                    }

                    let hide_requested = lanes::state::read_switch_hide_requested();
                    if hide_requested != last_hide_requested {
                        last_hide_requested = hide_requested;
                        if let Some(win) = handle.get_webview_window("main") {
                            let _ = win.hide();
                        }
                    }
                }
                let relevant = event.paths.iter()
                    .any(|p| is_relevant_change(&repo_watches, &sessions_dir, &state_dir, &lanes_config_dir, &global_config_path, p));
                if relevant && last_emit.map_or(true, |t| t.elapsed() >= debounce) {
                    handle.emit("sessions-changed", ()).ok();
                    last_emit = Some(Instant::now());
                }
            }
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let pinned_at_startup = lanes::state::read_switch_pinned();
            let pin_item = CheckMenuItem::with_id(app, "pin", "Pin on Top", true, pinned_at_startup, None::<&str>)?;
            let menu = MenuBuilder::new(app).item(&pin_item).build()?;
            let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray-icon.png"))?;

            watch_paths(app.handle().clone(), pin_item.clone());
            apply_pin(app.handle(), &pin_item, pinned_at_startup);

            let pin_item_for_handler = pin_item.clone();
            TrayIconBuilder::new()
                .icon(icon)
                .icon_as_template(true)
                .menu(&menu)
                .on_menu_event(move |app, event| {
                    if event.id.0.as_str() == "pin" {
                        if let Ok(pinned) = pin_item_for_handler.is_checked() {
                            lanes::state::set_switch_pinned(pinned);
                            apply_pin(app, &pin_item_for_handler, pinned);
                        }
                    }
                })
                .build(app)?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![get_snapshot, execute_action, set_current_lane, activate_lane])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
