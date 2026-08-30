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
    // Window placement isn't part of scope - unlike everything else here,
    // it has no observable state and nothing to navigate to, it's a pure
    // imperative action ("move this app to this screen zone") triggered on
    // lane focus. Doesn't fit the scope/observation model, so it stays its
    // own thing rather than being forced into a ScopeElement kind with no
    // observations and no real locator identity.
    #[serde(default)]
    pub windows: Vec<WindowPlacement>,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WindowPlacement {
    pub path: String,
    pub zone: String,
}

// --- Signals ---

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignalAction {
    SwitchClaudeSession { session_id: String },
    FocusRepoPane { session: String, path: String },
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Signal {
    #[serde(flatten)]
    pub reason: SignalReason,
    pub urgency: Urgency,
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
}

impl Signal {
    pub fn kind(&self) -> SignalKind {
        self.reason.kind()
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
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeSessionReason {
    Active,
    Awaiting,
    Permission,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepoReason {
    PendingCommit,
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
        }
    }

    pub fn urgency(&self) -> Urgency {
        match self {
            SignalReason::ClaudeSession(ClaudeSessionReason::Permission) => Urgency::Blocking,
            SignalReason::ClaudeSession(ClaudeSessionReason::Awaiting) => Urgency::Attention,
            SignalReason::ClaudeSession(ClaudeSessionReason::Active) => Urgency::Info,
            SignalReason::Repo(RepoReason::PendingCommit) => Urgency::Attention,
            SignalReason::Lanes(LanesReason::SessionMissing) => Urgency::Blocking,
            SignalReason::Lanes(LanesReason::SessionNotRunning) => Urgency::Warning,
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
    Window { path: String, zone: String },
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
        let signal = Signal {
            reason: SignalReason::ClaudeSession(ClaudeSessionReason::Awaiting),
            urgency: Urgency::Attention,
            cyclable: true,
            action: None,
        };
        let json = serde_json::to_value(&signal).unwrap();
        assert_eq!(json["kind"], "claude_session");
        assert_eq!(json["reason"], "awaiting");
        assert_eq!(json["urgency"], "attention");
        assert_eq!(json["cyclable"], true);
        assert!(json.get("action").is_none());
    }

    #[test]
    fn signal_kind_matches_the_reason_it_wraps() {
        assert_eq!(
            Signal {
                reason: SignalReason::Repo(RepoReason::PendingCommit),
                urgency: Urgency::Attention,
                cyclable: false,
                action: None,
            }
            .kind(),
            SignalKind::Repo
        );
        assert_eq!(
            Signal {
                reason: SignalReason::Lanes(LanesReason::SessionNotRunning),
                urgency: Urgency::Warning,
                cyclable: false,
                action: None,
            }
            .kind(),
            SignalKind::Lanes
        );
    }

    #[test]
    fn urgency_matches_documented_policy_per_reason() {
        assert_eq!(SignalReason::ClaudeSession(ClaudeSessionReason::Permission).urgency(), Urgency::Blocking);
        assert_eq!(SignalReason::ClaudeSession(ClaudeSessionReason::Awaiting).urgency(), Urgency::Attention);
        assert_eq!(SignalReason::ClaudeSession(ClaudeSessionReason::Active).urgency(), Urgency::Info);
        assert_eq!(SignalReason::Repo(RepoReason::PendingCommit).urgency(), Urgency::Attention);
        assert_eq!(SignalReason::Lanes(LanesReason::SessionMissing).urgency(), Urgency::Blocking);
        assert_eq!(SignalReason::Lanes(LanesReason::SessionNotRunning).urgency(), Urgency::Warning);
    }
}

