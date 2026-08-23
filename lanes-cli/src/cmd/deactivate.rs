use lanes::config::Config;

pub fn run(id: Option<String>, cfg: &Config) {
    let lane_id = match lanes::resolve_lane_id(id, cfg) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    };
    if let Err(e) = lanes::config::set_lane_active(&lane_id, false) {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }
    let name = cfg.lanes.iter().find(|l| l.id == lane_id).map(|l| l.name.as_str()).unwrap_or(&lane_id);
    println!("Deactivated: {}", name);
}
