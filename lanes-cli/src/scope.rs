// The scope element model - what a lane's scope is made of, replacing the
// old Facet concept. model::Lane.scope is Vec<ScopeElement>; config loading
// (config.rs) parses TOML into it, and `lanes list`/`lanes doctor` read its
// accessors directly for display. The observe()/signals_from() resolution
// pipeline that used to sit below this (turning a ScopeElement into a
// Signal by polling drivers directly, including Trello card support) was
// deleted once gather_lanes() - its only caller - was removed: lensd's own
// push-based Mirror (lensd.rs) is the one remaining way lane signals get
// built, and it's shaped nothing like this pull-based pipeline was. Trello
// support would need rebuilding as a real lensd source (same shape as
// source::boards in the lensd repo), not resurrecting this code, if it's
// ever wanted again - ScopeElement's own Trello variant and accessors are
// unaffected and still in active use.
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

    /// A specific position within a list - e.g. "the 3rd card" - rather than
    /// the whole list. Not a `card://` locator: Trello has no "fetch card N"
    /// endpoint, so this is really "list X, but only fetch as far as
    /// position N" - a query shape on the list locator, encoded as its own
    /// URI suffix the same way board vs. list already share one enum
    /// variant.
    pub fn trello_list_card(list_id: &str, position: u32) -> Self {
        ScopeElement::Trello { locator: format!("trello://list/{}/position/{}", list_id, position) }
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
            // Deliberately excludes the "/position/N" shape - strip_prefix
            // alone would happily return "abc123/position/3" as if it were
            // a bare list id, silently corrupting the plain-list code path
            // instead of falling through to the position-aware one.
            ScopeElement::Trello { locator } => {
                let rest = locator.strip_prefix("trello://list/")?;
                if rest.contains('/') { None } else { Some(rest) }
            }
            _ => None,
        }
    }

    pub fn trello_list_card_position(&self) -> Option<(&str, u32)> {
        match self {
            ScopeElement::Trello { locator } => {
                let rest = locator.strip_prefix("trello://list/")?;
                let (list_id, position) = rest.split_once("/position/")?;
                Some((list_id, position.parse().ok()?))
            }
            _ => None,
        }
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
    fn trello_list_card_locator_encodes_list_id_and_position() {
        let el = ScopeElement::trello_list_card("xyz789", 3);
        assert_eq!(el, ScopeElement::Trello { locator: "trello://list/xyz789/position/3".to_string() });
        assert_eq!(el.trello_list_card_position(), Some(("xyz789", 3)));
    }

    #[test]
    fn trello_list_card_locator_does_not_match_plain_list_or_board_accessors() {
        let el = ScopeElement::trello_list_card("xyz789", 3);
        assert_eq!(el.trello_list_id(), None);
        assert_eq!(el.trello_board_id(), None);
    }

    #[test]
    fn plain_trello_list_locator_does_not_match_the_position_accessor() {
        let el = ScopeElement::trello_list("xyz789");
        assert_eq!(el.trello_list_card_position(), None);
        assert_eq!(el.trello_list_id(), Some("xyz789"));
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
    fn accessors_return_none_for_the_wrong_kind() {
        let repo = ScopeElement::repo("/a/b");
        assert_eq!(repo.claude_session_id(), None);
        assert_eq!(repo.zellij_session_name(), None);
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
