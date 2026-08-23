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

pub fn run() {
    let cfg = lanes::config::Config::load();

    let mut checks = vec![
        check_lanes_registry(),
        check_logs(),
        check_wezterm_tab_cache(&cfg),
        check_session_naming(&cfg),
        check_lane_order(&cfg),
        check_lane_order_vs_ztabs(&cfg),
    ];

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
/// `session_is_live` (and thus everything gather_lanes() shows) already
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

/// Two distinct ways state.kdl's wezterm-tab-id cache can go wrong, since
/// `lanes` trusts this cache rather than ever polling WezTerm itself (see
/// lib.rs::lane_session_missing) - whichever tool owns a tab's lifecycle is
/// responsible for keeping the cache honest, and neither failure mode is
/// otherwise visible without a one-off check like this:
///
/// - orphaned: a cached session that doesn't match any lane at all anymore
///   (e.g. a renamed/removed lane's leftover entry) - detectable from config
///   alone.
/// - stale: a cached id for a lane that's active and tracked, but the id
///   doesn't match any currently-open WezTerm tab - needs one live
///   `wezterm cli list` round trip. Fine here specifically: doctor is a
///   manual, on-demand command, not the 10s polling path gather_lanes()
///   deliberately avoids hitting WezTerm from.
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
    cmd.args(["cli", "list", "--format", "json"]);
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

fn check_lane_order(cfg: &lanes::config::Config) -> Check {
    let order = match &cfg.order {
        None => {
            return Check {
                label: "lane order",
                status: Status::Ok,
                message: "no `order` configured in lanes.toml (falling back to alphabetical)".to_string(),
                hint: None,
            };
        }
        Some(order) => order,
    };

    let unknown: Vec<&str> = order.iter()
        .map(|id| id.as_str())
        .filter(|id| !cfg.lanes.iter().any(|l| &l.id == id))
        .collect();
    let missing: Vec<&str> = cfg.lanes.iter()
        .map(|l| l.id.as_str())
        .filter(|id| !order.iter().any(|o| o == id))
        .collect();

    if unknown.is_empty() && missing.is_empty() {
        Check {
            label: "lane order",
            status: Status::Ok,
            message: "lanes.toml `order` covers exactly the configured lanes".to_string(),
            hint: None,
        }
    } else {
        let mut parts = Vec::new();
        if !unknown.is_empty() {
            parts.push(format!("order lists unknown lane id(s): {}", unknown.join(", ")));
        }
        if !missing.is_empty() {
            parts.push(format!("lane(s) missing from order (sorted alphabetically last): {}", missing.join(", ")));
        }
        Check {
            label: "lane order",
            status: Status::Warn,
            message: parts.join("; "),
            hint: Some(
                "not auto-fixed - edit `order` in ~/.config/lanes.toml by hand to match \
                 the current set of lane ids.".to_string()
            ),
        }
    }
}

/// `ztabs`' own session order, straight from its config file - not a live
/// query of anything. This is the other tool's core state, same class of
/// thing as lanes.toml's `order`, just owned by a different tool.
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
/// not cover exactly the same set of sessions (e.g. lanes.toml has a lane
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
            message: "lanes.toml `order` agrees with zellij-tabs.toml's session order".to_string(),
            hint: None,
        }
    } else {
        Check {
            label: "lane order vs ztabs",
            status: Status::Warn,
            message: format!(
                "lanes.toml order gives {}, but zellij-tabs.toml gives {}",
                lanes_side.join(", "),
                ztabs_side.join(", ")
            ),
            hint: Some(
                "not auto-fixed - edit `order` in ~/.config/lanes.toml or the tab order in \
                 ~/.config/infra/zellij-tabs.toml so the two agree."
                    .to_string(),
            ),
        }
    }
}

fn check_lanes_registry() -> Check {
    let dir = lanes::config::config_dir();

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
                .filter(|e| {
                    let name = e.file_name();
                    let s = name.to_string_lossy();
                    s.ends_with(".toml") && s != "config.toml"
                })
                .count();
            Check {
                label: "lanes config",
                status: Status::Ok,
                message: format!("{} lane(s) defined in {}", count, dir.display()),
                hint: None,
            }
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

    fn test_config(lanes: Vec<Lane>) -> Config {
        Config { drivers: None, monitors: HashMap::new(), order: None, lanes }
    }

    fn lane(id: &str, name: &str, active: bool, session: &str) -> Lane {
        Lane {
            id: id.to_string(),
            name: name.to_string(),
            active,
            scope: vec![ScopeElement::zellij_session(session)],
            windows: vec![],
        }
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
        Config {
            drivers: None,
            monitors: HashMap::new(),
            order: Some(order.into_iter().map(String::from).collect()),
            lanes,
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
    fn lane_order_warns_on_missing_lane() {
        let cfg = test_config_with_order(
            vec![lane("infra", "Infra", true, "infra"), lane("lanes-dev", "Lanes Dev", true, "lanes")],
            vec!["infra"],
        );
        let check = check_lane_order(&cfg);
        assert!(matches!(check.status, Status::Warn));
        assert!(check.message.contains("lanes-dev"));
    }
}
