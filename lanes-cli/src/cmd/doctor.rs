use std::path::PathBuf;
use std::process::Command;

struct Check {
    label: &'static str,
    status: Status,
    message: String,
    hint: Option<String>,
}

enum Status {
    Ok,
    Warn,
    Fail,
}

impl Status {
    fn symbol(&self) -> &str {
        match self {
            Status::Ok => "✓",
            Status::Warn => "⚠",
            Status::Fail => "✗",
        }
    }
}

pub fn run(fix: bool) {
    let cfg = lanes::config::Config::load();

    let boards_dir = boards_data_dir();
    if fix {
        for project in materialize_boards_projects(&cfg, &boards_dir) {
            println!("created {}/board.yaml", boards_dir.join(&project).display());
        }
        if write_boards_order(&cfg, &boards_dir).is_some() {
            println!("wrote {}", boards_order_path(&boards_dir).display());
        }
    }

    let mut checks = vec![
        check_lanes_registry(),
        check_logs(),
        check_wezterm_tab_cache(&cfg),
        check_session_naming(&cfg),
        check_primary_targets(&cfg),
        check_layout_templates(&cfg, &lanes::config::config_dir().join("layout-templates")),
        check_lane_order(&cfg),
        check_lane_order_vs_ztabs(&cfg),
        check_git_default_branches(&cfg),
        check_lanes_wm(),
        check_monitor_config(&cfg),
        check_target_binaries(&cfg),
        check_wezterm_gui_sockets(),
    ];

    if has_boards_target(&cfg) {
        checks.push(check_boards_projects(&cfg, &boards_dir));
        checks.push(check_boards_order(&cfg, &boards_dir));
    }

    if cfg.driver_enabled("zellij") {
        checks.push(check_zellij());
    }
    if cfg.driver_enabled("claude") {
        checks.push(check_claude());
        checks.push(check_renamed_claude_sessions());
    }
    if cfg.driver_enabled("brotab") {
        checks.push(check_brotab());
    };
    if cfg.driver_enabled("shell") {
        checks.push(check_shell());
    }

    let mut any_fail = false;
    for c in &checks {
        println!("{} {}: {}", c.status.symbol(), c.label, c.message);
        if let Some(hint) = &c.hint {
            println!("  {}", hint);
        }
        if matches!(c.status, Status::Fail) {
            any_fail = true;
        }
    }

    if any_fail {
        std::process::exit(1);
    }
}

fn check_zellij() -> Check {
    let version_out = Command::new("/opt/homebrew/bin/zellij").arg("--version").output();
    match version_out {
        Err(_) => Check {
            label: "zellij",
            status: Status::Fail,
            message: "not found".to_string(),
            hint: Some("brew install zellij".to_string()),
        },
        Ok(out) => {
            let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let sessions_out = Command::new("/opt/homebrew/bin/zellij")
                .args(["list-sessions", "--no-formatting", "--short"])
                .output();
            let session_summary = match sessions_out {
                Ok(o) => {
                    let count = String::from_utf8_lossy(&o.stdout)
                        .lines()
                        .filter(|l| !l.trim().is_empty())
                        .count();
                    format!("{} session(s)", count)
                }
                Err(_) => "could not list sessions".to_string(),
            };
            Check {
                label: "zellij",
                status: Status::Ok,
                message: format!("{} — {}", version, session_summary),
                hint: None,
            }
        }
    }
}

fn check_claude() -> Check {
    let registry = PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join(".claude")
        .join("active-sessions");

    match std::fs::read_dir(&registry) {
        Err(_) => Check {
            label: "claude sessions",
            status: Status::Warn,
            message: format!("registry not found at {}", registry.display()),
            hint: Some("start a Claude Code session to create the registry".to_string()),
        },
        Ok(entries) => {
            let count = entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map_or(false, |x| x == "json"))
                .count();
            Check {
                label: "claude sessions",
                status: Status::Ok,
                message: format!("{} active session(s)", count),
                hint: None,
            }
        }
    }
}

/// A Claude session whose process is genuinely alive but whose registry
/// entry names a Zellij session that no longer exists - most likely
/// because that session was renamed after this Claude session started.
/// `session_is_live` (and thus everything a lane snapshot shows) already
/// excludes this entry entirely, since its recorded zellij_session isn't
/// live - so unlike a real dead session, this one is a live process
/// silently invisible to every lane until it's noticed and fixed.
fn check_renamed_claude_sessions() -> Check {
    let candidates = lanes::possibly_renamed_claude_sessions();

    if candidates.is_empty() {
        Check {
            label: "claude sessions (renamed Zellij session)",
            status: Status::Ok,
            message: "none found".to_string(),
            hint: None,
        }
    } else {
        let details: Vec<String> = candidates.iter()
            .map(|c| format!(
                "{} (pid={}, cwd={}, recorded zellij_session={:?})",
                c.session_id, c.pid, c.cwd.as_deref().unwrap_or("?"), c.old_zellij_session
            ))
            .collect();
        Check {
            label: "claude sessions (renamed Zellij session)",
            status: Status::Warn,
            message: details.join("; "),
            hint: Some(
                "process is alive but invisible to every lane - either restart/resume that \
                 Claude session (re-fires the SessionStart hook with the current Zellij session \
                 name), or edit zellij_session by hand in \
                 ~/.claude/active-sessions/<session_id>.json".to_string()
            ),
        }
    }
}

fn check_brotab() -> Check {
    let bt = Command::new("bt").arg("clients").output();
    match bt {
        Err(_) => Check {
            label: "brotab",
            status: Status::Fail,
            message: "bt not found — browser facet unavailable".to_string(),
            hint: Some(
                "pipx install brotab  →  bt install  →  install Firefox extension from addons.mozilla.org/en-US/firefox/addon/brotab/".to_string(),
            ),
        },
        Ok(out) if out.stdout.is_empty() => Check {
            label: "brotab",
            status: Status::Warn,
            message: "bt found but no connected browsers".to_string(),
            hint: Some(
                "ensure the BroTab extension is installed in Firefox and bt install has been run".to_string(),
            ),
        },
        Ok(out) => {
            let clients = String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count();
            Check {
                label: "brotab",
                status: Status::Ok,
                message: format!("{} connected browser(s)", clients),
                hint: None,
            }
        }
    }
}

fn check_logs() -> Check {
    let dir = lanes::logging::state_dir();
    let missing: Vec<&str> = ["switch-ui.log", "hammerspoon.log"]
        .into_iter()
        .filter(|f| !dir.join(f).exists())
        .collect();

    if missing.is_empty() {
        Check {
            label: "logs",
            status: Status::Ok,
            message: format!("switch-ui.log and hammerspoon.log present in {}", dir.display()),
            hint: None,
        }
    } else {
        Check {
            label: "logs",
            status: Status::Warn,
            message: format!("missing: {}", missing.join(", ")),
            hint: Some("run `lanes init`".to_string()),
        }
    }
}

/// Two distinct ways state.kdl's wezterm-tab-id cache can go wrong.
/// Whichever tool owns a tab's lifecycle (e.g. `ztabs`) is responsible for
/// keeping this cache honest by pushing/clearing it as tabs come and go;
/// neither failure mode below is otherwise visible without a one-off check
/// like this:
///
/// - orphaned: a cached session that doesn't match any lane at all anymore
///   (e.g. a renamed/removed lane's leftover entry) - detectable from config
///   alone.
/// - stale: a cached id for a lane that's active and tracked, but the id
///   doesn't match any currently-open WezTerm tab - needs one live
///   `wezterm cli list` round trip. Fine here specifically: doctor is a
///   manual, on-demand command, not the live dashboard's own refresh path,
///   which deliberately avoids hitting WezTerm directly and trusts the
///   cache instead (see `lensd.rs`'s reachability computation, which reads
///   the same cache).
fn check_wezterm_tab_cache(cfg: &lanes::config::Config) -> Check {
    let cached = lanes::state::all_wezterm_tab_ids();
    let live_ids = live_wezterm_tab_ids();
    let (orphaned, stale) = classify_tab_cache(&cached, cfg, &live_ids);

    if orphaned.is_empty() && stale.is_empty() {
        Check {
            label: "wezterm tab cache",
            status: Status::Ok,
            message: format!("{} cached mapping(s), all consistent", cached.len()),
            hint: None,
        }
    } else {
        let mut parts = Vec::new();
        let mut hints = Vec::new();
        if !orphaned.is_empty() {
            parts.push(format!(
                "Zellij session name(s) cached in state.kdl but matching no lane in lanes' own config: {}",
                orphaned.join(", ")
            ));
            hints.push(format!(
                "no-longer-a-lane (run `lanes tabs clear <zellij-session-name>` for each): {}",
                orphaned.join(", ")
            ));
        }
        if !stale.is_empty() {
            parts.push(format!(
                "Zellij session name(s) whose cached WezTerm tab-id doesn't match any open WezTerm tab: {}",
                stale.join(", ")
            ));
            hints.push(format!(
                "cached id stale, lane still exists (re-run `ztabs sync`/`up`, \
                 or `lanes tabs set <zellij-session-name> <wezterm-tab-id>` by hand): {}",
                stale.join(", ")
            ));
        }
        Check {
            label: "wezterm tab cache",
            status: Status::Warn,
            message: parts.join("; "),
            hint: Some(hints.join(". ")),
        }
    }
}

fn classify_tab_cache(
    cached: &[(String, u64)],
    cfg: &lanes::config::Config,
    live_ids: &std::collections::HashSet<u64>,
) -> (Vec<String>, Vec<String>) {
    let known_sessions: std::collections::HashSet<&str> = cfg.lanes.iter()
        .filter_map(|l| l.terminal_session())
        .collect();

    let orphaned: Vec<String> = cached.iter()
        .filter(|(session, _)| !known_sessions.contains(session.as_str()))
        .map(|(session, id)| format!("{session} (cached wezterm tab-id={id})"))
        .collect();

    let stale: Vec<String> = cached.iter()
        .filter(|(session, _)| known_sessions.contains(session.as_str()))
        .filter(|(session, id)| {
            cfg.lane_for_session(session).is_some_and(|l| l.active) && !live_ids.contains(id)
        })
        .map(|(session, id)| format!("{session} (cached wezterm tab-id={id})"))
        .collect();

    (orphaned, stale)
}

fn live_wezterm_tab_ids() -> std::collections::HashSet<u64> {
    let mut cmd = Command::new("/opt/homebrew/bin/wezterm");
    cmd.args(["cli", "--no-auto-start", "list", "--format", "json"]);
    if let Some(sock) = lanes::wezterm_socket() {
        cmd.env("WEZTERM_UNIX_SOCKET", sock);
    }
    let Ok(out) = cmd.output() else { return std::collections::HashSet::new() };
    if !out.status.success() { return std::collections::HashSet::new(); }
    let Ok(panes) = serde_json::from_slice::<Vec<serde_json::Value>>(&out.stdout) else {
        return std::collections::HashSet::new();
    };
    panes.iter().filter_map(|p| p.get("tab_id").and_then(|v| v.as_u64())).collect()
}

/// A Zellij session name containing anything but lowercase letters,
/// digits, and single hyphens between segments - the same convention
/// `infra_zellij.py` now validates on its own side. A non-kebab-case name
/// (a space, in the one real instance so far) is exactly what let a
/// rename slip past unnoticed and orphan an already-running Claude
/// session's registry entry (see the "renamed Zellij session" check
/// above) - env vars and registry files don't get updated when a session
/// is renamed, so a name that's easy to typo or rename inconsistently is
/// a real hazard, not just a style nitpick.
fn is_kebab_case(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    s.split('-').all(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()))
}

/// The one check in this file that makes a real network call, deliberately
/// - the live dashboard's own non-default-branch signal (lensd's
/// `default_branch` relation) reads a repo's cached
/// refs/remotes/origin/HEAD purely locally, which is fast but can silently
/// go stale if the remote's own default branch changes after clone (git has
/// no hook that fires on that, and a plain `git fetch` doesn't refresh this
/// ref either - only `git remote set-head origin -a` does). There's no way
/// to detect that staleness without actually asking the remote, so it
/// belongs here - an occasional, manually-run diagnostic - not on every UI
/// refresh.
fn check_git_default_branches(cfg: &lanes::config::Config) -> Check {
    let repo_paths: std::collections::HashSet<String> = cfg.lanes.iter()
        .flat_map(|l| l.scope.iter())
        .filter_map(|el| el.repo_path())
        .map(lanes::expand_tilde)
        .collect();

    let mismatches: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = repo_paths.into_iter()
            .map(|path| scope.spawn(move || check_one_repo_default_branch(&path)))
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok().flatten()).collect()
    });

    if mismatches.is_empty() {
        Check {
            label: "git default branches",
            status: Status::Ok,
            message: "cached origin/HEAD matches the remote for every checkable repo".to_string(),
            hint: None,
        }
    } else {
        Check {
            label: "git default branches",
            status: Status::Warn,
            message: format!("cached default branch is stale for: {}", mismatches.join("; ")),
            hint: Some(
                "run `git remote set-head origin -a` in each - lanes reads this locally \
                 and has no way to notice a remote's default branch changing on its own"
                    .to_string(),
            ),
        }
    }
}

/// `None` covers three cases lanes treats identically here: no remote named
/// "origin" (a purely local repo), the network call failed (offline, or a
/// slow/unreachable remote - no timeout wrapping this yet, a known gap if
/// that turns out to actually hang in practice), or the cached and actual
/// values simply agree. Only a confirmed, live mismatch returns `Some`.
fn check_one_repo_default_branch(path: &str) -> Option<String> {
    let cached = cached_origin_head(path)?;
    let actual = remote_origin_head(path)?;
    if cached == actual {
        return None;
    }
    Some(format!("{path} (cached \"{cached}\", remote says \"{actual}\")"))
}

fn cached_origin_head(path: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["-C", path, "symbolic-ref", "--short", "-q", "refs/remotes/origin/HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().strip_prefix("origin/").map(str::to_string)
}

fn remote_origin_head(path: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["-C", path, "ls-remote", "--symref", "origin", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_ls_remote_symref(&String::from_utf8_lossy(&out.stdout))
}

/// `git ls-remote --symref origin HEAD` prints a symref line
/// (`ref: refs/heads/main\tHEAD`) followed by a plain sha/ref line - this
/// only ever needs the branch name out of the first one.
fn parse_ls_remote_symref(output: &str) -> Option<String> {
    output.lines()
        .find_map(|line| line.strip_prefix("ref: refs/heads/"))
        .and_then(|rest| rest.split('\t').next())
        .map(|s| s.trim().to_string())
}

fn check_session_naming(cfg: &lanes::config::Config) -> Check {
    let bad: Vec<&str> = cfg.lanes.iter()
        .filter_map(|l| l.terminal_session())
        .filter(|s| !is_kebab_case(s))
        .collect();

    if bad.is_empty() {
        Check {
            label: "session naming",
            status: Status::Ok,
            message: "all Zellij session names are kebab-case".to_string(),
            hint: None,
        }
    } else {
        Check {
            label: "session naming",
            status: Status::Warn,
            message: format!("not kebab-case: {}", bad.join(", ")),
            hint: Some(
                "not auto-renamed - renaming a live Zellij session can orphan anything \
                 already running in it (see the check above). Fix the lane's config for new \
                 sessions, or rename the existing session by hand with care.".to_string()
            ),
        }
    }
}

/// At most one target per lane may be marked `primary` - `land_lane`
/// (lanes-cli's own lib.rs) trusts this and just takes the first match it
/// finds rather than detecting or resolving a tie, so an ambiguous lane is
/// a real config error to catch here, not something to guess through at
/// switch time.
fn check_primary_targets(cfg: &lanes::config::Config) -> Check {
    let offending: Vec<&str> = cfg.lanes.iter()
        .filter(|l| l.targets.iter().filter(|t| t.primary).count() > 1)
        .map(|l| l.id.as_str())
        .collect();

    if offending.is_empty() {
        Check {
            label: "primary targets",
            status: Status::Ok,
            message: "every lane has at most one primary target".to_string(),
            hint: None,
        }
    } else {
        Check {
            label: "primary targets",
            status: Status::Fail,
            message: format!("lane(s) with more than one target marked primary: {}", offending.join(", ")),
            hint: Some("edit that lane's layout.toml (or the global default) so only one target sets primary = true".to_string()),
        }
    }
}

/// Every lane's `layout_template` (if set) must resolve to a real file
/// under `layout-templates/` - a `Fail`, not a `Warn`: a lane silently
/// missing its whole template layer on a typo is an easy-to-miss bug, not a
/// soft problem. `templates_dir` is always
/// `config_dir().join("layout-templates")` outside tests - a parameter for
/// the same hermeticity reason `config::load_lanes`'s `dir` is.
fn check_layout_templates(cfg: &lanes::config::Config, templates_dir: &std::path::Path) -> Check {
    let missing: Vec<String> = cfg.lanes.iter()
        .filter_map(|l| l.layout_template.as_ref().map(|t| (l.id.as_str(), t.as_str())))
        .filter(|(_, name)| !templates_dir.join(format!("{name}.toml")).is_file())
        .map(|(id, name)| format!("{id} -> \"{name}\""))
        .collect();

    if missing.is_empty() {
        Check {
            label: "layout templates",
            status: Status::Ok,
            message: "every lane's layout_template (if any) resolves to a real file".to_string(),
            hint: None,
        }
    } else {
        Check {
            label: "layout templates",
            status: Status::Fail,
            message: format!("layout_template doesn't resolve to a file under layout-templates/: {}", missing.join(", ")),
            hint: Some(format!("create the missing file(s) under {:?}, or fix the typo in that lane's layout.toml", templates_dir)),
        }
    }
}

fn check_lane_order(cfg: &lanes::config::Config) -> Check {
    let order = match &cfg.order {
        None => {
            return Check {
                label: "context order",
                status: Status::Ok,
                message: "no `order` configured in contexts.toml (falling back to alphabetical)".to_string(),
                hint: None,
            };
        }
        Some(order) => order,
    };

    // Checked against every context directory, not just `cfg.lanes` - a
    // context doesn't need a lane.toml to need a place in `order` (e.g. a
    // board-only context like `development`).
    let unknown: Vec<&str> = order.iter()
        .map(|id| id.as_str())
        .filter(|id| !cfg.context_ids.iter().any(|c| c == id))
        .collect();
    let missing = cfg.unregistered_contexts();

    if unknown.is_empty() && missing.is_empty() {
        return Check {
            label: "context order",
            status: Status::Ok,
            message: "contexts.toml `order` covers exactly the configured contexts".to_string(),
            hint: None,
        };
    }

    let mut parts = Vec::new();
    if !unknown.is_empty() {
        parts.push(format!("order lists unknown context id(s): {}", unknown.join(", ")));
    }
    if !missing.is_empty() {
        parts.push(format!("context(s) missing from order (sorted alphabetically last): {}", missing.join(", ")));
    }

    // A context directory not registered in `order` works fine at runtime
    // (it just sorts last, alphabetically) but is a real registration gap -
    // fail doctor so it gets noticed, same as any other config that's
    // tolerated at runtime but enforced at startup/doctor time.
    let status = if missing.is_empty() { Status::Warn } else { Status::Fail };

    Check {
        label: "context order",
        status,
        message: parts.join("; "),
        hint: Some(
            "not auto-fixed - edit `order` in ~/.config/lanes/contexts.toml by hand to match \
             the current set of context ids.".to_string()
        ),
    }
}

/// `ztabs`' own session order, straight from its config file - not a live
/// query of anything. This is the other tool's core state, same class of
/// thing as contexts.toml's `order`, just owned by a different tool.
fn ztabs_session_order() -> Option<Vec<String>> {
    let home = std::env::var("HOME").unwrap_or_default();
    let path = PathBuf::from(home).join(".config/infra/zellij-tabs.toml");
    let content = std::fs::read_to_string(path).ok()?;
    let doc: toml::Value = toml::from_str(&content).ok()?;
    let tabs = doc.get("tabs")?.as_array()?;
    Some(
        tabs.iter()
            .filter_map(|t| t.get("session").and_then(|v| v.as_str()).map(String::from))
            .collect(),
    )
}

/// `order`, restricted to whichever entries also appear in `other`, keeping
/// `order`'s own relative sequence. Used to compare two orderings that may
/// not cover exactly the same set of sessions (e.g. contexts.toml has a lane
/// ztabs doesn't manage) without that difference in *membership* being
/// mistaken for a difference in *order*.
fn restrict_to_common(order: &[String], other: &[String]) -> Vec<String> {
    let other_set: std::collections::HashSet<&String> = other.iter().collect();
    order.iter().filter(|s| other_set.contains(s)).cloned().collect()
}

fn check_lane_order_vs_ztabs(cfg: &lanes::config::Config) -> Check {
    let Some(ztabs_order) = ztabs_session_order() else {
        return Check {
            label: "lane order vs ztabs",
            status: Status::Ok,
            message: "no ztabs config found, skipping".to_string(),
            hint: None,
        };
    };

    let lane_session_order: Vec<String> = cfg
        .lanes
        .iter()
        .filter_map(|l| l.terminal_session())
        .map(String::from)
        .collect();

    let lanes_side = restrict_to_common(&lane_session_order, &ztabs_order);
    let ztabs_side = restrict_to_common(&ztabs_order, &lane_session_order);

    if lanes_side == ztabs_side {
        Check {
            label: "lane order vs ztabs",
            status: Status::Ok,
            message: "contexts.toml `order` agrees with zellij-tabs.toml's session order".to_string(),
            hint: None,
        }
    } else {
        Check {
            label: "lane order vs ztabs",
            status: Status::Warn,
            message: format!(
                "contexts.toml order gives {}, but zellij-tabs.toml gives {}",
                lanes_side.join(", "),
                ztabs_side.join(", ")
            ),
            hint: Some(
                "not auto-fixed - edit `order` in ~/.config/lanes/contexts.toml or the tab order in \
                 ~/.config/infra/zellij-tabs.toml so the two agree."
                    .to_string(),
            ),
        }
    }
}

fn check_lanes_registry() -> Check {
    let dir = lanes::config::config_dir().join("contexts");

    match std::fs::read_dir(&dir) {
        Err(_) => Check {
            label: "lanes config",
            status: Status::Warn,
            message: format!("config dir not found at {}", dir.display()),
            hint: Some("create ~/.config/lanes/ and add lane TOML files".to_string()),
        },
        Ok(entries) => {
            let count = entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .count();
            Check {
                label: "lanes config",
                status: Status::Ok,
                message: format!("{} context(s) defined in {}", count, dir.display()),
                hint: None,
            }
        }
    }
}

/// The `shell` driver is enabled but silent unless the hook is installed
/// and writing records. This surfaces the two things that break that: the
/// state dir, and whether any records are actually landing / are sane.
fn check_shell() -> Check {
    let dir = lanes::logging::state_dir().join("shell");
    if !dir.exists() {
        return Check {
            label: "shell driver",
            status: Status::Warn,
            message: format!("state dir {} does not exist", dir.display()),
            hint: Some("run `lanes init`, then add `lanes shell-init fish | source` to config.fish".to_string()),
        };
    }

    let records = lanes::drivers::shell::enumerate();
    if records.is_empty() {
        return Check {
            label: "shell driver",
            status: Status::Warn,
            message: format!("no command records in {}", dir.display()),
            hint: Some("add `lanes shell-init fish | source` to config.fish (and reload the shell), then run something longer than LANES_SHELL_LATCH_SECS".to_string()),
        };
    }

    let now = chrono::Utc::now();
    let implausible = records.iter().any(|r| {
        r.started_at
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .is_some_and(|t| (t.with_timezone(&chrono::Utc) - now).num_seconds() > 60)
    });
    if implausible {
        return Check {
            label: "shell driver",
            status: Status::Warn,
            message: "a record has a far-future started_at - clock skew or a malformed hook write".to_string(),
            hint: None,
        };
    }

    Check {
        label: "shell driver",
        status: Status::Ok,
        message: format!("{} command record(s) in {}", records.len(), dir.display()),
        hint: None,
    }
}

/// Checks lanes-wm itself: that it resolves on PATH at all (everything
/// window-placement-related shells out to it by bare name - if the symlink
/// ever points somewhere stale or gets removed, every placement fails),
/// and - if it does resolve - whether it currently reports itself trusted
/// for Accessibility. Uses `lanes-wm trusted`, not `request-access`: the
/// latter can pop the system permission dialog, which a passive health
/// check should never do just by running.
fn check_lanes_wm() -> Check {
    let status = Command::new("lanes-wm").arg("trusted").status();
    match status {
        Err(_) => Check {
            label: "lanes-wm",
            status: Status::Fail,
            message: "lanes-wm not found on PATH".to_string(),
            hint: Some("window placement on lane switch will silently do nothing until this is fixed - see lanes-wm's README".to_string()),
        },
        Ok(s) if s.success() => Check {
            label: "lanes-wm",
            status: Status::Ok,
            message: "found on PATH, trusted for Accessibility".to_string(),
            hint: None,
        },
        Ok(_) => Check {
            label: "lanes-wm",
            status: Status::Fail,
            message: "found on PATH, but not trusted for Accessibility".to_string(),
            hint: Some("run `lanes-wm request-access` and click Allow - see lanes-wm's README for why this can silently drop after a rebuild".to_string()),
        },
    }
}

/// Flags dead `gui-sock-*` files (a WezTerm GUI that died without cleanup)
/// and a stray `wezterm-mux-server` process - both symptoms of
/// BUG-wezterm-stale-socket.md. `wezterm_socket()` already skips dead
/// sockets at switch time, so this is purely diagnostic (nothing here
/// would otherwise cause a failure), but a pile of dead socket files or a
/// mux server nobody's using is worth surfacing rather than silently
/// accumulating `wezterm-log-*.txt` files on every poll.
fn check_wezterm_gui_sockets() -> Check {
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = PathBuf::from(&home).join(".local/share/wezterm");
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => {
            return Check {
                label: "wezterm gui sockets",
                status: Status::Ok,
                message: format!("no {} directory - WezTerm has never run here", dir.display()),
                hint: None,
            };
        }
    };

    let mut dead = Vec::new();
    let mut live_count = 0;
    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("gui-sock-") {
            continue;
        }
        if std::os::unix::net::UnixStream::connect(entry.path()).is_ok() {
            live_count += 1;
        } else {
            dead.push(name);
        }
    }

    let mux_running = Command::new("pgrep").args(["-f", "wezterm-mux-server"]).output()
        .is_ok_and(|o| o.status.success());

    if dead.is_empty() && !mux_running {
        return Check {
            label: "wezterm gui sockets",
            status: Status::Ok,
            message: format!("{live_count} live socket(s), no dead ones, no stray mux server"),
            hint: None,
        };
    }

    let mut parts = Vec::new();
    let mut hints = Vec::new();
    if !dead.is_empty() {
        parts.push(format!("{} dead socket file(s): {}", dead.len(), dead.join(", ")));
        hints.push(format!("safe to remove: rm {}/{{{}}}", dir.display(), dead.join(",")));
    }
    if mux_running {
        parts.push("a wezterm-mux-server process is running".to_string());
        hints.push("if unix_domains isn't configured in wezterm.lua, nothing talks to this - find and kill it (ps -o pid,lstart,command -ax | grep wezterm-mux-server)".to_string());
    }

    Check {
        label: "wezterm gui sockets",
        status: Status::Warn,
        message: parts.join("; "),
        hint: Some(hints.join(". ")),
    }
}

/// Cross-checks every `[monitors.*]` UUID in lanes.toml against what's
/// actually attached right now (`lanes-wm monitors`), the same staleness
/// that had lg-left pointing at a UUID matching no currently-attached
/// display earlier this project - caught then by accident, this catches
/// it proactively.
/// Which configured monitor handles' UUIDs don't match any currently-live
/// one - pulled out of `check_monitor_config` so the comparison itself is
/// testable without an actual `lanes-wm monitors` call. `live_uuids` is
/// expected pre-uppercased; handle UUIDs are compared case-insensitively.
fn stale_monitor_handles(
    monitors: &std::collections::HashMap<String, lanes::config::MonitorConfig>,
    live_uuids: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut stale: Vec<String> = monitors
        .iter()
        .filter_map(|(handle, mc)| {
            let uuid = mc.uuid.as_ref()?;
            (!live_uuids.contains(&uuid.to_uppercase())).then(|| format!("{handle} ({uuid})"))
        })
        .collect();
    stale.sort();
    stale
}

fn check_monitor_config(cfg: &lanes::config::Config) -> Check {
    let output = match Command::new("lanes-wm").args(["monitors", "--compact"]).output() {
        Ok(o) if o.status.success() => o,
        _ => {
            return Check {
                label: "monitor config",
                status: Status::Warn,
                message: "could not run `lanes-wm monitors` - skipping".to_string(),
                hint: None,
            };
        }
    };
    let Ok(live) = serde_json::from_slice::<Vec<serde_json::Value>>(&output.stdout) else {
        return Check {
            label: "monitor config",
            status: Status::Warn,
            message: "could not parse `lanes-wm monitors` output - skipping".to_string(),
            hint: None,
        };
    };
    let live_uuids: std::collections::HashSet<String> = live
        .iter()
        .filter_map(|m| m.get("uuid").and_then(|u| u.as_str()).map(|s| s.to_uppercase()))
        .collect();

    let stale = stale_monitor_handles(&cfg.monitors, &live_uuids);

    if stale.is_empty() {
        Check {
            label: "monitor config",
            status: Status::Ok,
            message: format!("{} monitor handle(s), all match a currently-attached display", cfg.monitors.len()),
            hint: None,
        }
    } else {
        Check {
            label: "monitor config",
            status: Status::Warn,
            message: format!("handle(s) not matching any currently-attached display: {}", stale.join(", ")),
            hint: Some("reconnect the display, or update the UUID in ~/.config/lanes/lanes.toml (see `lanes-wm monitors` for current UUIDs)".to_string()),
        }
    }
}

/// Boards' own data-root resolution order (BOARDS_DATA_DIR env var, then
/// `data_dir` in ~/.config/boards/config.yaml, then
/// ~/.local/share/bmiller/boards) - duplicated here deliberately rather
/// than linking against Boards, which is a separate Tauri app with no
/// library to depend on. Boards' own `src-tauri/src/data_dir.rs` is the
/// source of truth for this order; keep it in sync if that ever changes
/// (as it did when the default moved off ~/boards).
fn boards_data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("BOARDS_DATA_DIR") {
        return PathBuf::from(lanes::expand_tilde(&dir));
    }
    let config_path = PathBuf::from(lanes::expand_tilde("~/.config/boards/config.yaml"));
    if let Ok(content) = std::fs::read_to_string(&config_path) {
        for line in content.lines() {
            if let Some(val) = line.trim().strip_prefix("data_dir:") {
                let val = val.trim().trim_matches('"').trim_matches('\'');
                if !val.is_empty() {
                    return PathBuf::from(lanes::expand_tilde(val));
                }
            }
        }
    }
    PathBuf::from(lanes::expand_tilde("~/.local/share/bmiller/boards"))
}

/// Every Boards project name named by a `board.toml` facet across every
/// context - read directly from `cfg.boards_targets`, independent of
/// `cfg.lanes`, so a context with no `lane.toml` (e.g. `development`) is
/// still managed here. Ordered by `cfg.order` (contexts.toml), same
/// fallback (alphabetical-by-id, after any listed ones) `sort_by_order`
/// uses for lanes - `cfg.boards_targets` itself carries no order of its own.
fn boards_project_names(cfg: &lanes::config::Config) -> Vec<(&str, &str)> {
    let mut targets: Vec<&(String, String, lanes::model::Target)> = cfg.boards_targets.iter().collect();
    if let Some(order) = &cfg.order {
        targets.sort_by_key(|(id, ..)| order.iter().position(|o| o == id).unwrap_or(order.len()));
    } else {
        targets.sort_by(|(a, ..), (b, ..)| a.cmp(b));
    }
    targets
        .into_iter()
        .filter_map(|(_, name, t)| match &t.driver {
            lanes::model::TargetDriver::Boards { project } => Some((name.as_str(), project.as_str())),
            _ => None,
        })
        .collect()
}

/// Every Boards project name, in `boards_project_names`' own (context-)
/// order, deduplicated keeping the first occurrence - two contexts can
/// legitimately point at the same project (see e.g. sheetwork-dev/
/// sheetwork-sandbox sharing one board historically) and it should only
/// appear once, at whichever context names it first.
fn derived_boards_order(cfg: &lanes::config::Config) -> Vec<&str> {
    let mut seen = std::collections::HashSet::new();
    boards_project_names(cfg)
        .into_iter()
        .map(|(_, project)| project)
        .filter(|project| seen.insert(*project))
        .collect()
}

/// `<data_dir>/.order` - one Boards project name per line, in lane order.
/// Boards itself has no concept of "lane order" (deliberately - it's a
/// standalone, general-purpose kanban app, see PLAN-window-targets.md's
/// repo-boundary note); this file is lanes-cli pushing its own order to
/// Boards without Boards needing to understand lanes' config at all - same
/// pattern as ztabs maintaining zellij-tabs.toml as a derived cache rather
/// than Zellij/WezTerm knowing anything about lanes.
fn boards_order_path(data_dir: &std::path::Path) -> std::path::PathBuf {
    data_dir.join(".order")
}

/// Writes the order file if its content would actually change (so `--fix`
/// doesn't touch its mtime on every run for no reason). `None` if nothing
/// was written (already current, or the write failed).
fn write_boards_order(cfg: &lanes::config::Config, data_dir: &std::path::Path) -> Option<()> {
    let order = derived_boards_order(cfg);
    let content = order.join("\n");
    let path = boards_order_path(data_dir);
    if std::fs::read_to_string(&path).ok().as_deref() == Some(content.as_str()) {
        return None;
    }
    std::fs::create_dir_all(data_dir).ok()?;
    std::fs::write(&path, content).ok()
}

/// Flags when `.order` doesn't match what `contexts.toml`'s order currently
/// derives - e.g. right after reordering lanes or adding a new board.toml,
/// before the next `--fix`. Not itself a sign of anything broken in Boards;
/// it just means the sidebar is showing a stale order until `--fix` runs.
fn check_boards_order(cfg: &lanes::config::Config, data_dir: &std::path::Path) -> Check {
    let expected = derived_boards_order(cfg).join("\n");
    let order_path = boards_order_path(data_dir);
    let actual = std::fs::read_to_string(&order_path).unwrap_or_default();
    if actual == expected {
        Check {
            label: "boards order",
            status: Status::Ok,
            message: format!("{} matches the current lane order", order_path.display()),
            hint: None,
        }
    } else {
        Check {
            label: "boards order",
            status: Status::Warn,
            message: format!("{} is stale or missing", order_path.display()),
            hint: Some("run `lanes doctor --fix` to refresh it".to_string()),
        }
    }
}

fn has_boards_target(cfg: &lanes::config::Config) -> bool {
    !boards_project_names(cfg).is_empty()
}

/// Flags any context whose board.toml names a Boards project that doesn't
/// exist on disk yet - Boards is deliberately generic (any subfolder of its
/// data root with a board.yaml is a project), so this never teaches Boards
/// anything new, it just checks the folder's there. Not auto-fixed by a
/// plain `doctor` run - see `materialize_boards_projects` for `--fix`.
fn check_boards_projects(cfg: &lanes::config::Config, data_dir: &std::path::Path) -> Check {
    let missing: Vec<&str> = boards_project_names(cfg)
        .into_iter()
        .filter(|(_, project)| !data_dir.join(project).join("board.yaml").exists())
        .map(|(_, project)| project)
        .collect();

    if missing.is_empty() {
        Check {
            label: "boards projects",
            status: Status::Ok,
            message: "every context's board.toml project exists under the Boards data root".to_string(),
            hint: None,
        }
    } else {
        Check {
            label: "boards projects",
            status: Status::Warn,
            message: format!("project(s) missing a board.yaml in {}: {}", data_dir.display(), missing.join(", ")),
            hint: Some("run `lanes doctor --fix` to create them with default columns".to_string()),
        }
    }
}

/// Creates `<data_dir>/<project>/board.yaml` for any context's board.toml
/// project that doesn't exist yet, using the context's display name as the
/// title and a single `Inbox` column - the user's own convention: every
/// fresh board starts as just an inbox, with no presumed workflow stages
/// (no Backlog/Doing/Done baked in), and gets whatever columns it actually
/// needs added later by hand. Never touches a project that already exists -
/// once created, its content (columns, cards) is Boards' to own, not
/// lanes-cli's to overwrite on every `--fix`. Returns the project names
/// actually created.
fn materialize_boards_projects(cfg: &lanes::config::Config, data_dir: &std::path::Path) -> Vec<String> {
    let mut created = Vec::new();
    for (name, project) in boards_project_names(cfg) {
        let board_yaml = data_dir.join(project).join("board.yaml");
        if board_yaml.exists() {
            continue;
        }
        if let Some(parent) = board_yaml.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                continue;
            }
        }
        // Double-quoted YAML scalar, so a display name with a colon,
        // hash, or other flow-scalar-breaking character is still valid.
        let escaped_name = name.replace('\\', "\\\\").replace('"', "\\\"");
        let content = format!("title: \"{escaped_name}\"\ncolumns: [Inbox]\n");
        if std::fs::write(&board_yaml, content).is_ok() {
            created.push(project.to_string());
        }
    }
    created
}

/// Every external binary a configured target's driver actually needs
/// (`stree`, `code`) is on PATH. `code` in particular is opt-in per-machine
/// (VS Code's own "install shell command" step), easy to forget - and
/// unlike a missing `sqlite3`/`lanes-wm`, this failure is scoped to just
/// the one driver, so each missing binary is its own line rather than
/// failing the whole check.
fn check_target_binaries(cfg: &lanes::config::Config) -> Check {
    let mut needed: std::collections::HashSet<&'static str> = std::collections::HashSet::new();
    for target in cfg.lanes.iter().flat_map(|l| &l.targets) {
        match &target.driver {
            lanes::model::TargetDriver::Sourcetree { .. } => { needed.insert("stree"); }
            lanes::model::TargetDriver::Vscode { .. } => { needed.insert("code"); }
            _ => {}
        }
    }

    let missing: Vec<&str> = needed
        .into_iter()
        .filter(|bin| Command::new("which").arg(bin).output().is_ok_and(|o| !o.status.success()))
        .collect();

    if missing.is_empty() {
        Check {
            label: "target binaries",
            status: Status::Ok,
            message: "every binary referenced by a configured target driver is on PATH".to_string(),
            hint: None,
        }
    } else {
        Check {
            label: "target binaries",
            status: Status::Warn,
            message: format!("referenced by a target but not found on PATH: {}", missing.join(", ")),
            hint: Some("code: VS Code > Cmd+Shift+P > \"Shell Command: Install 'code' command in PATH\". stree: SourceTree > Install Command Line Tools".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lanes::config::Config;
    use lanes::model::Lane;
    use lanes::scope::ScopeElement;
    use std::collections::{HashMap, HashSet};

    /// Derives `boards_targets`/`context_ids` from the given lanes' own
    /// targets/ids - real `Config::load()` reads these independently of
    /// `lanes.targets` (see `context_board_targets`), but for a test config
    /// built straight from `Lane`s it's simplest to mirror whatever Boards
    /// target each lane already carries, same as before this distinction
    /// existed.
    fn test_config(lanes: Vec<Lane>) -> Config {
        let boards_targets = lanes.iter()
            .filter_map(|l| {
                l.targets.iter().find_map(|t| match &t.driver {
                    lanes::model::TargetDriver::Boards { .. } => Some((l.id.clone(), l.name.clone(), t.clone())),
                    _ => None,
                })
            })
            .collect();
        let context_ids = lanes.iter().map(|l| l.id.clone()).collect();
        Config { drivers: None, monitors: HashMap::new(), order: None, cycle_policy: lanes::model::CyclePolicy::default(), lanes, boards_targets, context_ids, shell_ignored_commands: Vec::new() }
    }

    fn lane(id: &str, name: &str, active: bool, session: &str) -> Lane {
        Lane {
            id: id.to_string(),
            name: name.to_string(),
            active,
            scope: vec![ScopeElement::zellij_session(session)],
            targets: vec![],
            layout_template: None,
        }
    }

    fn lane_with_targets(id: &str, targets: Vec<lanes::model::Target>) -> Lane {
        Lane { targets, ..lane(id, id, true, id) }
    }

    fn lane_with_layout_template(id: &str, layout_template: &str) -> Lane {
        Lane { layout_template: Some(layout_template.to_string()), ..lane(id, id, true, id) }
    }

    fn monitor(uuid: &str) -> lanes::config::MonitorConfig {
        lanes::config::MonitorConfig { uuid: Some(uuid.to_string()), name: None }
    }

    fn boards_target(project: &str) -> lanes::model::Target {
        lanes::model::Target {
            driver: lanes::model::TargetDriver::Boards { project: project.to_string() },
            monitor: None,
            position: None,
            position_solo: None,
            launch: false,
            raise: false,
            minimized: None,
            primary: false,
        }
    }

    /// A context's board.toml target is read for Boards management even
    /// when that context is never a lane at all (no lane.toml) - this is
    /// the actual scenario `development` represents.
    fn config_with_board_only_context(id: &str, project: &str, order: Vec<&str>) -> Config {
        Config {
            drivers: None,
            monitors: HashMap::new(),
            order: Some(order.into_iter().map(String::from).collect()),
            cycle_policy: lanes::model::CyclePolicy::default(),
            lanes: Vec::new(),
            boards_targets: vec![(id.to_string(), id.to_string(), boards_target(project))],
            context_ids: vec![id.to_string()],
            shell_ignored_commands: Vec::new(),
        }
    }

    #[test]
    fn boards_project_names_includes_a_board_only_context_with_no_lane_toml() {
        let cfg = config_with_board_only_context("development", "Development", vec!["development"]);
        assert_eq!(boards_project_names(&cfg), vec![("development", "Development")]);
        assert!(has_boards_target(&cfg));
    }

    #[test]
    fn has_boards_target_false_when_none_configured() {
        let cfg = test_config(vec![lane("infra", "Infra", true, "infra")]);
        assert!(!has_boards_target(&cfg));
    }

    #[test]
    fn boards_project_names_pairs_display_name_with_project() {
        let cfg = test_config(vec![lane_with_targets("infra", vec![boards_target("infra-board")])]);
        assert_eq!(boards_project_names(&cfg), vec![("infra", "infra-board")]);
    }

    fn boards_test_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lanes-doctor-test-boards-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn check_boards_projects_ok_when_board_yaml_already_exists() {
        let dir = boards_test_dir("check-ok");
        std::fs::create_dir_all(dir.join("infra-board")).unwrap();
        std::fs::write(dir.join("infra-board/board.yaml"), "title: \"Infra\"\n").unwrap();

        let cfg = test_config(vec![lane_with_targets("infra", vec![boards_target("infra-board")])]);
        let check = check_boards_projects(&cfg, &dir);
        assert!(matches!(check.status, Status::Ok));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_boards_projects_warns_on_a_missing_board_yaml() {
        let dir = boards_test_dir("check-warn");
        let cfg = test_config(vec![lane_with_targets("infra", vec![boards_target("infra-board")])]);
        let check = check_boards_projects(&cfg, &dir);
        assert!(matches!(check.status, Status::Warn));
        assert!(check.message.contains("infra-board"));
    }

    #[test]
    fn materialize_boards_projects_creates_a_missing_board_yaml() {
        let dir = boards_test_dir("materialize-new");
        let cfg = test_config(vec![lane_with_targets("infra", vec![boards_target("infra-board")])]);
        let created = materialize_boards_projects(&cfg, &dir);
        assert_eq!(created, vec!["infra-board".to_string()]);

        let content = std::fs::read_to_string(dir.join("infra-board/board.yaml")).unwrap();
        assert!(content.contains("title: \"infra\""));
        assert!(content.contains("columns: [Inbox]"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn materialize_boards_projects_never_overwrites_an_existing_board_yaml() {
        let dir = boards_test_dir("materialize-existing");
        std::fs::create_dir_all(dir.join("infra-board")).unwrap();
        std::fs::write(dir.join("infra-board/board.yaml"), "title: \"Custom\"\ncolumns: [A, B]\n").unwrap();

        let cfg = test_config(vec![lane_with_targets("infra", vec![boards_target("infra-board")])]);
        let created = materialize_boards_projects(&cfg, &dir);
        assert!(created.is_empty());

        let content = std::fs::read_to_string(dir.join("infra-board/board.yaml")).unwrap();
        assert!(content.contains("Custom"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn derived_boards_order_follows_lane_order_and_dedupes() {
        let cfg = test_config(vec![
            lane_with_targets("a", vec![boards_target("Shared")]),
            lane_with_targets("b", vec![boards_target("Only-B")]),
            lane_with_targets("c", vec![boards_target("Shared")]),
        ]);
        assert_eq!(derived_boards_order(&cfg), vec!["Shared", "Only-B"]);
    }

    #[test]
    fn write_boards_order_creates_the_order_file() {
        let dir = boards_test_dir("write-order-new");
        let cfg = test_config(vec![
            lane_with_targets("infra", vec![boards_target("Infra")]),
            lane_with_targets("lanes-dev", vec![boards_target("Lanes")]),
        ]);
        assert!(write_boards_order(&cfg, &dir).is_some());
        let content = std::fs::read_to_string(boards_order_path(&dir)).unwrap();
        assert_eq!(content, "Infra\nLanes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_boards_order_is_a_noop_when_already_current() {
        let dir = boards_test_dir("write-order-noop");
        let cfg = test_config(vec![lane_with_targets("infra", vec![boards_target("Infra")])]);
        assert!(write_boards_order(&cfg, &dir).is_some());
        assert!(write_boards_order(&cfg, &dir).is_none(), "second write with no change should be a no-op");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_boards_order_warns_when_missing_then_ok_after_fix() {
        let dir = boards_test_dir("check-order");
        let cfg = test_config(vec![lane_with_targets("infra", vec![boards_target("Infra")])]);
        assert!(matches!(check_boards_order(&cfg, &dir).status, Status::Warn));
        write_boards_order(&cfg, &dir);
        assert!(matches!(check_boards_order(&cfg, &dir).status, Status::Ok));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn orphaned_when_no_lane_matches_the_cached_session() {
        let cfg = test_config(vec![lane("infra", "Infra", true, "infra")]);
        let cached = vec![("sheetwork1".to_string(), 1)];
        let (orphaned, stale) = classify_tab_cache(&cached, &cfg, &HashSet::new());
        assert_eq!(orphaned, vec!["sheetwork1 (cached wezterm tab-id=1)".to_string()]);
        assert!(stale.is_empty());
    }

    #[test]
    fn stale_when_active_lanes_cached_id_is_not_a_live_tab() {
        let cfg = test_config(vec![lane("infra", "Infra", true, "infra")]);
        let cached = vec![("infra".to_string(), 4)];
        let live_ids: HashSet<u64> = [1, 2, 3].into_iter().collect();
        let (orphaned, stale) = classify_tab_cache(&cached, &cfg, &live_ids);
        assert!(orphaned.is_empty());
        assert_eq!(stale, vec!["infra (cached wezterm tab-id=4)".to_string()]);
    }

    #[test]
    fn not_stale_when_cached_id_matches_a_live_tab() {
        let cfg = test_config(vec![lane("infra", "Infra", true, "infra")]);
        let cached = vec![("infra".to_string(), 2)];
        let live_ids: HashSet<u64> = [2].into_iter().collect();
        let (orphaned, stale) = classify_tab_cache(&cached, &cfg, &live_ids);
        assert!(orphaned.is_empty());
        assert!(stale.is_empty());
    }

    #[test]
    fn inactive_lanes_stale_id_is_not_flagged() {
        // An inactive lane isn't expected to have a live tab anyway - a
        // leftover cached id for it isn't a bug worth surfacing.
        let cfg = test_config(vec![lane("job-hunting", "Job Hunting", false, "job-hunting")]);
        let cached = vec![("job-hunting".to_string(), 0)];
        let (orphaned, stale) = classify_tab_cache(&cached, &cfg, &HashSet::new());
        assert!(orphaned.is_empty());
        assert!(stale.is_empty());
    }

    #[test]
    fn kebab_case_accepts_plain_hyphenated_name() {
        assert!(is_kebab_case("sheetwork-planner"));
    }

    #[test]
    fn kebab_case_accepts_single_word() {
        assert!(is_kebab_case("infra"));
    }

    #[test]
    fn kebab_case_rejects_spaces() {
        assert!(!is_kebab_case("sheetwork planner"));
    }

    #[test]
    fn kebab_case_rejects_uppercase() {
        assert!(!is_kebab_case("Sheetwork-Planner"));
    }

    #[test]
    fn kebab_case_rejects_underscores() {
        assert!(!is_kebab_case("sheetwork_planner"));
    }

    #[test]
    fn kebab_case_rejects_leading_or_trailing_hyphen() {
        assert!(!is_kebab_case("-sheetwork"));
        assert!(!is_kebab_case("sheetwork-"));
    }

    #[test]
    fn kebab_case_rejects_consecutive_hyphens() {
        assert!(!is_kebab_case("sheetwork--planner"));
    }

    #[test]
    fn kebab_case_rejects_empty_string() {
        assert!(!is_kebab_case(""));
    }

    fn test_config_with_order(lanes: Vec<Lane>, order: Vec<&str>) -> Config {
        let context_ids = lanes.iter().map(|l| l.id.clone()).collect();
        Config {
            drivers: None,
            monitors: HashMap::new(),
            order: Some(order.into_iter().map(String::from).collect()),
            cycle_policy: lanes::model::CyclePolicy::default(),
            lanes,
            boards_targets: Vec::new(),
            context_ids,
            shell_ignored_commands: Vec::new(),
        }
    }

    #[test]
    fn lane_order_ok_when_none_configured() {
        let cfg = test_config(vec![lane("infra", "Infra", true, "infra")]);
        let check = check_lane_order(&cfg);
        assert!(matches!(check.status, Status::Ok));
    }

    #[test]
    fn lane_order_ok_when_it_covers_exactly_the_lanes() {
        let cfg = test_config_with_order(
            vec![lane("infra", "Infra", true, "infra"), lane("lanes-dev", "Lanes Dev", true, "lanes")],
            vec!["infra", "lanes-dev"],
        );
        let check = check_lane_order(&cfg);
        assert!(matches!(check.status, Status::Ok));
    }

    #[test]
    fn lane_order_warns_on_unknown_id() {
        let cfg = test_config_with_order(
            vec![lane("infra", "Infra", true, "infra")],
            vec!["infra", "ghost"],
        );
        let check = check_lane_order(&cfg);
        assert!(matches!(check.status, Status::Warn));
        assert!(check.message.contains("ghost"));
    }

    #[test]
    fn restrict_to_common_keeps_first_list_order_dropping_entries_absent_from_second() {
        let order = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let other = vec!["c".to_string(), "a".to_string()];
        assert_eq!(restrict_to_common(&order, &other), vec!["a".to_string(), "c".to_string()]);
    }

    #[test]
    fn restrict_to_common_detects_a_real_order_disagreement() {
        // lanes.toml says sheetwork-planner, infra; zellij-tabs.toml says infra, sheetwork-planner.
        let lane_order = vec!["sheetwork-planner".to_string(), "infra".to_string()];
        let ztabs_order = vec!["infra".to_string(), "sheetwork-planner".to_string()];
        assert_ne!(
            restrict_to_common(&lane_order, &ztabs_order),
            restrict_to_common(&ztabs_order, &lane_order)
        );
    }

    #[test]
    fn restrict_to_common_agrees_when_orders_match_modulo_extra_entries() {
        // lanes.toml has an extra lane (spinner) ztabs doesn't manage at all.
        let lane_order = vec!["sheetwork-planner".to_string(), "infra".to_string(), "spinner".to_string()];
        let ztabs_order = vec!["sheetwork-planner".to_string(), "infra".to_string()];
        assert_eq!(
            restrict_to_common(&lane_order, &ztabs_order),
            restrict_to_common(&ztabs_order, &lane_order)
        );
    }

    #[test]
    fn check_primary_targets_ok_when_none_are_marked() {
        let cfg = test_config(vec![lane_with_targets("infra", vec![boards_target("Dev")])]);
        let check = check_primary_targets(&cfg);
        assert!(matches!(check.status, Status::Ok));
    }

    #[test]
    fn check_primary_targets_ok_with_exactly_one_marked() {
        let mut target = boards_target("Dev");
        target.primary = true;
        let cfg = test_config(vec![lane_with_targets("infra", vec![target])]);
        let check = check_primary_targets(&cfg);
        assert!(matches!(check.status, Status::Ok));
    }

    #[test]
    fn check_primary_targets_fails_when_a_lane_has_two() {
        let mut a = boards_target("Dev");
        a.primary = true;
        let mut b = boards_target("Japanese");
        b.primary = true;
        let cfg = test_config(vec![lane_with_targets("infra", vec![a, b])]);
        let check = check_primary_targets(&cfg);
        assert!(matches!(check.status, Status::Fail));
        assert!(check.message.contains("infra"));
    }

    #[test]
    fn check_layout_templates_ok_when_no_lane_sets_one() {
        let cfg = test_config(vec![lane("infra", "Infra", true, "infra")]);
        let check = check_layout_templates(&cfg, std::path::Path::new("/nonexistent"));
        assert!(matches!(check.status, Status::Ok));
    }

    #[test]
    fn check_layout_templates_ok_when_the_referenced_file_exists() {
        let dir = std::env::temp_dir().join(format!(
            "lanes-doctor-test-layout-templates-ok-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("dev.toml"), "").unwrap();

        let cfg = test_config(vec![lane_with_layout_template("lanes-dev", "dev")]);
        let check = check_layout_templates(&cfg, &dir);
        assert!(matches!(check.status, Status::Ok));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn check_layout_templates_fails_when_the_referenced_file_is_missing() {
        let cfg = test_config(vec![lane_with_layout_template("lanes-dev", "ghost")]);
        let check = check_layout_templates(&cfg, std::path::Path::new("/nonexistent"));
        assert!(matches!(check.status, Status::Fail));
        assert!(check.message.contains("lanes-dev"));
        assert!(check.message.contains("ghost"));
    }

    #[test]
    fn lane_order_fails_on_missing_lane() {
        let cfg = test_config_with_order(
            vec![lane("infra", "Infra", true, "infra"), lane("lanes-dev", "Lanes Dev", true, "lanes")],
            vec!["infra"],
        );
        let check = check_lane_order(&cfg);
        assert!(matches!(check.status, Status::Fail));
        assert!(check.message.contains("lanes-dev"));
    }

    #[test]
    fn lane_order_fails_when_both_unknown_and_missing() {
        let cfg = test_config_with_order(
            vec![lane("infra", "Infra", true, "infra"), lane("lanes-dev", "Lanes Dev", true, "lanes")],
            vec!["infra", "ghost"],
        );
        let check = check_lane_order(&cfg);
        assert!(matches!(check.status, Status::Fail));
        assert!(check.message.contains("ghost"));
        assert!(check.message.contains("lanes-dev"));
    }

    #[test]
    fn parse_ls_remote_symref_reads_the_branch_off_the_symref_line() {
        let output = "ref: refs/heads/main\tHEAD\nabc123def456\tHEAD\n";
        assert_eq!(parse_ls_remote_symref(output), Some("main".to_string()));
    }

    #[test]
    fn parse_ls_remote_symref_handles_a_non_default_branch_name() {
        let output = "ref: refs/heads/data-model\tHEAD\nabc123def456\tHEAD\n";
        assert_eq!(parse_ls_remote_symref(output), Some("data-model".to_string()));
    }

    #[test]
    fn parse_ls_remote_symref_is_none_without_a_symref_line() {
        // e.g. ls-remote output for a remote with no symbolic HEAD ref advertised.
        let output = "abc123def456\tHEAD\n";
        assert_eq!(parse_ls_remote_symref(output), None);
    }

    #[test]
    fn parse_ls_remote_symref_is_none_on_empty_output() {
        assert_eq!(parse_ls_remote_symref(""), None);
    }

    #[test]
    fn stale_monitor_handles_flags_uuid_matching_no_live_display() {
        let monitors: HashMap<String, lanes::config::MonitorConfig> =
            [("lg-left".to_string(), monitor("AAAA"))].into_iter().collect();
        let live: HashSet<String> = ["BBBB".to_string()].into_iter().collect();
        assert_eq!(stale_monitor_handles(&monitors, &live), vec!["lg-left (AAAA)".to_string()]);
    }

    #[test]
    fn stale_monitor_handles_empty_when_uuid_matches_case_insensitively() {
        let monitors: HashMap<String, lanes::config::MonitorConfig> =
            [("main".to_string(), monitor("aaaa"))].into_iter().collect();
        let live: HashSet<String> = ["AAAA".to_string()].into_iter().collect();
        assert!(stale_monitor_handles(&monitors, &live).is_empty());
    }

    #[test]
    fn stale_monitor_handles_ignores_a_handle_with_no_uuid() {
        let monitors: HashMap<String, lanes::config::MonitorConfig> =
            [("main".to_string(), lanes::config::MonitorConfig { uuid: None, name: None })]
                .into_iter()
                .collect();
        assert!(stale_monitor_handles(&monitors, &HashSet::new()).is_empty());
    }

}
