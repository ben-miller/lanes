use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::{RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::menu::{CheckMenuItem, MenuBuilder, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Emitter, Manager};

/// `(session, pane)` keys with an armed one-shot timer for a `running` shell
/// command still under `shell_notice_secs()` - see `arm_pending_running`.
/// Tauri-managed state, one instance for the app's lifetime.
#[derive(Default, Clone)]
struct RunningTimers(Arc<Mutex<HashSet<(String, u32)>>>);

/// Arms a one-shot timer per pane in `pending` that doesn't already have one
/// running, so a command still under the notice floor gets a second look
/// once it crosses it - nothing else would, since lensd only re-diffs a
/// fact that's actually changed, and "N seconds have now elapsed" isn't a
/// fact change on its own. No cancellation: if the command finishes before
/// the timer fires, the resulting refetch just shows `done`/`failed`
/// instead of `running`, which is correct, not a bug.
fn arm_pending_running(handle: &tauri::AppHandle, timers: &RunningTimers, pending: Vec<(String, u32, i64)>) {
    for (session, pane, remaining_secs) in pending {
        let key = (session.clone(), pane);
        {
            let mut armed = timers.0.lock().unwrap();
            if !armed.insert(key.clone()) {
                continue;
            }
        }
        let handle = handle.clone();
        let timers = timers.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_secs(remaining_secs.max(0) as u64)).await;
            timers.0.lock().unwrap().remove(&key);
            handle.emit("sessions-changed", ()).ok();
        });
    }
}

/// Shows and focuses the main window - shared by the `switch.sock` "show"
/// message and the single-instance plugin's second-launch callback, so a
/// second launch attempt behaves exactly like any other "show the switcher"
/// trigger rather than its own slightly-different path.
fn show_main_window(handle: &tauri::AppHandle) {
    if let Some(win) = handle.get_webview_window("main") {
        let _ = win.show();
        let _ = win.set_focus();
        // The frontend skips its resize/recenter while the window is
        // hidden (see App.svelte's refresh) - tell it we're visible again
        // so it re-measures now rather than reappearing at a stale size.
        handle.emit("switch-shown", ()).ok();
    }
}

/// Listens on a local Unix socket for direct notifications from the `lanes`
/// CLI, so latency-sensitive updates (a switch's new lane, show/hide) don't
/// have to wait on the state.kdl file watcher's inherent floor latency
/// (write -> OS notices -> watcher wakes up -> app reacts) - the CLI writes
/// state.kdl too, so that path remains the fallback for when this app isn't
/// running yet to receive the message.
fn start_switch_socket(
    handle: tauri::AppHandle,
    edit_mode_item: CheckMenuItem<tauri::Wry>,
    show_inactive_item: CheckMenuItem<tauri::Wry>,
    last_applied_edit_mode: Arc<AtomicBool>,
) {
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
                    show_main_window(&handle);
                } else if line == "hide" {
                    if let Some(win) = handle.get_webview_window("main") {
                        let _ = win.hide();
                    }
                } else if let Some(rest) = line.strip_prefix("edit-mode:") {
                    if let Ok(enabled) = rest.trim().parse::<bool>() {
                        lanes::logging::perf("ui.socket_received_edit_mode", &format!("enabled={enabled}"));
                        // Swap-and-compare against the *shared* last-applied
                        // value (also touched by watch_paths' own fs-watcher
                        // loop below) - without this, the state.kdl write
                        // this same message rides in on would independently
                        // trigger watch_paths to apply the identical change
                        // again moments later (each loop otherwise only
                        // tracks its own local "last seen" value), causing a
                        // redundant second show()/set_focus() call - visible
                        // as a flicker, not just wasted work.
                        if last_applied_edit_mode.swap(enabled, Ordering::SeqCst) != enabled {
                            apply_edit_mode(&handle, &edit_mode_item, &show_inactive_item, enabled, true);
                        }
                    }
                } else if line == "show-inactive-noop" {
                    handle.emit("show-inactive-noop", ()).ok();
                }
            }
        }
    });
}

/// `async` + `spawn_blocking` keeps the config and state.kdl reads off the
/// main thread, which Tauri v2 otherwise uses to run non-async commands.
/// Signals come from lensd's live mirror; with lensd down the answer is
/// `{"lensd_connected": false}` and there is no fallback.
#[tauri::command]
async fn get_snapshot(
    handle: tauri::AppHandle,
    lensd: tauri::State<'_, lanes::lensd::Client>,
    running_timers: tauri::State<'_, RunningTimers>,
) -> Result<serde_json::Value, ()> {
    let lensd = lensd.inner().clone();
    let running_timers = running_timers.inner().clone();
    let (value, pending) = tauri::async_runtime::spawn_blocking(move || {
        let t0 = std::time::Instant::now();
        lanes::logging::perf("ui.get_snapshot.start", "");
        let Some(mirror) = lensd.current() else {
            return (serde_json::json!({ "lensd_connected": false }), Vec::new());
        };
        let cfg = lanes::config::Config::load();
        let snapshot = lanes::lensd::snapshot_from_mirror(&cfg, &mirror);
        let pending = lanes::lensd::pending_running_commands(&cfg, &mirror);
        lanes::logging::perf("ui.get_snapshot.done", &format!("elapsed_us={}", t0.elapsed().as_micros()));
        // Always returns every lane: inactive-lane filtering is the
        // frontend's job (App.svelte's visibleLanes).
        (serde_json::to_value(&snapshot).unwrap(), pending)
    })
    .await
    .expect("get_snapshot blocking task panicked");
    arm_pending_running(&handle, &running_timers, pending);
    Ok(value)
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
    if let Err(e) = lanes::focus_lane(&lane_id) {
        lanes::logging::append_line("switch-ui.log", "warn", &format!("focus_lane({lane_id}): {e}"));
    }
}

#[tauri::command]
fn get_edit_mode() -> bool {
    lanes::state::read_edit_mode()
}

/// Called directly by the frontend (Escape while armed) rather than only
/// ever flowing from the tray checkbox / hypo+E - state.kdl is still the
/// single source of truth either way, so the tray checkbox's `set_checked`
/// (driven by watch_paths noticing this same write) stays in sync no
/// matter which of the three triggered it.
#[tauri::command]
fn set_edit_mode(enabled: bool) {
    lanes::state::set_edit_mode(enabled);
}

#[tauri::command]
fn set_lane_active(lane_id: String, active: bool) -> Result<(), String> {
    lanes::config::set_lane_active(&lane_id, active).map_err(|e| e.to_string())
}

#[tauri::command]
fn set_claude_session_disabled(session_id: String, disabled: bool) {
    lanes::state::set_claude_session_disabled(&session_id, disabled);
}

#[tauri::command]
fn get_disabled_claude_sessions() -> Vec<String> {
    lanes::state::all_disabled_claude_sessions()
}

/// Dismiss (or un-dismiss, though the UI only ever dismisses) a Latched
/// signal by its `dismiss_id`. state.kdl is the source of truth; the next
/// gather_lanes() drops the signal. See PLAN-shell-signal.md.
#[tauri::command]
fn set_signal_dismissed(id: String, dismissed: bool) {
    lanes::state::set_signal_dismissed(&id, dismissed);
}

#[tauri::command]
fn get_show_inactive() -> bool {
    lanes::state::read_show_inactive()
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
        lanes::model::SignalAction::FocusPane { session, pane } => {
            lanes::focus_pane(&session, pane)
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
    // `.tmp` staging files from write-then-rename (state::atomic_write,
    // config::set_lane_active) - the real change lands as the rename onto
    // the target path, so the staging write is noise.
    if path.extension().and_then(|e| e.to_str()) == Some("tmp") {
        return false;
    }
    if path.starts_with(sessions_dir) || path.starts_with(lanes_config_dir) || path == global_config_path {
        return true;
    }
    if path.starts_with(state_dir) {
        // *.log files (perf.log, switch-ui.log, hammerspoon.log) live in
        // this same directory but are never state a refresh should react
        // to - perf.log in particular is written on every logging::perf
        // call, so treating it as "relevant" turned every refresh into a
        // self-triggering loop: log the refresh -> watcher sees perf.log
        // change -> refresh again.
        //
        // state_dir/cache/ (the list-panes TTL cache - see
        // drivers::zellij::list_panes) is the same story: cycling a lane
        // (cycle_claude_session_impl's pane-position tiebreak) is one of
        // the things that populates it, so treating a cache write as
        // relevant would trigger a refresh right after a cycle.
        if path.starts_with(state_dir.join("cache")) {
            return false;
        }
        // (*.tmp already excluded above.) *.log files - perf.log especially -
        // would self-trigger a refresh loop.
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
            Path::new("/home/x/.config/lanes/lanes.toml"),
            &state_dir.join("state.kdl"),
        ));
    }

    #[test]
    fn log_files_in_state_dir_are_never_relevant() {
        // Regression test: perf.log is written on every single logging::perf
        // call - if the watcher ever treats a *.log write in state_dir as
        // relevant again, every refresh re-triggers itself forever (log the
        // refresh -> watcher fires -> refresh again).
        let state_dir = Path::new("/home/x/.local/state/lanes");
        for name in ["perf.log", "switch-ui.log", "hammerspoon.log"] {
            assert!(
                !is_relevant_change(
                    &[],
                    Path::new("/home/x/.claude/active-sessions"),
                    state_dir,
                    Path::new("/home/x/.config/lanes"),
                    Path::new("/home/x/.config/lanes/lanes.toml"),
                    &state_dir.join(name),
                ),
                "{name} should not be treated as relevant"
            );
        }
    }

    #[test]
    fn state_kdl_tmp_staging_writes_are_never_relevant() {
        // state::atomic_write stages every state.kdl write in a sibling
        // state.kdl.tmp before rename(2) - the real change lands as the
        // rename onto state.kdl itself, so reacting to the .tmp write too
        // is just a redundant refresh.
        let state_dir = Path::new("/home/x/.local/state/lanes");
        assert!(
            !is_relevant_change(
                &[],
                Path::new("/home/x/.claude/active-sessions"),
                state_dir,
                Path::new("/home/x/.config/lanes"),
                Path::new("/home/x/.config/lanes/lanes.toml"),
                &state_dir.join("state.kdl.tmp"),
            ),
            "a state.kdl.tmp write should not be treated as relevant"
        );
        // ...but the rename target still is.
        assert!(is_relevant_change(
            &[],
            Path::new("/home/x/.claude/active-sessions"),
            state_dir,
            Path::new("/home/x/.config/lanes"),
            Path::new("/home/x/.config/lanes/lanes.toml"),
            &state_dir.join("state.kdl"),
        ));
    }

    #[test]
    fn lane_toml_tmp_staging_writes_are_never_relevant() {
        // config::set_lane_active stages the toggle write the same way.
        let cfg_dir = Path::new("/home/x/.config/lanes");
        let args = |p: &Path| is_relevant_change(
            &[],
            Path::new("/home/x/.claude/active-sessions"),
            Path::new("/home/x/.local/state/lanes"),
            cfg_dir,
            Path::new("/home/x/.config/lanes/lanes.toml"),
            p,
        );
        assert!(!args(&cfg_dir.join("lanes-wm.toml.tmp")));
        assert!(args(&cfg_dir.join("lanes-wm.toml")));
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
                Path::new("/home/x/.config/lanes/lanes.toml"),
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

/// Unlike apply_pin, turning edit mode off never hides the window - you'd
/// still want the dashboard visible right after you're done editing it,
/// just back in its plain read-only form.
///
/// `raise_on_enable` controls whether turning edit mode *on* also brings the
/// window to the front. That's wanted when the change came from a deliberate
/// user action - the hypo+E shortcut and the tray checkbox are both meant to
/// work from anywhere, not just while Lanes Switch already happens to be
/// visible - so those callers pass `true`. The state.kdl fs-watcher passes
/// `false`: it's a passive sync path (keep the tray checkbox and the webview
/// in step with whatever wrote the file), and a raise from there fights the
/// hypo+J/K show/hide - a switch rewrites state.kdl, the watcher sees it,
/// and edit mode being on would drag the window back up right after the
/// Control-Option release hid it. The socket message the deliberate actions
/// also send is what does the raise; the watcher is only the fallback for
/// when the app wasn't running to receive that.
///
/// Doesn't touch show-inactive's *value* at all - the frontend's own filter
/// (see App.svelte's visibleLanes) already shows everything while editMode
/// is on, so there's nothing to force or restore on this side. It does
/// disable the tray item, though: show-inactive genuinely can't do
/// anything right now (same reasoning), and `ToggleShowInactive` in the CLI
/// refuses to change the value at all while edit mode is on - disabling
/// the checkbox here keeps that same rule visible and true from the tray
/// too, rather than leaving a clickable control that silently does nothing.
fn apply_edit_mode(
    app: &tauri::AppHandle,
    edit_mode_item: &CheckMenuItem<tauri::Wry>,
    show_inactive_item: &CheckMenuItem<tauri::Wry>,
    enabled: bool,
    raise_on_enable: bool,
) {
    let _ = edit_mode_item.set_checked(enabled);
    let _ = show_inactive_item.set_enabled(!enabled);
    if enabled && raise_on_enable {
        if let Some(win) = app.get_webview_window("main") {
            let _ = win.show();
            let _ = win.set_focus();
        }
    }
    app.emit("edit-mode-changed", enabled).ok();
    lanes::logging::perf("ui.emitted_edit_mode_changed", &format!("enabled={enabled}"));
}

fn watch_paths(
    handle: tauri::AppHandle,
    pin_item: CheckMenuItem<tauri::Wry>,
    edit_mode_item: CheckMenuItem<tauri::Wry>,
    show_inactive_item: CheckMenuItem<tauri::Wry>,
    last_applied_edit_mode: Arc<AtomicBool>,
) {
    let home = std::env::var("HOME").unwrap_or_default();
    let sessions_dir = PathBuf::from(&home).join(".claude").join("active-sessions");
    let state_dir = PathBuf::from(&home).join(".local/state/lanes");
    let lanes_config_dir = lanes::config::config_dir();
    // Nested inside lanes_config_dir now (~/.config/lanes/lanes.toml), so
    // watching lanes_config_dir already covers it - kept as its own
    // variable/watch anyway since is_relevant_change compares against it by
    // exact path, independent of how it happens to be laid out on disk.
    let global_config_path = lanes_config_dir.join("lanes.toml");

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
        let mut last_show_inactive = lanes::state::read_show_inactive();

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
                    let edit_mode = lanes::state::read_edit_mode();
                    if last_applied_edit_mode.swap(edit_mode, Ordering::SeqCst) != edit_mode {
                        apply_edit_mode(&handle, &edit_mode_item, &show_inactive_item, edit_mode, false);
                    }
                    let show_inactive = lanes::state::read_show_inactive();
                    if show_inactive != last_show_inactive {
                        last_show_inactive = show_inactive;
                        // Keeps both the tray checkbox and the dashboard's
                        // own filtering (see App.svelte's visibleLanes) from
                        // going stale when the write came from outside the
                        // tray (hypo+I, the CLI directly).
                        let _ = show_inactive_item.set_checked(show_inactive);
                        handle.emit("show-inactive-changed", show_inactive).ok();
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
/// Publishes Lanes' facts now, and again whenever a context or a shell
/// record changes (debounced), for as long as the app runs.
fn publish_lanes_facts(facts: appctl::facts::Facts) {
    lanes::facts::publish(&facts);
    std::thread::spawn(move || {
        let (tx, rx) = std::sync::mpsc::channel();
        let Ok(mut watcher) = notify::recommended_watcher(tx) else { return };
        // Missing directories are skipped; the facts then just lack them.
        watcher.watch(&lanes::facts::contexts_dir(), RecursiveMode::Recursive).ok();
        watcher.watch(&lanes::facts::shell_dir(), RecursiveMode::NonRecursive).ok();
        while rx.recv().is_ok() {
            // A burst of writes (an editor's save, a hook's rewrite) settles first.
            std::thread::sleep(std::time::Duration::from_millis(150));
            while rx.try_recv().is_ok() {}
            lanes::facts::publish(&facts);
        }
    });
}

pub fn run() {
    tauri::Builder::default()
        // Registered first so a second launch attempt exits as soon as
        // possible (per the plugin's own docs) - a second instance just
        // shows/focuses the existing window via the same path `switch.sock`'s
        // "show" message already uses, rather than running a duplicate app.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_opener::init())
        // This instance's appctl socket, publishing Lanes' facts (which lanes
        // exist, their sessions/repos/boards, recorded shell commands) for
        // lensd, which sees them as lanes.*. They exist while this app runs.
        .plugin(tauri_plugin_appctl::init(appctl::config!("lanes").facts(lanes::facts::RELATIONS)))
        .setup(|app| {
            if let Some(facts) = app.try_state::<appctl::facts::Facts>() {
                publish_lanes_facts(facts.inner().clone());
            }
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            lanes::logging::init_state();

            // Fail-fast, mirroring lensd's main(): a context directory
            // missing from lanes.toml `order` still works at runtime (it
            // just sorts last), but refuse to show a dashboard over an
            // unregistered context rather than silently tolerating the gap.
            // Once already running, the fs watcher noticing a *newly*
            // unregistered context must not take the app down - this check
            // only runs here, at startup.
            let startup_cfg = lanes::config::Config::load();
            let unregistered = startup_cfg.unregistered_contexts();
            if !unregistered.is_empty() {
                return Err(format!(
                    "context(s) missing from `order` in ~/.config/lanes/lanes.toml: {}. \
                     Run `lanes doctor` and add them to `order` before starting Switch UI.",
                    unregistered.join(", ")
                ).into());
            }

            let lensd_handle = app.handle().clone();
            let lensd = lanes::lensd::Client::start(lanes::lensd::default_socket(), move || {
                lensd_handle.emit("sessions-changed", ()).ok();
            });
            app.manage(lensd);
            app.manage(RunningTimers::default());

            let pinned_at_startup = lanes::state::read_switch_pinned();
            let pin_item = CheckMenuItem::with_id(app, "pin", "Pin on Top", true, pinned_at_startup, None::<&str>)?;
            let show_inactive_at_startup = lanes::state::read_show_inactive();
            let show_inactive_item = CheckMenuItem::with_id(app, "show-inactive", "Show Inactive", true, show_inactive_at_startup, None::<&str>)?;
            let edit_mode_at_startup = lanes::state::read_edit_mode();
            let edit_mode_item = CheckMenuItem::with_id(app, "edit-mode", "Edit Lanes", true, edit_mode_at_startup, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit Lanes", true, None::<&str>)?;
            let menu = MenuBuilder::new(app)
                .item(&pin_item)
                .item(&show_inactive_item)
                .item(&edit_mode_item)
                .separator()
                .item(&quit_item)
                .build()?;
            let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray-icon.png"))?;

            let last_applied_edit_mode = Arc::new(AtomicBool::new(edit_mode_at_startup));
            watch_paths(
                app.handle().clone(),
                pin_item.clone(),
                edit_mode_item.clone(),
                show_inactive_item.clone(),
                last_applied_edit_mode.clone(),
            );
            start_switch_socket(
                app.handle().clone(),
                edit_mode_item.clone(),
                show_inactive_item.clone(),
                last_applied_edit_mode.clone(),
            );
            apply_pin(app.handle(), &pin_item, pinned_at_startup);

            let pin_item_for_handler = pin_item.clone();
            let show_inactive_item_for_handler = show_inactive_item.clone();
            let edit_mode_item_for_handler = edit_mode_item.clone();
            let last_applied_edit_mode_for_handler = last_applied_edit_mode.clone();
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
                            // The frontend filters show-inactive itself
                            // against whatever it already has (see
                            // App.svelte's visibleLanes) - no need to
                            // trigger a full resnapshot, just tell it the
                            // new value.
                            app.emit("show-inactive-changed", show).ok();
                        }
                    } else if event.id.0.as_str() == "edit-mode" {
                        if let Ok(enabled) = edit_mode_item_for_handler.is_checked() {
                            lanes::state::set_edit_mode(enabled);
                            last_applied_edit_mode_for_handler.store(enabled, Ordering::SeqCst);
                            apply_edit_mode(app, &edit_mode_item_for_handler, &show_inactive_item_for_handler, enabled, true);
                        }
                    } else if event.id.0.as_str() == "quit" {
                        app.exit(0);
                    }
                })
                .build(app)?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            execute_action,
            set_focused_lane,
            focus_lane,
            log_ui_event,
            get_edit_mode,
            set_edit_mode,
            set_lane_active,
            get_show_inactive,
            set_claude_session_disabled,
            get_disabled_claude_sessions,
            set_signal_dismissed
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
