use lanes::model::{Facet, Lane};

pub fn run(lanes: &[Lane]) {
    if lanes.is_empty() {
        eprintln!("No lanes found in ~/.config/lanes/");
        return;
    }
    for lane in lanes {
        println!("{}", lane.display_name());
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
