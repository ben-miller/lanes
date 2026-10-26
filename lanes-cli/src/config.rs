use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::model::{BoardFacet, Context, CyclePolicy, Lane, Target, TargetDriver};
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
    /// Context ids to render indented one level, from `contexts.toml`'s
    /// `indented` - purely a rendering cue (Boards' sidebar draws these
    /// one level in). It doesn't reference any other context: there's no
    /// "parent," nothing is assumed about what a reader finds above an
    /// indented context in `order` besides whatever's already there. See
    /// `set_context_indented`.
    pub indented: Vec<String>,
    /// Which candidate list `lanes cycle` steps through - see
    /// `CyclePolicy`'s own doc comment.
    pub cycle_policy: CyclePolicy,
    /// Every context, lane or not - the primary, single-scan view (see
    /// `load_contexts`, `model::Context`). `lanes`/`boards_targets`/
    /// `context_ids` below are cheap derivations of this, kept only so the
    /// many existing callers shaped around "just the lanes" or "just the
    /// boards targets" don't all need rewriting at once - reach for this
    /// field directly in anything new.
    pub contexts: Vec<Context>,
    /// Discovered lane definitions - only contexts whose `lane` facet is
    /// `Some` (see `model::Context`). A context with no `lane.toml` simply
    /// isn't a lane: it never appears here, regardless of what other facets
    /// (`tab.toml`, `board.toml`) it has.
    pub lanes: Vec<Lane>,
    /// Every context's Boards target, derived from `contexts`' `board`
    /// facets - a context doesn't need a `lane.toml` for Boards
    /// materialization/ordering to manage it. `(context id, display name,
    /// target)`; the target's non-`driver` fields (monitor/position/...)
    /// are never meaningful here - nothing reads them off this list, only
    /// `.driver`'s `project` - a lane's own placement already lives in its
    /// `targets` instead.
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
        let indented = load_context_indented();
        // "default" is just a template like any other - it's the one every
        // context implicitly, unconditionally starts its cascade from (no
        // opt-out).
        let templates_dir = config_dir().join("layout-templates");
        let default_targets = load_layout_template(&templates_dir, "default");
        let contexts_dir = config_dir().join("contexts");
        let contexts = load_contexts(&contexts_dir, &templates_dir, &default_targets);
        let mut lanes = derive_lanes(&contexts);
        let boards_targets = derive_boards_targets(&contexts);
        let context_ids = contexts.iter().map(|c| c.id.clone()).collect();
        if let Some(order) = &order {
            sort_by_order(&mut lanes, order);
        }
        Self { drivers, monitors, order, indented, cycle_policy, contexts, lanes, boards_targets, context_ids, shell_ignored_commands }
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

/// Flip a context's `active` flag in its `lane.toml` facet, in place - the
/// cycling-membership bit (see `Lane::active`'s doc comment), distinct from
/// `set_lane_enabled` below (whether the facet is even present at all).
pub fn set_lane_active(context_id: &str, active: bool) -> std::io::Result<()> {
    set_facet_flag(context_id, "lane.toml", "active", active)
}

/// Flip a context's `enabled` flag in its `lane.toml` facet - see
/// `LaneToml`'s doc comment for what disabling it does. Separate key, and a
/// separate command (`lanes ctx disable-lane`/`enable-lane`), from
/// `set_lane_active`/`lanes activate`/`deactivate`: "active" is cycling
/// membership for a lane that's still fully present; "enabled" is whether
/// the lane facet exists at all.
pub fn set_lane_enabled(context_id: &str, enabled: bool) -> std::io::Result<()> {
    set_facet_flag(context_id, "lane.toml", "enabled", enabled)
}

/// Flip a context's `enabled` flag in its `tab.toml` facet - see TabToml's
/// doc comment for what disabling it does.
pub fn set_tab_enabled(context_id: &str, enabled: bool) -> std::io::Result<()> {
    set_facet_flag(context_id, "tab.toml", "enabled", enabled)
}

/// Flip a context's `enabled` flag in its `board.toml` facet - see
/// `read_board_target`'s doc comment for what disabling it does.
pub fn set_board_enabled(context_id: &str, enabled: bool) -> std::io::Result<()> {
    set_facet_flag(context_id, "board.toml", "enabled", enabled)
}

/// Sets `key` to `value` in `<context>/<facet_file>`, in place. Uses
/// toml_edit rather than the parse-only `toml` crate so the rest of the
/// file (formatting, key order, any future comments) is left untouched -
/// these files are hand-maintained. Every facet file here is optional (see
/// `load_contexts`), so a context that's never had this one yet gets a
/// fresh file containing just this one key, not an error.
fn set_facet_flag(context_id: &str, facet_file: &str, key: &str, value: bool) -> std::io::Result<()> {
    let path = config_dir().join("contexts").join(context_id).join(facet_file);
    let mut doc = match std::fs::read_to_string(&path) {
        Ok(text) => text.parse::<toml_edit::DocumentMut>().map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
        })?,
        Err(_) => toml_edit::DocumentMut::new(),
    };
    doc[key] = toml_edit::value(value);
    // Write-then-rename: the Lanes Switch fs-watcher reads this dir the
    // instant it sees a change, and a torn read makes toml parsing fail,
    // which drops the lane from that refresh's snapshot entirely (see
    // load_contexts) - the toggle appears to flicker or not stick. rename(2)
    // is atomic on one filesystem. Same fix as state::atomic_write.
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string())?;
    std::fs::rename(&tmp, &path)
}

/// Appends `id` to `contexts.toml`'s `order` array, in place - creating the
/// file (and the array) if either doesn't exist yet. `lanes ctx <id> add`
/// calls this so a freshly created context is immediately covered by
/// `doctor`'s "context order" check, the same as every other context,
/// rather than silently landing in `unregistered_contexts`. A no-op if
/// `id` is already listed (so this is safe to call more than once).
pub fn append_context_order(id: &str) -> std::io::Result<()> {
    let path = config_dir().join("contexts.toml");
    let mut doc = match std::fs::read_to_string(&path) {
        Ok(text) => text.parse::<toml_edit::DocumentMut>().map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
        })?,
        Err(_) => toml_edit::DocumentMut::new(),
    };
    if doc.get("order").is_none() {
        doc["order"] = toml_edit::Item::Value(toml_edit::Value::Array(toml_edit::Array::new()));
    }
    let array = doc["order"].as_array_mut().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "contexts.toml's `order` is not an array")
    })?;
    if !array.iter().any(|v| v.as_str() == Some(id)) {
        array.push(id);
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string())?;
    std::fs::rename(&tmp, &path)
}

/// Removes `id` from `contexts.toml`'s `order` array, in place - the
/// counterpart to `append_context_order`, used by `lanes ctx <id> remove
/// --delete`. A no-op, not an error, if `id` was never listed, or if
/// `contexts.toml` doesn't exist at all - there's nothing to remove it
/// from either way.
pub fn remove_context_order(id: &str) -> Result<(), String> {
    let path = config_dir().join("contexts.toml");
    if !path.exists() {
        return Ok(());
    }
    edit_context_order(id, |ids| {
        ids.retain(|x| x != id);
        Ok(())
    })
}

/// Adds or removes `id` from `contexts.toml`'s `indented` array, in place -
/// `lanes ctx <id> indent`/`unindent`. Purely a rendering flag (Boards'
/// sidebar draws these one level in); doesn't touch `order` at all, and
/// adding/removing it is independent of whatever else is happening to
/// `order`. A no-op if `id` is already in the requested state.
pub fn set_context_indented(id: &str, indented: bool) -> std::io::Result<()> {
    let path = config_dir().join("contexts.toml");
    let mut doc = match std::fs::read_to_string(&path) {
        Ok(text) => text
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?,
        Err(_) => toml_edit::DocumentMut::new(),
    };
    if doc.get("indented").is_none() {
        doc["indented"] = toml_edit::Item::Value(toml_edit::Value::Array(toml_edit::Array::new()));
    }
    let array = doc["indented"].as_array_mut().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "contexts.toml's `indented` is not an array")
    })?;
    let already = array.iter().any(|v| v.as_str() == Some(id));
    if indented && !already {
        array.push(id);
    } else if !indented && already {
        let pos = array.iter().position(|v| v.as_str() == Some(id)).expect("just checked `already`");
        array.remove(pos);
    } else {
        return Ok(());
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string())?;
    std::fs::rename(&tmp, &path)
}

/// Which neighbor `move_context_order` swaps `id` with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveDirection {
    Up,
    Down,
}

/// What `move_context_order` actually did - distinct from an error, since
/// "already at that end" is a normal, expected outcome (repeatedly
/// pressing "move up" eventually hits the top), not a failure.
#[derive(Debug, PartialEq, Eq)]
pub enum MoveOutcome {
    Moved { swapped_with: String },
    AlreadyFirst,
    AlreadyLast,
}

/// Shared plumbing for every `contexts.toml` `order`-reordering function:
/// reads the array into a plain `Vec<String>`, lets `f` reorder it however
/// it likes (swap, remove+insert, ...), then writes the result back
/// verbatim. `id_for_error` is only used for the "no contexts.toml at all"
/// case, where no id-specific error from `f` has a chance to run yet.
fn edit_context_order<T>(id_for_error: &str, f: impl FnOnce(&mut Vec<String>) -> Result<T, String>) -> Result<T, String> {
    let path = config_dir().join("contexts.toml");
    let text = std::fs::read_to_string(&path)
        .map_err(|_| format!("'{id_for_error}' is not in contexts.toml's order - no contexts.toml found"))?;
    let mut doc = text.parse::<toml_edit::DocumentMut>().map_err(|e| e.to_string())?;
    let array = doc
        .get_mut("order")
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| "contexts.toml has no `order` array".to_string())?;

    let mut ids: Vec<String> = array.iter().filter_map(|v| v.as_str().map(String::from)).collect();
    let result = f(&mut ids)?;

    while !array.is_empty() {
        array.remove(0);
    }
    for id in &ids {
        array.push(id.as_str());
    }

    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;

    Ok(result)
}

/// Swaps `id` with its adjacent neighbor in `contexts.toml`'s `order` -
/// `lanes ctx <id> move up`/`down`. This is the one true order - Boards
/// and `infra zellij`'s tab order both read it directly (see
/// DECISIONS.md), nothing derived from it needs a separate push - so
/// reordering here is reordering everything that matters. For moving
/// further than one step, see `move_context_before`/`move_context_after` -
/// repeatedly calling this one step at a time is what this function used
/// to require, and hand-editing contexts.toml was the only alternative.
pub fn move_context_order(id: &str, direction: MoveDirection) -> Result<MoveOutcome, String> {
    edit_context_order(id, |ids| {
        let pos = ids.iter().position(|x| x == id).ok_or_else(|| format!("'{id}' is not in contexts.toml's order"))?;
        let swap_with = match direction {
            MoveDirection::Up if pos == 0 => return Ok(MoveOutcome::AlreadyFirst),
            MoveDirection::Up => pos - 1,
            MoveDirection::Down if pos + 1 == ids.len() => return Ok(MoveOutcome::AlreadyLast),
            MoveDirection::Down => pos + 1,
        };
        let swapped_with = ids[swap_with].clone();
        ids.swap(pos, swap_with);
        Ok(MoveOutcome::Moved { swapped_with })
    })
}

/// Moves `id` to sit immediately before `before_id` in `contexts.toml`'s
/// `order`, in one jump - whatever their current positions, however far
/// apart. `lanes ctx <id> move before <before_id>`.
pub fn move_context_before(id: &str, before_id: &str) -> Result<(), String> {
    if id == before_id {
        return Err(format!("'{id}' can't move before itself"));
    }
    edit_context_order(id, |ids| {
        let pos = ids.iter().position(|x| x == id).ok_or_else(|| format!("'{id}' is not in contexts.toml's order"))?;
        ids.remove(pos);
        let target = ids
            .iter()
            .position(|x| x == before_id)
            .ok_or_else(|| format!("'{before_id}' is not in contexts.toml's order"))?;
        ids.insert(target, id.to_string());
        Ok(())
    })
}

/// Moves `id` to sit immediately after `after_id` in `contexts.toml`'s
/// `order`, in one jump. `lanes ctx <id> move after <after_id>`.
pub fn move_context_after(id: &str, after_id: &str) -> Result<(), String> {
    if id == after_id {
        return Err(format!("'{id}' can't move after itself"));
    }
    edit_context_order(id, |ids| {
        let pos = ids.iter().position(|x| x == id).ok_or_else(|| format!("'{id}' is not in contexts.toml's order"))?;
        ids.remove(pos);
        let target = ids
            .iter()
            .position(|x| x == after_id)
            .ok_or_else(|| format!("'{after_id}' is not in contexts.toml's order"))?;
        ids.insert(target + 1, id.to_string());
        Ok(())
    })
}

impl Default for Config {
    fn default() -> Self {
        Self {
            drivers: None,
            monitors: HashMap::new(),
            order: None,
            indented: Vec::new(),
            cycle_policy: CyclePolicy::default(),
            contexts: Vec::new(),
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
    #[serde(default)]
    indented: Option<Vec<String>>,
}

/// `<context>/lane.toml` - the switcher-facing facet, and the one thing
/// that makes a context a *lane* at all. A context dir with no `lane.toml`,
/// or one with `enabled = false`, is not a lane - it's invisible to
/// `cfg.lanes`, the dashboard, and cycling entirely, even though its other
/// facets (`tab.toml`, `board.toml`) are still read directly for their own
/// purposes (see `load_contexts`).
///
/// Two independent bits, not one - don't conflate them:
/// - `enabled` (default true): whether this facet exists at all. `false`
///   behaves exactly as if `lane.toml` were absent (see `load_contexts`),
///   but the file's own content (name, whatever else gets added later)
///   stays on disk, so re-enabling restores it. Set via `lanes ctx
///   disable-lane`/`enable-lane`.
/// - `active` (default true): cycling membership for a lane that's still
///   fully present and visible - "is this part of the current working
///   set," same as before this field existed. Set via `lanes
///   activate`/`deactivate`.
///
/// Identity (the old `id` field) isn't part of this - it's the context
/// directory's own name, which can't disagree with itself the way a
/// hand-written `id` field could.
#[derive(Deserialize, Default)]
struct LaneToml {
    name: Option<String>,
    active: Option<bool>,
    enabled: Option<bool>,
}

/// `<context>/tab.toml` - the Zellij/repo anchor (what used to be the one
/// do-everything lane file's `[[scope]]` array). Same human-friendly
/// `ScopeElementRaw` shape as before, just split into its own file.
///
/// `enabled` (default true) disables this facet without deleting it: a
/// disabled tab behaves exactly as if `tab.toml` didn't exist at all (empty
/// scope - see `load_contexts`), but its `scope` entries stay on disk so
/// re-enabling restores them verbatim rather than making you re-author them.
#[derive(Deserialize)]
struct TabToml {
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    scope: Vec<ScopeElementRaw>,
}

impl Default for TabToml {
    fn default() -> Self {
        Self { enabled: true, scope: Vec::new() }
    }
}

fn default_true() -> bool {
    true
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

fn load_context_indented() -> Vec<String> {
    let path = config_dir().join("contexts.toml");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let cfg: ContextsConfig = toml::from_str(&content).unwrap_or_default();
    cfg.indented.unwrap_or_default()
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
/// `layout_template` (`load_contexts`). `templates_dir` is always
/// `config_dir().join("layout-templates")` outside tests - a parameter for
/// the same reason `load_contexts`'s `dir` is (see its doc comment). Absent/
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

/// Just the `enabled` key of a facet file (`board.toml` here), read
/// independently of that file's own `Target`/driver shape so toggling it
/// doesn't need a dedicated field on the shared `Target` struct (which
/// every other target kind in `layout.toml` also uses, and has no such
/// concept). Missing key or missing file both mean "enabled" - see
/// `default_true`.
#[derive(Deserialize, Default)]
struct EnabledFlag {
    #[serde(default = "default_true")]
    enabled: bool,
}

/// `<context>/board.toml` - the Boards projection: which Boards project
/// this context corresponds to, plus optional placement. Kept as its own
/// facet (one `Target`, not a list - a context has at most one board)
/// rather than folded into `layout.toml`, because Boards is architecturally
/// different from the other target drivers: it's generated/materialized
/// (see PLAN-context-model.md), not just another thing to place on switch.
///
/// `enabled` (default true) disables this facet without deleting it: a
/// disabled board behaves exactly as if `board.toml` didn't exist (no
/// `BoardFacet`, nothing folded into the lane's own `targets`), but the
/// file's `project`/placement stay on disk for re-enabling later.
fn read_board_target(path: &Path) -> Option<Target> {
    let content = std::fs::read_to_string(path).ok()?;
    if !toml::from_str::<EnabledFlag>(&content).map(|f| f.enabled).unwrap_or(true) {
        return None;
    }
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
/// other test reading it) is a context; the directory name *is* its id (no
/// separate field to hand-write, and so nothing to disagree with it - see
/// LaneToml's doc comment). Its facets - `lane`, `board` - are each
/// independently present or absent; "lane" is not a synonym for "context,"
/// it's one optional facet a context can have (see `model::Context`'s doc
/// comment). One scan, one read of each file - this used to be two
/// separate scans (`load_lanes`, since removed, requiring `lane.toml` to
/// exist at all; `context_board_targets`, re-reading `board.toml` *and*
/// `lane.toml` again from scratch just to also catch a board-only
/// context), which is exactly the duplication a single `Context` type
/// exists to remove: a lane with a board was read correctly either way,
/// but "read once" beats "read twice and trust they agree."
fn load_contexts(dir: &Path, templates_dir: &Path, default_targets: &[Target]) -> Vec<Context> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut contexts: Vec<Context> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let id = e.file_name().to_string_lossy().into_owned();
            if id.contains(char::is_whitespace) {
                eprintln!("warning: skipping context dir {:?} - the directory name (id) must not contain spaces", id);
                return None;
            }
            let path = e.path();

            let board_target = read_board_target(&path.join("board.toml"));
            let board = board_target.as_ref().and_then(|t| match &t.driver {
                TargetDriver::Boards { project } => Some(BoardFacet { project: project.clone() }),
                _ => None,
            });

            // Presence, not content, is what opts a context into also
            // having a lane facet (an empty `lane.toml` still counts;
            // `read_facet` alone can't tell "file absent" apart from "file
            // present but empty", both deserializing to
            // `LaneToml::default()`, hence the separate existence check) -
            // *unless* `enabled = false`, which behaves exactly as if the
            // file were absent (see `LaneToml`'s doc comment).
            let lane = path.join("lane.toml").is_file().then(|| read_facet(&path.join("lane.toml"))).and_then(|lane_toml: LaneToml| {
                if !lane_toml.enabled.unwrap_or(true) {
                    return None;
                }

                let tab_toml: TabToml = read_facet(&path.join("tab.toml"));
                let layout_toml: LayoutToml = read_facet(&path.join("layout.toml"));

                let layout_template = layout_toml.layout_template.clone();
                let templated_defaults = match &layout_template {
                    Some(name) => merge_targets(default_targets, load_layout_template(templates_dir, name)),
                    None => default_targets.to_vec(),
                };

                let mut own_targets = layout_toml.targets;
                own_targets.extend(board_target.clone());

                // A disabled tab (`enabled: false`) behaves exactly as if
                // tab.toml didn't exist - empty scope - while its `scope`
                // entries stay on disk for re-enabling (see TabToml's doc
                // comment).
                let scope = if tab_toml.enabled {
                    tab_toml.scope.into_iter().map(ScopeElement::from).collect()
                } else {
                    Vec::new()
                };

                Some(Lane {
                    name: lane_toml.name.unwrap_or_else(|| id.clone()),
                    active: lane_toml.active.unwrap_or(true),
                    scope,
                    targets: merge_targets(&templated_defaults, own_targets),
                    layout_template,
                    id: id.clone(),
                })
            });

            Some(Context { id, lane, board })
        })
        .collect();

    contexts.sort_by(|a, b| a.id.cmp(&b.id));
    contexts
}

/// `Config.lanes`, derived from `contexts` (no filesystem access - the
/// data's already in memory from `load_contexts`'s single scan).
fn derive_lanes(contexts: &[Context]) -> Vec<Lane> {
    contexts.iter().filter_map(|c| c.lane.clone()).collect()
}

/// `Config.boards_targets`, derived from `contexts` the same way - see
/// `Config.boards_targets`'s own doc comment for why the placeholder
/// `Target` here only ever needs a correct `driver`. A lane's own name
/// wins; a board-only context has no lane name slot, so it falls back to
/// the board's own `project` (a real human-readable name, not the bare
/// kebab-case id - same fallback order as `cmd::ls::display_name`), which
/// is what keeps `doctor`'s display-name-consistency check from comparing
/// a board-only context's id against board.yaml's title and always losing.
fn derive_boards_targets(contexts: &[Context]) -> Vec<(String, String, Target)> {
    contexts
        .iter()
        .filter_map(|c| {
            let board = c.board.as_ref()?;
            let name = c.lane.as_ref().map(|l| l.name.clone()).unwrap_or_else(|| board.project.clone());
            Some((c.id.clone(), name, boards_only_target(board.project.clone())))
        })
        .collect()
}

/// A `Target` carrying nothing but a `Boards` driver - see
/// `Config.boards_targets`'s doc comment for why every other field is
/// never meaningful on one of these.
fn boards_only_target(project: String) -> Target {
    Target {
        driver: TargetDriver::Boards { project },
        monitor: None,
        position: None,
        position_solo: None,
        launch: false,
        raise: false,
        minimized: None,
        primary: false,
    }
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
            position_solo: None,
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
            position_solo: None,
            launch: false,
            raise: false,
            minimized: None,
            primary: false,
        }
    }

    fn config_with(lanes: Vec<Lane>, order: Option<Vec<&str>>) -> Config {
        let context_ids = lanes.iter().map(|l| l.id.clone()).collect();
        let contexts = lanes.iter().map(|l| Context { id: l.id.clone(), lane: Some(l.clone()), board: None }).collect();
        Config {
            drivers: None,
            monitors: HashMap::new(),
            order: order.map(|o| o.into_iter().map(String::from).collect()),
            indented: Vec::new(),
            cycle_policy: CyclePolicy::default(),
            contexts,
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
        // load_contexts() falls back to the directory name (see its own test
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
    fn parses_layout_toml_with_an_appctl_target_raised_in_place() {
        let content = r#"
[[targets]]
driver = "appctl"
app = "console"
cmd = "show"
raise = true
"#;
        let file: LayoutToml = toml::from_str(content).unwrap();
        assert_eq!(file.targets.len(), 1);
        assert_eq!(file.targets[0].driver.name(), "appctl");
        assert!(file.targets[0].monitor.is_none(), "raised in place - no position at all");
        assert!(file.targets[0].raise);
        assert!(matches!(
            &file.targets[0].driver,
            crate::model::TargetDriver::Appctl { app, cmd, .. } if app == "console" && cmd == "show"
        ));
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
    fn read_board_target_is_none_when_enabled_is_false_even_though_the_driver_is_valid() {
        let dir = std::env::temp_dir().join(format!("lanes-config-test-board-enabled-{}-{:?}", std::process::id(), std::thread::current().id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("board.toml");
        std::fs::write(&path, "enabled = false\ndriver = \"boards\"\nproject = \"infra\"\n").unwrap();
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

    /// Points `config_dir()`'s result at a throwaway tree for the duration
    /// of one `set_facet_active`-based test, via `$HOME` - the one seam
    /// those functions have. Serialized by a lock so parallel tests don't
    /// race the shared `$HOME` env var.
    fn with_fake_home<T>(f: impl FnOnce(&Path) -> T) -> T {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!("lanes-config-test-home-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".config/lanes/contexts")).unwrap();
        let prev = std::env::var("HOME").ok();
        std::env::set_var("HOME", &home);
        let result = f(&home);
        match prev {
            Some(p) => std::env::set_var("HOME", p),
            None => std::env::remove_var("HOME"),
        }
        std::fs::remove_dir_all(&home).ok();
        result
    }

    #[test]
    fn set_tab_enabled_creates_tab_toml_when_absent() {
        with_fake_home(|home| {
            std::fs::create_dir_all(home.join(".config/lanes/contexts/ctx")).unwrap();
            set_tab_enabled("ctx", false).unwrap();
            let text = std::fs::read_to_string(home.join(".config/lanes/contexts/ctx/tab.toml")).unwrap();
            assert!(text.contains("enabled = false"));
        });
    }

    #[test]
    fn set_board_enabled_preserves_other_keys() {
        with_fake_home(|home| {
            let dir = home.join(".config/lanes/contexts/ctx");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("board.toml"), "driver = \"boards\"\nproject = \"infra\"\n").unwrap();
            set_board_enabled("ctx", false).unwrap();
            let text = std::fs::read_to_string(dir.join("board.toml")).unwrap();
            assert!(text.contains("enabled = false"));
            assert!(text.contains("project = \"infra\""));
        });
    }

    #[test]
    fn set_lane_enabled_is_independent_of_set_lane_active() {
        with_fake_home(|home| {
            let dir = home.join(".config/lanes/contexts/ctx");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("lane.toml"), "name = \"Ctx\"\nactive = true\n").unwrap();
            set_lane_enabled("ctx", false).unwrap();
            let text = std::fs::read_to_string(dir.join("lane.toml")).unwrap();
            assert!(text.contains("enabled = false"));
            // The unrelated `active` (cycling membership) key is untouched.
            assert!(text.contains("active = true"));
        });
    }

    #[test]
    fn append_context_order_creates_contexts_toml_when_absent() {
        with_fake_home(|home| {
            append_context_order("new-ctx").unwrap();
            let text = std::fs::read_to_string(home.join(".config/lanes/contexts.toml")).unwrap();
            let cfg: ContextsConfig = toml::from_str(&text).unwrap();
            assert_eq!(cfg.order, Some(vec!["new-ctx".to_string()]));
        });
    }

    #[test]
    fn append_context_order_preserves_existing_entries_and_their_order() {
        with_fake_home(|home| {
            std::fs::write(home.join(".config/lanes/contexts.toml"), "order = [\"a\", \"b\"]\n").unwrap();
            append_context_order("c").unwrap();
            let text = std::fs::read_to_string(home.join(".config/lanes/contexts.toml")).unwrap();
            let cfg: ContextsConfig = toml::from_str(&text).unwrap();
            assert_eq!(cfg.order, Some(vec!["a".to_string(), "b".to_string(), "c".to_string()]));
        });
    }

    #[test]
    fn append_context_order_is_a_noop_when_the_id_is_already_listed() {
        with_fake_home(|home| {
            std::fs::write(home.join(".config/lanes/contexts.toml"), "order = [\"a\", \"b\"]\n").unwrap();
            append_context_order("a").unwrap();
            let text = std::fs::read_to_string(home.join(".config/lanes/contexts.toml")).unwrap();
            let cfg: ContextsConfig = toml::from_str(&text).unwrap();
            assert_eq!(cfg.order, Some(vec!["a".to_string(), "b".to_string()]));
        });
    }

    fn read_order(home: &Path) -> Vec<String> {
        let text = std::fs::read_to_string(home.join(".config/lanes/contexts.toml")).unwrap();
        toml::from_str::<ContextsConfig>(&text).unwrap().order.unwrap()
    }

    #[test]
    fn move_context_order_up_swaps_with_the_previous_entry() {
        with_fake_home(|home| {
            std::fs::write(home.join(".config/lanes/contexts.toml"), "order = [\"a\", \"b\", \"c\"]\n").unwrap();
            let outcome = move_context_order("b", MoveDirection::Up).unwrap();
            assert_eq!(outcome, MoveOutcome::Moved { swapped_with: "a".to_string() });
            assert_eq!(read_order(home), vec!["b".to_string(), "a".to_string(), "c".to_string()]);
        });
    }

    #[test]
    fn move_context_order_down_swaps_with_the_next_entry() {
        with_fake_home(|home| {
            std::fs::write(home.join(".config/lanes/contexts.toml"), "order = [\"a\", \"b\", \"c\"]\n").unwrap();
            let outcome = move_context_order("b", MoveDirection::Down).unwrap();
            assert_eq!(outcome, MoveOutcome::Moved { swapped_with: "c".to_string() });
            assert_eq!(read_order(home), vec!["a".to_string(), "c".to_string(), "b".to_string()]);
        });
    }

    #[test]
    fn move_context_order_up_is_a_noop_when_already_first() {
        with_fake_home(|home| {
            std::fs::write(home.join(".config/lanes/contexts.toml"), "order = [\"a\", \"b\"]\n").unwrap();
            let outcome = move_context_order("a", MoveDirection::Up).unwrap();
            assert_eq!(outcome, MoveOutcome::AlreadyFirst);
            assert_eq!(read_order(home), vec!["a".to_string(), "b".to_string()]);
        });
    }

    #[test]
    fn move_context_order_down_is_a_noop_when_already_last() {
        with_fake_home(|home| {
            std::fs::write(home.join(".config/lanes/contexts.toml"), "order = [\"a\", \"b\"]\n").unwrap();
            let outcome = move_context_order("b", MoveDirection::Down).unwrap();
            assert_eq!(outcome, MoveOutcome::AlreadyLast);
            assert_eq!(read_order(home), vec!["a".to_string(), "b".to_string()]);
        });
    }

    #[test]
    fn move_context_order_errors_when_the_id_is_not_in_order() {
        with_fake_home(|home| {
            std::fs::write(home.join(".config/lanes/contexts.toml"), "order = [\"a\", \"b\"]\n").unwrap();
            assert!(move_context_order("nope", MoveDirection::Up).is_err());
        });
    }

    #[test]
    fn move_context_before_jumps_from_the_end_to_the_front_in_one_call() {
        with_fake_home(|home| {
            std::fs::write(home.join(".config/lanes/contexts.toml"), "order = [\"a\", \"b\", \"c\", \"admin\"]\n").unwrap();
            move_context_before("admin", "a").unwrap();
            assert_eq!(read_order(home), vec!["admin".to_string(), "a".to_string(), "b".to_string(), "c".to_string()]);
        });
    }

    #[test]
    fn move_context_after_jumps_from_the_front_to_the_end_in_one_call() {
        with_fake_home(|home| {
            std::fs::write(home.join(".config/lanes/contexts.toml"), "order = [\"a\", \"b\", \"c\"]\n").unwrap();
            move_context_after("a", "c").unwrap();
            assert_eq!(read_order(home), vec!["b".to_string(), "c".to_string(), "a".to_string()]);
        });
    }

    #[test]
    fn move_context_before_errors_when_moving_before_itself() {
        with_fake_home(|home| {
            std::fs::write(home.join(".config/lanes/contexts.toml"), "order = [\"a\", \"b\"]\n").unwrap();
            assert!(move_context_before("a", "a").is_err());
        });
    }

    #[test]
    fn move_context_before_errors_when_the_target_id_is_unknown() {
        with_fake_home(|home| {
            std::fs::write(home.join(".config/lanes/contexts.toml"), "order = [\"a\", \"b\"]\n").unwrap();
            assert!(move_context_before("a", "nope").is_err());
            // Unchanged on error - the removal shouldn't stick if the
            // reinsertion target can't be found.
            assert_eq!(read_order(home), vec!["a".to_string(), "b".to_string()]);
        });
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
            indented: Vec::new(),
            cycle_policy: CyclePolicy::default(),
            contexts: Vec::new(),
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
            indented: Vec::new(),
            cycle_policy: CyclePolicy::default(),
            contexts: Vec::new(),
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
            indented: Vec::new(),
            cycle_policy: CyclePolicy::default(),
            contexts: Vec::new(),
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
    fn load_contexts_reads_a_real_directory_tree_end_to_end() {
        let root = std::env::temp_dir().join(format!(
            "lanes-config-test-load-contexts-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);

        // A fully-specified context: all three facet files.
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

        // A bare context: just a directory, no facet files at all - no
        // lane, no board, but still a context (it has an id).
        std::fs::create_dir_all(root.join("placeholder")).unwrap();

        let contexts = load_contexts(&root, Path::new("/nonexistent"), &[]);
        assert_eq!(contexts.len(), 2);

        let infra_ctx = contexts.iter().find(|c| c.id == "infra").unwrap();
        let infra_lane = infra_ctx.lane.as_ref().expect("infra has a lane.toml");
        assert_eq!(infra_lane.name, "Infra");
        assert!(!infra_lane.active);
        assert_eq!(infra_lane.scope.len(), 1);
        assert_eq!(infra_lane.targets.len(), 1);
        assert!(matches!(&infra_lane.targets[0].driver, crate::model::TargetDriver::Boards { project } if project == "Infra"));
        assert_eq!(infra_ctx.board.as_ref().map(|b| b.project.as_str()), Some("Infra"));

        let placeholder = contexts.iter().find(|c| c.id == "placeholder").unwrap();
        assert!(placeholder.lane.is_none());
        assert!(placeholder.board.is_none());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_contexts_an_empty_lane_toml_still_defaults_active_true() {
        // The file's mere *presence* is what opts a context into having a
        // lane facet, regardless of whether it sets anything - an empty
        // lane.toml is enough.
        let root = std::env::temp_dir().join(format!(
            "lanes-config-test-empty-lane-toml-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("ctx");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lane.toml"), "").unwrap();

        let contexts = load_contexts(&root, Path::new("/nonexistent"), &[]);
        assert_eq!(contexts.len(), 1);
        assert!(contexts[0].lane.as_ref().unwrap().active);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_contexts_a_disabled_lane_has_no_lane_facet_at_all_unlike_merely_inactive() {
        let root = std::env::temp_dir().join(format!(
            "lanes-config-test-disabled-lane-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);

        // `active = false` (cycling membership) still produces a real,
        // visible lane facet - the pre-existing behavior.
        let inactive = root.join("inactive");
        std::fs::create_dir_all(&inactive).unwrap();
        std::fs::write(inactive.join("lane.toml"), "name = \"Inactive\"\nactive = false\n").unwrap();

        // `enabled = false` behaves exactly as if lane.toml didn't exist -
        // no lane facet at all, not even an inactive one.
        let disabled = root.join("disabled");
        std::fs::create_dir_all(&disabled).unwrap();
        std::fs::write(disabled.join("lane.toml"), "name = \"Disabled\"\nenabled = false\n").unwrap();

        let contexts = load_contexts(&root, Path::new("/nonexistent"), &[]);
        assert!(contexts.iter().find(|c| c.id == "inactive").unwrap().lane.is_some());
        assert!(contexts.iter().find(|c| c.id == "disabled").unwrap().lane.is_none());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_contexts_a_disabled_tab_has_empty_scope_but_an_enabled_lane() {
        let root = std::env::temp_dir().join(format!(
            "lanes-config-test-disabled-tab-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("ctx");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lane.toml"), "").unwrap();
        std::fs::write(dir.join("tab.toml"), "enabled = false\n\n[[scope]]\nkind = \"repo\"\npath = \"~/src/infra\"\n").unwrap();

        let contexts = load_contexts(&root, Path::new("/nonexistent"), &[]);
        let lane = contexts[0].lane.as_ref().unwrap();
        // The tab facet is disabled, so its scope behaves as if tab.toml
        // doesn't exist - it must not leak through even though the file
        // still has real entries on disk.
        assert!(lane.scope.is_empty());
        // Disabling the tab facet is independent of the lane facet itself.
        assert!(lane.active);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_contexts_a_disabled_board_has_no_board_facet_and_no_folded_in_target() {
        let root = std::env::temp_dir().join(format!(
            "lanes-config-test-disabled-board-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("ctx");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lane.toml"), "").unwrap();
        std::fs::write(dir.join("board.toml"), "enabled = false\ndriver = \"boards\"\nproject = \"infra\"\n").unwrap();

        let contexts = load_contexts(&root, Path::new("/nonexistent"), &[]);
        assert!(contexts[0].board.is_none());
        let lane = contexts[0].lane.as_ref().unwrap();
        assert!(lane.targets.iter().all(|t| !matches!(t.driver, TargetDriver::Boards { .. })));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_contexts_board_toml_with_no_enabled_key_defaults_enabled() {
        let root = std::env::temp_dir().join(format!(
            "lanes-config-test-board-default-enabled-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("ctx");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("board.toml"), "driver = \"boards\"\nproject = \"infra\"\n").unwrap();

        let contexts = load_contexts(&root, Path::new("/nonexistent"), &[]);
        assert_eq!(contexts[0].board.as_ref().map(|b| b.project.as_str()), Some("infra"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_contexts_skips_a_directory_name_with_whitespace() {
        let root = std::env::temp_dir().join(format!(
            "lanes-config-test-whitespace-id-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("has space")).unwrap();
        std::fs::create_dir_all(root.join("fine")).unwrap();
        std::fs::write(root.join("fine").join("lane.toml"), "").unwrap();

        let contexts = load_contexts(&root, Path::new("/nonexistent"), &[]);
        assert_eq!(contexts.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), vec!["fine"]);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_contexts_cascades_through_a_layout_template() {
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
        let contexts = load_contexts(&contexts_dir, &templates_dir, &default_targets);
        assert_eq!(contexts.len(), 1);
        let targets = &contexts[0].lane.as_ref().unwrap().targets;

        let wezterm = targets.iter().find(|t| matches!(t.driver, crate::model::TargetDriver::Wezterm { .. })).unwrap();
        assert_eq!(wezterm.monitor.as_deref(), Some("lg-left"), "the template should override the global default");

        let boards = targets.iter().find(|t| matches!(&t.driver, crate::model::TargetDriver::App { name, .. } if name == "Boards")).unwrap();
        assert_eq!(boards.monitor.as_deref(), Some("main"), "the context's own target should override the template");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_contexts_uses_only_the_default_targets_when_no_layout_template_is_set() {
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
        let contexts = load_contexts(&contexts_dir, Path::new("/nonexistent"), &default_targets);
        assert_eq!(contexts.len(), 1);
        let lane = contexts[0].lane.as_ref().unwrap();
        assert_eq!(lane.targets.len(), 1);
        assert_eq!(lane.targets[0].monitor.as_deref(), Some("lg-right"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_contexts_a_board_only_context_has_no_lane_facet_but_is_still_a_context() {
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

        let contexts = load_contexts(&root, Path::new("/nonexistent"), &[]);
        assert_eq!(contexts.len(), 1, "a board.toml-only directory is still a context");
        let ctx = &contexts[0];
        assert_eq!(ctx.id, "development");
        assert!(ctx.lane.is_none(), "a board.toml-only context must never become a Lane");
        assert_eq!(ctx.board.as_ref().map(|b| b.project.as_str()), Some("Development"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn derive_boards_targets_uses_the_board_project_not_the_bare_id_for_a_board_only_context() {
        // Regression: this used to fall back to the bare, kebab-case
        // context id (e.g. "app-ideas") instead of the board's own real
        // name, which made `doctor`'s display-name-consistency check
        // spuriously flag every board-only context against its own
        // board.yaml title.
        let contexts = vec![Context { id: "app-ideas".to_string(), lane: None, board: Some(BoardFacet { project: "App Ideas".to_string() }) }];
        let boards_targets = derive_boards_targets(&contexts);
        assert_eq!(boards_targets.len(), 1);
        assert_eq!(boards_targets[0].1, "App Ideas");
    }
}
