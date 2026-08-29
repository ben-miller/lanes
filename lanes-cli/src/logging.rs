use std::io::Write;
use std::path::PathBuf;

pub fn state_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local/state/lanes")
}

/// Appends one timestamped line to `~/.local/state/lanes/<file_name>` -
/// plain text, not JSON, so `tail -f` on the raw file is directly readable
/// without a wrapper tool (the same reason Docker's default log driver is
/// just a file you can tail, not a bespoke streaming protocol). Silently
/// does nothing if the file can't be opened - logging a failure should
/// never itself become a second failure worth surfacing.
pub fn append_line(file_name: &str, level: &str, msg: &str) {
    let path = state_dir().join(file_name);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let ts = chrono::Utc::now().to_rfc3339();
    let _ = writeln!(f, "{ts} [{level}] {msg}");
}

/// Appends one timestamped line to the performance/diagnostics log
/// (`~/.local/state/lanes/perf.log`) - the lane-switch latency timeline:
/// when a switch is triggered, when it actually completes, when the UI
/// receives and renders it. Gated by `state::read_diagnostics_enabled()`
/// and kept in its own file rather than folded into `append_line`'s
/// switch-ui.log/hammerspoon.log: it's written on every single switch (not
/// just warnings/errors), from multiple processes (`lanes` CLI, the Tauri
/// backend, and the frontend via `log_ui_event`), so it needs to be both
/// toggleable independently and easy to `tail -f` in isolation while
/// chasing a specific lag report. Timestamps are microsecond-precision
/// RFC3339 - `append_line`'s second precision is too coarse for gaps that
/// are the whole point of measuring.
pub fn perf(event: &str, detail: &str) {
    if !crate::state::read_diagnostics_enabled() {
        return;
    }
    let path = state_dir().join("perf.log");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    let _ = writeln!(f, "{ts} {event} {detail}");
}

/// Creates `~/.local/state/lanes/<file_name>` (empty, if it doesn't already
/// exist).
fn ensure_log_file(file_name: &str) {
    let path = state_dir().join(file_name);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::OpenOptions::new().create(true).append(true).open(&path);
}

/// Touches both log files into existence - `lanes init`'s whole job. Both,
/// regardless of which one the caller cares about: it's cheap, and it means
/// `lanes switch-ui logs`/`lanes hammerspoon logs` has something to tail
/// from the very first run no matter which component happens to init first,
/// rather than only appearing once that component's first warning fires.
pub fn init_state() {
    ensure_log_file("switch-ui.log");
    ensure_log_file("hammerspoon.log");
    ensure_log_file("perf.log");
}
