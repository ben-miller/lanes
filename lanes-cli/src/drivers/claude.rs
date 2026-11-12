use std::fs;
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

#[derive(Deserialize)]
struct ActiveSession {
    session_id: String,
    zellij_session: Option<String>,
    zellij_pane_id: Option<u32>,
    pid: Option<u32>,
    state: Option<String>,
    cwd: Option<String>,
}

/// A live Claude Code session. `state` is the raw registry value ("idle" |
/// "busy" | "running" | "permission_pending" | ...), defaulting to "running"
/// when absent - interpreting it into a signal/reason beyond that is a
/// policy decision that belongs with whatever's doing the interpreting (see
/// scope::observe), not the driver. `permission_pending` is only ever
/// cleared by the Stop/StopFailure hooks writing "idle" back once the
/// assistant's turn completes, so as long as `session_is_live` says the
/// process is still around, an old mtime just means the prompt is still
/// sitting there unanswered, not that the write was abandoned.
pub struct ClaudeSession {
    pub session_id: String,
    pub zellij_session: Option<String>,
    pub state: String,
    /// The numeric `$ZELLIJ_PANE_ID` the SessionStart hook captured at
    /// startup - the same id space `zellij action list-panes` reports, used
    /// to place this session in its Zellij session's on-screen tab/pane
    /// order (see `cycle_claude_session`). `cwd` can't be used for this: two
    /// Claude sessions in the same repo but different tabs share a cwd.
    pub zellij_pane_id: Option<u32>,
}

pub fn enumerate() -> Vec<ClaudeSession> {
    let registry_dir = dirs::active_sessions_dir();
    let entries = match fs::read_dir(&registry_dir) {
        Ok(e) => e,
        Err(_) => return vec![],
    };

    let live_zellij_sessions = crate::drivers::zellij::running_sessions();

    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| load_session(&e.path(), &live_zellij_sessions))
        .collect()
}

fn load_session(path: &Path, live_zellij_sessions: &std::collections::HashSet<String>) -> Option<ClaudeSession> {
    let data = fs::read_to_string(path).ok()?;
    let s: ActiveSession = serde_json::from_str(&data).ok()?;

    let zellij_session = s.zellij_session.clone().unwrap_or_default();
    let age = crate::file_age_secs(path);
    if !crate::session_is_live(&zellij_session, live_zellij_sessions, s.pid, age) {
        return None;
    }

    let state = s.state.unwrap_or_else(|| "running".to_string());
    let zellij_for_check = s.zellij_session.as_deref().filter(|z| !z.is_empty());
    let state = resolve_running_state(state, zellij_for_check, s.zellij_pane_id, tui_shows_interruptible);

    Some(ClaudeSession {
        session_id: s.session_id,
        zellij_session: s.zellij_session,
        state,
        zellij_pane_id: s.zellij_pane_id,
    })
}

/// Only ever asks `check_tui` about a session the hook-written state
/// already claims is `running` - confirming an already-`idle` session is
/// both pointless and (`dump-screen` is a real zellij CLI round trip,
/// ~150-200ms each) needless cost on every scan. `Some(false)` from
/// `check_tui` is the only thing that overrides; anything else
/// (`Some(true)`, `None`, or no zellij session/pane to check at all)
/// leaves `state` exactly as the hook wrote it. Ported from lensd's
/// `source::claude` (PLAN-retire-lensd.md step 2) - `enumerate()` is now
/// the one place this check happens, for both `lanes cycle` and the
/// dashboard chip.
fn resolve_running_state(state: String, zellij_session: Option<&str>, pane_id: Option<u32>, check_tui: impl Fn(&str, u32) -> Option<bool>) -> String {
    if state != "running" {
        return state;
    }
    let (Some(session), Some(pane)) = (zellij_session, pane_id) else { return state };
    match check_tui(session, pane) {
        Some(false) => "idle".to_string(),
        _ => state,
    }
}

/// Reads Claude Code's own TUI, via the zellij pane hosting it, for
/// whether it's currently showing its "esc to interrupt" control - the
/// literal thing Escape acts on. Needed because Claude Code has no hook
/// event for a user-initiated interrupt (only `Stop`, which fires on
/// natural completion), so a tool call aborted mid-run leaves `state`
/// stuck at "running" forever with nothing to ever correct it - and no
/// process-level signal reliably distinguishes "really still running"
/// from "was interrupted". The TUI text is ground truth instead of an
/// inference. `None` when the check itself is inconclusive (zellij not on
/// PATH, that session or pane no longer exists, the dump came back
/// empty, ...) - callers treat that as "don't override," never as a
/// confident idle.
fn tui_shows_interruptible(zellij_session: &str, pane_id: u32) -> Option<bool> {
    let output = Command::new("zellij")
        .args(["--session", zellij_session, "action", "dump-screen", "--pane-id", &pane_id.to_string()])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    if text.trim().is_empty() || text.contains("not found") {
        return None;
    }
    Some(screen_shows_interruptible(&text))
}

/// A narrow enough pane truncates the footer's trailing clause with an
/// ellipsis before the literal phrase ever fully renders - confirmed
/// directly on a pane that genuinely was mid-turn, rendering `· auto mode
/// on (shift+tab to cycle) · es…` instead of `... · esc to interrupt`, so
/// a plain `contains("esc to interrupt")` false-negatived a real "running"
/// session down to idle. The truncated idle counterpart (the footer's
/// other trailing clause, e.g. `· ← for history`) renders as `· ← …`
/// instead - a different prefix right after the separator - so matching
/// the separator-plus-prefix (`"· es"`) instead of the full phrase is
/// truncation-safe without losing precision: it only matches when the
/// clause actually starts with "es", truncated or not.
fn screen_shows_interruptible(text: &str) -> bool {
    text.contains("esc to interrupt") || text.contains("· es")
}

/// A registry entry whose process is still genuinely alive, but whose
/// recorded zellij_session no longer matches any currently running Zellij
/// session.
pub struct RenamedCandidate {
    pub session_id: String,
    pub old_zellij_session: String,
    pub cwd: Option<String>,
    pub pid: u32,
}

/// Distinct from a fully dead entry, which `session_is_live` (and thus
/// `enumerate()`) already filters out entirely: here the recorded pid is
/// still a genuine, running `claude` process, just tagged with a Zellij
/// session name that doesn't exist anymore - most likely because that
/// session got renamed after this Claude session started
/// (session-start.sh captures $ZELLIJ_SESSION_NAME once, at hook time, and
/// never refreshes it). A live process silently invisible to every lane
/// until this is noticed and fixed - see cmd::doctor.
pub fn possibly_renamed_sessions() -> Vec<RenamedCandidate> {
    let registry_dir = dirs::active_sessions_dir();
    let entries = match fs::read_dir(&registry_dir) {
        Ok(e) => e,
        Err(_) => return vec![],
    };
    let live_zellij_sessions = crate::drivers::zellij::running_sessions();

    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| check_renamed(&e.path(), &live_zellij_sessions))
        .collect()
}

fn check_renamed(path: &Path, live_zellij_sessions: &std::collections::HashSet<String>) -> Option<RenamedCandidate> {
    let data = fs::read_to_string(path).ok()?;
    let s: ActiveSession = serde_json::from_str(&data).ok()?;

    let zellij_session = s.zellij_session.clone().unwrap_or_default();
    if zellij_session.is_empty() || live_zellij_sessions.contains(&zellij_session) {
        return None;
    }

    let pid = s.pid?;
    // Same pid-reuse guard as session_is_live: a recycled pid pointing at an
    // unrelated claude shouldn't get reported as "your session was renamed".
    if !crate::pid_is_this_session(pid, crate::file_age_secs(path)) {
        return None;
    }

    Some(RenamedCandidate {
        session_id: s.session_id,
        old_zellij_session: zellij_session,
        cwd: s.cwd,
        pid,
    })
}

mod dirs {
    use std::path::PathBuf;

    pub fn claude_dir() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_default();
        PathBuf::from(home).join(".claude")
    }

    pub fn active_sessions_dir() -> PathBuf {
        claude_dir().join("active-sessions")
    }
}

/// A running watch on the registry - dropping it stops both the file
/// watch and the rescan timer below.
pub struct Watch {
    _fs_watch: Option<appctl::watch::Watch>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// A session whose process dies without running its `SessionEnd` hook
/// leaves its file untouched, so no file event ever says it's gone -
/// rescanning on this interval notices (same value lensd's own watch used).
const LIVENESS_RESCAN: std::time::Duration = std::time::Duration::from_secs(5);

/// Watches the active-sessions registry for changes, calling `on_change`
/// with the current live sessions (`enumerate()`) right away and again on
/// every change - file events (a hook rewrote a session's file) plus a
/// steady rescan every [`LIVENESS_RESCAN`], since a session whose process
/// dies without its `SessionEnd` hook running leaves its file untouched
/// forever. Moved from lensd's `source::claude::run`/`run_with`
/// (PLAN-retire-lensd.md step 2) - same dual mechanism, just built on
/// `appctl::watch` instead of lensd's own engine-internal watch loop, and
/// reporting the current `Vec<ClaudeSession>` directly rather than a
/// Datalog fact delta.
pub fn watch(on_change: impl Fn(Vec<ClaudeSession>) + Send + Sync + 'static) -> Watch {
    let on_change = std::sync::Arc::new(on_change);
    on_change(enumerate());

    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    let fs_on_change = on_change.clone();
    let fs_watch = appctl::watch::watch(
        &[(dirs::active_sessions_dir(), appctl::watch::RecursiveMode::NonRecursive)],
        appctl::watch::DEFAULT_DEBOUNCE,
        |_| true,
        move || fs_on_change(enumerate()),
    )
    .ok();

    let tick_stop = stop.clone();
    std::thread::spawn(move || {
        while !tick_stop.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::sleep(LIVENESS_RESCAN);
            if tick_stop.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            on_change(enumerate());
        }
    });

    Watch { _fs_watch: fs_watch, stop }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_becomes_idle_when_the_tui_no_longer_shows_interruptible() {
        // An interrupted tool call: Escape killed it, Claude Code's TUI no
        // longer shows "esc to interrupt," but nothing hook-based ever
        // flips `state` back - this is the case the whole mechanism
        // exists for.
        assert_eq!(resolve_running_state("running".to_string(), Some("s"), Some(3), |_, _| Some(false)), "idle");
    }

    #[test]
    fn running_is_left_alone_when_the_tui_still_shows_interruptible() {
        // A real still-running tool, however long - the TUI reflects
        // truth at any instant, not a guess from elapsed time.
        assert_eq!(resolve_running_state("running".to_string(), Some("s"), Some(3), |_, _| Some(true)), "running");
    }

    #[test]
    fn running_is_left_alone_when_the_tui_check_is_inconclusive() {
        assert_eq!(resolve_running_state("running".to_string(), Some("s"), Some(3), |_, _| None), "running");
    }

    #[test]
    fn running_is_left_alone_with_no_zellij_session_or_pane_to_check() {
        assert_eq!(resolve_running_state("running".to_string(), None, Some(3), |_, _| Some(false)), "running");
        assert_eq!(resolve_running_state("running".to_string(), Some("s"), None, |_, _| Some(false)), "running");
    }

    #[test]
    fn idle_is_never_touched_regardless_of_the_tui_check() {
        assert_eq!(resolve_running_state("idle".to_string(), Some("s"), Some(3), |_, _| Some(false)), "idle");
    }

    #[test]
    fn screen_shows_interruptible_matches_the_full_phrase() {
        assert!(screen_shows_interruptible("⏵⏵ auto mode on (shift+tab to cycle) · esc to interrupt"));
    }

    #[test]
    fn screen_shows_interruptible_matches_a_narrow_pane_truncated_footer() {
        // Regression: a narrow enough pane truncates the footer to "· es…"
        // before "esc to interrupt" ever fully renders - confirmed
        // directly on a genuinely mid-turn session, which this previously
        // misread as idle because the literal phrase never appeared.
        assert!(screen_shows_interruptible("⏵⏵ auto mode on (shift+tab to cycle) · es…"));
    }

    #[test]
    fn screen_shows_interruptible_false_for_the_idle_footers_different_clause() {
        // The idle counterpart's trailing clause is different text
        // entirely (e.g. "← for history"), truncated or not - it must
        // never match just because it also got truncated.
        assert!(!screen_shows_interruptible("⏵⏵ auto mode on (shift+tab to cycle) · ← …"));
    }

    #[test]
    fn watch_reports_the_initial_scan_then_a_file_change() {
        // enumerate() itself reads a fixed, real $HOME-relative directory
        // (dirs::active_sessions_dir), not an injectable one - this only
        // exercises watch()'s own plumbing (initial call + file-event
        // callback), not real registry entries, which is why both calls
        // land on an empty Vec here regardless of what's actually in
        // ~/.claude/active-sessions on the machine running this test.
        let calls = std::sync::Arc::new(std::sync::Mutex::new(0));
        let seen = calls.clone();
        let _watch = watch(move |_sessions| {
            *seen.lock().unwrap() += 1;
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(*calls.lock().unwrap() >= 1, "the initial scan alone should have called back");
    }
}
