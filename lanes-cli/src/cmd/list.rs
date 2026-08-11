use lanes::model::Lane;
use lanes::scope::ScopeElement;

pub fn run(lanes: &[Lane], json: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(lanes).unwrap());
        return;
    }
    if lanes.is_empty() {
        eprintln!("No lanes found in ~/.config/lanes/");
        return;
    }
    for lane in lanes {
        println!("{} ({})", lane.id, lane.name);
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
            }
        }
        for w in &lane.windows {
            println!("  window          {} -> {}", w.path, w.zone);
        }
    }
}
