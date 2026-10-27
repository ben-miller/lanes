//! Lanes' facts for lensd (see ~/src/dev-infra/PLAN-lensd-facts.md): which
//! lanes exist (from the contexts in `~/.config/lanes/contexts/<id>/`) and
//! the long-running commands Lanes' shell hook records
//! (`~/.local/state/lanes/shell/`). Lanes Switch publishes them over its
//! `appctl` socket while it runs; lensd sees them prefixed, as `lanes.lane`
//! and so on. This code moved here from lensd's `lanes` and `shell` sources,
//! so the knowledge of these formats lives with Lanes.

use std::path::{Path, PathBuf};

use appctl::facts::{FactRow, FactValue};
use serde::Deserialize;

/// Every relation Lanes publishes, with its fields, for `Config::facts`.
pub const RELATIONS: &[(&str, &[&str])] = &[
    ("lane", &["lane"]),
    ("lane_name", &["lane", "name"]),
    ("lane_active", &["lane"]),
    // The lane being worked in now (state.kdl's focused-lane): at most one
    // row. Apps follow it (tauri_plugin_appctl's lane following: Console
    // shows that lane's repos).
    ("lane_focused", &["lane"]),
    ("lane_zellij", &["lane", "session"]),
    ("lane_repo", &["lane", "repo"]),
    ("lane_repo_path", &["lane", "path"]),
    ("lane_boards_project", &["lane", "project"]),
    ("shell_command", &["session", "pane", "state"]),
    ("shell_command_cmd", &["session", "pane", "cmd"]),
    ("shell_command_started", &["session", "pane", "at"]),
    ("shell_command_ended", &["session", "pane", "at"]),
    ("shell_command_exit", &["session", "pane", "code"]),
];

fn row(relation: &str, values: Vec<FactValue>) -> FactRow {
    FactRow { relation: relation.to_string(), values }
}

/// Publishes everything true now: each relation replaced with its current
/// rows (subscribers get only what changed). Mistakes are logged by appctl.
pub fn publish(facts: &appctl::facts::Facts) {
    let mut rows = scan_contexts(&contexts_dir());
    rows.extend(scan_shell(&shell_dir()));
    rows.extend(focused(crate::state::read_focused_lane()));
    for (relation, _) in RELATIONS {
        let mine = rows.iter().filter(|r| r.relation == *relation).map(|r| r.values.clone());
        let _ = facts.replace(relation, mine);
    }
}

/// The `lane_focused` row: the focused lane, if one is recorded.
fn focused(lane: Option<String>) -> Vec<FactRow> {
    lane.into_iter().filter(|l| !l.is_empty()).map(|l| row("lane_focused", vec![FactValue::from(l)])).collect()
}

pub fn contexts_dir() -> PathBuf {
    crate::config::config_dir().join("contexts")
}

pub fn shell_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local/state/lanes/shell")
}

// --- Contexts -------------------------------------------------------------

#[derive(Deserialize, Default)]
struct LaneToml {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    active: Option<bool>,
    /// `false`: the lane is disabled, as if it didn't exist (as everywhere
    /// else in Lanes, `load_contexts`): it publishes nothing.
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Deserialize, Default)]
struct TabToml {
    #[serde(default)]
    scope: Vec<Scope>,
}

#[derive(Deserialize)]
struct Scope {
    kind: String,
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    path: Option<String>,
}

#[derive(Deserialize, Default)]
struct BoardToml {
    #[serde(default)]
    driver: Option<String>,
    #[serde(default)]
    project: Option<String>,
}

fn read_facet<T: serde::de::DeserializeOwned + Default>(path: &Path) -> T {
    let Ok(text) = std::fs::read_to_string(path) else { return T::default() };
    toml::from_str(&text).unwrap_or_default()
}

/// The lane facts for every context directory with a `lane.toml`.
pub fn scan_contexts(dir: &Path) -> Vec<FactRow> {
    let mut facts = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return facts };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().into_owned();
        if id.contains(char::is_whitespace) {
            continue;
        }
        // Presence matters: a context with no lane.toml isn't a lane.
        if !path.join("lane.toml").is_file() {
            continue;
        }
        let lane: LaneToml = read_facet(&path.join("lane.toml"));
        if lane.enabled == Some(false) {
            continue;
        }
        let tab: TabToml = read_facet(&path.join("tab.toml"));
        let board: BoardToml = read_facet(&path.join("board.toml"));

        facts.push(row("lane", vec![FactValue::from(id.clone())]));
        if let (Some(driver), Some(project)) = (board.driver.as_deref(), board.project) {
            if driver == "boards" {
                facts.push(row("lane_boards_project", vec![FactValue::from(id.clone()), FactValue::from(project)]));
            }
        }
        if let Some(name) = lane.name {
            facts.push(row("lane_name", vec![FactValue::from(id.clone()), FactValue::from(name)]));
        }
        if lane.active.unwrap_or(true) {
            facts.push(row("lane_active", vec![FactValue::from(id.clone())]));
        }
        for scope in tab.scope {
            match (scope.kind.as_str(), scope.session, scope.path) {
                ("zellij_session", Some(session), _) => {
                    facts.push(row("lane_zellij", vec![FactValue::from(id.clone()), FactValue::from(session)]));
                }
                ("repo", _, Some(path)) => {
                    let resolved = resolve_path(&path);
                    // Basename, to join git's repo names.
                    let name = Path::new(path.trim_end_matches('/'))
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or(path);
                    facts.push(row("lane_repo", vec![FactValue::from(id.clone()), FactValue::from(name)]));
                    facts.push(row("lane_repo_path", vec![FactValue::from(id.clone()), FactValue::from(resolved)]));
                }
                _ => {}
            }
        }
    }
    facts
}

/// Expands a leading `~/` and resolves symlinks (falling back to the expanded
/// path if it doesn't exist), so it compares equal to git's `repo_path`.
pub fn resolve_path(path: &str) -> String {
    let expanded = match path.strip_prefix("~/") {
        Some(rest) => format!("{}/{}", std::env::var("HOME").unwrap_or_default(), rest),
        None => path.to_string(),
    };
    let trimmed = expanded.trim_end_matches('/');
    std::fs::canonicalize(trimmed).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| trimmed.to_string())
}

// --- Shell command records --------------------------------------------------

/// The record format version this reads (see drivers/shell.rs).
pub const SUPPORTED_V: u32 = 1;

#[derive(Deserialize)]
struct Record {
    #[serde(default)]
    v: u32,
    session: String,
    pane: u32,
    #[serde(default)]
    cmd: Option<String>,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    ended_at: Option<String>,
    state: String,
    #[serde(default)]
    exit_code: Option<i32>,
}

/// The shell-command facts for every record; malformed ones are skipped.
pub fn scan_shell(dir: &Path) -> Vec<FactRow> {
    let mut facts = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return facts };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let Ok(rec) = serde_json::from_str::<Record>(&text) else { continue };
        if rec.v != SUPPORTED_V {
            continue;
        }
        let with = |extra: FactValue| vec![FactValue::from(rec.session.clone()), FactValue::Int(rec.pane as i64), extra];
        facts.push(row("shell_command", with(FactValue::from(rec.state.clone()))));
        if let Some(cmd) = &rec.cmd {
            facts.push(row("shell_command_cmd", with(FactValue::from(cmd.clone()))));
        }
        if let Some(at) = &rec.started_at {
            facts.push(row("shell_command_started", with(FactValue::from(at.clone()))));
        }
        if let Some(at) = &rec.ended_at {
            facts.push(row("shell_command_ended", with(FactValue::from(at.clone()))));
        }
        if let Some(code) = rec.exit_code {
            facts.push(row("shell_command_exit", with(FactValue::Int(code as i64))));
        }
    }
    facts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_context(dir: &Path, id: &str, lane_toml: Option<&str>, tab_toml: Option<&str>) {
        write_context_with_board(dir, id, lane_toml, tab_toml, None)
    }

    fn write_context_with_board(
        dir: &Path,
        id: &str,
        lane_toml: Option<&str>,
        tab_toml: Option<&str>,
        board_toml: Option<&str>,
    ) {
        let ctx = dir.join(id);
        std::fs::create_dir_all(&ctx).unwrap();
        if let Some(content) = lane_toml {
            std::fs::write(ctx.join("lane.toml"), content).unwrap();
        }
        if let Some(content) = tab_toml {
            std::fs::write(ctx.join("tab.toml"), content).unwrap();
        }
        if let Some(content) = board_toml {
            std::fs::write(ctx.join("board.toml"), content).unwrap();
        }
    }

    #[test]
    fn a_disabled_lane_publishes_nothing() {
        let dir = std::env::temp_dir().join(format!("lanes-facts-disabled-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (id, toml) in [("on", "name = \"On\"\n"), ("off", "name = \"Off\"\nenabled = false\n")] {
            std::fs::create_dir_all(dir.join(id)).unwrap();
            std::fs::write(dir.join(id).join("lane.toml"), toml).unwrap();
        }
        let rows = scan_contexts(&dir);
        assert!(rows.iter().any(|r| r.relation == "lane" && r.values == vec![FactValue::from("on")]));
        assert!(!rows.iter().any(|r| r.values.first() == Some(&FactValue::from("off"))), "{rows:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_focused_lane_is_one_row_or_none() {
        assert_eq!(focused(Some("dev-infra".into())), vec![row("lane_focused", vec![FactValue::from("dev-infra")])]);
        assert!(focused(None).is_empty());
        assert!(focused(Some(String::new())).is_empty());
    }

    #[test]
    fn context_dir_becomes_lane_zellij_and_repo_facts() {
        let tmp = tempfile::tempdir().unwrap();
        write_context(
            tmp.path(),
            "lanes-dev",
            Some("name = \"Lanes Dev\"\n"),
            Some(
                r#"
[[scope]]
kind = "zellij_session"
session = "lanes"

[[scope]]
kind = "repo"
path = "~/src/projects/lanes"
"#,
            ),
        );

        let facts = scan_contexts(tmp.path());
        let id = || FactValue::from("lanes-dev");
        assert!(facts.contains(&row("lane", vec![id()])));
        assert!(facts.contains(&row("lane_name", vec![id(), FactValue::from("Lanes Dev")])));
        assert!(facts.contains(&row("lane_active", vec![id()])), "active defaults to true");
        assert!(facts.contains(&row("lane_zellij", vec![id(), FactValue::from("lanes")])));
        assert!(facts.contains(&row("lane_repo", vec![id(), FactValue::from("lanes")])));
        assert!(facts.contains(&row("lane_repo_path", vec![id(), FactValue::from(resolve_path("~/src/projects/lanes"))])));
    }

    #[test]
    fn inactive_lane_has_no_lane_active_fact() {
        let tmp = tempfile::tempdir().unwrap();
        write_context(tmp.path(), "x", Some("active = false\n"), None);
        let facts = scan_contexts(tmp.path());
        assert!(facts.contains(&row("lane", vec![FactValue::from("x")])));
        assert!(!facts.iter().any(|f| f.relation == "lane_active"));
    }

    #[test]
    fn context_with_no_lane_toml_yields_no_lane_facts_at_all() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("placeholder")).unwrap();
        let facts = scan_contexts(tmp.path());
        // No lane.toml at all -> not a lane, period - not even the bare
        // `lane` fact. Mirrors lanes-cli's own config.rs fix: a context
        // only opts into being a real lane by having a lane.toml, even an
        // empty one.
        assert!(!facts.iter().any(|f| f.relation == "lane"));
        assert!(!facts.iter().any(|f| f.relation == "lane_active"));
        assert!(!facts.iter().any(|f| f.relation == "lane_name"));
    }

    #[test]
    fn context_with_an_empty_lane_toml_is_still_active() {
        // Distinct from the placeholder case above: the file's mere
        // *presence* is what opts a context in, regardless of content.
        let tmp = tempfile::tempdir().unwrap();
        write_context(tmp.path(), "bare", Some(""), None);
        let facts = scan_contexts(tmp.path());
        assert!(facts.contains(&row("lane_active", vec![FactValue::from("bare")])));
    }

    #[test]
    fn a_plain_file_alongside_context_dirs_is_not_a_lane() {
        let tmp = tempfile::tempdir().unwrap();
        write_context(tmp.path(), "infra", Some("name = \"Infra\"\n"), None);
        std::fs::write(tmp.path().join("stray.toml"), "not a context\n").unwrap();
        let facts = scan_contexts(tmp.path());
        assert_eq!(facts.iter().filter(|f| f.relation == "lane").count(), 1);
    }

    #[test]
    fn board_toml_with_boards_driver_becomes_lane_boards_project() {
        let tmp = tempfile::tempdir().unwrap();
        write_context_with_board(
            tmp.path(),
            "boards-dev",
            Some("name = \"Boards Dev\"\n"),
            None,
            Some("driver = \"boards\"\nproject = \"Boards\"\nmonitor = \"main\"\nposition = \"full\"\n"),
        );
        let facts = scan_contexts(tmp.path());
        assert!(facts.contains(&row(
            "lane_boards_project",
            vec![FactValue::from("boards-dev"), FactValue::from("Boards")]
        )));
    }

    #[test]
    fn board_toml_with_a_non_boards_driver_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        write_context_with_board(
            tmp.path(),
            "x",
            Some("name = \"X\"\n"),
            None,
            Some("driver = \"app\"\nname = \"Whatever\"\n"),
        );
        let facts = scan_contexts(tmp.path());
        assert!(!facts.iter().any(|f| f.relation == "lane_boards_project"));
    }

    #[test]
    fn no_board_toml_means_no_lane_boards_project_fact() {
        let tmp = tempfile::tempdir().unwrap();
        write_context(tmp.path(), "x", Some("name = \"X\"\n"), None);
        let facts = scan_contexts(tmp.path());
        assert!(!facts.iter().any(|f| f.relation == "lane_boards_project"));
    }

    #[test]
    fn malformed_lane_toml_falls_back_to_defaults_instead_of_dropping_the_context() {
        let tmp = tempfile::tempdir().unwrap();
        write_context(tmp.path(), "broken", Some("this is not valid toml {{{"), None);
        let facts = scan_contexts(tmp.path());
        assert!(facts.contains(&row("lane", vec![FactValue::from("broken")])));
        assert!(facts.contains(&row("lane_active", vec![FactValue::from("broken")])));
    }

    // --- shell ---

    #[test]
    fn done_record_becomes_state_cmd_started_and_exit_facts() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("lanes--3.json"),
            r#"{"v":1,"shell":"fish","session":"lanes","pane":3,"cmd":"cargo test",
                "started_at":"2026-09-04T10:00:00Z","ended_at":"2026-09-04T10:00:07Z","state":"failed","exit_code":101}"#,
        )
        .unwrap();
        let facts = scan_shell(tmp.path());
        let k = |extra: FactValue| vec![FactValue::from("lanes"), FactValue::Int(3), extra];
        assert!(facts.contains(&row("shell_command", k(FactValue::from("failed")))));
        assert!(facts.contains(&row("shell_command_cmd", k(FactValue::from("cargo test")))));
        assert!(facts.contains(&row("shell_command_started", k(FactValue::from("2026-09-04T10:00:00Z")))));
        assert!(facts.contains(&row("shell_command_ended", k(FactValue::from("2026-09-04T10:00:07Z")))));
        assert!(facts.contains(&row("shell_command_exit", k(FactValue::Int(101)))));
    }

    #[test]
    fn wrong_version_malformed_and_non_json_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.json"), r#"{"v":2,"session":"x","pane":1,"state":"done"}"#).unwrap();
        std::fs::write(tmp.path().join("b.json"), r#"{"v":1,"pane":1,"state":"done"}"#).unwrap();
        std::fs::write(tmp.path().join("c.json"), "garbage").unwrap();
        std::fs::write(tmp.path().join("d.txt"), r#"{"v":1,"session":"x","pane":1,"state":"done"}"#).unwrap();
        assert!(scan_shell(tmp.path()).is_empty());
    }

    #[test]
    fn missing_dir_is_empty() {
        assert!(scan_shell(Path::new("/definitely/not/here")).is_empty());
    }
}
