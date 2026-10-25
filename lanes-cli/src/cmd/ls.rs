//! `lanes ls`: discovery over every context, lane or board-only. Two
//! tiers, not a facet-summary in between - `lanes ls` alone is just ids,
//! fast to scan; `-l`/`--json` is full detail (every scope element, every
//! target with its placement), the same content `lanes list` used to show
//! before it was folded in here.

use lanes::model::Context;
use lanes::scope::ScopeElement;

pub fn run(contexts: &[Context], long: bool, json: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(contexts).unwrap());
        return;
    }
    if contexts.is_empty() {
        eprintln!("No contexts found in ~/.config/lanes/contexts/");
        return;
    }

    for ctx in contexts {
        // `[x]`/`[ ]` for an active/inactive lane, `[-]` for a context with
        // no lane facet at all - a single glance-able bit, not detail, so
        // it's in the bare listing too.
        let mark = match &ctx.lane {
            Some(lane) if lane.active => "x",
            Some(_) => " ",
            None => "-",
        };
        println!("[{mark}] {} ({})", ctx.id, display_name(ctx));

        if !long {
            continue;
        }

        if let Some(lane) = &ctx.lane {
            for el in &lane.scope {
                match el {
                    ScopeElement::ZellijSession { .. } => {
                        println!("  zellij_session  session={}", el.zellij_session_name().unwrap_or(""));
                    }
                    ScopeElement::Repo { .. } => {
                        println!("  repo            {}", el.repo_path().unwrap_or(""));
                    }
                    ScopeElement::ClaudeSession { .. } => {
                        println!("  claude_session  {}", el.claude_session_id().unwrap_or(""));
                    }
                    ScopeElement::Trello { .. } => {
                        let id = el
                            .trello_board_id()
                            .map(|id| format!("board={id}"))
                            .or_else(|| el.trello_list_id().map(|id| format!("list={id}")))
                            .or_else(|| el.trello_list_card_position().map(|(id, pos)| format!("list={id} position={pos}")))
                            .unwrap_or_default();
                        println!("  trello          {id}");
                    }
                }
            }
            for t in &lane.targets {
                let placement = match (&t.monitor, &t.position) {
                    (Some(m), Some(p)) => format!(" -> {m} ({})", serde_json::to_string(p).unwrap_or_default()),
                    (Some(m), None) => format!(" -> {m}"),
                    (None, _) => String::new(),
                };
                println!("  target          {}{placement}", t.driver.name());
            }
        } else if let Some(board) = &ctx.board {
            // No scope, no targets to place - a board-only context is
            // only ever named, never activated (see DECISIONS.md, "Context
            // vs lane").
            println!("  board           project={}", board.project);
        }
    }
}

/// A lane's own name wins; otherwise fall back to the board's project
/// (already a real human-readable name, same one board.yaml's title has -
/// see DECISIONS.md, "Display names stay denormalized") before finally
/// falling back to the bare, kebab-case id, which is only ever a last
/// resort.
fn display_name(ctx: &Context) -> &str {
    ctx.lane
        .as_ref()
        .map(|l| l.name.as_str())
        .or_else(|| ctx.board.as_ref().map(|b| b.project.as_str()))
        .unwrap_or(&ctx.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lanes::model::BoardFacet;

    #[test]
    fn display_name_prefers_the_lane_name_over_the_board_project() {
        let ctx = Context {
            id: "ctx".to_string(),
            lane: Some(lanes::model::Lane {
                id: "ctx".to_string(),
                name: "Ctx".to_string(),
                active: true,
                scope: vec![],
                targets: vec![],
                layout_template: None,
            }),
            board: Some(BoardFacet { project: "Ctx Board".to_string() }),
        };
        assert_eq!(display_name(&ctx), "Ctx");
    }

    #[test]
    fn display_name_falls_back_to_the_board_project_for_a_board_only_context() {
        let ctx = Context { id: "app-ideas".to_string(), lane: None, board: Some(BoardFacet { project: "App Ideas".to_string() }) };
        assert_eq!(display_name(&ctx), "App Ideas");
    }

    #[test]
    fn display_name_falls_back_to_the_bare_id_when_there_is_no_facet_at_all() {
        let ctx = Context { id: "bare".to_string(), lane: None, board: None };
        assert_eq!(display_name(&ctx), "bare");
    }
}
