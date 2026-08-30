use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::{RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tauri::menu::{CheckMenuItem, MenuBuilder, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Emitter, Manager};

/// Listens on a local Unix socket for direct notifications from the `lanes`
/// CLI, so latency-sensitive updates (a switch's new lane, show/hide) don't
/// have to wait on the state.kdl file watcher's inherent floor latency
/// (write -> OS notices -> watcher wakes up -> app reacts) - the CLI writes
/// state.kdl too, so that path remains the fallback for when this app isn't
/// running yet to receive the message.
fn start_switch_socket(handle: tauri::AppHandle) {
    let home = std::env::var("HOME").unwrap_or_default();
    let sock_path = PathBuf::from(&home).join(".local/state/lanes/switch.sock");
    let _ = std::fs::remove_file(&sock_path);
    let listener = match std::os::unix::net::UnixListener::bind(&sock_path) {
        Ok(l) => l,
        Err(_) => return,
    };

    std::thread::spawn(move || {
        use std::io::BufRead;
        for stream in listener.incoming().flatten() {
            let mut lines = std::io::BufReader::new(stream).lines();
            while let Some(Ok(line)) = lines.next() {
                if let Some(rest) = line.strip_prefix("switch:") {
                    let (lane, session) = rest.split_once('|').unwrap_or((rest, ""));
                    lanes::logging::perf("ui.socket_received", &format!("lane={lane} session={session}"));
                    let payload = serde_json::json!({
                        "lane": if lane.is_empty() { None } else { Some(lane) },
                        "session": if session.is_empty() { None } else { Some(session) },
                    });
                    handle.emit("lane-changed", payload).ok();
                    lanes::logging::perf("ui.emitted_lane_changed", &format!("lane={lane} session={session}"));
                } else if line == "show" {
                    if let Some(win) = handle.get_webview_window("main") {
                        let _ = win.show();
                        let _ = win.set_focus();
                    }
                } else if line == "hide" {
                    if let Some(win) = handle.get_webview_window("main") {
                        let _ = win.hide();
                    }
                }
            }
        }
    });
}

/// `async` + `spawn_blocking` is load-bearing, not decoration: Tauri v2 runs
/// a non-async command's body directly on the main thread, and
/// gather_lanes() blocks for ~500-600ms doing real subprocess I/O (see the
/// README's Diagnostics section). While that's running synchronously on the
/// main thread, nothing else the app needs that thread for can happen
/// either - confirmed via perf.log: the direct-socket "lane-changed"
/// highlight update (meant to land in single-digit milliseconds) was
/// measured stalling 500-600ms, resuming within ~10ms of whichever
/// get_snapshot call was in flight finishing. Moving the actual work to a
/// blocking-pool thread and only awaiting it here frees the main thread to
/// keep dispatching that event (and everything else) while a refresh runs.
#[tauri::command]
async fn get_snapshot() -> serde_json::Value {
    tauri::async_runtime::spawn_blocking(|| {
        let t0 = std::time::Instant::now();
        lanes::logging::perf("ui.get_snapshot.start", "");
        let cfg = lanes::config::Config::load();
        let mut snapshot = lanes::gather_lanes(&cfg);
        lanes::logging::perf("ui.get_snapshot.done", &format!("elapsed_us={}", t0.elapsed().as_micros()));
        // Inactive-lane filtering is a dashboard display concern, not a
        // gather_lanes()-level one - the CLI (`lanes list`/`snapshot`/`signals`)
        // always sees every lane regardless of this toggle; only what Lanes
        // Switch itself renders is affected.
        if !lanes::state::read_show_inactive() {
            snapshot.lanes.retain(|l| l.active);
        }
        serde_json::to_value(&snapshot).unwrap()
    })
    .await
    .expect("get_snapshot blocking task panicked")
}

/// Lets the frontend add its own entries to perf.log - the only way it can,
/// since a webview has no filesystem access of its own. Used to mark the
/// moment the UI actually applies a lane/session change, closing the loop
/// that starts at `switch.trigger` in `lib.rs`'s `switch_claude_session`.
#[tauri::command]
fn log_ui_event(event: String, detail: String) {
    lanes::logging::perf(&event, &detail);
}

#[tauri::command]
fn set_focused_lane(lane_id: String) {
    lanes::state::set_focused_lane(&lane_id);
}

#[tauri::command]
fn focus_lane(lane_id: String) {
    // focus=true, matching every other caller in the codebase (switch_
    // claude_session, navigate_to_repo_pane, the CLI's own `lanes focus`) -
    // false meant WezTerm's active tab switched silently in the background
    // without the app ever coming to the foreground, which is exactly why
    // clicking a lane with nothing more specific to route through looked
    // like it did nothing at all.
    if let Err(e) = lanes::focus_lane(&lane_id, true) {
        lanes::logging::append_line("switch-ui.log", "warn", &format!("focus_lane({lane_id}): {e}"));
    }
}

#[tauri::command]
fn execute_action(action: lanes::model::SignalAction) -> Result<(), String> {
    let result = match action {
        lanes::model::SignalAction::FocusRepoPane { session, path } => {
            lanes::navigate_to_repo_pane(&session, &path)
        }
        lanes::model::SignalAction::SwitchClaudeSession { session_id } => {
            lanes::switch_claude_session(&session_id)
        }
    };
    if let Err(ref e) = result {
        lanes::logging::append_line("switch-ui.log", "error", &format!("execute_action: {e}"));
    }
    result
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
    if path.starts_with(sessions_dir) || path.starts_with(lanes_config_dir) || path == global_config_path {
        return true;
    }
    if path.starts_with(state_dir) {
        // *.log files (perf.log, switch-ui.log, hammerspoon.log) live in
        // this same directory but are never state a refresh should react
        // to - perf.log in particular is written on every single
        // gather_lanes() call (see logging::perf), so treating it as
        // "relevant" turned every refresh into a self-triggering loop: log
        // the refresh -> watcher sees perf.log change -> refresh again.
        //
        // state_dir/cache/ (the list-panes TTL cache - see
        // drivers::zellij::list_panes) is the same story: gather_lanes()
        // itself is one of the things that populates it, so treating a
        // cache write as relevant would trigger the exact refresh that just
        // populated it.
        if path.starts_with(state_dir.join("cache")) {
            return false;
        }
        return path.extension().and_then(|e| e.to_str()) != Some("log");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_kdl_change_is_relevant() {
        let state_dir = Path::new("/home/x/.local/state/lanes");
        assert!(is_relevant_change(
            &[],
            Path::new("/home/x/.claude/active-sessions"),
            state_dir,
            Path::new("/home/x/.config/lanes"),
            Path::new("/home/x/.config/lanes.toml"),
            &state_dir.join("state.kdl"),
        ));
    }

    #[test]
    fn log_files_in_state_dir_are_never_relevant() {
        // Regression test: perf.log is written on every single gather_lanes()
        // call (see logging::perf) - if the watcher ever treats a *.log
        // write in state_dir as relevant again, every refresh re-triggers
        // itself forever (log the refresh -> watcher fires -> refresh again).
        let state_dir = Path::new("/home/x/.local/state/lanes");
        for name in ["perf.log", "switch-ui.log", "hammerspoon.log"] {
            assert!(
                !is_relevant_change(
                    &[],
                    Path::new("/home/x/.claude/active-sessions"),
                    state_dir,
                    Path::new("/home/x/.config/lanes"),
                    Path::new("/home/x/.config/lanes.toml"),
                    &state_dir.join(name),
                ),
                "{name} should not be treated as relevant"
            );
        }
    }

    #[test]
    fn cache_dir_writes_in_state_dir_are_never_relevant() {
        // Same class of regression as the *.log exclusion above:
        // drivers::zellij's list-panes TTL cache lives at
        // state_dir/cache/*.json, and gather_lanes() is itself one of the
        // things that populates it - treating a cache write as relevant
        // would trigger the exact refresh that just populated it.
        let state_dir = Path::new("/home/x/.local/state/lanes");
        assert!(
            !is_relevant_change(
                &[],
                Path::new("/home/x/.claude/active-sessions"),
                state_dir,
                Path::new("/home/x/.config/lanes"),
                Path::new("/home/x/.config/lanes.toml"),
                &state_dir.join("cache").join("list-panes-formation.json"),
            ),
            "a cache/ write should not be treated as relevant"
        );
    }
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
        .flat_map(|lane| lane.scope.iter())
        .filter_map(|el| {
            let path = el.repo_path()?;
            let p = PathBuf::from(expand_tilde(path));
            if p.exists() { Some(build_repo_watch(p)) } else { None }
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
                }
                let relevant = event.paths.iter()
                    .any(|p| is_relevant_change(&repo_watches, &sessions_dir, &state_dir, &lanes_config_dir, &global_config_path, p));
                if relevant && last_emit.map_or(true, |t| t.elapsed() >= debounce) {
                    lanes::logging::perf(
                        "ui.fs_watcher_emitted_sessions_changed",
                        &format!("paths={:?}", event.paths),
                    );
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

            lanes::logging::init_state();

            let pinned_at_startup = lanes::state::read_switch_pinned();
            let pin_item = CheckMenuItem::with_id(app, "pin", "Pin on Top", true, pinned_at_startup, None::<&str>)?;
            let show_inactive_at_startup = lanes::state::read_show_inactive();
            let show_inactive_item = CheckMenuItem::with_id(app, "show-inactive", "Show Inactive", true, show_inactive_at_startup, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit Lanes", true, None::<&str>)?;
            let menu = MenuBuilder::new(app)
                .item(&pin_item)
                .item(&show_inactive_item)
                .separator()
                .item(&quit_item)
                .build()?;
            let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray-icon.png"))?;

            watch_paths(app.handle().clone(), pin_item.clone());
            start_switch_socket(app.handle().clone());
            apply_pin(app.handle(), &pin_item, pinned_at_startup);

            let pin_item_for_handler = pin_item.clone();
            let show_inactive_item_for_handler = show_inactive_item.clone();
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
                    } else if event.id.0.as_str() == "show-inactive" {
                        if let Ok(show) = show_inactive_item_for_handler.is_checked() {
                            lanes::state::set_show_inactive(show);
                            // Not reflected in gather_lanes() output until the
                            // next get_snapshot() call - nudge the frontend to
                            // make that call now instead of waiting on the
                            // next unrelated refresh.
                            app.emit("sessions-changed", ()).ok();
                        }
                    } else if event.id.0.as_str() == "quit" {
                        app.exit(0);
                    }
                })
                .build(app)?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![get_snapshot, execute_action, set_focused_lane, focus_lane, log_ui_event])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
