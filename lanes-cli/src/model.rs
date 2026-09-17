use serde::{Deserialize, Serialize};

use crate::scope::ScopeElement;

// --- Lane config types ---

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Lane {
    pub id: String,
    pub name: String,
    // Whether this lane is part of the current working set at all, distinct
    // from `focused_lane` (which one you're looking at right now) - a lane
    // can be inactive while still being the one recorded as focused, since
    // nothing forces a jump away from it on deactivation (see
    // gather_lanes/state::read_focused_lane). Defaults to true so existing
    // lane files without this field keep behaving exactly as before.
    #[serde(default = "default_true")]
    pub active: bool,
    #[serde(default)]
    pub scope: Vec<ScopeElement>,
    // Targets aren't part of scope - unlike everything else here, they have
    // no observable state and nothing to navigate to, they're a pure
    // imperative action ("activate this, optionally place it") triggered on
    // lane focus. Doesn't fit the scope/observation model, so it stays its
    // own thing rather than being forced into a ScopeElement kind with no
    // observations and no real locator identity. See PLAN-window-targets.md.
    #[serde(default)]
    pub targets: Vec<Target>,
}

fn default_true() -> bool {
    true
}

impl Lane {
    pub fn display_name(&self) -> &str {
        &self.name
    }

    pub fn terminal_session(&self) -> Option<&str> {
        self.scope.iter().find_map(ScopeElement::zellij_session_name)
    }
}

/// One thing a lane wants activated (made frontmost/focused) on lane focus,
/// and optionally placed on a monitor afterward. Every driver is the same
/// shape - a name plus that driver's own fields - so adding a new app means
/// adding one driver, never new config syntax. See PLAN-window-targets.md
/// (in the lanes repo root) for the full design rationale.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Target {
    #[serde(flatten)]
    pub driver: TargetDriver,
    /// Monitor handle, resolved against `[monitors.*]` in `~/.config/lanes.toml`.
    /// None if this target only activates, no placement.
    #[serde(default)]
    pub monitor: Option<String>,
    /// Passed straight through to lanes-wm's `apply` as the `position`
    /// field, unexamined - a preset string or a `{cols, col, ...}` grid
    /// span (see lanes-wm's README). lanes-cli has no opinion on what a
    /// position means, only lanes-wm does.
    #[serde(default)]
    pub position: Option<toml::Value>,
    /// Whether to launch the app first if it isn't already running.
    /// Deliberately opt-in, not automatic - see PLAN-window-targets.md's
    /// "App-not-running behavior".
    #[serde(default)]
    pub launch: bool,
    /// Force raising this target's window even though it also has a
    /// placement - the default (raise only when there's no placement)
    /// assumes a placed target doesn't need to be seen right away, which
    /// is true for WezTerm/Firefox but not for a "peek" app whose entire
    /// purpose is to be glanced at.
    #[serde(default)]
    pub raise: bool,
}

/// Which app-specific mechanism activates this target, and that
/// mechanism's own identifying fields. Adding an app means adding one
/// variant here, never inventing new top-level config syntax.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "driver", rename_all = "kebab-case")]
pub enum TargetDriver {
    /// Activate a cached WezTerm tab by Zellij session name - same
    /// mechanism `[[scope]]`'s Terminal facet already uses. `session`
    /// defaults to the owning lane's own terminal session when omitted -
    /// what makes a single global default target (e.g. "WezTerm always
    /// goes to lg-right") meaningful across lanes with different sessions.
    Wezterm {
        #[serde(default)]
        session: Option<String>,
    },
    /// Focus a specific pane in a Zellij session (defaults to the
    /// leftmost/topmost pane if `pane` is omitted). `session` defaults the
    /// same way `Wezterm`'s does.
    Zellij {
        #[serde(default)]
        session: Option<String>,
        #[serde(default)]
        pane: Option<u32>,
    },
    /// Open a specific Obsidian vault via its `obsidian://` URI.
    Obsidian { vault: String },
    /// Open a repo's working copy in SourceTree (`stree`, run from the
    /// repo's own directory).
    Sourcetree { repo: String },
    /// Open (or reuse the existing window for) a folder in VS Code.
    Vscode { folder: String },
    /// Bring the named app forward (`open -a <name>`). `bundle_id` is
    /// optional and only needed if this target also wants placement
    /// (`monitor`/`position`) - without it, this is activate-only, no
    /// window/tab addressing at all, deliberately the ceiling for apps
    /// without one of the drivers above (see PLAN-window-targets.md).
    App {
        name: String,
        #[serde(default)]
        bundle_id: Option<String>,
    },
    /// Switch to a specific Firefox profile - addressed by the name you
    /// gave it in Firefox's own profile switcher (Firefox's newer built-in
    /// "Profiles" feature, not the legacy -P/profiles.ini system). Since
    /// every profile shares the same bundle ID, this resolves to a
    /// specific PID (matching a running process's `--profile <path>`
    /// launch argument, or launching one if none is running) rather than
    /// going through the bundle-ID-based drivers above.
    FirefoxProfile { profile: String },
}

impl TargetDriver {
    /// The `driver` string as written in config - used for display
    /// (`FacetSnapshot::Target`) rather than re-deriving it from the enum
    /// variant's Debug output.
    pub fn name(&self) -> &'static str {
        match self {
            TargetDriver::Wezterm { .. } => "wezterm",
            TargetDriver::Zellij { .. } => "zellij",
            TargetDriver::Obsidian { .. } => "obsidian",
            TargetDriver::Sourcetree { .. } => "sourcetree",
            TargetDriver::Vscode { .. } => "vscode",
            TargetDriver::App { .. } => "app",
            TargetDriver::FirefoxProfile { .. } => "firefox-profile",
        }
    }
}

// --- Signals ---

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignalAction {
    SwitchClaudeSession { session_id: String },
    FocusRepoPane { session: String, path: String },
    /// Focus a specific Zellij pane by its numeric id (the same id space
    /// `zellij action list-panes` reports and the shell hook captures from
    /// `$ZELLIJ_PANE_ID`). Used by Command-kind signals to jump to the pane
    /// a long-running command finished in.
    FocusPane { session: String, pane: u32 },
}

/// Which domain a signal is about. Not just a tag - it's the type that
/// actually owns which reasons are valid, so a `Repo` signal carrying a
/// `Permission` reason (nonsensical - permission prompts are a Claude
/// concept) is a compile error, not a discipline problem. `SignalReason`
/// wraps one of these three per-domain reason enums (adjacently tagged: see
/// its own doc comment for why the wire shape is unaffected by any of
/// this). `Lanes` is distinct from the other two: it's Lanes reporting a
/// fact about its own tracking (e.g. a lane whose Zellij session has no
/// cached WezTerm tab, or isn't running at all), not relaying an
/// observation from an external tool the way ClaudeSession/Repo signals do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalKind {
    ClaudeSession,
    Repo,
    Lanes,
    /// A foreground shell command that ran long enough to be worth
    /// surfacing (see the `shell` driver). Distinct from the tool-named
    /// kinds: the data comes from a shell hook Lanes itself ships, not from
    /// adapting to an external tool's interface.
    Command,
}

/// How a signal's existence is governed - the axis that decides whether a
/// user can dismiss it and what makes it go away. See PLAN-shell-signal.md.
///
/// - `Live`: the signal mirrors a condition that is true *right now*
///   (a running Claude session, a dirty repo, a missing zellij session). It
///   clears itself when the condition ends and cannot be dismissed - hiding
///   it would misreport reality. Every signal was implicitly this before
///   `Latched` existed.
/// - `Latched`: the signal marks that something *happened* and hasn't been
///   dealt with (a command finished or failed). It never auto-clears; it
///   goes away when the user dismisses it, or when it's superseded (the
///   next command in that pane). Dismissal is one-way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Live,
    Latched,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Signal {
    #[serde(flatten)]
    pub reason: SignalReason,
    pub urgency: Urgency,
    // Set from `reason` at construction (see `Signal::new`); never a
    // placeholder the way `cyclable`/`visible` are. `Latched` signals are
    // the only dismissable ones and the only ones carrying `dismiss_id`.
    pub lifecycle: Lifecycle,
    // Whether this signal renders as a chip in the dashboard's normal
    // (non-edit) view. Not the same question as `cyclable`: a running
    // command is visible but not a cycle target. Placeholder at
    // construction, corrected in gather_lanes() alongside `cyclable`.
    pub visible: bool,
    // Stable per-occurrence id for a `Latched` signal, echoed back by the
    // UI's dismiss control (see state::set_signal_dismissed). `None` for
    // `Live` signals, which can't be dismissed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dismiss_id: Option<String>,
    // Whether this specific signal is something `sessions next`/`prev`
    // would actually land on - see lib.rs's signal_cyclable(). Not known at
    // construction time (signal_for() builds signals before a lane's
    // reachability is resolved), so every construction site sets this to
    // `false` as a placeholder; gather_lanes() corrects it once the lane's
    // own cyclable fact is known. Never trust this field on a Signal that
    // didn't pass through that correction pass.
    pub cyclable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<SignalAction>,
    // Static, human-readable context about this specific signal instance -
    // orthogonal to `action` (that's "is there something to click," this is
    // "is there something worth explaining"), and unrelated to the
    // frontend's own `status` concept (the *outcome* of actually invoking
    // `action`, computed client-side, never sent from here). Most reasons
    // don't need one - a "claude · idle" chip's label already says
    // everything relevant. Reserved for reasons where the label alone
    // doesn't say enough to be useful (e.g. Lanes::SessionMissing/
    // SessionNotRunning naming which Zellij session was expected).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Signal {
    pub fn kind(&self) -> SignalKind {
        self.reason.kind()
    }

    /// Build a signal from its reason plus optional action/detail. `urgency`
    /// and `lifecycle` follow from the reason; `cyclable`/`visible` are left
    /// as placeholders for gather_lanes()'s correction pass to set once the
    /// lane's own facts are known.
    pub fn new(reason: SignalReason, action: Option<SignalAction>, detail: Option<String>) -> Self {
        Signal {
            urgency: reason.urgency(),
            lifecycle: reason.lifecycle(),
            reason,
            cyclable: false,
            visible: false,
            dismiss_id: None,
            action,
            detail,
        }
    }
}

/// One reason per domain, namespaced so e.g. ClaudeSessionReason::Active and
/// a hypothetical future RepoReason::Active could never be confused for each
/// other - each domain's vocabulary lives in its own enum, closed to just
/// the reasons that actually make sense there.
///
/// Adjacently tagged (`tag = "kind", content = "reason"`) specifically so
/// the wire shape is unaffected by this being nested internally: serializing
/// `SignalReason::ClaudeSession(ClaudeSessionReason::Active)` produces
/// `{"kind": "claude_session", "reason": "active"}` - the exact same two
/// sibling string fields a flat enum would have produced. `Signal` flattens
/// this field in so those two keys sit directly on the outer signal object,
/// same as before. Existing consumers (the frontend's `signal.kind`/
/// `signal.reason`) don't need to know any of this changed.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "reason", rename_all = "snake_case")]
pub enum SignalReason {
    ClaudeSession(ClaudeSessionReason),
    Repo(RepoReason),
    Lanes(LanesReason),
    Command(CommandReason),
}

/// A long-running foreground shell command, past its finish. `Running` is
/// deliberately absent for now - Phase 2 (see PLAN-shell-signal.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandReason {
    /// Exited 0 after running longer than the latch threshold.
    Done,
    /// Exited non-zero (a signal-terminated command - Ctrl-C'd server -
    /// never produces a record at all, so this is a genuine failure).
    Failed,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeSessionReason {
    Active,
    Awaiting,
    Permission,
    /// Never constructed directly by signal_for() - lane cyclability isn't
    /// known yet at that point in the pipeline. Only ever synthesized
    /// downstream, by gather_lanes() upgrading an Awaiting signal once its
    /// lane turns out cyclable (see lib.rs's upgrade_awaiting_to_ready) -
    /// same precedent as `cyclable` itself being a correction-pass-only
    /// fact. An idle Claude session that's actually part of the cycling
    /// rotation, as opposed to one sitting idle off to the side.
    Ready,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepoReason {
    PendingCommit,
    /// The checked-out branch isn't the repo's actual default (origin/HEAD,
    /// or a local main/master fallback - see lib.rs's git_default_branch).
    /// Only ever produced when they genuinely differ - there's no config
    /// toggle to show/hide this per se, it's just never generated at all
    /// when you're already on the default branch.
    NonDefaultBranch,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanesReason {
    /// A lane that's active but whose Zellij session has no cached WezTerm
    /// tab, so there's nothing for a switch to actually land on.
    SessionMissing,
    /// A lane whose declared Zellij session isn't a running process at all
    /// - distinct from SessionMissing (a WezTerm-tab-caching fact). Fires
    /// regardless of the lane's active state: an inactive lane with no
    /// session is the common case (you tore the environment down), an
    /// active one is more surprising, but both are the same underlying
    /// fact and get the same ambient Info urgency - it's a status chip to
    /// One tier below Blocking (Warning, not Attention) - a lane missing
    /// its whole terminal session is more concerning than "worth a look
    /// when convenient" (Attention/ready-green's actual meaning), but nothing
    /// here is itself waiting on you the way a permission prompt is.
    SessionNotRunning,
}

// Declared least to most urgent so derived Ord/PartialOrd rank them
// correctly (Blocking > Warning > Attention > Info) - lets callers compare
// or sort signals by urgency without a separate ranking table.
//
// This is deliberately the naive case: one reason maps to exactly one fixed
// urgency (see SignalReason::urgency() below), with no awareness of other
// signals, staleness, or lane context. A fuller version of this - urgency as
// a function of the whole signal set plus outside context (elapsed time,
// how many other signals are competing for attention, etc.) - is a real
// design problem for later, not solved here.
//
// Four tiers, not three: Attention was deliberately redefined to mean
// "ready for you, a good state" (green) rather than "caution" - idle Claude
// and a pending commit both live there. That reinterpretation left no home
// for the classic "something's off, not blocking" case, which is exactly
// what SessionNotRunning is - Warning fills that gap as its own tier
// (orange) rather than overloading Attention's now-positive meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Urgency {
    Info,
    Attention,
    Warning,
    Blocking,
}

impl SignalReason {
    pub fn kind(&self) -> SignalKind {
        match self {
            SignalReason::ClaudeSession(_) => SignalKind::ClaudeSession,
            SignalReason::Repo(_) => SignalKind::Repo,
            SignalReason::Lanes(_) => SignalKind::Lanes,
            SignalReason::Command(_) => SignalKind::Command,
        }
    }

    /// See `Lifecycle`. Every existing reason is `Live`; only a finished or
    /// failed command latches.
    pub fn lifecycle(&self) -> Lifecycle {
        match self {
            SignalReason::Command(CommandReason::Done | CommandReason::Failed) => Lifecycle::Latched,
            _ => Lifecycle::Live,
        }
    }

    pub fn urgency(&self) -> Urgency {
        match self {
            SignalReason::ClaudeSession(ClaudeSessionReason::Permission) => Urgency::Blocking,
            SignalReason::ClaudeSession(ClaudeSessionReason::Awaiting) => Urgency::Attention,
            // Same tier as Awaiting for now - Ready is a data-model
            // distinction (is this session part of the cycling rotation),
            // not yet a severity one. The existing cyclable-driven opacity
            // dimming already separates the two visually as a byproduct.
            SignalReason::ClaudeSession(ClaudeSessionReason::Ready) => Urgency::Attention,
            SignalReason::ClaudeSession(ClaudeSessionReason::Active) => Urgency::Info,
            SignalReason::Repo(RepoReason::PendingCommit) => Urgency::Attention,
            // Warning, not Attention/"ready" - Attention's redefined
            // meaning in this palette is "positive, ready for you," which
            // fits uncommitted work fine but not "you might be on the
            // wrong branch." Same tier as SessionNotRunning: worth
            // noticing, not blocking anything.
            SignalReason::Repo(RepoReason::NonDefaultBranch) => Urgency::Warning,
            SignalReason::Lanes(LanesReason::SessionMissing) => Urgency::Blocking,
            SignalReason::Lanes(LanesReason::SessionNotRunning) => Urgency::Warning,
            // Done is a "ready for you, go look" state, same green tier as a
            // pending commit or an idle Claude session. A failed command is
            // "something's off," one tier up but not blocking you the way a
            // permission prompt is - same tier as a non-default branch.
            SignalReason::Command(CommandReason::Done) => Urgency::Attention,
            SignalReason::Command(CommandReason::Failed) => Urgency::Warning,
        }
    }
}

// --- Pane kinds ---

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PaneKind {
    Shell,
    ClaudeSession { awaiting: bool },
    Editor,
    Other { command: String },
}

impl PaneKind {
    pub fn from_command(cmd: Option<&str>) -> Self {
        match cmd {
            None | Some("fish") | Some("bash") | Some("zsh") | Some("sh") => PaneKind::Shell,
            Some("claude") => PaneKind::ClaudeSession { awaiting: false },
            Some("nvim") | Some("hx") | Some("vim") | Some("emacs") | Some("nano") => PaneKind::Editor,
            Some(other) => PaneKind::Other { command: other.to_string() },
        }
    }
}

#[derive(Clone, Serialize)]
pub struct PaneSnapshot {
    pub focused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(flatten)]
    pub kind: PaneKind,
}

// --- Lane snapshot (runtime state per lane) ---

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FacetSnapshot {
    Terminal {
        session: String,
        running: bool,
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        panes: Vec<PaneSnapshot>,
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        signals: Vec<Signal>,
    },
    Target { driver: String },
    Repo { path: String, signals: Vec<Signal> },
}

impl FacetSnapshot {
    pub fn signals(&self) -> &[Signal] {
        match self {
            FacetSnapshot::Terminal { signals, .. } => signals,
            FacetSnapshot::Repo { signals, .. } => signals,
            _ => &[],
        }
    }
}

#[derive(Clone, Serialize)]
pub struct LaneSnapshot {
    pub id: String,
    pub name: String,
    pub active: bool,
    // Whether this lane would actually be visited by `sessions next`/`prev`
    // right now - active, reachable, and hosting at least one live Claude
    // session. Computed in gather_lanes() via the same
    // reachable_lane_decision() cycle_claude_session's own filter uses, not
    // a UI-side guess re-derived from `active`/`facets` - see that
    // function's doc comment.
    pub cyclable: bool,
    pub facets: Vec<FacetSnapshot>,
}

impl LaneSnapshot {
    pub fn has_signals(&self) -> bool {
        self.facets.iter().any(|f| !f.signals().is_empty())
    }
}

#[derive(Clone, Serialize)]
pub struct LanewiseSnapshot {
    pub taken_at: String,
    pub lanes: Vec<LaneSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focused_lane: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focused_claude_session: Option<String>,
}

// --- Shapes (observed current arrangement) ---

#[derive(Clone, Serialize, Deserialize)]
pub struct PaneInfo {
    pub command: Option<String>,
    pub focused: bool,
    pub cwd: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct TabInfo {
    pub name: String,
    pub focused: bool,
    pub panes: Vec<PaneInfo>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct TerminalShape {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub tabs: Vec<TabInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_reason_flattens_into_flat_kind_and_reason_on_signal() {
        // The whole point of #[serde(flatten)] on Signal::reason: even
        // though SignalReason is internally nested (adjacently tagged), a
        // Signal on the wire still has plain sibling "kind"/"reason" string
        // fields, not a nested {"reason": {"kind": ..., "reason": ...}}
        // object - existing consumers (the frontend) don't see this
        // refactor at all.
        let mut signal = Signal::new(
            SignalReason::ClaudeSession(ClaudeSessionReason::Awaiting),
            None,
            Some("waiting for your input".to_string()),
        );
        signal.cyclable = true;
        let json = serde_json::to_value(&signal).unwrap();
        assert_eq!(json["kind"], "claude_session");
        assert_eq!(json["reason"], "awaiting");
        assert_eq!(json["urgency"], "attention");
        assert_eq!(json["cyclable"], true);
        assert_eq!(json["detail"], "waiting for your input");
        assert!(json.get("action").is_none());
    }

    #[test]
    fn signal_omits_detail_from_json_when_none() {
        let signal = Signal::new(SignalReason::Repo(RepoReason::PendingCommit), None, None);
        let json = serde_json::to_value(&signal).unwrap();
        assert!(json.get("detail").is_none());
    }

    #[test]
    fn signal_kind_matches_the_reason_it_wraps() {
        assert_eq!(
            Signal::new(SignalReason::Repo(RepoReason::PendingCommit), None, None).kind(),
            SignalKind::Repo
        );
        assert_eq!(
            Signal::new(SignalReason::Lanes(LanesReason::SessionNotRunning), None, None).kind(),
            SignalKind::Lanes
        );
        assert_eq!(
            Signal::new(SignalReason::Command(CommandReason::Failed), None, None).kind(),
            SignalKind::Command
        );
    }

    #[test]
    fn command_done_and_failed_latch_everything_else_is_live() {
        assert_eq!(SignalReason::Command(CommandReason::Done).lifecycle(), Lifecycle::Latched);
        assert_eq!(SignalReason::Command(CommandReason::Failed).lifecycle(), Lifecycle::Latched);
        assert_eq!(SignalReason::Repo(RepoReason::PendingCommit).lifecycle(), Lifecycle::Live);
        assert_eq!(SignalReason::ClaudeSession(ClaudeSessionReason::Awaiting).lifecycle(), Lifecycle::Live);
    }

    #[test]
    fn command_signal_serializes_with_kind_command_and_lifecycle() {
        let json = serde_json::to_value(
            Signal::new(SignalReason::Command(CommandReason::Done), None, Some("cargo test · 3m12s".into())),
        ).unwrap();
        assert_eq!(json["kind"], "command");
        assert_eq!(json["reason"], "done");
        assert_eq!(json["lifecycle"], "latched");
        assert_eq!(json["urgency"], "attention");
    }

    #[test]
    fn focus_pane_action_round_trips() {
        let action = SignalAction::FocusPane { session: "lanes".into(), pane: 3 };
        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(json["kind"], "focus_pane");
        assert_eq!(json["session"], "lanes");
        assert_eq!(json["pane"], 3);
        let back: SignalAction = serde_json::from_value(json).unwrap();
        assert!(matches!(back, SignalAction::FocusPane { pane: 3, .. }));
    }

    #[test]
    fn urgency_matches_documented_policy_per_reason() {
        assert_eq!(SignalReason::ClaudeSession(ClaudeSessionReason::Permission).urgency(), Urgency::Blocking);
        assert_eq!(SignalReason::ClaudeSession(ClaudeSessionReason::Awaiting).urgency(), Urgency::Attention);
        assert_eq!(SignalReason::ClaudeSession(ClaudeSessionReason::Active).urgency(), Urgency::Info);
        assert_eq!(SignalReason::ClaudeSession(ClaudeSessionReason::Ready).urgency(), Urgency::Attention);
        assert_eq!(SignalReason::Repo(RepoReason::PendingCommit).urgency(), Urgency::Attention);
        assert_eq!(SignalReason::Lanes(LanesReason::SessionMissing).urgency(), Urgency::Blocking);
        assert_eq!(SignalReason::Lanes(LanesReason::SessionNotRunning).urgency(), Urgency::Warning);
    }
}

