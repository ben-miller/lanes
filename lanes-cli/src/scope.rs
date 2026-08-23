// The scope element model - what a lane's scope is made of, replacing the
// old Facet concept. model::Lane.scope is Vec<ScopeElement>; config loading
// (config.rs) parses TOML into it, gather_lanes() resolves it.
//
// A ScopeElement is a concrete reference to something a lane's scope
// includes, not a predicate. Granularity ("point at a whole project" vs
// "point at one issue within it") lives in the locator itself, not in a
// separate query language or a nested tree structure - the locator is a
// URI whose specificity varies with the kind's own nature. Some kinds
// (ClaudeSession) are naturally atomic and never need more than an
// identity; a kind like a hypothetical Jira project would have a locator
// that goes from a whole-project URI down to a single-issue URI.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScopeElement {
    Repo { locator: String },
    ClaudeSession { locator: String },
    // The Zellij session, not the WezTerm tab it happens to live in -
    // WezTerm-tab resolution is a private detail of navigation (via the
    // existing wezterm-tab-id cache), not a distinct thing worth its own
    // scope element kind. Its tab/pane layout (with each pane's cwd) is
    // observed data, not locator depth - too volatile to be something you'd
    // declare as part of a lane's scope, the same reason a specific pane
    // never gets its own kind either.
    ZellijSession { locator: String },
    // A Trello board or a specific list within one - two depths of the same
    // kind, same locator-encodes-depth pattern as everywhere else. No
    // finer-grained "specific card" depth (yet): a card is closer to a
    // Zellij pane than a Zellij session here - too volatile/numerous to
    // declare individually, better fetched as data under the board/list.
    Trello { locator: String },
}

impl ScopeElement {
    pub fn repo(path: &str) -> Self {
        ScopeElement::Repo { locator: format!("file://{}", path) }
    }

    pub fn claude_session(session_id: &str) -> Self {
        ScopeElement::ClaudeSession { locator: format!("claude://session/{}", session_id) }
    }

    pub fn zellij_session(session_name: &str) -> Self {
        ScopeElement::ZellijSession { locator: format!("zellij://session/{}", session_name) }
    }

    pub fn trello_board(board_id: &str) -> Self {
        ScopeElement::Trello { locator: format!("trello://board/{}", board_id) }
    }

    pub fn trello_list(list_id: &str) -> Self {
        ScopeElement::Trello { locator: format!("trello://list/{}", list_id) }
    }

    /// The raw repo path, if this is a Repo element - the inverse of
    /// `repo()`. Named accessors like this exist so callers extract values
    /// back out of a locator in one place instead of re-typing the scheme
    /// prefix (and risking a typo) at every call site.
    pub fn repo_path(&self) -> Option<&str> {
        match self {
            ScopeElement::Repo { locator } => locator.strip_prefix("file://"),
            _ => None,
        }
    }

    pub fn claude_session_id(&self) -> Option<&str> {
        match self {
            ScopeElement::ClaudeSession { locator } => locator.strip_prefix("claude://session/"),
            _ => None,
        }
    }

    pub fn zellij_session_name(&self) -> Option<&str> {
        match self {
            ScopeElement::ZellijSession { locator } => locator.strip_prefix("zellij://session/"),
            _ => None,
        }
    }

    pub fn trello_board_id(&self) -> Option<&str> {
        match self {
            ScopeElement::Trello { locator } => locator.strip_prefix("trello://board/"),
            _ => None,
        }
    }

    pub fn trello_list_id(&self) -> Option<&str> {
        match self {
            ScopeElement::Trello { locator } => locator.strip_prefix("trello://list/"),
            _ => None,
        }
    }
}

/// A fact currently observed about what a scope element points to - the
/// dynamic counterpart to ScopeElement's static reference. `kind` is a
/// closed vocabulary (eventually), `data` is kind-specific payload.
#[derive(Clone, Debug)]
pub struct Observation {
    pub kind: String,
    pub data: serde_json::Value,
}

// Exposed so callers who already have this data from elsewhere (e.g.
// gather_lanes()'s parallel-prefetched git/layout status) can construct an
// Observation directly instead of going through observe() and re-querying.
pub const KIND_GIT_DIRTY: &str = "git.dirty";
pub const KIND_CLAUDE_SESSION_STATE: &str = "claude.session.state";
pub const KIND_ZELLIJ_SESSION_RUNNING: &str = "zellij.session.running";
pub const KIND_TRELLO_CARD: &str = "trello.card";

/// Resolve one scope element to what's currently true about it - dispatches
/// on kind to the driver that actually knows how to check. Reuses the real
/// driver logic (git_has_changes, claude::enumerate, running_zellij_sessions)
/// rather than re-implementing it here.
pub fn observe(element: &ScopeElement) -> Vec<Observation> {
    match element {
        ScopeElement::Repo { .. } => observe_repo(element),
        ScopeElement::ClaudeSession { .. } => observe_claude_session(element),
        ScopeElement::ZellijSession { .. } => observe_zellij_session(element),
        ScopeElement::Trello { .. } => observe_trello(element),
    }
}

fn observe_repo(element: &ScopeElement) -> Vec<Observation> {
    let Some(path) = element.repo_path() else { return vec![] };
    if crate::git_has_changes(path) {
        vec![Observation { kind: KIND_GIT_DIRTY.to_string(), data: serde_json::json!({}) }]
    } else {
        vec![]
    }
}

fn observe_claude_session(element: &ScopeElement) -> Vec<Observation> {
    let Some(session_id) = element.claude_session_id() else { return vec![] };
    crate::drivers::claude::enumerate()
        .into_iter()
        .find(|s| s.session_id == session_id)
        .map(|s| Observation {
            kind: KIND_CLAUDE_SESSION_STATE.to_string(),
            data: serde_json::json!({ "state": s.state }),
        })
        .into_iter()
        .collect()
}

fn observe_zellij_session(element: &ScopeElement) -> Vec<Observation> {
    let Some(name) = element.zellij_session_name() else { return vec![] };
    let running = crate::running_zellij_sessions().contains(name);
    vec![Observation {
        kind: KIND_ZELLIJ_SESSION_RUNNING.to_string(),
        data: serde_json::json!({ "running": running }),
    }]
}

/// Unlike the local kinds, this needs real credentials and a network call -
/// shells out to curl rather than adding an HTTP client dependency, same
/// "shell out to a CLI" approach used for git/zellij/wezterm elsewhere in
/// this codebase. Quietly returns nothing if TRELLO_API_KEY/TRELLO_API_TOKEN
/// aren't set, same "not configured -> empty" pattern as the old brotab
/// driver had for a missing `bt` binary.
fn observe_trello(element: &ScopeElement) -> Vec<Observation> {
    let (resource, id) = if let Some(board_id) = element.trello_board_id() {
        ("boards", board_id)
    } else if let Some(list_id) = element.trello_list_id() {
        ("lists", list_id)
    } else {
        return vec![];
    };

    let Ok(key) = std::env::var("TRELLO_API_KEY") else { return vec![] };
    let Ok(token) = std::env::var("TRELLO_API_TOKEN") else { return vec![] };

    let url = format!(
        "https://api.trello.com/1/{resource}/{id}/cards?key={key}&token={token}&filter=open&fields=name,shortUrl,due"
    );
    let Ok(output) = std::process::Command::new("curl").args(["-s", &url]).output() else {
        return vec![];
    };
    let Ok(body) = String::from_utf8(output.stdout) else { return vec![] };
    let Ok(cards) = serde_json::from_str::<Vec<serde_json::Value>>(&body) else { return vec![] };

    trello_cards_to_observations(&cards)
}

/// Split from observe_trello() so the actual parsing logic - the part that
/// matters and can go wrong - is unit-testable against a real response
/// shape without needing live credentials or a network call.
fn trello_cards_to_observations(cards: &[serde_json::Value]) -> Vec<Observation> {
    cards.iter()
        .filter_map(|c| {
            let name = c.get("name")?.as_str()?.to_string();
            let url = c.get("shortUrl").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let due = c.get("due").cloned().unwrap_or(serde_json::Value::Null);
            Some(Observation {
                kind: KIND_TRELLO_CARD.to_string(),
                data: serde_json::json!({ "name": name, "url": url, "due": due }),
            })
        })
        .collect()
}

/// Resolve a whole scope (e.g. one lane's), pairing each element with its
/// current observations.
pub fn resolve_scope(scope: &[ScopeElement]) -> Vec<(ScopeElement, Vec<Observation>)> {
    scope.iter().map(|el| (el.clone(), observe(el))).collect()
}

/// The policy layer: which observations are worth surfacing to a human as
/// an actionable chip, and how. Not a pure filter - for Claude sessions
/// every observed state maps to *some* signal (busy sessions still show,
/// just with a different reason), it only actually filters out the repo
/// case, where observe_repo() simply won't have produced a git.dirty
/// observation if the repo is clean.
pub fn signals_from(resolved: &[(ScopeElement, Vec<Observation>)]) -> Vec<crate::model::Signal> {
    // A Repo element has no session of its own to navigate to - that's the
    // whole reason ZellijSession exists as a separate kind. Use whichever
    // one's present elsewhere in the same scope as the navigation anchor.
    let zellij_session = resolved.iter().find_map(|(el, _)| el.zellij_session_name());

    resolved.iter()
        .flat_map(|(el, obs)| {
            obs.iter().filter_map(move |o| signal_for(el, o, zellij_session))
        })
        .collect()
}

fn signal_for(element: &ScopeElement, obs: &Observation, zellij_session: Option<&str>) -> Option<crate::model::Signal> {
    use crate::model::{Signal, SignalAction, SignalReason};

    match (element, obs.kind.as_str()) {
        (ScopeElement::Repo { .. }, KIND_GIT_DIRTY) => {
            let path = element.repo_path()?;
            // Still a signal even without a sibling ZellijSession to
            // navigate to - just not a clickable one. Matches the pre-scope
            // behavior, where a repo facet in a lane with no terminal facet
            // still showed pending-commit, only without an action.
            let action = zellij_session.map(|session| SignalAction::FocusRepoPane {
                session: session.to_string(),
                path: path.to_string(),
            });
            Some(Signal { reason: SignalReason::PendingCommit, urgency: SignalReason::PendingCommit.urgency(), action })
        }

        (ScopeElement::ClaudeSession { .. }, KIND_CLAUDE_SESSION_STATE) => {
            let session_id = element.claude_session_id()?;
            let state = obs.data.get("state").and_then(|v| v.as_str()).unwrap_or("");
            let reason = match state {
                "idle" => SignalReason::ClaudeSessionAwaiting,
                "permission_pending" => SignalReason::ClaudeSessionPermission,
                _ => SignalReason::ClaudeSessionActive,
            };
            Some(Signal {
                urgency: reason.urgency(),
                reason,
                action: Some(SignalAction::SwitchClaudeSession { session_id: session_id.to_string() }),
            })
        }

        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_locator_is_a_file_uri() {
        let el = ScopeElement::repo("/Users/bmiller/src/infra");
        assert_eq!(
            el,
            ScopeElement::Repo { locator: "file:///Users/bmiller/src/infra".to_string() }
        );
    }

    #[test]
    fn claude_session_locator_is_identity_based() {
        let el = ScopeElement::claude_session("d58b14eb-99bb-4b5d-8f0e-09ac662a8be9");
        assert_eq!(
            el,
            ScopeElement::ClaudeSession {
                locator: "claude://session/d58b14eb-99bb-4b5d-8f0e-09ac662a8be9".to_string()
            }
        );
    }

    #[test]
    fn zellij_session_locator_is_identity_based() {
        let el = ScopeElement::zellij_session("infra");
        assert_eq!(
            el,
            ScopeElement::ZellijSession { locator: "zellij://session/infra".to_string() }
        );
    }

    #[test]
    fn trello_board_locator_is_identity_based() {
        let el = ScopeElement::trello_board("abc123");
        assert_eq!(el, ScopeElement::Trello { locator: "trello://board/abc123".to_string() });
    }

    #[test]
    fn trello_list_locator_is_identity_based() {
        let el = ScopeElement::trello_list("xyz789");
        assert_eq!(el, ScopeElement::Trello { locator: "trello://list/xyz789".to_string() });
    }

    #[test]
    fn accessors_round_trip_the_constructors() {
        assert_eq!(ScopeElement::repo("/a/b").repo_path(), Some("/a/b"));
        assert_eq!(ScopeElement::claude_session("abc").claude_session_id(), Some("abc"));
        assert_eq!(ScopeElement::zellij_session("infra").zellij_session_name(), Some("infra"));
        assert_eq!(ScopeElement::trello_board("abc123").trello_board_id(), Some("abc123"));
        assert_eq!(ScopeElement::trello_list("xyz789").trello_list_id(), Some("xyz789"));
    }

    #[test]
    fn trello_board_and_list_locators_dont_cross_match() {
        let board = ScopeElement::trello_board("abc123");
        assert_eq!(board.trello_list_id(), None);
        let list = ScopeElement::trello_list("xyz789");
        assert_eq!(list.trello_board_id(), None);
    }

    #[test]
    fn parses_trello_cards_into_observations() {
        // Real response shape per Trello's REST API docs: an array of card
        // objects, each with at least the fields we requested via `fields=`.
        let cards: Vec<serde_json::Value> = serde_json::from_str(r#"[
            {
                "id": "111",
                "name": "Ship the thing",
                "shortUrl": "https://trello.com/c/abc111",
                "due": "2026-08-20T00:00:00.000Z"
            },
            {
                "id": "222",
                "name": "Write the doc",
                "shortUrl": "https://trello.com/c/abc222",
                "due": null
            }
        ]"#).unwrap();

        let observations = trello_cards_to_observations(&cards);
        assert_eq!(observations.len(), 2);
        assert_eq!(observations[0].kind, KIND_TRELLO_CARD);
        assert_eq!(observations[0].data["name"], "Ship the thing");
        assert_eq!(observations[0].data["url"], "https://trello.com/c/abc111");
        assert_eq!(observations[0].data["due"], "2026-08-20T00:00:00.000Z");
        assert_eq!(observations[1].data["name"], "Write the doc");
        assert!(observations[1].data["due"].is_null());
    }

    #[test]
    fn skips_cards_missing_a_name() {
        let cards: Vec<serde_json::Value> = serde_json::from_str(r#"[
            {"id": "111", "shortUrl": "https://trello.com/c/abc111"}
        ]"#).unwrap();
        assert!(trello_cards_to_observations(&cards).is_empty());
    }

    #[test]
    fn claude_permission_signal_is_blocking() {
        let resolved = vec![(
            ScopeElement::claude_session("abc"),
            vec![Observation { kind: KIND_CLAUDE_SESSION_STATE.to_string(), data: serde_json::json!({ "state": "permission_pending" }) }],
        )];
        let signals = signals_from(&resolved);
        assert_eq!(signals[0].urgency, crate::model::Urgency::Blocking);
    }

    #[test]
    fn claude_idle_and_pending_commit_signals_are_attention_not_blocking() {
        let resolved = vec![(
            ScopeElement::claude_session("abc"),
            vec![Observation { kind: KIND_CLAUDE_SESSION_STATE.to_string(), data: serde_json::json!({ "state": "idle" }) }],
        )];
        let signals = signals_from(&resolved);
        assert_eq!(signals[0].urgency, crate::model::Urgency::Attention);

        let resolved = vec![(ScopeElement::repo("/a/b"), vec![Observation { kind: KIND_GIT_DIRTY.to_string(), data: serde_json::json!({}) }])];
        let signals = signals_from(&resolved);
        assert_eq!(signals[0].urgency, crate::model::Urgency::Attention);
    }

    #[test]
    fn claude_active_signal_is_merely_informational() {
        let resolved = vec![(
            ScopeElement::claude_session("abc"),
            vec![Observation { kind: KIND_CLAUDE_SESSION_STATE.to_string(), data: serde_json::json!({ "state": "running" }) }],
        )];
        let signals = signals_from(&resolved);
        assert_eq!(signals[0].urgency, crate::model::Urgency::Info);
    }

    #[test]
    fn urgency_ordering_ranks_blocking_above_attention_above_info() {
        use crate::model::Urgency;
        assert!(Urgency::Blocking > Urgency::Attention);
        assert!(Urgency::Attention > Urgency::Info);
    }

    #[test]
    fn accessors_return_none_for_the_wrong_kind() {
        let repo = ScopeElement::repo("/a/b");
        assert_eq!(repo.claude_session_id(), None);
        assert_eq!(repo.zellij_session_name(), None);
    }

    #[test]
    fn repo_dirty_signal_uses_sibling_zellij_session_for_navigation() {
        let resolved = vec![
            (
                ScopeElement::repo("/Users/bmiller/src/infra"),
                vec![Observation { kind: "git.dirty".to_string(), data: serde_json::json!({}) }],
            ),
            (ScopeElement::zellij_session("infra"), vec![]),
        ];
        let signals = signals_from(&resolved);
        assert_eq!(signals.len(), 1);
        assert!(matches!(signals[0].reason, crate::model::SignalReason::PendingCommit));
        match &signals[0].action {
            Some(crate::model::SignalAction::FocusRepoPane { session, path }) => {
                assert_eq!(session, "infra");
                assert_eq!(path, "/Users/bmiller/src/infra");
            }
            other => panic!("expected FocusRepoPane action, got {other:?}"),
        }
    }

    #[test]
    fn repo_signal_has_no_action_without_a_sibling_zellij_session() {
        let resolved = vec![(
            ScopeElement::repo("/Users/bmiller/src/infra"),
            vec![Observation { kind: "git.dirty".to_string(), data: serde_json::json!({}) }],
        )];
        let signals = signals_from(&resolved);
        assert_eq!(signals.len(), 1);
        assert!(matches!(signals[0].reason, crate::model::SignalReason::PendingCommit));
        assert!(signals[0].action.is_none());
    }

    #[test]
    fn clean_repo_produces_no_signal() {
        let resolved = vec![
            (ScopeElement::repo("/Users/bmiller/src/infra"), vec![]),
            (ScopeElement::zellij_session("infra"), vec![]),
        ];
        assert!(signals_from(&resolved).is_empty());
    }

    #[test]
    fn claude_session_state_maps_to_matching_reason() {
        let cases = [
            ("idle", "claude_session_awaiting"),
            ("permission_pending", "claude_session_permission"),
            ("busy", "claude_session_active"),
            ("running", "claude_session_active"),
        ];
        for (state, expected_reason) in cases {
            let resolved = vec![(
                ScopeElement::claude_session("abc123"),
                vec![Observation {
                    kind: "claude.session.state".to_string(),
                    data: serde_json::json!({ "state": state }),
                }],
            )];
            let signals = signals_from(&resolved);
            assert_eq!(signals.len(), 1, "state={state}");
            let reason_json = serde_json::to_value(&signals[0].reason).unwrap();
            assert_eq!(reason_json, expected_reason, "state={state}");
            match &signals[0].action {
                Some(crate::model::SignalAction::SwitchClaudeSession { session_id }) => {
                    assert_eq!(session_id, "abc123");
                }
                other => panic!("expected SwitchClaudeSession action, got {other:?}"),
            }
        }
    }

    #[test]
    fn prints_the_json_shape() {
        let scope = vec![
            ScopeElement::repo("/Users/bmiller/src/infra"),
            ScopeElement::claude_session("d58b14eb-99bb-4b5d-8f0e-09ac662a8be9"),
            ScopeElement::zellij_session("infra"),
            ScopeElement::trello_board("abc123"),
        ];
        let json = serde_json::to_string_pretty(&scope).unwrap();
        println!("{}", json);
        assert!(json.contains("\"kind\": \"repo\""));
        assert!(json.contains("\"kind\": \"claude_session\""));
        assert!(json.contains("\"kind\": \"zellij_session\""));
        assert!(json.contains("\"kind\": \"trello\""));
    }
}
