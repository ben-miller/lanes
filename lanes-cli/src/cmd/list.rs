use lanes::model::{Facet, Lane};

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
        if let Some(name) = &lane.name {
            println!("{} ({})", lane.id, name);
        } else {
            println!("{}", lane.id);
        }
        for facet in &lane.facets {
            match facet {
                Facet::Terminal { session } => {
                    println!("  terminal  session={}", session);
                }
                Facet::Window { path, zone } => {
                    println!("  window    {} -> {}", path, zone);
                }
            }
        }
    }
}
