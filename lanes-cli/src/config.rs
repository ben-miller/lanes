use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::model::{CyclePolicy, Lane, Target, TargetDriver};
use crate::scope::ScopeElement;

#[derive(Clone)]
pub struct MonitorConfig {
    pub uuid: Option<String>,
    pub name: Option<String>,
}

pub struct Config {
    /// Drivers to run. None means all drivers.
    pub drivers: Option<Vec<String>>,
    /// Monitor handle -> config, from lanes.toml [monitors.*].
    pub monitors: HashMap<String, MonitorConfig>,
    /// Lane display order, from lanes.toml `order` (list of lane ids). Lanes
    /// not listed fall back to alphabetical-by-id, after any listed ones.
    pub order: Option<Vec<String>>,
    /// Which candidate list `lanes cycle` steps through - see
    /// `CyclePolicy`'s own doc comment.
    pub cycle_policy: CyclePolicy,
    /// Discovered lane definitions.
    pub lanes: Vec<Lane>,
}

impl Config {
    pub fn load() -> Self {
        let (drivers, monitors, order, cycle_policy) = load_global_config();
        // Top-level layout.toml supplies the defaults every context's own
        // layout.toml cascades on top of (merge_targets) - same file name
        // at both levels, deliberately: one is "defaults", the other is
        // "overrides" of the same thing, not two different concepts.
        let default_targets: LayoutToml = read_facet(&config_dir().join("layout.toml"));
        let mut lanes = load_lanes(&config_dir(), &default_targets.targets);
        if let Some(order) = &order {
            sort_by_order(&mut lanes, order);
        }
        Self { drivers, monitors, order, cycle_policy, lanes }
    }

    pub fn monitor_uuid(&self, handle: &str) -> Option<&str> {
        self.monitors.get(handle)?.uuid.as_deref()
    }

    pub fn driver_enabled(&self, name: &str) -> bool {
        match &self.drivers {
            None => true,
            Some(list) => list.iter().any(|d| d == name),
        }
    }

    /// Returns a map of zellij session name -> lane display name,
    /// derived from Terminal facets across all lanes.
    pub fn zellij_lane_names(&self) -> HashMap<String, String> {
        self.lanes
            .iter()
            .filter_map(|lane| {
                lane.terminal_session()
                    .map(|s| (s.to_string(), lane.display_name().to_string()))
            })
            .collect()
    }

    /// The lane whose Terminal facet session matches the given Zellij session
    /// name, if any.
    pub fn lane_for_session(&self, session: &str) -> Option<&Lane> {
        self.lanes.iter().find(|l| l.terminal_session() == Some(session))
    }

    /// Context ids present as directories but absent from `order`. Doesn't
    /// break anything at runtime (`sort_by_order`'s fallback still places
    /// them, alphabetically, after any listed ones) but is the registration
    /// gap `lanes doctor` and switch-ui's startup check both treat as fatal -
    /// see PLAN-context-model.md point 7.
    pub fn unregistered_contexts(&self) -> Vec<&str> {
        let Some(order) = &self.order else {
            return Vec::new();
        };
        self.lanes
            .iter()
            .map(|l| l.id.as_str())
            .filter(|id| !order.iter().any(|o| o == id))
            .collect()
    }
}

/// Flip a context's `active` flag in its `lane.toml` facet, in place. Uses
/// toml_edit rather than the parse-only `toml` crate so the rest of the
/// file (formatting, key order, any future comments) is left untouched -
/// these files are hand-maintained. `lane.toml` is an optional facet (see
/// `load_lanes`), so a context that's never had one yet gets a fresh file
/// containing just this one key, not an error.
pub fn set_lane_active(lane_id: &str, active: bool) -> std::io::Result<()> {
    let path = config_dir().join(lane_id).join("lane.toml");
    let mut doc = match std::fs::read_to_string(&path) {
        Ok(text) => text.parse::<toml_edit::DocumentMut>().map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
        })?,
        Err(_) => toml_edit::DocumentMut::new(),
    };
    doc["active"] = toml_edit::value(active);
    // Write-then-rename: the Lanes Switch fs-watcher reads this dir the
    // instant it sees a change, and a torn read makes toml parsing fail,
    // which drops the lane from that refresh's snapshot entirely (see
    // load_lanes) - the toggle appears to flicker or not stick. rename(2)
    // is atomic on one filesystem. Same fix as state::atomic_write.
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string())?;
    std::fs::rename(&tmp, &path)
}

impl Default for Config {
    fn default() -> Self {
        Self {
            drivers: None,
            monitors: HashMap::new(),
            order: None,
            cycle_policy: CyclePolicy::default(),
            lanes: Vec::new(),
        }
    }
}

/// Reorders `lanes` in place: lanes whose id appears in `order` come first,
/// in `order`'s sequence; any lanes not listed keep their existing
/// (alphabetical-by-id) relative order and are appended after.
fn sort_by_order(lanes: &mut [Lane], order: &[String]) {
    lanes.sort_by_key(|lane| {
        order.iter().position(|id| id == &lane.id).unwrap_or(order.len())
    });
}

pub fn config_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".config").join("lanes")
}

// --- Deserialization helpers ---

#[derive(Deserialize)]
struct GlobalConfig {
    #[serde(default)]
    drivers: Option<Vec<String>>,
    #[serde(default)]
    monitors: HashMap<String, MonitorConfigRaw>,
    #[serde(default)]
    order: Option<Vec<String>>,
    #[serde(default)]
    cycle_policy: CyclePolicy,
}

#[derive(Deserialize)]
struct MonitorConfigRaw {
    uuid: Option<String>,
    name: Option<String>,
}

/// `<context>/lane.toml` - the switcher-facing facet. Entirely optional,
/// same as every facet (see `load_lanes`): a context dir with no
/// `lane.toml` still exists (and still gets a Zellij session / Boards
/// project if `tab.toml`/`board.toml` are present), it just never renders
/// a row in the Lanes Switch dashboard beyond the sensible defaults below.
/// Identity (the old `id` field) isn't part of this anymore - it's the
/// context directory's own name, which can't disagree with itself the way
/// a hand-written `id` field could.
#[derive(Deserialize, Default)]
struct LaneToml {
    name: Option<String>,
    active: Option<bool>,
}

/// `<context>/tab.toml` - the Zellij/repo anchor (what used to be the one
/// do-everything lane file's `[[scope]]` array). Same human-friendly
/// `ScopeElementRaw` shape as before, just split into its own file.
#[derive(Deserialize, Default)]
struct TabToml {
    #[serde(default)]
    scope: Vec<ScopeElementRaw>,
}

/// `layout.toml` - window-placement targets applied on switch (app /
/// obsidian / firefox-profile / wezterm / zellij-placement / vscode /
/// sourcetree). Same shape at two levels: a top-level `~/.config/lanes/layout.toml`
/// supplies defaults applied to every context (see `load_global_config`),
/// and a context's own `<id>/layout.toml` overrides/adds to them (see
/// `merge_targets`). Boards is deliberately *not* included here - see
/// `read_board_target` for why it gets its own facet file instead.
#[derive(Deserialize, Default)]
struct LayoutToml {
    #[serde(default)]
    targets: Vec<Target>,
}

/// TOML-facing shape for a scope element - human-friendly fields
/// (`session`, `path`) rather than making config authors hand-write the
/// real ScopeElement's URI locators. Converted to the real type on load,
/// same pattern as TabToml vs. the real Lane.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ScopeElementRaw {
    ZellijSession { session: String },
    Repo { path: String },
    // Only the list+position shape for now - board/list-without-position
    // aren't used by any lane yet, so there's nothing to validate a choice
    // between. Add them here (and to the `From` impl below) when a lane
    // actually wants one.
    Trello { list: String, position: u32 },
}

impl From<ScopeElementRaw> for ScopeElement {
    fn from(raw: ScopeElementRaw) -> Self {
        match raw {
            ScopeElementRaw::ZellijSession { session } => ScopeElement::zellij_session(&session),
            ScopeElementRaw::Repo { path } => ScopeElement::repo(&path),
            ScopeElementRaw::Trello { list, position } => ScopeElement::trello_list_card(&list, position),
        }
    }
}

// --- Loaders ---

type GlobalConfigLoaded = (Option<Vec<String>>, HashMap<String, MonitorConfig>, Option<Vec<String>>, CyclePolicy);

fn load_global_config() -> GlobalConfigLoaded {
    let path = config_dir().join("lanes.toml");
    let content = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return (None, HashMap::new(), None, CyclePolicy::default()),
    };
    let cfg: GlobalConfig = match toml::from_str(&content) {
        Ok(c) => c,
        Err(_) => return (None, HashMap::new(), None, CyclePolicy::default()),
    };
    let monitors = cfg.monitors.into_iter()
        .map(|(k, v)| (k, MonitorConfig { uuid: v.uuid, name: v.name }))
        .collect();
    (cfg.drivers, monitors, cfg.order, cfg.cycle_policy)
}

/// A target's identity for cascade purposes: a lane's own target overrides
/// a default of the same identity (dropped from the merged result) rather
/// than duplicating it. Driver kind alone is the identity for everything
/// except `app`, which also needs the app name - a lane might reasonably
/// want more than one `app` target, or override just one of several
/// defaults sharing that driver.
fn target_identity(t: &Target) -> (&'static str, Option<&str>) {
    match &t.driver {
        TargetDriver::App { name, .. } => ("app", Some(name.as_str())),
        other => (other.name(), None),
    }
}

/// Cascade a lane's own targets on top of the global defaults - same idea
/// as CSS: defaults apply everywhere, a lane's own target of the same
/// identity overrides the matching default instead of duplicating it,
/// anything else from both lists is kept.
fn merge_targets(defaults: &[Target], lane_targets: Vec<Target>) -> Vec<Target> {
    let overridden: Vec<_> = lane_targets.iter().map(target_identity).collect();
    let mut merged: Vec<Target> = defaults
        .iter()
        .filter(|d| !overridden.contains(&target_identity(d)))
        .cloned()
        .collect();
    merged.extend(lane_targets);
    merged
}

/// Read one optional facet file. Absent means "this facet doesn't apply to
/// this context" - not an error, hence the `Default` rather than an
/// `Option`/`Result`. A *present but malformed* file still only warns and
/// falls back to that facet's defaults - same partial-failure discipline
/// the old single-file loader had (a bad file drops only what it would
/// have contributed, never the whole context).
fn read_facet<T: DeserializeOwned + Default>(path: &Path) -> T {
    let Ok(content) = std::fs::read_to_string(path) else {
        return T::default();
    };
    match toml::from_str(&content) {
        Ok(v) => v,
        Err(err) => {
            eprintln!("warning: could not parse {:?}: {}", path, err);
            T::default()
        }
    }
}

/// `<context>/board.toml` - the Boards projection: which Boards project
/// this context corresponds to, plus optional placement. Kept as its own
/// facet (one `Target`, not a list - a context has at most one board)
/// rather than folded into `layout.toml`, because Boards is architecturally
/// different from the other target drivers: it's generated/materialized
/// (see PLAN-context-model.md), not just another thing to place on switch.
fn read_board_target(path: &Path) -> Option<Target> {
    let content = std::fs::read_to_string(path).ok()?;
    match toml::from_str::<Target>(&content) {
        Ok(t) if matches!(t.driver, TargetDriver::Boards { .. }) => Some(t),
        Ok(t) => {
            eprintln!(
                "warning: {:?}: board.toml must use driver = \"boards\", got {:?}",
                path,
                t.driver.name()
            );
            None
        }
        Err(err) => {
            eprintln!("warning: could not parse {:?}: {}", path, err);
            None
        }
    }
}

/// Every subdirectory of `dir` (always `config_dir()` outside tests - taken
/// as a parameter purely so tests can point it at a throwaway tree instead
/// of mutating `$HOME`, which would race every other test reading it) is
/// one context; the directory name *is* its id (no separate field to
/// hand-write, and so nothing to disagree with it - see LaneToml's doc
/// comment). Inside, `lane.toml`, `tab.toml`, `layout.toml` and
/// `board.toml` are each independently optional - a context can be just a
/// tab, just a board, all of them, or (rare but valid) none, in which case
/// it's an inert placeholder.
fn load_lanes(dir: &Path, default_targets: &[Target]) -> Vec<Lane> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut lanes: Vec<Lane> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let id = e.file_name().to_string_lossy().into_owned();
            if id.contains(char::is_whitespace) {
                eprintln!("warning: skipping context dir {:?} - the directory name (id) must not contain spaces", id);
                return None;
            }
            let path = e.path();
            let lane_toml: LaneToml = read_facet(&path.join("lane.toml"));
            let tab_toml: TabToml = read_facet(&path.join("tab.toml"));
            let layout_toml: LayoutToml = read_facet(&path.join("layout.toml"));

            let mut own_targets = layout_toml.targets;
            own_targets.extend(read_board_target(&path.join("board.toml")));

            Some(Lane {
                name: lane_toml.name.unwrap_or_else(|| id.clone()),
                active: lane_toml.active.unwrap_or(true),
                scope: tab_toml.scope.into_iter().map(ScopeElement::from).collect(),
                targets: merge_targets(default_targets, own_targets),
                id,
            })
        })
        .collect();

    lanes.sort_by(|a, b| a.id.cmp(&b.id));
    lanes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lane(id: &str) -> Lane {
        Lane {
            id: id.to_string(),
            name: id.to_string(),
            active: true,
            scope: Vec::new(),
            targets: Vec::new(),
        }
    }

    fn wezterm_target(monitor: &str) -> Target {
        Target {
            driver: TargetDriver::Wezterm { session: None },
            monitor: Some(monitor.to_string()),
            position: Some(toml::Value::String("full".to_string())),
            launch: false,
            raise: false,
            minimized: None,
            primary: false,
        }
    }

    fn app_target(name: &str) -> Target {
        Target {
            driver: TargetDriver::App { name: name.to_string(), bundle_id: None },
            monitor: None,
            position: None,
            launch: false,
            raise: false,
            minimized: None,
            primary: false,
        }
    }

    fn config_with(lanes: Vec<Lane>, order: Option<Vec<&str>>) -> Config {
        Config {
            drivers: None,
            monitors: HashMap::new(),
            order: order.map(|o| o.into_iter().map(String::from).collect()),
            cycle_policy: CyclePolicy::default(),
            lanes,
        }
    }

    #[test]
    fn unregistered_contexts_is_empty_when_order_is_none() {
        let cfg = config_with(vec![lane("infra")], None);
        assert_eq!(cfg.unregistered_contexts(), Vec::<&str>::new());
    }

    #[test]
    fn unregistered_contexts_is_empty_when_order_covers_every_lane() {
        let cfg = config_with(vec![lane("infra"), lane("lanes-dev")], Some(vec!["infra", "lanes-dev"]));
        assert_eq!(cfg.unregistered_contexts(), Vec::<&str>::new());
    }

    #[test]
    fn unregistered_contexts_lists_lanes_missing_from_order() {
        let cfg = config_with(vec![lane("infra"), lane("lanes-dev")], Some(vec!["infra"]));
        assert_eq!(cfg.unregistered_contexts(), vec!["lanes-dev"]);
    }

    #[test]
    fn merge_targets_appends_defaults_a_lane_has_no_override_for() {
        let defaults = vec![wezterm_target("lg-right")];
        let merged = merge_targets(&defaults, vec![app_target("Japanese")]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].driver.name(), "wezterm");
        assert_eq!(merged[1].driver.name(), "app");
    }

    #[test]
    fn merge_targets_lane_override_replaces_the_matching_default_not_duplicates_it() {
        let defaults = vec![wezterm_target("lg-right")];
        let merged = merge_targets(&defaults, vec![wezterm_target("main")]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].monitor.as_deref(), Some("main"));
    }

    #[test]
    fn merge_targets_app_identity_includes_name_so_different_apps_coexist() {
        let defaults = vec![app_target("Firefox")];
        let merged = merge_targets(&defaults, vec![app_target("Japanese")]);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn parses_global_order() {
        let cfg: super::GlobalConfig =
            toml::from_str(r#"order = ["b", "a"]"#).unwrap();
        assert_eq!(cfg.order, Some(vec!["b".to_string(), "a".to_string()]));
    }

    #[test]
    fn sort_by_order_places_listed_lanes_first_in_order() {
        let mut lanes = vec![lane("a"), lane("b"), lane("c")];
        sort_by_order(&mut lanes, &["c".to_string(), "a".to_string()]);
        assert_eq!(
            lanes.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
            vec!["c", "a", "b"]
        );
    }

    #[test]
    fn sort_by_order_appends_unlisted_lanes_keeping_relative_order() {
        let mut lanes = vec![lane("a"), lane("b"), lane("c"), lane("d")];
        sort_by_order(&mut lanes, &["c".to_string()]);
        assert_eq!(
            lanes.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
            vec!["c", "a", "b", "d"]
        );
    }

    #[test]
    fn parses_global_drivers() {
        let cfg: super::GlobalConfig =
            toml::from_str(r#"drivers = ["zellij", "claude"]"#).unwrap();
        assert_eq!(
            cfg.drivers,
            Some(vec!["zellij".to_string(), "claude".to_string()])
        );
    }

    #[test]
    fn global_config_cycle_policy_defaults_to_claude_sessions_when_absent() {
        let cfg: super::GlobalConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.cycle_policy, CyclePolicy::ClaudeSessions);
    }

    #[test]
    fn global_config_parses_cycle_policy_lanes() {
        let cfg: super::GlobalConfig = toml::from_str(r#"cycle_policy = "lanes""#).unwrap();
        assert_eq!(cfg.cycle_policy, CyclePolicy::Lanes);
    }

    #[test]
    fn global_config_parses_cycle_policy_claude_sessions_explicitly() {
        let cfg: super::GlobalConfig = toml::from_str(r#"cycle_policy = "claude-sessions""#).unwrap();
        assert_eq!(cfg.cycle_policy, CyclePolicy::ClaudeSessions);
    }

    #[test]
    fn lane_toml_defaults_when_absent() {
        // Simulates a context dir with no lane.toml at all.
        let lane_toml = LaneToml::default();
        assert_eq!(lane_toml.name, None);
        assert_eq!(lane_toml.active, None);
    }

    #[test]
    fn parses_tab_toml_zellij_session_scope_element() {
        let content = r#"
[[scope]]
kind = "zellij_session"
session = "sheetwork"
"#;
        let file: TabToml = toml::from_str(content).unwrap();
        assert_eq!(file.scope.len(), 1);
        assert!(matches!(&file.scope[0], ScopeElementRaw::ZellijSession { session } if session == "sheetwork"));
    }

    #[test]
    fn parses_tab_toml_trello_list_card_scope_element() {
        let content = r#"
[[scope]]
kind = "trello"
list = "abc123"
position = 1
"#;
        let file: TabToml = toml::from_str(content).unwrap();
        assert_eq!(file.scope.len(), 1);
        assert!(matches!(
            &file.scope[0],
            ScopeElementRaw::Trello { list, position } if list == "abc123" && *position == 1
        ));
        let element: ScopeElement = file.scope.into_iter().next().unwrap().into();
        assert_eq!(element.trello_list_card_position(), Some(("abc123", 1)));
    }

    #[test]
    fn lane_toml_active_true_is_parsed() {
        let file: LaneToml = toml::from_str("name = \"Sheetwork\"\nactive = true\n").unwrap();
        assert_eq!(file.active, Some(true));
    }

    #[test]
    fn lane_toml_active_false_is_parsed() {
        let file: LaneToml = toml::from_str("name = \"Sheetwork\"\nactive = false\n").unwrap();
        assert_eq!(file.active, Some(false));
    }

    #[test]
    fn lane_toml_without_name_still_parses_name_is_optional_now() {
        // Unlike the old single-file [lane] header, `name` is optional here -
        // load_lanes() falls back to the directory name (see its own test
        // coverage via the id-is-the-dirname design). lane.toml itself must
        // not require it.
        let file: LaneToml = toml::from_str("active = false\n").unwrap();
        assert_eq!(file.name, None);
        assert_eq!(file.active, Some(false));
    }

    #[test]
    fn parses_layout_toml_with_name_and_target() {
        let content = r#"
[[targets]]
driver = "wezterm"
session = "lanes"
monitor = "lg-right"
position = "full"
"#;
        let file: LayoutToml = toml::from_str(content).unwrap();
        assert_eq!(file.targets.len(), 1);
        assert_eq!(file.targets[0].driver.name(), "wezterm");
        assert_eq!(file.targets[0].monitor.as_deref(), Some("lg-right"));
        assert!(matches!(&file.targets[0].driver, crate::model::TargetDriver::Wezterm { session } if session.as_deref() == Some("lanes")));
    }

    #[test]
    fn parses_layout_toml_with_a_minimized_target_and_no_monitor() {
        let content = r#"
[[targets]]
driver = "app"
name = "Wispr Flow"
bundle_id = "com.electron.wispr-flow.accessibility-mac-app"
minimized = true
"#;
        let file: LayoutToml = toml::from_str(content).unwrap();
        assert_eq!(file.targets.len(), 1);
        assert_eq!(file.targets[0].minimized, Some(true));
        assert_eq!(file.targets[0].monitor, None);
    }

    #[test]
    fn parses_target_with_grid_position_and_no_monitor() {
        // A target can activate-only (no monitor/position at all - e.g.
        // wezterm/zellij, which never place a window) or carry a raw grid
        // span instead of a preset - lanes-cli doesn't interpret `position`,
        // just forwards it to lanes-wm, so it only needs to parse as TOML.
        let content = r#"
[[targets]]
driver = "obsidian"
vault = "Formation"
monitor = "main"

[targets.position]
cols = 3
col = [1, 2]

[[targets]]
driver = "zellij"
session = "formation"
"#;
        let file: LayoutToml = toml::from_str(content).unwrap();
        assert_eq!(file.targets.len(), 2);
        assert!(matches!(&file.targets[0].driver, crate::model::TargetDriver::Obsidian { vault } if vault == "Formation"));
        assert!(file.targets[0].position.is_some());
        assert!(file.targets[1].monitor.is_none());
        assert!(matches!(&file.targets[1].driver, crate::model::TargetDriver::Zellij { session, pane: None } if session.as_deref() == Some("formation")));
    }

    #[test]
    fn board_toml_parses_as_a_single_boards_target() {
        let content = r#"
driver = "boards"
project = "Infra"
monitor = "main"
position = "full"
raise = true
"#;
        let target: Target = toml::from_str(content).unwrap();
        assert!(matches!(&target.driver, crate::model::TargetDriver::Boards { project } if project == "Infra"));
        assert!(target.raise);
    }

    #[test]
    fn read_board_target_rejects_a_non_boards_driver() {
        let dir = std::env::temp_dir().join(format!("lanes-config-test-{}-{:?}", std::process::id(), std::thread::current().id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("board.toml");
        std::fs::write(&path, "driver = \"app\"\nname = \"Whatever\"\n").unwrap();
        assert!(read_board_target(&path).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_facet_falls_back_to_default_when_file_is_absent() {
        let missing = std::env::temp_dir().join("lanes-config-test-does-not-exist").join("lane.toml");
        let lane_toml: LaneToml = read_facet(&missing);
        assert_eq!(lane_toml.name, None);
        assert_eq!(lane_toml.active, None);
    }

    #[test]
    fn read_facet_falls_back_to_default_on_malformed_toml_and_warns_not_panics() {
        let dir = std::env::temp_dir().join(format!("lanes-config-test-malformed-{}-{:?}", std::process::id(), std::thread::current().id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("lane.toml");
        std::fs::write(&path, "this is not valid = = toml").unwrap();
        let lane_toml: LaneToml = read_facet(&path);
        assert_eq!(lane_toml.name, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scope_element_raw_converts_to_real_scope_element() {
        let el: ScopeElement = ScopeElementRaw::ZellijSession { session: "infra".to_string() }.into();
        assert_eq!(el.zellij_session_name(), Some("infra"));

        let el: ScopeElement = ScopeElementRaw::Repo { path: "~/src/infra".to_string() }.into();
        assert_eq!(el.repo_path(), Some("~/src/infra"));
    }

    #[test]
    fn driver_enabled_with_list() {
        let cfg = Config {
            drivers: Some(vec!["zellij".to_string(), "claude".to_string()]),
            monitors: HashMap::new(),
            order: None,
            cycle_policy: CyclePolicy::default(),
            lanes: Vec::new(),
        };
        assert!(cfg.driver_enabled("zellij"));
        assert!(cfg.driver_enabled("claude"));
        assert!(!cfg.driver_enabled("brotab"));
    }

    #[test]
    fn driver_enabled_without_list() {
        let cfg = Config::default();
        assert!(cfg.driver_enabled("zellij"));
        assert!(cfg.driver_enabled("brotab"));
    }

    #[test]
    fn zellij_lane_names_derived_from_scope() {
        let cfg = Config {
            drivers: None,
            monitors: HashMap::new(),
            order: None,
            cycle_policy: CyclePolicy::default(),
            lanes: vec![
                Lane {
                    id: "sheetwork".to_string(),
                    name: "Sheetwork".to_string(),
                    active: true,
                    scope: vec![ScopeElement::zellij_session("sheetwork")],
                    targets: vec![],
                },
                Lane {
                    id: "lanes-dev".to_string(),
                    name: "lanes dev".to_string(),
                    active: true,
                    scope: vec![ScopeElement::zellij_session("lanes")],
                    targets: vec![],
                },
            ],
        };
        let names = cfg.zellij_lane_names();
        assert_eq!(names.get("sheetwork").map(|s| s.as_str()), Some("Sheetwork"));
        assert_eq!(names.get("lanes").map(|s| s.as_str()), Some("lanes dev"));
    }

    #[test]
    fn lane_for_session_finds_lane_by_zellij_session_scope_element() {
        let cfg = Config {
            drivers: None,
            monitors: HashMap::new(),
            order: None,
            cycle_policy: CyclePolicy::default(),
            lanes: vec![
                Lane {
                    id: "sheetwork1".to_string(),
                    name: "Sheetwork 1".to_string(),
                    active: true,
                    scope: vec![ScopeElement::zellij_session("sheetwork1")],
                    targets: vec![],
                },
                Lane {
                    id: "lanes-dev".to_string(),
                    name: "lanes dev".to_string(),
                    active: true,
                    scope: vec![ScopeElement::zellij_session("lanes")],
                    targets: vec![],
                },
            ],
        };
        assert_eq!(cfg.lane_for_session("lanes").map(|l| l.id.as_str()), Some("lanes-dev"));
        assert_eq!(cfg.lane_for_session("sheetwork1").map(|l| l.id.as_str()), Some("sheetwork1"));
        assert!(cfg.lane_for_session("job-hunting").is_none());
    }

    #[test]
    fn load_lanes_reads_a_real_directory_tree_end_to_end() {
        let root = std::env::temp_dir().join(format!(
            "lanes-config-test-load-lanes-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);

        // A fully-specified context: all four facets.
        let infra = root.join("infra");
        std::fs::create_dir_all(&infra).unwrap();
        std::fs::write(infra.join("lane.toml"), "name = \"Infra\"\nactive = false\n").unwrap();
        std::fs::write(
            infra.join("tab.toml"),
            "[[scope]]\nkind = \"zellij_session\"\nsession = \"infra\"\n",
        ).unwrap();
        std::fs::write(
            infra.join("board.toml"),
            "driver = \"boards\"\nproject = \"Infra\"\nmonitor = \"main\"\nposition = \"full\"\n",
        ).unwrap();

        // A bare context: just a directory, no facet files at all.
        std::fs::create_dir_all(root.join("placeholder")).unwrap();

        let lanes = load_lanes(&root, &[]);
        assert_eq!(lanes.len(), 2);

        let infra_lane = lanes.iter().find(|l| l.id == "infra").unwrap();
        assert_eq!(infra_lane.name, "Infra");
        assert!(!infra_lane.active);
        assert_eq!(infra_lane.scope.len(), 1);
        assert_eq!(infra_lane.targets.len(), 1);
        assert!(matches!(&infra_lane.targets[0].driver, crate::model::TargetDriver::Boards { project } if project == "Infra"));

        let placeholder = lanes.iter().find(|l| l.id == "placeholder").unwrap();
        assert_eq!(placeholder.name, "placeholder");
        assert!(placeholder.active);
        assert!(placeholder.scope.is_empty());
        assert!(placeholder.targets.is_empty());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_lanes_skips_a_directory_name_with_whitespace() {
        let root = std::env::temp_dir().join(format!(
            "lanes-config-test-whitespace-id-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("has space")).unwrap();
        std::fs::create_dir_all(root.join("fine")).unwrap();

        let lanes = load_lanes(&root, &[]);
        assert_eq!(lanes.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(), vec!["fine"]);

        std::fs::remove_dir_all(&root).ok();
    }
}
