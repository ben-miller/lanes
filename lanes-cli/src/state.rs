use std::path::{Path, PathBuf};

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
    // Write-then-rename rather than a plain `fs::write`: state.kdl is
    // rewritten in full on every scalar change (a switch touches
    // claude-cursor + focused-lane together), and Lanes Switch's fs-watcher
    // reads the file the instant it sees a change event. A non-atomic
    // truncate-then-write leaves a window where that reader sees a partial
    // or empty file - load_doc() then parses nothing and every field reads
    // as its default, which the watcher acts on (e.g. edit-mode flips
    // false, then true again a few ms later, and apply_edit_mode's window
    // raise fires on the bounce). rename(2) is atomic on the same
    // filesystem, so a reader sees either the old file or the new one,
    // never a torn one.
    atomic_write(&path, &doc.to_string());
}

/// Write `contents` to `path` via a sibling `.tmp` file plus `rename(2)`, so
/// a concurrent reader (Lanes Switch's fs-watcher) sees either the whole old
/// file or the whole new one, never a torn write. The staging file sits next
/// to the target so the rename stays on one filesystem; Lanes Switch's
/// watcher ignores `*.tmp` in the state dir (see is_relevant_change there).
fn atomic_write(path: &Path, contents: &str) {
    let tmp = path.with_extension("kdl.tmp");
    if std::fs::write(&tmp, contents).is_ok() && std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

// state.kdl mostly holds flat scalar nodes (`name value`) - one value per
// name, last write wins. These getters plus `put_scalar` are the single
// implementation of that pattern; every field below is a thin, named
// wrapper around them.

fn get_scalar_str(doc: &KdlDocument, name: &str) -> Option<String> {
    doc.get(name).and_then(|n| n.get(0)).and_then(|v| v.as_string()).map(|s| s.to_string())
}

fn get_scalar_bool(doc: &KdlDocument, name: &str) -> bool {
    get_scalar_bool_or(doc, name, false)
}

fn get_scalar_bool_or(doc: &KdlDocument, name: &str, default: bool) -> bool {
    doc.get(name).and_then(|n| n.get(0)).and_then(|v| v.as_bool()).unwrap_or(default)
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

/// Whether Lanes Switch's dashboard is in edit mode (a per-lane
/// enable/disable toggle switch visible on each row). Same persisted-toggle
/// pattern as switch-pinned/show-inactive - defaults to false so a fresh
/// launch always opens in the plain read-only view.
pub fn read_edit_mode() -> bool {
    get_scalar_bool(&load_doc(), "edit-mode")
}

pub fn set_edit_mode(enabled: bool) {
    let mut doc = load_doc();
    put_scalar(&mut doc, "edit-mode", enabled);
    save_doc(&doc);
}

/// Whether `logging::perf` writes to perf.log - the switch-latency
/// timeline (trigger -> actual switch -> UI update). On by default, unlike
/// switch-pinned/show-inactive: this exists specifically to have data ready
/// the moment a lag complaint comes in, not to opt into after the fact.
/// Same persisted-toggle pattern as those two otherwise.
pub fn read_diagnostics_enabled() -> bool {
    get_scalar_bool_or(&load_doc(), "diagnostics-enabled", true)
}

pub fn set_diagnostics_enabled(enabled: bool) {
    let mut doc = load_doc();
    put_scalar(&mut doc, "diagnostics-enabled", enabled);
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

/// Removes the cached tab-id for a session entirely, rather than leaving a
/// stale one behind - called by `ztabs deactivate` the moment it
/// kills a pane, so a deactivated lane has no cached id at all instead of
/// one that (mis)leadingly still looks like it points somewhere.
pub fn clear_wezterm_tab_id(session: &str) {
    let mut doc = load_doc();
    clear_wezterm_tab_id_from(&mut doc, session);
    save_doc(&doc);
}

fn clear_wezterm_tab_id_from(doc: &mut KdlDocument, session: &str) {
    doc.nodes_mut().retain(|n| {
        !(n.name().value() == "wezterm-tab-id"
            && n.get("session").and_then(|v| v.as_string()) == Some(session))
    });
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

/// Whether a live Claude session is excluded from `sessions next`/`prev`
/// cycling - keyed by its own session_id (the UUID from
/// `~/.claude/active-sessions/*.json`), not the Zellij pane it happens to
/// be running in. That UUID is regenerated every time `claude` restarts in
/// that pane, so this exclusion resets on restart rather than surviving it
/// - a deliberate choice, not a gap: re-excluding a freshly restarted
/// session is one click, and a stale entry for a session that's gone never
/// matches anything again, so nothing needs cleaning up either. Absence =
/// included (the default).
pub fn is_claude_session_disabled(session_id: &str) -> bool {
    claude_session_disabled_in(&load_doc(), session_id)
}

fn claude_session_disabled_in(doc: &KdlDocument, session_id: &str) -> bool {
    doc.nodes().iter().any(|n| {
        n.name().value() == "claude-session-disabled"
            && n.get("id").and_then(|v| v.as_string()) == Some(session_id)
    })
}

pub fn set_claude_session_disabled(session_id: &str, disabled: bool) {
    let mut doc = load_doc();
    set_claude_session_disabled_in(&mut doc, session_id, disabled);
    save_doc(&doc);
}

fn set_claude_session_disabled_in(doc: &mut KdlDocument, session_id: &str, disabled: bool) {
    doc.nodes_mut().retain(|n| {
        !(n.name().value() == "claude-session-disabled"
            && n.get("id").and_then(|v| v.as_string()) == Some(session_id))
    });
    if disabled {
        let mut node = KdlNode::new("claude-session-disabled");
        node.insert("id", session_id);
        doc.nodes_mut().push(node);
    }
}

/// Every currently-disabled session_id, for the frontend to cross-reference
/// against each ClaudeSession signal's own session_id when deciding what
/// edit mode's per-chip toggle should show - cheaper than threading a new
/// field through the whole Signal/serialization pipeline for something only
/// ever meaningful on one signal kind.
pub fn all_disabled_claude_sessions() -> Vec<String> {
    load_doc().nodes().iter()
        .filter(|n| n.name().value() == "claude-session-disabled")
        .filter_map(|n| n.get("id").and_then(|v| v.as_string()).map(str::to_string))
        .collect()
}

/// Whether a `Latched` signal (see model::Lifecycle) has been dismissed by
/// the user. Keyed by the signal's own per-occurrence id (e.g.
/// `command:<session>--<pane>--<started_at>` for a finished shell command)
/// so dismissing one occurrence never suppresses a future one. Dismissal is
/// one-way; a stale marker whose id no longer matches any live signal is
/// harmless and can be GC'd, exactly like `claude-session-disabled`.
pub fn is_signal_dismissed(id: &str) -> bool {
    signal_dismissed_in(&load_doc(), id)
}

fn signal_dismissed_in(doc: &KdlDocument, id: &str) -> bool {
    doc.nodes().iter().any(|n| {
        n.name().value() == "signal-dismissed"
            && n.get("id").and_then(|v| v.as_string()) == Some(id)
    })
}

pub fn set_signal_dismissed(id: &str, dismissed: bool) {
    let mut doc = load_doc();
    set_signal_dismissed_in(&mut doc, id, dismissed);
    save_doc(&doc);
}

fn set_signal_dismissed_in(doc: &mut KdlDocument, id: &str, dismissed: bool) {
    doc.nodes_mut().retain(|n| {
        !(n.name().value() == "signal-dismissed"
            && n.get("id").and_then(|v| v.as_string()) == Some(id))
    });
    if dismissed {
        let mut node = KdlNode::new("signal-dismissed");
        node.insert("id", id);
        doc.nodes_mut().push(node);
    }
}

fn cycle_lock_dir() -> PathBuf {
    state_path().with_file_name("cycle.lock")
}

/// A crash older than this is assumed dead, not just slow - a lock this old
/// almost certainly means a previous holder never got to clean up (killed,
/// panicked), not that it's still legitimately working. Kept comfortably
/// above `LOCK_WAIT_TIMEOUT` so a holder that's merely slow (still within
/// its own wait window from some other caller's perspective) is never
/// mistaken for a crashed one and have its lock pulled out from under it.
const STALE_LOCK_AGE: std::time::Duration = std::time::Duration::from_secs(30);
/// One real transaction (apply targets, switch WezTerm/Zellij, write state)
/// measured around 0.5s - this needs to comfortably absorb several of
/// those queued back to back (a held-down or rapidly repeated hotkey),
/// not just one. Found by testing: 2s was enough for 2 concurrent callers
/// but not 7 - the 7th would've waited ~3s for the other 6 to each take
/// their turn, past the old deadline, and fallen through to the
/// unprotected path, reintroducing the exact race this exists to fix.
const LOCK_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// A cross-process mutex around one full cycle transaction (read whichever
/// lane/session is current, decide what's next, switch to it) - without
/// this, two cycle invocations started close together (a held-down or
/// rapidly repeated hotkey, each keypress spawning its own one-shot `lanes
/// cycle` process) both read the *same* stale "current" before either has
/// written its result, so they compute the same "next" and one keypress's
/// worth of advancement gets silently lost per overlap. `mkdir` is atomic
/// on POSIX - exactly one concurrent caller sees `Ok`, every other sees
/// `AlreadyExists` - so it doubles as a lock with no extra dependency.
///
/// Waits up to `LOCK_WAIT_TIMEOUT` for a concurrent holder to finish rather
/// than blocking forever (a cycle that's somehow stuck must not wedge every
/// future keypress), and clears a lock older than `STALE_LOCK_AGE` outright
/// rather than waiting out the deadline for a holder that's already dead.
pub fn with_cycle_lock<T>(f: impl FnOnce() -> T) -> T {
    with_lock_at(&cycle_lock_dir(), f)
}

/// `with_cycle_lock`'s actual logic, taking the lock directory as a
/// parameter purely so tests can point it at a throwaway path instead of
/// the real state dir - same reasoning as `load_contexts` taking `dir`
/// explicitly rather than reading `$HOME` itself (see config.rs).
fn with_lock_at<T>(lock_dir: &Path, f: impl FnOnce() -> T) -> T {
    let deadline = std::time::Instant::now() + LOCK_WAIT_TIMEOUT;
    loop {
        match std::fs::create_dir(lock_dir) {
            Ok(()) => break,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let is_stale = std::fs::metadata(lock_dir)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|modified| modified.elapsed().ok())
                    .is_some_and(|age| age > STALE_LOCK_AGE);
                if is_stale {
                    let _ = std::fs::remove_dir(lock_dir);
                }
                // Checked on every iteration, stale-removal attempted or
                // not - a `remove_dir` that silently fails (the directory
                // somehow isn't empty, a permissions quirk, ...) must never
                // turn this into an infinite loop. Found exactly this way:
                // an early test left a file inside the lock dir, `is_stale`
                // kept being true, `remove_dir` kept silently failing on a
                // non-empty directory, and this spun forever with no
                // deadline check reachable from that branch.
                if std::time::Instant::now() >= deadline {
                    // Proceed without the lock rather than hang forever -
                    // a missed update on timeout is the same failure mode
                    // this is fixing, just rarer, not a new one.
                    break;
                }
                if !is_stale {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
            // Some other filesystem error (permissions, disk full, ...) -
            // don't let a lock we can't even create hang every cycle.
            Err(_) => break,
        }
    }
    let result = f();
    let _ = std::fs::remove_dir(lock_dir);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock_test_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lanes-state-test-lock-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn with_lock_at_returns_the_closures_result() {
        let lock_dir = lock_test_dir("return-value");
        let result = with_lock_at(&lock_dir, || 42);
        assert_eq!(result, 42);
    }

    #[test]
    fn with_lock_at_removes_the_lock_dir_when_done() {
        let lock_dir = lock_test_dir("cleanup");
        with_lock_at(&lock_dir, || ());
        assert!(!lock_dir.exists());
    }

    #[test]
    fn with_lock_at_serializes_concurrent_callers() {
        // The actual bug this exists to fix: two overlapping transactions
        // must never see each other's "before" state - one must fully
        // finish (including its write) before the other even starts its
        // read. Modeled here as two threads incrementing a shared counter
        // through a read-then-write-with-a-delay step that would lose an
        // update if they interleaved.
        let lock_dir = std::sync::Arc::new(lock_test_dir("serialize"));
        let counter = std::sync::Arc::new(std::sync::Mutex::new(0));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let lock_dir = lock_dir.clone();
                let counter = counter.clone();
                std::thread::spawn(move || {
                    with_lock_at(&lock_dir, || {
                        let current = *counter.lock().unwrap();
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        *counter.lock().unwrap() = current + 1;
                    });
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(*counter.lock().unwrap(), 8, "a racing pair would have lost at least one increment");
        let _ = std::fs::remove_dir_all(&*lock_dir);
    }

    #[test]
    fn with_lock_at_clears_a_stale_lock_instead_of_waiting_out_the_deadline() {
        let lock_dir = lock_test_dir("stale");
        std::fs::create_dir(&lock_dir).unwrap();
        // Backdate it past STALE_LOCK_AGE, standing in for a lock a
        // crashed process never cleaned up - std's own File::set_modified
        // (stable since 1.75) is enough for this, no extra dependency
        // needed just to touch an mtime. The directory must stay empty -
        // remove_dir only removes empty directories, and a non-empty one
        // here would make this test exercise the "stale removal silently
        // fails" case instead (a real bug `with_lock_at` had to be fixed
        // for separately - see the comment at its deadline check).
        let backdated = std::time::SystemTime::now() - (STALE_LOCK_AGE + std::time::Duration::from_secs(1));
        std::fs::File::open(&lock_dir).unwrap().set_modified(backdated).unwrap();

        let start = std::time::Instant::now();
        with_lock_at(&lock_dir, || ());
        // A stale lock is cleared and retried immediately, not waited out -
        // this should finish almost instantly, nowhere near the full
        // contention timeout.
        assert!(start.elapsed() < LOCK_WAIT_TIMEOUT / 2);
        let _ = std::fs::remove_dir_all(&lock_dir);
    }

    #[test]
    fn atomic_write_replaces_contents_and_leaves_no_tmp_behind() {
        let dir = std::env::temp_dir().join(format!("lanes-state-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.kdl");

        atomic_write(&path, "focused-lane infra\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "focused-lane infra\n");
        // Regression guard: a torn read is exactly what the switcher's
        // fs-watcher used to act on (edit-mode flickering false->true, its
        // window raise firing on the bounce) - the staging file must be gone
        // once the write lands, and never be the thing the watcher sees.
        assert!(!path.with_extension("kdl.tmp").exists());

        atomic_write(&path, "focused-lane lanes-dev\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "focused-lane lanes-dev\n");
        assert!(!path.with_extension("kdl.tmp").exists());

        std::fs::remove_dir_all(&dir).ok();
    }

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
    fn scalar_bool_or_uses_default_when_absent_but_not_when_present() {
        let mut doc = KdlDocument::new();
        assert_eq!(get_scalar_bool_or(&doc, "diagnostics-enabled", true), true);
        put_scalar(&mut doc, "diagnostics-enabled", false);
        assert_eq!(get_scalar_bool_or(&doc, "diagnostics-enabled", true), false);
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
    fn clear_wezterm_tab_id_removes_only_the_matching_session() {
        let mut doc = KdlDocument::new();
        put_wezterm_tab_id(&mut doc, "infra", 3);
        put_wezterm_tab_id(&mut doc, "lanes", 4);
        clear_wezterm_tab_id_from(&mut doc, "infra");
        assert_eq!(wezterm_tab_id_from(&doc, "infra"), None);
        assert_eq!(wezterm_tab_id_from(&doc, "lanes"), Some(4));
    }

    #[test]
    fn clear_wezterm_tab_id_is_a_no_op_when_nothing_matches() {
        let mut doc = KdlDocument::new();
        put_wezterm_tab_id(&mut doc, "lanes", 4);
        clear_wezterm_tab_id_from(&mut doc, "infra");
        assert_eq!(wezterm_tab_id_from(&doc, "lanes"), Some(4));
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

    #[test]
    fn claude_session_disabled_defaults_false_when_absent() {
        let doc = KdlDocument::new();
        assert!(!claude_session_disabled_in(&doc, "abc-123"));
    }

    #[test]
    fn set_claude_session_disabled_in_marks_a_session_disabled() {
        let mut doc = KdlDocument::new();
        set_claude_session_disabled_in(&mut doc, "abc-123", true);
        assert!(claude_session_disabled_in(&doc, "abc-123"));
    }

    #[test]
    fn set_claude_session_disabled_in_re_enables_without_leaving_a_duplicate() {
        let mut doc = KdlDocument::new();
        set_claude_session_disabled_in(&mut doc, "abc-123", true);
        set_claude_session_disabled_in(&mut doc, "abc-123", false);
        assert!(!claude_session_disabled_in(&doc, "abc-123"));
        assert_eq!(doc.nodes().iter().filter(|n| n.name().value() == "claude-session-disabled").count(), 0);
    }

    #[test]
    fn claude_session_disabled_is_independent_per_session() {
        let mut doc = KdlDocument::new();
        set_claude_session_disabled_in(&mut doc, "abc-123", true);
        assert!(claude_session_disabled_in(&doc, "abc-123"));
        assert!(!claude_session_disabled_in(&doc, "def-456"));
    }

    #[test]
    fn set_claude_session_disabled_in_true_twice_does_not_duplicate() {
        let mut doc = KdlDocument::new();
        set_claude_session_disabled_in(&mut doc, "abc-123", true);
        set_claude_session_disabled_in(&mut doc, "abc-123", true);
        assert_eq!(doc.nodes().iter().filter(|n| n.name().value() == "claude-session-disabled").count(), 1);
    }

    #[test]
    fn signal_dismissed_defaults_false_and_round_trips_per_id() {
        let mut doc = KdlDocument::new();
        let id = "command:lanes--3--2026-09-04T10:00:00Z";
        assert!(!signal_dismissed_in(&doc, id));
        set_signal_dismissed_in(&mut doc, id, true);
        assert!(signal_dismissed_in(&doc, id));
        // a different occurrence is untouched
        assert!(!signal_dismissed_in(&doc, "command:lanes--3--2026-09-04T11:00:00Z"));
        set_signal_dismissed_in(&mut doc, id, false);
        assert!(!signal_dismissed_in(&doc, id));
        assert_eq!(doc.nodes().iter().filter(|n| n.name().value() == "signal-dismissed").count(), 0);
    }

    #[test]
    fn set_signal_dismissed_in_true_twice_does_not_duplicate() {
        let mut doc = KdlDocument::new();
        set_signal_dismissed_in(&mut doc, "x", true);
        set_signal_dismissed_in(&mut doc, "x", true);
        assert_eq!(doc.nodes().iter().filter(|n| n.name().value() == "signal-dismissed").count(), 1);
    }
}
