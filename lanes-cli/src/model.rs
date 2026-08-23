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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Signal {
    pub reason: SignalReason,
    pub urgency: Urgency,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<SignalAction>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalReason {
    PendingCommit,
    ClaudeSessionActive,
    ClaudeSessionAwaiting,
    ClaudeSessionPermission,
}

// Declared least to most urgent so derived Ord/PartialOrd rank them
// correctly (Blocking > Attention > Info) - lets callers compare or sort
// signals by urgency without a separate ranking table.
//
// This is deliberately the naive case: one reason maps to exactly one fixed
// urgency (see SignalReason::urgency() below), with no awareness of other
// signals, staleness, or lane context. A fuller version of this - urgency as
// a function of the whole signal set plus outside context (elapsed time,
// how many other signals are competing for attention, etc.) - is a real
// design problem for later, not solved here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Urgency {
    Info,
    Attention,
    Blocking,
}

impl SignalReason {
    pub fn urgency(&self) -> Urgency {
        match self {
            SignalReason::ClaudeSessionPermission => Urgency::Blocking,
            SignalReason::ClaudeSessionAwaiting | SignalReason::PendingCommit => Urgency::Attention,
            SignalReason::ClaudeSessionActive => Urgency::Info,
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

#[derive(Clone, Serialize, Deserialize)]
pub struct TerminalShape {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub tabs: Vec<TabInfo>,
}

