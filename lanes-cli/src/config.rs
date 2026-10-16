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
    /// Context display order, from `contexts.toml`'s `order` (list of
    /// context ids) - deliberately its own file, not lanes.toml: this is an
    /// ordering over *contexts* in general (lane or not - e.g. a board-only
    /// context like `development`), not something owned by "being a lane."
    /// Contexts not listed fall back to alphabetical-by-id, after any listed
    /// ones.
    pub order: Option<Vec<String>>,
    /// Which candidate list `lanes cycle` steps through - see
    /// `CyclePolicy`'s own doc comment.
    pub cycle_policy: CyclePolicy,
    /// Discovered lane definitions - only contexts that actually have a
    /// `lane.toml` (see `load_lanes`). A context with no `lane.toml` simply
    /// isn't a lane: it never appears here, regardless of what other facets
    /// (`tab.toml`, `board.toml`) it has.
    pub lanes: Vec<Lane>,
    /// Every context's Boards target (`board.toml`), read directly and
    /// independently of `lanes` above - a context doesn't need a
    /// `lane.toml` for Boards materialization/ordering to manage it (see
    /// `context_board_targets`). `(context id, display name, target)`.
    pub boards_targets: Vec<(String, String, Target)>,
    /// Every context directory's id, regardless of which facets (if any) it
    /// has - what `order` is actually checked against (see
    /// `unregistered_contexts`), since a context doesn't have to be a lane
    /// to need a place in that order.
    pub context_ids: Vec<String>,
    /// `HISTIGNORE`-style glob patterns (`*`/`?` only - see `glob_match`),
    /// each matched against a shell-hook record's whole command line, that
    /// the `shell` driver should never surface a Command signal for. From
    /// lanes.toml's `ignored_shell_commands`. Nothing is excluded by name
    /// in code - not even Claude, whose chip is otherwise genuinely
    /// redundant with its own dedicated ClaudeSession signal; excluding it
    /// is an entry in this list (the real lanes.toml's default), not a
    /// special case `is_shell_signal_ignored` knows about.
    pub shell_ignored_commands: Vec<String>,
}

impl Config {
    pub fn load() -> Self {
        let (drivers, monitors, cycle_policy, shell_ignored_commands) = load_global_config();
        let order = load_context_order();
        // "default" is just a template like any other - it's the one every
        // context implicitly, unconditionally starts its cascade from (no
        // opt-out).
        let templates_dir = config_dir().join("layout-templates");
        let default_targets = load_layout_template(&templates_dir, "default");
        let contexts_dir = config_dir().join("contexts");
        let mut lanes = load_lanes(&contexts_dir, &templates_dir, &default_targets);
        let boards_targets = context_board_targets(&contexts_dir);
        let context_ids = all_context_ids(&contexts_dir);
        if let Some(order) = &order {
            sort_by_order(&mut lanes, order);
        }
        Self { drivers, monitors, order, cycle_policy, lanes, boards_targets, context_ids, shell_ignored_commands }
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
        self.context_ids
            .iter()
            .map(|id| id.as_str())
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
    let path = config_dir().join("contexts").join(lane_id).join("lane.toml");
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
            boards_targets: Vec::new(),
            context_ids: Vec::new(),
            shell_ignored_commands: Vec::new(),
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
    cycle_policy: CyclePolicy,
    #[serde(default)]
    ignored_shell_commands: Vec<String>,
}

#[derive(Deserialize)]
struct MonitorConfigRaw {
    uuid: Option<String>,
    name: Option<String>,
}

/// `contexts.toml` - context order, kept separate from lanes.toml since it's
/// an ordering over contexts in general, not something "being a lane" owns
/// (see `Config::order`'s doc comment).
#[derive(Deserialize, Default)]
struct ContextsConfig {
    #[serde(default)]
    order: Option<Vec<String>>,
}

/// `<context>/lane.toml` - the switcher-facing facet, and the one thing
/// that makes a context a *lane* at all. A context dir with no `lane.toml`
/// is not a lane - it's invisible to `cfg.lanes`, the dashboard, and
/// cycling entirely, even though its other facets (`tab.toml`,
/// `board.toml`) are still read directly for their own purposes (see
/// `context_board_targets`). Having *any* `lane.toml`, even an empty one,
/// is what opts a context in - its own `active` key (if present) then only
/// controls whether that real lane is currently shown/cycled to, same as
/// before. Identity (the old `id` field) isn't part of this anymore - it's
/// the context directory's own name, which can't disagree with itself the
/// way a hand-written `id` field could.
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
/// obsidian / wezterm / zellij-placement / vscode /
/// sourcetree). Same shape at three cascade levels, each a plain
/// `LayoutToml` merged on top of the previous one (`merge_targets`):
/// `layout-templates/default.toml` (always applied - see
/// `load_layout_template`), the named template a context opts into via
/// `layout_template` (if any), then the context's own
/// `contexts/<id>/layout.toml`. Boards is deliberately *not* included here -
/// see `read_board_target` for why it gets its own facet file instead.
#[derive(Deserialize, Default)]
struct LayoutToml {
    /// Name of a `layout-templates/<name>.toml` to cascade in between the
    /// implicit `default.toml` and this file's own `targets`. Single
    /// inheritance only, deliberately - see DECISIONS.md for why mixins
    /// (multiple templates) are out of scope for now.
    #[serde(default)]
    layout_template: Option<String>,
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

type GlobalConfigLoaded = (Option<Vec<String>>, HashMap<String, MonitorConfig>, CyclePolicy, Vec<String>);

fn load_global_config() -> GlobalConfigLoaded {
    let path = config_dir().join("lanes.toml");
    let content = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return (None, HashMap::new(), CyclePolicy::default(), Vec::new()),
    };
    let cfg: GlobalConfig = match toml::from_str(&content) {
        Ok(c) => c,
        Err(_) => return (None, HashMap::new(), CyclePolicy::default(), Vec::new()),
    };
    let monitors = cfg.monitors.into_iter()
        .map(|(k, v)| (k, MonitorConfig { uuid: v.uuid, name: v.name }))
        .collect();
    (cfg.drivers, monitors, cfg.cycle_policy, cfg.ignored_shell_commands)
}

/// `~/.config/lanes/contexts.toml`'s `order` - see `Config::order`'s doc
/// comment for why this lives in its own file rather than lanes.toml.
fn load_context_order() -> Option<Vec<String>> {
    let path = config_dir().join("contexts.toml");
    let content = std::fs::read_to_string(&path).ok()?;
    let cfg: ContextsConfig = toml::from_str(&content).unwrap_or_default();
    cfg.order
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

/// `<templates_dir>/<name>.toml`'s targets - used both for the always-on
/// `"default"` template (`Config::load`) and for a context's optional
/// `layout_template` (`load_lanes`). `templates_dir` is always
/// `config_dir().join("layout-templates")` outside tests - a parameter for
/// the same reason `load_lanes`'s `dir` is (see its doc comment). Absent/
/// malformed behaves exactly like any other facet file (`read_facet`) - a
/// missing or broken template just contributes nothing, it's `doctor`'s job
/// (not this function's) to flag a context referencing one that doesn't
/// resolve.
fn load_layout_template(templates_dir: &Path, name: &str) -> Vec<Target> {
    let layout: LayoutToml = read_facet(&templates_dir.join(format!("{name}.toml")));
    layout.targets
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

/// Every subdirectory of `dir` (always `config_dir().join("contexts")`
/// outside tests - taken as a parameter purely so tests can point it at a
/// throwaway tree instead of mutating `$HOME`, which would race every
/// other test reading it) is a *candidate* context; the directory name *is*
/// its id (no separate field to hand-write, and so nothing to disagree with
/// it - see LaneToml's doc comment). Only a context with an actual
/// `lane.toml` becomes a `Lane` here - presence, not content, is what opts
/// a context into being a lane at all (an empty `lane.toml` still counts;
/// `read_facet` alone can't tell "file absent" apart from "file present but
/// empty", both deserializing to `LaneToml::default()`, hence the separate
/// existence check). A context with no `lane.toml` is simply not a lane: it
/// never appears in the returned list, regardless of whether it has a
/// `tab.toml`/`board.toml`/`layout.toml` - those facets are read directly by
/// `context_board_targets` and friends for non-lane purposes instead.
fn load_lanes(dir: &Path, templates_dir: &Path, default_targets: &[Target]) -> Vec<Lane> {
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
            if !path.join("lane.toml").is_file() {
                return None;
            }
            let lane_toml: LaneToml = read_facet(&path.join("lane.toml"));
            let tab_toml: TabToml = read_facet(&path.join("tab.toml"));
            let layout_toml: LayoutToml = read_facet(&path.join("layout.toml"));

            let layout_template = layout_toml.layout_template.clone();
            let templated_defaults = match &layout_template {
                Some(name) => merge_targets(default_targets, load_layout_template(templates_dir, name)),
                None => default_targets.to_vec(),
            };

            let mut own_targets = layout_toml.targets;
            own_targets.extend(read_board_target(&path.join("board.toml")));

            Some(Lane {
                name: lane_toml.name.unwrap_or_else(|| id.clone()),
                active: lane_toml.active.unwrap_or(true),
                scope: tab_toml.scope.into_iter().map(ScopeElement::from).collect(),
                targets: merge_targets(&templated_defaults, own_targets),
                layout_template,
                id,
            })
        })
        .collect();

    lanes.sort_by(|a, b| a.id.cmp(&b.id));
    lanes
}

/// Every context directory's id under `dir`, regardless of which facets (if
/// any) it has - what `order` (contexts.toml) is checked against, since a
/// context doesn't need to be a lane to need a place in that order (e.g. a
/// board-only context like `development`).
fn all_context_ids(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut ids: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|id| !id.contains(char::is_whitespace))
        .collect();
    ids.sort();
    ids
}

/// Every context's `board.toml` target, read directly - independent of
/// `lane.toml`, so Boards materialization/ordering (`doctor`) still works
/// for a context that isn't a lane at all. Display name falls back to the
/// directory id when there's no `lane.toml` name to use (a board-only
/// context has no `lane.toml` at all, so nowhere else to get a name from).
fn context_board_targets(dir: &Path) -> Vec<(String, String, Target)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for e in entries.filter_map(|e| e.ok()) {
        let path = e.path();
        if !path.is_dir() {
            continue;
        }
        let id = e.file_name().to_string_lossy().into_owned();
        if id.contains(char::is_whitespace) {
            continue;
        }
        let Some(target) = read_board_target(&path.join("board.toml")) else {
            continue;
        };
        let lane_toml: LaneToml = read_facet(&path.join("lane.toml"));
        let name = lane_toml.name.unwrap_or_else(|| id.clone());
        out.push((id, name, target));
    }
    out
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
            layout_template: None,
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
        let context_ids = lanes.iter().map(|l| l.id.clone()).collect();
        Config {
            drivers: None,
            monitors: HashMap::new(),
            order: order.map(|o| o.into_iter().map(String::from).collect()),
            cycle_policy: CyclePolicy::default(),
            lanes,
            boards_targets: Vec::new(),
            context_ids,
            shell_ignored_commands: Vec::new(),
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
    fn unregistered_contexts_includes_a_board_only_context_not_in_cfg_lanes() {
        // A context with no lane.toml never appears in cfg.lanes, but it
        // still needs a place in `order` - unregistered_contexts checks
        // against context_ids, not lanes, so it still catches this.
        let mut cfg = config_with(vec![lane("infra")], Some(vec!["infra"]));
        cfg.context_ids.push("development".to_string());
        assert_eq!(cfg.unregistered_contexts(), vec!["development"]);
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
    fn parses_contexts_config_order() {
        let cfg: super::ContextsConfig =
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
            boards_targets: Vec::new(),
            context_ids: Vec::new(),
            shell_ignored_commands: Vec::new(),
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
                    layout_template: None,
                },
                Lane {
                    id: "lanes-dev".to_string(),
                    name: "lanes dev".to_string(),
                    active: true,
                    scope: vec![ScopeElement::zellij_session("lanes")],
                    targets: vec![],
                    layout_template: None,
                },
            ],
            boards_targets: Vec::new(),
            context_ids: Vec::new(),
            shell_ignored_commands: Vec::new(),
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
                    layout_template: None,
                },
                Lane {
                    id: "lanes-dev".to_string(),
                    name: "lanes dev".to_string(),
                    active: true,
                    scope: vec![ScopeElement::zellij_session("lanes")],
                    targets: vec![],
                    layout_template: None,
                },
            ],
            boards_targets: Vec::new(),
            context_ids: Vec::new(),
            shell_ignored_commands: Vec::new(),
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

        // A bare context: just a directory, no facet files at all - not a
        // lane at all, since it has no lane.toml.
        std::fs::create_dir_all(root.join("placeholder")).unwrap();

        let lanes = load_lanes(&root, Path::new("/nonexistent"), &[]);
        assert_eq!(lanes.len(), 1);

        let infra_lane = lanes.iter().find(|l| l.id == "infra").unwrap();
        assert_eq!(infra_lane.name, "Infra");
        assert!(!infra_lane.active);
        assert_eq!(infra_lane.scope.len(), 1);
        assert_eq!(infra_lane.targets.len(), 1);
        assert!(matches!(&infra_lane.targets[0].driver, crate::model::TargetDriver::Boards { project } if project == "Infra"));

        // No lane.toml at all -> not a lane, period - it never appears in
        // load_lanes' output regardless of what else it might have.
        assert!(lanes.iter().all(|l| l.id != "placeholder"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_lanes_an_empty_lane_toml_still_defaults_active_true() {
        // Distinct from the placeholder case above: the file's mere
        // *presence* is what opts a context in, regardless of whether it
        // sets anything - an empty lane.toml is enough.
        let root = std::env::temp_dir().join(format!(
            "lanes-config-test-empty-lane-toml-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("ctx");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lane.toml"), "").unwrap();

        let lanes = load_lanes(&root, Path::new("/nonexistent"), &[]);
        assert_eq!(lanes.len(), 1);
        assert!(lanes[0].active);

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
        std::fs::write(root.join("fine").join("lane.toml"), "").unwrap();

        let lanes = load_lanes(&root, Path::new("/nonexistent"), &[]);
        assert_eq!(lanes.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(), vec!["fine"]);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_lanes_cascades_through_a_layout_template() {
        let root = std::env::temp_dir().join(format!(
            "lanes-config-test-layout-template-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let contexts_dir = root.join("contexts");
        let templates_dir = root.join("layout-templates");
        std::fs::create_dir_all(&templates_dir).unwrap();

        // The template overrides the global default's wezterm monitor, and
        // adds a boards target of its own.
        std::fs::write(
            templates_dir.join("dev.toml"),
            "[[targets]]\ndriver = \"wezterm\"\nmonitor = \"lg-left\"\nposition = \"full\"\n\n\
             [[targets]]\ndriver = \"app\"\nname = \"Boards\"\nmonitor = \"lg-right\"\nposition = \"full\"\n",
        ).unwrap();

        let lane_dir = contexts_dir.join("lanes-dev");
        std::fs::create_dir_all(&lane_dir).unwrap();
        std::fs::write(lane_dir.join("lane.toml"), "").unwrap();
        std::fs::write(
            lane_dir.join("layout.toml"),
            "layout_template = \"dev\"\n\n\
             [[targets]]\ndriver = \"app\"\nname = \"Boards\"\nmonitor = \"main\"\nposition = \"full\"\n",
        ).unwrap();

        let default_targets = [wezterm_target("lg-right")];
        let lanes = load_lanes(&contexts_dir, &templates_dir, &default_targets);
        assert_eq!(lanes.len(), 1);
        let targets = &lanes[0].targets;

        let wezterm = targets.iter().find(|t| matches!(t.driver, crate::model::TargetDriver::Wezterm { .. })).unwrap();
        assert_eq!(wezterm.monitor.as_deref(), Some("lg-left"), "the template should override the global default");

        let boards = targets.iter().find(|t| matches!(&t.driver, crate::model::TargetDriver::App { name, .. } if name == "Boards")).unwrap();
        assert_eq!(boards.monitor.as_deref(), Some("main"), "the context's own target should override the template");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_lanes_uses_only_the_default_targets_when_no_layout_template_is_set() {
        let root = std::env::temp_dir().join(format!(
            "lanes-config-test-no-layout-template-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let contexts_dir = root.join("contexts");
        let infra = contexts_dir.join("infra");
        std::fs::create_dir_all(&infra).unwrap();
        std::fs::write(infra.join("lane.toml"), "").unwrap();

        let default_targets = [wezterm_target("lg-right")];
        let lanes = load_lanes(&contexts_dir, Path::new("/nonexistent"), &default_targets);
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].targets.len(), 1);
        assert_eq!(lanes[0].targets[0].monitor.as_deref(), Some("lg-right"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_lanes_skips_a_context_with_no_lane_toml_even_with_other_facets() {
        let root = std::env::temp_dir().join(format!(
            "lanes-config-test-no-lane-toml-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("development");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("board.toml"),
            "driver = \"boards\"\nproject = \"Development\"\nmonitor = \"main\"\nposition = \"full\"\n",
        ).unwrap();

        let lanes = load_lanes(&root, Path::new("/nonexistent"), &[]);
        assert!(lanes.is_empty(), "a board.toml-only context must never become a Lane");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn context_board_targets_reads_a_board_only_context_with_no_lane_toml() {
        let root = std::env::temp_dir().join(format!(
            "lanes-config-test-board-targets-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("development");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("board.toml"),
            "driver = \"boards\"\nproject = \"Development\"\nmonitor = \"main\"\nposition = \"full\"\n",
        ).unwrap();

        let targets = context_board_targets(&root);
        assert_eq!(targets.len(), 1);
        let (id, name, target) = &targets[0];
        assert_eq!(id, "development");
        // No lane.toml at all -> falls back to the directory id as the name.
        assert_eq!(name, "development");
        assert!(matches!(&target.driver, crate::model::TargetDriver::Boards { project } if project == "Development"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn all_context_ids_lists_every_context_dir_regardless_of_facets() {
        let root = std::env::temp_dir().join(format!(
            "lanes-config-test-all-context-ids-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("development")).unwrap();
        std::fs::create_dir_all(root.join("infra")).unwrap();
        std::fs::write(root.join("infra").join("lane.toml"), "").unwrap();

        assert_eq!(all_context_ids(&root), vec!["development".to_string(), "infra".to_string()]);

        std::fs::remove_dir_all(&root).ok();
    }
}
