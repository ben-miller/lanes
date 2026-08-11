// Draft of the "scope element" model discussed as a successor to Facet.
// NOT wired into Lane/config/gather_lanes yet - this is a sketch to see the
// shape and JSON output before committing to a real migration. model::Lane
// and model::Facet are still what config loading and gather_lanes actually
// use; nothing here touches them.
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
}

/// Sketch of how a lane would look with a scope instead of facets - not the
/// real Lane type, kept separate so this draft can't accidentally break
/// anything real.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LaneScopeDraft {
    pub scope: Vec<ScopeElement>,
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
    fn prints_the_json_shape() {
        let draft = LaneScopeDraft {
            scope: vec![
                ScopeElement::repo("/Users/bmiller/src/infra"),
                ScopeElement::claude_session("d58b14eb-99bb-4b5d-8f0e-09ac662a8be9"),
                ScopeElement::zellij_session("infra"),
            ],
        };
        let json = serde_json::to_string_pretty(&draft).unwrap();
        println!("{}", json);
        assert!(json.contains("\"kind\": \"repo\""));
        assert!(json.contains("\"kind\": \"claude_session\""));
        assert!(json.contains("\"kind\": \"zellij_session\""));
    }
}
