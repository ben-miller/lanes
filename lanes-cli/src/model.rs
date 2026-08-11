use serde::{Deserialize, Serialize};

use crate::scope::ScopeElement;

// --- Lane config types ---

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Lane {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub scope: Vec<ScopeElement>,
    // Window placement isn't part of scope - unlike everything else here,
    // it has no observable state and nothing to navigate to, it's a pure
    // imperative action ("move this app to this screen zone") triggered on
    // lane activation. Doesn't fit the scope/observation model, so it stays
    // its own thing rather than being forced into a ScopeElement kind with
    // no observations and no real locator identity.
    #[serde(default)]
    pub windows: Vec<WindowPlacement>,
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
    pub current_lane: Option<String>,
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

