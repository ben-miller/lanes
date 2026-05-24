use serde::{Deserialize, Serialize};

// --- Lane config types ---

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Lane {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub facets: Vec<Facet>,
}

impl Lane {
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }

    pub fn terminal_session(&self) -> Option<&str> {
        self.facets.iter().find_map(|f| {
            if let Facet::Terminal { session } = f {
                Some(session.as_str())
            } else {
                None
            }
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Facet {
    Terminal { session: String },
    Window { path: String, zone: String },
    Repo { path: String },
}

// --- Signals ---

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignalAction {
    SwitchClaudeSession { session_id: String },
    FocusRepoPane { session: String, path: String },
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Signal {
    pub reason: SignalReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<SignalAction>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalReason {
    PendingCommit,
    ClaudeSessionActive,
    ClaudeSessionAwaiting,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
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
}

// --- Selectors (durable handles) ---

#[derive(Clone, Serialize, Deserialize)]
pub struct TerminalSel {
    pub driver: String, // "zellij" | "claude"
    pub id: String,     // session name | session UUID
}

#[derive(Clone, Serialize, Deserialize)]
pub struct BrowserSel {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct EditorSel {
    pub path: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct NotesSel {
    pub vault_path: String,
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

#[derive(Clone, Serialize, Deserialize)]
pub struct BrowserTabInfo {
    pub window_id: String,
    pub tab_id: String,
    pub title: String,
    pub url: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct BrowserShape {
    pub tabs: Vec<BrowserTabInfo>,
}

// --- Driver-specific state ---

#[derive(Clone, Serialize, Deserialize)]
pub struct ClaudeState {
    pub activity: String, // raw registry value: "idle" | "busy" | ...
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ReplState {
    pub activity: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct TabState {
    pub loading: bool,
}

// --- Core types ---

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Selector {
    Terminal(TerminalSel),
    Browser(BrowserSel),
    Editor(EditorSel),
    Notes(NotesSel),
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Shape {
    Terminal(TerminalShape),
    Browser(BrowserShape),
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Idle,
    Busy,
    NeedsAttention,
    Gone,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DriverState {
    Claude(ClaudeState),
    Repl(ReplState),
    Tab(TabState),
}

#[derive(Clone, Serialize, Deserialize)]
pub struct State {
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<DriverState>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Observed {
    pub selector: Selector,
    pub locator: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shape: Option<Shape>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<State>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    pub extra: serde_json::Value,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub taken_at: String, // RFC3339
    pub resources: Vec<Observed>,
}
